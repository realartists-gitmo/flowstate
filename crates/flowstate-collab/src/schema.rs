//! Shared Loro container schema for Flowstate rich-text documents.
//!
//! The schema uses one `LoroText` per paragraph inside a movable block list.
//! Concurrent edits after a split can therefore converge into the first half of
//! the split paragraph; this is the accepted per-paragraph CRDT trade-off from
//! the implementation plan.

use std::{collections::HashMap, hash::BuildHasher, ops::Range};

use anyhow::Result;
use gpui_flowtext::{
  AssetStore, Block, HighlightStyle, InputBlock, InputBlockAlignment, InputEquationDisplay, InputImageSizing, InputParagraph, InputRun,
  InputTableBlock, RunSemanticStyle, RunStyles, TextRun, input_block_from_block,
};
use loro::{ExpandType, LoroDoc, LoroResult, LoroText, LoroValue, StyleConfig, StyleConfigMap, TextDelta, cursor::PosType};
use serde::{Deserialize, Serialize};

pub const META: &str = "meta";
pub const BLOCKS: &str = "blocks";
pub const META_SCHEMA: &str = "schema";
pub const META_SESSION: &str = "session";
pub const META_TITLE: &str = "title";
pub const SCHEMA_VERSION: i64 = 1;

pub const KIND: &str = "kind";
pub const KIND_PARAGRAPH: &str = "p";
pub const KIND_IMAGE: &str = "image";
pub const KIND_EQUATION: &str = "equation";
pub const KIND_TABLE: &str = "table";
pub const TEXT: &str = "text";
pub const STYLE: &str = "style";
pub const DATA: &str = "data";
pub const REV: &str = "rev";

pub const MARK_SEMANTIC: &str = "sem";
pub const MARK_UNDERLINE: &str = "ul";
pub const MARK_STRIKE: &str = "strike";
pub const MARK_HIGHLIGHT: &str = "hl";

pub type MarkIntervals = [Vec<(Range<usize>, LoroValue)>; 4];

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum BlockPayload {
  Image {
    asset_id: u128,
    mime: String,
    original_name: Option<String>,
    content_hash: u64,
    byte_len: u64,
    alt_text: String,
    caption: Option<InputParagraph>,
    sizing: InputImageSizing,
    alignment: InputBlockAlignment,
  },
  Equation {
    source: String,
    display: InputEquationDisplay,
  },
  Table(InputTableBlock),
}

#[must_use]
pub fn payload_from_block(block: &Block, assets: &AssetStore) -> Option<BlockPayload> {
  match input_block_from_block(block) {
    InputBlock::Paragraph(_) => None,
    InputBlock::Image(image) => {
      let asset = assets.assets.get(&image.asset_id);
      Some(BlockPayload::Image {
        asset_id: image.asset_id.0,
        mime: asset.map_or_else(String::new, |asset| asset.mime_type.to_string()),
        original_name: asset.and_then(|asset| asset.original_name.as_ref().map(ToString::to_string)),
        content_hash: asset.map_or(0, |asset| asset.content_hash),
        byte_len: asset.map_or(0, |asset| asset.bytes.len() as u64),
        alt_text: image.alt_text,
        caption: image.caption,
        sizing: image.sizing,
        alignment: image.alignment,
      })
    },
    InputBlock::Equation(equation) => Some(BlockPayload::Equation {
      source: equation.source,
      display: equation.display,
    }),
    InputBlock::Table(table) => Some(BlockPayload::Table(table)),
  }
}

#[must_use]
pub fn block_from_payload(payload: BlockPayload, _assets: &AssetStore) -> InputBlock {
  match payload {
    BlockPayload::Image {
      asset_id,
      alt_text,
      caption,
      sizing,
      alignment,
      mime: _,
      original_name: _,
      content_hash: _,
      byte_len: _,
    } => InputBlock::Image(gpui_flowtext::InputImageBlock {
      asset_id: gpui_flowtext::AssetId(asset_id),
      alt_text,
      caption,
      sizing,
      alignment,
    }),
    BlockPayload::Equation { source, display } => InputBlock::Equation(gpui_flowtext::InputEquationBlock {
      source,
      syntax: gpui_flowtext::InputEquationSyntax::Latex,
      display,
    }),
    BlockPayload::Table(table) => InputBlock::Table(table),
  }
}

#[must_use]
pub fn new_configured_doc() -> LoroDoc {
  let doc = LoroDoc::new();
  configure_text_styles(&doc);
  doc
}

pub fn configure_text_styles(doc: &LoroDoc) {
  let mut styles = StyleConfigMap::new();
  let no_expand = StyleConfig { expand: ExpandType::None };
  styles.insert(MARK_SEMANTIC.into(), no_expand);
  styles.insert(MARK_UNDERLINE.into(), no_expand);
  styles.insert(MARK_STRIKE.into(), no_expand);
  styles.insert(MARK_HIGHLIGHT.into(), no_expand);
  doc.config_text_style(styles);
}

#[must_use]
pub fn loro_pos(text: &LoroText, utf8_byte: usize) -> usize {
  text
    .convert_pos(utf8_byte, PosType::Bytes, PosType::Unicode)
    .expect("UTF-8 byte offset must be in bounds for LoroText")
}

#[must_use]
pub fn utf8_byte(text: &LoroText, loro_pos: usize) -> usize {
  text
    .convert_pos(loro_pos, PosType::Unicode, PosType::Bytes)
    .expect("Unicode offset must be in bounds for LoroText")
}

