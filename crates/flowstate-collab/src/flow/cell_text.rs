//! Cell-scoped rich-text resolution + executors (flow spec Part B).
//!
//! The .db8 resolution law, applied to ONE cell's flow: identity + cursor over
//! hint, reject before mutation. Cells are paragraph-only and tiny, so the
//! context is a full (O(cell)) snapshot of the live text — boundaries are the
//! `\n` positions, paragraph `i` spans `(boundary[i], boundary[i+1])`.
//! Executors mutate the live containers WITHOUT committing; the flow commit
//! path owns the single commit and the compensation law.

use anyhow::{Context as _, Result};
use flowstate_document::loro_schema::{
  MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE, MARK_PARAGRAPH_STYLE, MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH,
};
use flowstate_document::paragraph_ids_by_boundary_in;
use flowstate_flow::format::CellId;
use flowstate_flow::loro_schema::{cell_paragraph_registry, cell_text, write_paragraph_record};
use gpui_flowtext::local_intents::{FragmentBlock, LocalIntent, TextAnchor};
use gpui_flowtext::{InputParagraph, ParagraphId, ParagraphStyle, RunStyles};
use loro::{ContainerTrait as _, LoroDoc, LoroMap, LoroText, cursor::Cursor};
use rustc_hash::FxHashMap;
use uuid::Uuid;

use super::runtime::FlowWriteRejected;
use crate::crdt_runtime::sentinel_protected_delete_range;

/// The live-text context one cell-text intent resolves against.
pub(crate) struct CellTextContext {
  pub(crate) text: LoroText,
  pub(crate) registry: LoroMap,
  /// The cell's full text, char-indexed (cells are tiny by design).
  chars: Vec<char>,
  /// `\n` boundary positions, ascending — one per paragraph row.
  boundaries: Vec<usize>,
  /// boundary unicode position → durable registry record key.
  record_keys: FxHashMap<usize, String>,
}

impl CellTextContext {
  pub(crate) fn resolve(doc: &LoroDoc, cell: CellId) -> Result<Self, FlowWriteRejected> {
    let text = cell_text(doc, cell).ok_or(FlowWriteRejected::UnknownCell(cell))?;
    let registry = cell_paragraph_registry(doc, cell).ok_or(FlowWriteRejected::UnknownCell(cell))?;
    let chars: Vec<char> = text.to_string().chars().collect();
    let boundaries: Vec<usize> = chars
      .iter()
      .enumerate()
      .filter_map(|(ix, ch)| (*ch == '\n').then_some(ix))
      .collect();
    let record_keys = paragraph_ids_by_boundary_in(doc, &registry, &text);
    Ok(Self {
      text,
      registry,
      chars,
      boundaries,
      record_keys,
    })
  }

  fn len_unicode(&self) -> usize {
    self.chars.len()
  }

  fn paragraph_count(&self) -> usize {
    self.boundaries.len()
  }

  /// Paragraph text span `(start, end)` in unicode positions (exclusive end).
  fn paragraph_span(&self, ix: usize) -> Option<(usize, usize)> {
    let start = self.boundaries.get(ix)? + 1;
    let end = self
      .boundaries
      .get(ix + 1)
      .copied()
      .unwrap_or(self.len_unicode());
    Some((start, end))
  }

  fn paragraph_text(&self, ix: usize) -> Option<String> {
    let (start, end) = self.paragraph_span(ix)?;
    Some(self.chars[start..end].iter().collect())
  }

  /// Map a `(paragraph_ix, byte)` render offset to a live unicode position.
  pub(crate) fn unicode_for_offset(&self, offset: gpui_flowtext::DocumentOffset) -> Option<usize> {
    let (start, end) = self.paragraph_span(offset.paragraph)?;
    let mut unicode = start;
    let mut bytes = 0usize;
    for ch in &self.chars[start..end] {
      if bytes >= offset.byte {
        break;
      }
      bytes += ch.len_utf8();
      unicode += 1;
    }
    Some(unicode)
  }

