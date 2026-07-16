use std::sync::Arc;

use crop::Rope;
use gpui::{Pixels, Size};
use std::{ops::Range, rc::Rc};

use super::*;

pub(super) const NO_REMAINDER_ITEM: u32 = u32::MAX;

pub(super) struct ItemSizesCache {
  pub(super) width: Pixels,
  pub(super) block_count: usize,
  pub(super) item_count: usize,
  pub(super) invisibility_mode: bool,
  pub(super) height_revision: u64,
  pub(super) items: Rc<Vec<VirtualItem>>,
  pub(super) block_item_ranges: Vec<Range<usize>>,
  pub(super) block_heights: Vec<Pixels>,
  pub(super) paragraph_chunk_item_ranges: Vec<Range<usize>>,
  pub(super) paragraph_remainder_items: Vec<u32>,
  pub(super) sizes: Rc<Vec<Size<Pixels>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) enum VirtualItem {
  HiddenBlock { block_ix: usize },
  ParagraphChunk { block_ix: usize, paragraph_ix: usize, chunk_ix: usize },
  ParagraphRemainder { block_ix: usize, paragraph_ix: usize },
  StructuralBlock { block_ix: usize },
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct VisibilityIndex {
  visible_blocks: Vec<bool>,
}

#[hotpath::measure_all]
impl VisibilityIndex {
  pub(super) fn build(document: &DocumentProjection, invisibility_mode: bool) -> Self {
    let mut visible_blocks = Vec::with_capacity(document.blocks.len());

    for block in document.blocks.iter() {
      match block {
        Block::Paragraph(paragraph) => {
          visible_blocks.push(!invisibility_mode || paragraph_is_visible(document, paragraph));
        },
        Block::Image(_) | Block::Equation(_) | Block::Table(_) => {
          visible_blocks.push(!invisibility_mode);
        },
      }
    }

    Self { visible_blocks }
  }

  pub(super) fn is_visible(&self, block_ix: usize) -> bool {
    self.visible_blocks.get(block_ix).copied().unwrap_or(true)
  }
}

#[hotpath::measure]
pub(super) fn paragraph_is_visible(document: &DocumentProjection, paragraph: &Paragraph) -> bool {
  paragraph_is_visible_for_theme(&document.theme, paragraph)
}

#[hotpath::measure]
pub(super) fn paragraph_is_visible_for_theme(theme: &DocumentTheme, paragraph: &Paragraph) -> bool {
  if paragraph_style_is_visible_for_theme(theme, paragraph.style) {
    return true;
  }
  paragraph
    .runs
    .iter()
    .any(|run| run_is_visible_for_theme(theme, run.styles))
}

pub(super) fn paragraph_style_is_visible_for_theme(theme: &DocumentTheme, style: ParagraphStyle) -> bool {
  matches!(
    style,
    ParagraphStyle::Custom(slot) if theme.invisibility_visible_paragraph_styles.contains(&(slot & 0x7f))
  )
}

pub(super) const INVISIBILITY_PROJECTED_VERSION_OFFSET: u64 = 0x9E37_79B9_7F4A_7C15;

#[hotpath::measure]
pub(super) fn invisibility_projected_document(document: &DocumentProjection, paragraph_ix: usize) -> Option<DocumentProjection> {
  let paragraph = document.paragraphs.get(paragraph_ix)?;
  if paragraph_style_is_visible_for_theme(&document.theme, paragraph.style) {
    return None;
  }

  let (text, runs) = projected_visible_paragraph_text_and_runs(document, paragraph_ix)?;

  let paragraph = Paragraph {
    style: ParagraphStyle::Normal,
    runs,
    // Give the projected paragraph a distinct cache key from the source
    // paragraph so invisible-mode layout cannot reuse a full-text layout.
    version: paragraph
      .version
      .wrapping_add(INVISIBILITY_PROJECTED_VERSION_OFFSET),
  };
  let paragraphs = vec![paragraph.clone()];
  let paragraph_count = paragraphs.len();
  let mut projected = DocumentProjection {
    frontier: document.frontier.clone(),
    text: Rope::from(text),
    blocks: BlockSeq::from_vec(vec![Block::Paragraph(paragraph)]),
    paragraphs: ParagraphSeq::from_vec(paragraphs),
    assets: document.assets.clone(),
    ids: document_ids_for_shape(paragraph_count, 1),
    sections: Arc::new(Vec::new()),
    outline: Arc::new(Vec::new()),
    theme: document.theme.clone(),
  };
  rebuild_document_sections(&mut projected);
  Some(projected)
}

#[hotpath::measure]
pub(super) fn run_is_visible(document: &DocumentProjection, styles: RunStyles) -> bool {
  run_is_visible_for_theme(&document.theme, styles)
}

#[hotpath::measure]
pub(super) fn run_is_visible_for_theme(theme: &DocumentTheme, styles: RunStyles) -> bool {
  match styles.semantic {
    RunSemanticStyle::Plain => {},
    RunSemanticStyle::Custom(slot)
      if theme
        .invisibility_visible_semantic_styles
        .contains(&(slot & 0x7f)) =>
    {
      return true;
    },
    RunSemanticStyle::Custom(_) => {},
  }
  match styles.highlight {
    Some(HighlightStyle::Custom(slot)) => theme
      .invisibility_visible_highlight_styles
      .contains(&(slot & 0x7f)),
    None => false,
  }
}

/// CT-S1: the editor-side remap cache shape — paragraph index → (paragraph
/// version, remap outcome). `None` = the paragraph lays out verbatim.
pub(super) type InvisibilityRemapCache = rustc_hash::FxHashMap<usize, (u64, Option<std::rc::Rc<InvisibilityRemap>>)>;

/// CT-S1: one visible piece of a projected paragraph — a doc-local byte range
/// and where it starts in the projected ("display") text.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct InvisibilitySegment {
  pub doc_start: usize,
  pub doc_end: usize,
  pub display_start: usize,
}