pub fn set_run_styles_utf8(text: &LoroText, range: Range<usize>, styles: RunStyles) -> LoroResult<()> {
  if range.is_empty() {
    return Ok(());
  }

  match styles.semantic {
    RunSemanticStyle::Plain => unmark_utf8(text, range.clone(), MARK_SEMANTIC)?,
    RunSemanticStyle::Custom(slot) => text.mark_utf8(range.clone(), MARK_SEMANTIC, LoroValue::I64(i64::from(slot)))?,
  }
  if styles.direct_underline {
    text.mark_utf8(range.clone(), MARK_UNDERLINE, LoroValue::Bool(true))?;
  } else {
    unmark_utf8(text, range.clone(), MARK_UNDERLINE)?;
  }
  if styles.strikethrough {
    text.mark_utf8(range.clone(), MARK_STRIKE, LoroValue::Bool(true))?;
  } else {
    unmark_utf8(text, range.clone(), MARK_STRIKE)?;
  }
  if let Some(HighlightStyle::Custom(slot)) = styles.highlight {
    text.mark_utf8(range, MARK_HIGHLIGHT, LoroValue::I64(i64::from(slot)))?;
  } else {
    unmark_utf8(text, range, MARK_HIGHLIGHT)?;
  }
  Ok(())
}

pub fn unmark_utf8(text: &LoroText, range: Range<usize>, key: &str) -> LoroResult<()> {
  if range.is_empty() {
    return Ok(());
  }
  let start = loro_pos(text, range.start);
  let end = loro_pos(text, range.end);
  text.unmark(start..end, key)
}

#[must_use]
pub fn input_runs_from_delta(delta: &[TextDelta]) -> Vec<InputRun> {
  let mut runs: Vec<InputRun> = Vec::new();
  for item in delta {
    let TextDelta::Insert { insert, attributes } = item else {
      continue;
    };
    if insert.is_empty() {
      continue;
    }
    let styles = run_styles_from_attrs(attributes.as_ref());
    if let Some(last) = runs.last_mut()
      && last.styles == styles
    {
      last.text.push_str(insert);
      continue;
    }
    runs.push(InputRun { text: insert.clone(), styles });
  }
  runs
}

#[must_use]
pub fn input_paragraph_from_text(text: &LoroText, style: gpui_flowtext::ParagraphStyle) -> InputParagraph {
  InputParagraph {
    style,
    runs: input_runs_from_delta(&text.to_delta()),
  }
}

#[must_use]
pub fn run_styles_from_attrs<S>(attrs: Option<&HashMap<String, LoroValue, S>>) -> RunStyles
where
  S: BuildHasher,
{
  RunStyles {
    semantic: attrs
      .and_then(|attrs| attrs.get(MARK_SEMANTIC))
      .and_then(loro_i64)
      .and_then(|slot| u8::try_from(slot).ok())
      .map_or(RunSemanticStyle::Plain, RunSemanticStyle::Custom),
    direct_underline: attrs
      .and_then(|attrs| attrs.get(MARK_UNDERLINE))
      .is_some_and(loro_true),
    strikethrough: attrs
      .and_then(|attrs| attrs.get(MARK_STRIKE))
      .is_some_and(loro_true),
    highlight: attrs
      .and_then(|attrs| attrs.get(MARK_HIGHLIGHT))
      .and_then(loro_i64)
      .and_then(|slot| u8::try_from(slot).ok())
      .map(HighlightStyle::Custom),
  }
}

#[must_use]
pub fn mark_intervals_from_runs(runs: &[TextRun]) -> MarkIntervals {
  let mut intervals: MarkIntervals = [Vec::new(), Vec::new(), Vec::new(), Vec::new()];
  let mut byte = 0;
  for run in runs {
    let range = byte..byte + run.len;
    byte = range.end;
    match run.styles.semantic {
      RunSemanticStyle::Plain => {},
      RunSemanticStyle::Custom(slot) => intervals[0].push((range.clone(), LoroValue::I64(i64::from(slot)))),
    }
    if run.styles.direct_underline {
      intervals[1].push((range.clone(), LoroValue::Bool(true)));
    }
    if run.styles.strikethrough {
      intervals[2].push((range.clone(), LoroValue::Bool(true)));
    }
    if let Some(HighlightStyle::Custom(slot)) = run.styles.highlight {
      intervals[3].push((range, LoroValue::I64(i64::from(slot))));
    }
  }
  intervals
}

pub fn apply_mark_intervals(text: &LoroText, intervals: &MarkIntervals) -> Result<()> {
  let keys = [MARK_SEMANTIC, MARK_UNDERLINE, MARK_STRIKE, MARK_HIGHLIGHT];
  for (key, intervals) in keys.into_iter().zip(intervals) {
    for (range, value) in intervals {
      text.mark_utf8(range.clone(), key, value.clone())?;
    }
  }
  Ok(())
}

#[must_use]
pub const fn encode_paragraph_style(style: gpui_flowtext::ParagraphStyle) -> i64 {
  match style {
    gpui_flowtext::ParagraphStyle::Normal => -1,
    gpui_flowtext::ParagraphStyle::Custom(slot) => slot as i64,
  }
}

#[must_use]
pub fn decode_paragraph_style(value: i64) -> gpui_flowtext::ParagraphStyle {
  if value < 0 {
    return gpui_flowtext::ParagraphStyle::Normal;
  }
  u8::try_from(value).map_or(gpui_flowtext::ParagraphStyle::Normal, gpui_flowtext::ParagraphStyle::Custom)
}

fn loro_i64(value: &LoroValue) -> Option<i64> {
  match value {
    LoroValue::I64(value) => Some(*value),
    LoroValue::Null
    | LoroValue::Bool(_)
    | LoroValue::Double(_)
    | LoroValue::Binary(_)
    | LoroValue::String(_)
    | LoroValue::List(_)
    | LoroValue::Map(_)
    | LoroValue::Container(_) => None,
  }
}

fn loro_true(value: &LoroValue) -> bool {
  matches!(value, LoroValue::Bool(true))
}