  /// Map a live unicode position back to `(paragraph_ix, byte)` render space.
  pub(crate) fn offset_for_unicode(&self, unicode: usize) -> Option<gpui_flowtext::DocumentOffset> {
    let ix = self
      .boundaries
      .partition_point(|boundary| *boundary < unicode)
      .checked_sub(1)?;
    let (start, end) = self.paragraph_span(ix)?;
    let clamped = unicode.clamp(start, end);
    let byte = self.chars[start..clamped]
      .iter()
      .map(|ch| ch.len_utf8())
      .sum();
    Some(gpui_flowtext::DocumentOffset { paragraph: ix, byte })
  }

  /// The resolution law: cursor basis first (must address THIS cell's text
  /// container — foreign cursors fall back), then paragraph identity + clamped
  /// byte hint.
  fn resolve_anchor(&self, doc: &LoroDoc, paragraph_ids: &[ParagraphId], anchor: &TextAnchor) -> Result<usize, FlowWriteRejected> {
    if let Some(encoded) = &anchor.cursor
      && let Ok(cursor) = Cursor::decode(encoded)
      && cursor.container == self.text.id()
      && let Ok(position) = doc.get_cursor_pos(&cursor)
    {
      let pos = if cursor.id.is_some() {
        position.current.pos
      } else {
        // Degraded id-less cursor: resolves to 0 or the whole-text end.
        position.current.pos.min(self.len_unicode())
      };
      // Clamp out of the sentinel: position 0 is never a valid caret.
      return Ok(pos.max(1).min(self.len_unicode()));
    }
    let ix = paragraph_ids
      .iter()
      .position(|id| *id == anchor.paragraph)
      .filter(|ix| *ix < self.paragraph_count())
      .ok_or(FlowWriteRejected::UnresolvedParagraph(anchor.paragraph))?;
    let text = self
      .paragraph_text(ix)
      .ok_or(FlowWriteRejected::UnresolvedParagraph(anchor.paragraph))?;
    let mut byte = anchor.byte_hint.min(text.len());
    while byte > 0 && !text.is_char_boundary(byte) {
      byte -= 1;
    }
    let (start, _) = self
      .paragraph_span(ix)
      .ok_or(FlowWriteRejected::UnresolvedParagraph(anchor.paragraph))?;
    Ok(start + text[..byte].chars().count())
  }

  fn resolve_range(
    &self,
    doc: &LoroDoc,
    paragraph_ids: &[ParagraphId],
    start: &TextAnchor,
    end: &TextAnchor,
  ) -> Result<(usize, usize), FlowWriteRejected> {
    let a = self.resolve_anchor(doc, paragraph_ids, start)?;
    let b = self.resolve_anchor(doc, paragraph_ids, end)?;
    Ok((a.min(b), a.max(b)))
  }

  fn boundary_of_paragraph(&self, paragraph_ids: &[ParagraphId], id: ParagraphId) -> Result<(usize, usize), FlowWriteRejected> {
    let ix = paragraph_ids
      .iter()
      .position(|candidate| *candidate == id)
      .filter(|ix| *ix < self.paragraph_count())
      .ok_or(FlowWriteRejected::UnresolvedParagraph(id))?;
    Ok((ix, self.boundaries[ix]))
  }
}

/// The fully-resolved cell-text execution plan (mutation-free until execute).
pub(crate) enum CellPlan {
  Insert {
    at: usize,
    text: String,
    style_override: Option<RunStyles>,
  },
  Delete {
    start: usize,
    len: usize,
    retire: Vec<String>,
  },
  Split {
    at: usize,
    style: ParagraphStyle,
  },
  Join {
    boundary: usize,
    retire: Option<String>,
  },
  Marks {
    start: usize,
    end: usize,
    styles: RunStyles,
  },
  ParagraphStyles {
    boundaries: Vec<usize>,
    style: ParagraphStyle,
  },
  Replaces {
    /// `(start, end, styles)`, sorted DESCENDING by start; same-paragraph,
    /// non-overlapping.
    matches: Vec<(usize, usize, Option<RunStyles>)>,
    replacement: String,
  },
  Fragment {
    at: usize,
    paragraphs: Vec<InputParagraph>,
  },
}