/// CT-S1: the bidirectional byte map for ONE projected paragraph. Everything
/// stored on the editor (selection, comment anchors, find matches, presence)
/// lives in REAL document bytes; the invisibility layout lays out projected
/// text. This is the translation at that boundary — overlays map doc→display
/// before painting, hit tests map display→doc before touching the model.
/// Empty `segments` = the paragraph is fully hidden (never laid out).
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct InvisibilityRemap {
  pub segments: Vec<InvisibilitySegment>,
  pub display_len: usize,
}

impl InvisibilityRemap {
  /// Doc-local byte → display byte. `round_up` decides which way a byte inside
  /// HIDDEN text snaps: range STARTS round up (to the next visible piece) and
  /// range ENDS round down (to the previous visible piece's end), so a range
  /// spanning hidden text maps to the envelope of its visible content.
  pub fn display_for_doc(&self, byte: usize, round_up: bool) -> usize {
    let mut previous_end = 0usize;
    for segment in &self.segments {
      if byte < segment.doc_start {
        return if round_up { segment.display_start } else { previous_end };
      }
      if byte <= segment.doc_end {
        return segment.display_start + (byte - segment.doc_start);
      }
      previous_end = segment.display_start + (segment.doc_end - segment.doc_start);
    }
    previous_end
  }

  /// Display byte → doc-local byte. Separator spaces (elision punctuation
  /// between pieces) snap forward to the next visible piece, so a click on the
  /// gap lands at real text.
  pub fn doc_for_display(&self, byte: usize) -> usize {
    let mut last_doc_end = 0usize;
    for segment in &self.segments {
      if byte < segment.display_start {
        return segment.doc_start;
      }
      let display_end = segment.display_start + (segment.doc_end - segment.doc_start);
      if byte <= display_end {
        return segment.doc_start + (byte - segment.display_start);
      }
      last_doc_end = segment.doc_end;
    }
    last_doc_end
  }
}

/// CT-S1: build the remap for a paragraph under invisibility. Returns `None`
/// when the paragraph lays out VERBATIM (its style is in the visible set — no
/// projection happens); `Some` with empty segments when it is fully hidden.
/// The walk mirrors `projected_visible_paragraph_text_and_runs` (and the prep
/// twin) exactly: same visibility test, same contiguity separator rule — the
/// unit tests below pin the three against each other.
pub(super) fn invisibility_paragraph_remap(theme: &DocumentTheme, paragraph: &Paragraph) -> Option<InvisibilityRemap> {
  if paragraph_style_is_visible_for_theme(theme, paragraph.style) {
    return None;
  }
  let paragraph_len = paragraph_text_len(paragraph);
  let mut segments = Vec::new();
  let mut display_len = 0usize;
  let mut byte = 0usize;
  let mut last_doc_end = None;
  for run in &paragraph.runs {
    let start = byte;
    let end = start + run.len;
    byte = end;
    if start >= end || end > paragraph_len || !run_is_visible_for_theme(theme, run.styles) {
      continue;
    }
    if display_len > 0 && last_doc_end != Some(start) {
      display_len += 1;
    }
    segments.push(InvisibilitySegment {
      doc_start: start,
      doc_end: end,
      display_start: display_len,
    });
    display_len += end - start;
    last_doc_end = Some(end);
  }
  Some(InvisibilityRemap { segments, display_len })
}

#[hotpath::measure]
pub(super) fn encode_remainder_item_ix(item_ix: usize) -> u32 {
  let encoded = u32::try_from(item_ix).expect("virtual item index exceeds u32::MAX");
  assert_ne!(encoded, NO_REMAINDER_ITEM);
  encoded
}

#[hotpath::measure]
pub(super) fn decode_remainder_item_ix(encoded: u32) -> Option<usize> {
  (encoded != NO_REMAINDER_ITEM).then_some(encoded as usize)
}

#[hotpath::measure]
pub(super) fn projected_visible_paragraph_text_and_runs(document: &DocumentProjection, paragraph_ix: usize) -> Option<(String, Vec<TextRun>)> {
  let paragraph = document.paragraphs.get(paragraph_ix)?;
  let paragraph_start = paragraph_byte_range(document, paragraph_ix).start;
  let paragraph_len = paragraph_text_len(paragraph);
  let visible_run_count = paragraph
    .runs
    .iter()
    .filter(|run| run.len > 0 && run_is_visible(document, run.styles))
    .count();
  if visible_run_count == 0 {
    return None;
  }
  let visible_text_len = paragraph
    .runs
    .iter()
    .filter(|run| run_is_visible(document, run.styles))
    .map(|run| run.len)
    .sum::<usize>();
  let mut text = String::with_capacity(visible_text_len.saturating_add(visible_run_count.saturating_sub(1)));
  let mut runs = Vec::with_capacity(visible_run_count.saturating_mul(2).saturating_sub(1));
  let mut byte = 0usize;
  let mut last_doc_index = None;

  for run in &paragraph.runs {
    let start = byte;
    let end = start + run.len;
    byte = end;
    if start >= end || end > paragraph_len || !run_is_visible(document, run.styles) {
      continue;
    }
    if !text.is_empty() && last_doc_index != Some(paragraph_start + start) {
      text.push(' ');
      runs.push(TextRun {
        len: 1,
        styles: RunStyles::default(),
      });
    }
    let piece_start = text.len();
    push_document_text_slice(document, paragraph_start + start..paragraph_start + end, &mut text);
    let piece_len = text.len().saturating_sub(piece_start);
    if piece_len == 0 {
      continue;
    }
    runs.push(TextRun {
      len: piece_len,
      styles: run.styles,
    });
    last_doc_index = Some(paragraph_start + end);
  }

  (!text.is_empty()).then_some((text, runs))
}

#[cfg(test)]
mod invisibility_remap_tests {
  use super::*;

  fn cite() -> RunStyles {
    RunStyles {
      semantic: RunSemanticStyle::Custom(1),
      ..Default::default()
    }
  }

  fn spoken() -> RunStyles {
    RunStyles {
      highlight: Some(HighlightStyle::Custom(1)),
      ..Default::default()
    }
  }