/// Resolve one editor intent against the cell context. Rejection = zero
/// mutation (I-15).
pub(crate) fn resolve_cell_plan(
  doc: &LoroDoc,
  ctx: &CellTextContext,
  paragraph_ids: &[ParagraphId],
  intent: &LocalIntent,
) -> Result<CellPlan, FlowWriteRejected> {
  match intent {
    LocalIntent::InsertText(intent) => {
      if intent.text.is_empty() {
        return Err(FlowWriteRejected::EmptyIntent);
      }
      if intent.text.contains('\n') {
        return Err(FlowWriteRejected::StructureViolation(
          "cell text inserts are single-paragraph; splits are explicit intents".into(),
        ));
      }
      Ok(CellPlan::Insert {
        at: ctx.resolve_anchor(doc, paragraph_ids, &intent.at)?,
        text: intent.text.clone(),
        style_override: intent.style_override,
      })
    },
    LocalIntent::DeleteRange(intent) => {
      let (start, end) = ctx.resolve_range(doc, paragraph_ids, &intent.start, &intent.end)?;
      let (start, len) = sentinel_protected_delete_range(start, end.saturating_sub(start)).ok_or(FlowWriteRejected::EmptyIntent)?;
      let retire = ctx
        .boundaries
        .iter()
        .filter(|boundary| (start..start + len).contains(boundary))
        .filter_map(|boundary| ctx.record_keys.get(boundary).cloned())
        .collect();
      Ok(CellPlan::Delete { start, len, retire })
    },
    LocalIntent::SplitParagraph(intent) => Ok(CellPlan::Split {
      at: ctx.resolve_anchor(doc, paragraph_ids, &intent.at)?,
      style: intent.inherited_style,
    }),
    LocalIntent::JoinParagraphs(intent) => {
      let (first_ix, _) = ctx.boundary_of_paragraph(paragraph_ids, intent.first)?;
      let (second_ix, second_boundary) = ctx.boundary_of_paragraph(paragraph_ids, intent.second)?;
      if second_ix != first_ix + 1 {
        return Err(FlowWriteRejected::StructureViolation(
          "join targets are no longer adjacent paragraphs".into(),
        ));
      }
      Ok(CellPlan::Join {
        boundary: second_boundary,
        retire: ctx.record_keys.get(&second_boundary).cloned(),
      })
    },
    LocalIntent::SetMarks(intent) => {
      let (start, end) = ctx.resolve_range(doc, paragraph_ids, &intent.start, &intent.end)?;
      if start == end {
        return Err(FlowWriteRejected::EmptyIntent);
      }
      Ok(CellPlan::Marks {
        start,
        end,
        styles: intent.styles,
      })
    },
    LocalIntent::SetParagraphStyle(intent) => {
      let (_, boundary) = ctx.boundary_of_paragraph(paragraph_ids, intent.paragraph)?;
      Ok(CellPlan::ParagraphStyles {
        boundaries: vec![boundary],
        style: intent.style,
      })
    },
    LocalIntent::SetParagraphStyles(intent) => {
      // Stale targets are skipped, not rejected (the .db8 batch law).
      let boundaries: Vec<usize> = intent
        .paragraphs
        .iter()
        .filter_map(|paragraph| ctx.boundary_of_paragraph(paragraph_ids, *paragraph).ok())
        .map(|(_, boundary)| boundary)
        .collect();
      if boundaries.is_empty() {
        return Err(FlowWriteRejected::EmptyIntent);
      }
      Ok(CellPlan::ParagraphStyles {
        boundaries,
        style: intent.style,
      })
    },
    LocalIntent::ReplaceMatches(intent) => {
      if intent.replacement.contains('\n') {
        return Err(FlowWriteRejected::StructureViolation(
          "replacement must not contain structural characters".into(),
        ));
      }
      let mut matches: Vec<(usize, usize, Option<RunStyles>)> = Vec::new();
      for candidate in &intent.matches {
        let Ok((start, end)) = ctx.resolve_range(doc, paragraph_ids, &candidate.start, &candidate.end) else {
          continue;
        };
        // Same-paragraph, non-collapsed: a range crossing a boundary was moved
        // by concurrent edits — skip, don't reject.
        if start == end
          || ctx
            .boundaries
            .iter()
            .any(|boundary| (start..end).contains(boundary))
        {
          continue;
        }
        matches.push((start, end, candidate.styles));
      }
      matches.sort_by_key(|entry| std::cmp::Reverse(entry.0));
      // Descending by start: drop a match that overlaps the previously kept
      // (higher-positioned) one — its end reaches past that match's start.
      matches.dedup_by(|current, previous| current.1 > previous.0);
      if matches.is_empty() {
        return Err(FlowWriteRejected::EmptyIntent);
      }
      Ok(CellPlan::Replaces {
        matches,
        replacement: intent.replacement.clone(),
      })
    },
    LocalIntent::InsertRichFragment(intent) => {
      let mut paragraphs = Vec::with_capacity(intent.blocks.len());
      for block in &intent.blocks {
        match block {
          FragmentBlock::Paragraph(paragraph) => paragraphs.push(paragraph.clone()),
          FragmentBlock::Object(_) => {
            return Err(FlowWriteRejected::StructureViolation("cell flows are paragraph-only".into()));
          },
        }
      }
      if paragraphs.is_empty() {
        return Err(FlowWriteRejected::EmptyIntent);
      }
      Ok(CellPlan::Fragment {
        at: ctx.resolve_anchor(doc, paragraph_ids, &intent.at)?,
        paragraphs,
      })
    },
    LocalIntent::InsertObject(_)
    | LocalIntent::ReplaceObject(_)
    | LocalIntent::DeleteBlocks(_)
    | LocalIntent::MoveBlock(_)
    | LocalIntent::ReplaceEquationSourceRange(_)
    | LocalIntent::ReplaceImageAltText(_)
    | LocalIntent::ReplaceImageCaption(_)
    | LocalIntent::SetImageLayout(_)
    | LocalIntent::Table(_) => Err(FlowWriteRejected::StructureViolation("cell flows are paragraph-only".into())),
  }
}

/// Execute a resolved plan against the live containers. Errors here trigger
/// the commit path's compensation (`revert_to`, origin `"repair"`).
pub(crate) fn execute_cell_plan(ctx: &CellTextContext, plan: &CellPlan) -> Result<Option<usize>> {
  match plan {
    CellPlan::Insert { at, text, style_override } => {
      ctx.text.insert(*at, text).context("inserting cell text")?;
      let inserted = text.chars().count();
      if let Some(styles) = style_override
        && inserted > 0
      {
        replace_run_styles(&ctx.text, *at..*at + inserted, *styles)?;
      }
      Ok(Some(at + inserted))
    },
    CellPlan::Delete { start, len, retire } => {
      ctx
        .text
        .delete(*start, *len)
        .context("deleting cell text range")?;
      for key in retire {
        if ctx.registry.get(key).is_some() {
          ctx
            .registry
            .delete(key)
            .context("retiring merged-away cell paragraph record")?;
        }
      }
      Ok(Some(*start))
    },
    CellPlan::Split { at, style } => {
      ctx
        .text
        .insert(*at, "\n")
        .context("inserting cell split boundary")?;
      ctx
        .text
        .mark(*at..*at + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(*style))
        .context("marking cell split style")?;
      // Sentinel hygiene: strip expand-`After` run keys off the boundary.
      for key in [MARK_RUN_SEMANTIC_STYLE, MARK_HIGHLIGHT_STYLE, MARK_DIRECT_UNDERLINE, MARK_STRIKETHROUGH] {
        ctx
          .text
          .unmark(*at..*at + 1, key)
          .context("unmarking split boundary run keys")?;
      }
      let key = format!("paragraph.{}", Uuid::new_v4().as_u128());
      write_paragraph_record(&ctx.registry, &ctx.text, &key, *at).context("writing split paragraph record")?;
      Ok(Some(at + 1))
    },
    CellPlan::Join { boundary, retire } => {
      ctx
        .text
        .delete(*boundary, 1)
        .context("deleting join boundary")?;
      if let Some(key) = retire
        && ctx.registry.get(key).is_some()
      {
        ctx
          .registry
          .delete(key)
          .context("retiring joined-away paragraph record")?;
      }
      Ok(Some(*boundary))
    },
    CellPlan::Marks { start, end, styles } => {
      replace_run_styles(&ctx.text, *start..*end, *styles)?;
      Ok(None)
    },
    CellPlan::ParagraphStyles { boundaries, style } => {
      for boundary in boundaries {
        ctx
          .text
          .mark(*boundary..*boundary + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(*style))
          .context("marking cell paragraph style")?;
      }
      Ok(None)
    },
    CellPlan::Replaces { matches, replacement } => {
      let mut caret = None;
      for (start, end, styles) in matches {
        ctx
          .text
          .delete(*start, end - start)
          .context("deleting replaced match")?;
        if !replacement.is_empty() {
          ctx
            .text
            .insert(*start, replacement)
            .context("inserting match replacement")?;
          let len = replacement.chars().count();
          if let Some(styles) = styles {
            replace_run_styles(&ctx.text, *start..*start + len, *styles)?;
          }
          caret = Some(start + len);
        } else {
          caret = Some(*start);
        }
      }
      Ok(caret)
    },
    CellPlan::Fragment { at, paragraphs } => {
      let mut pos = *at;
      for (ix, paragraph) in paragraphs.iter().enumerate() {
        if ix > 0 {
          ctx
            .text
            .insert(pos, "\n")
            .context("inserting fragment boundary")?;
          ctx
            .text
            .mark(pos..pos + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(paragraph.style))
            .context("marking fragment paragraph style")?;
          let key = format!("paragraph.{}", Uuid::new_v4().as_u128());
          write_paragraph_record(&ctx.registry, &ctx.text, &key, pos).context("writing fragment paragraph record")?;
          pos += 1;
        }
        for run in &paragraph.runs {
          let len = run.text.chars().count();
          if len == 0 {
            continue;
          }
          ctx
            .text
            .insert(pos, &run.text)
            .context("inserting fragment run")?;
          if run.styles != RunStyles::default() {
            replace_run_styles(&ctx.text, pos..pos + len, run.styles)?;
          }
          pos += len;
        }
      }
      Ok(Some(pos))
    },
  }
}