  fn fixture() -> DocumentProjection {
    let mut theme = DocumentTheme::default();
    theme.set_invisibility_visible_semantic_style(1);
    theme.set_invisibility_visible_highlight_style(1);
    document_from_input(
      theme,
      vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![
          InputRun {
            text: "hidden lead ".into(),
            styles: RunStyles::default(),
          },
          InputRun {
            text: "Author '24".into(),
            styles: cite(),
          },
          // Contiguous with the cite run: NO separator between them.
          InputRun {
            text: " spoken tail".into(),
            styles: spoken(),
          },
          InputRun {
            text: " hidden middle ".into(),
            styles: RunStyles::default(),
          },
          InputRun {
            text: "more spoken".into(),
            styles: spoken(),
          },
        ],
      }],
    )
  }

  /// The remap walk and the projected-text walk must agree byte-for-byte:
  /// segments slice the doc text into exactly the projected text.
  #[test]
  fn remap_agrees_with_projected_text() {
    let document = fixture();
    let paragraph = &document.paragraphs[0];
    let (text, _) = projected_visible_paragraph_text_and_runs(&document, 0).expect("projected text");
    let remap = invisibility_paragraph_remap(&document.theme, paragraph).expect("projected paragraph");

    assert_eq!(remap.display_len, text.len(), "remap length must equal projected text length");
    let doc_text = document.text.to_string();
    for segment in &remap.segments {
      assert_eq!(
        &text[segment.display_start..segment.display_start + (segment.doc_end - segment.doc_start)],
        &doc_text[segment.doc_start..segment.doc_end],
        "segment content must match"
      );
    }
    // Contiguous cite+spoken must NOT be separated; hidden middle must be.
    assert_eq!(text, "Author '24 spoken tail more spoken");
  }

  #[test]
  fn doc_and_display_round_trip_in_visible_text() {
    let document = fixture();
    let remap = invisibility_paragraph_remap(&document.theme, &document.paragraphs[0]).expect("projected");
    // A byte inside the cite run ("hidden lead " is 12 bytes, so doc 12.. is visible).
    for doc_byte in [12usize, 15, 21, 23, 30] {
      let display = remap.display_for_doc(doc_byte, true);
      assert_eq!(remap.doc_for_display(display), doc_byte, "round trip for visible doc byte {doc_byte}");
    }
  }

  #[test]
  fn hidden_bytes_snap_directionally() {
    let document = fixture();
    let remap = invisibility_paragraph_remap(&document.theme, &document.paragraphs[0]).expect("projected");
    // Byte 4 is inside "hidden lead " — a range START there snaps up to the
    // cite piece; a range END there snaps down to display 0 (nothing before).
    assert_eq!(remap.display_for_doc(4, true), 0);
    assert_eq!(remap.display_for_doc(4, false), 0);
    // "hidden middle" starts at doc 34 ("hidden lead "=12 + "Author '24"=10 +
    // " spoken tail"=12). A START in it snaps to the "more spoken" piece; an
    // END snaps back to the end of " spoken tail".
    let spoken_tail_display_end = remap.display_for_doc(34, false);
    let more_spoken_display_start = remap.display_for_doc(40, true);
    assert!(spoken_tail_display_end < more_spoken_display_start);
    assert_eq!(remap.doc_for_display(more_spoken_display_start), 49, "snaps to 'more spoken' doc start");
  }

  #[test]
  fn fully_hidden_paragraph_yields_empty_segments() {
    let mut theme = DocumentTheme::default();
    theme.set_invisibility_visible_semantic_style(1);
    let document = document_from_input(
      theme,
      vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![InputRun {
          text: "all hidden".into(),
          styles: RunStyles::default(),
        }],
      }],
    );
    let remap = invisibility_paragraph_remap(&document.theme, &document.paragraphs[0]).expect("hidden paragraph still remaps");
    assert!(remap.segments.is_empty());
    assert_eq!(remap.display_len, 0);
  }

  /// Style-visible paragraphs lay out verbatim — no remap.
  #[test]
  fn style_visible_paragraph_has_no_remap() {
    let mut theme = DocumentTheme::default();
    theme.set_invisibility_visible_paragraph_style(3);
    let document = document_from_input(
      theme,
      vec![InputParagraph {
        style: ParagraphStyle::Custom(3),
        runs: vec![InputRun {
          text: "a tag".into(),
          styles: RunStyles::default(),
        }],
      }],
    );
    assert!(invisibility_paragraph_remap(&document.theme, &document.paragraphs[0]).is_none());
  }
}