/// Unmark-then-set over a range (the .db8 `mark_run_styles` law: `SetMarks` is a
/// full restatement of the range's run styles).
fn replace_run_styles(text: &LoroText, range: std::ops::Range<usize>, styles: RunStyles) -> Result<()> {
  for key in [MARK_RUN_SEMANTIC_STYLE, MARK_HIGHLIGHT_STYLE, MARK_DIRECT_UNDERLINE, MARK_STRIKETHROUGH] {
    text
      .unmark(range.clone(), key)
      .context("unmarking run style key")?;
  }
  if let gpui_flowtext::RunSemanticStyle::Custom(slot) = styles.semantic {
    text.mark(range.clone(), MARK_RUN_SEMANTIC_STYLE, i64::from(slot))?;
  }
  if let Some(gpui_flowtext::HighlightStyle::Custom(slot)) = styles.highlight {
    text.mark(range.clone(), MARK_HIGHLIGHT_STYLE, i64::from(slot))?;
  }
  if styles.direct_underline {
    text.mark(range.clone(), MARK_DIRECT_UNDERLINE, true)?;
  }
  if styles.strikethrough {
    text.mark(range, MARK_STRIKETHROUGH, true)?;
  }
  Ok(())
}

pub(crate) fn paragraph_style_value(style: ParagraphStyle) -> i64 {
  match style {
    ParagraphStyle::Normal => 0,
    ParagraphStyle::Custom(slot) => i64::from(slot) + 1,
  }
}
