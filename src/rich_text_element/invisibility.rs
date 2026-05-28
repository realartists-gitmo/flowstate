use std::sync::Arc;

use crop::Rope;
use gpui::{Pixels, Size};
use std::{ops::Range, rc::Rc};

use super::*;

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
  pub(super) paragraph_remainder_items: Vec<Option<usize>>,
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
  pub(super) fn build(document: &Document, invisibility_mode: bool) -> Self {
    let mut visible_blocks = Vec::with_capacity(document.blocks.len());

    for block in document.blocks.iter() {
      match block {
        Block::Paragraph(paragraph) => {
          visible_blocks.push(!invisibility_mode || paragraph_is_visible(paragraph));
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
pub(super) fn paragraph_is_visible(paragraph: &Paragraph) -> bool {
  matches!(
    paragraph.style,
    ParagraphStyle::Pocket | ParagraphStyle::Hat | ParagraphStyle::Block | ParagraphStyle::Tag | ParagraphStyle::Undertag
  ) || paragraph.runs.iter().any(|run| run_is_visible(run.styles))
}

pub(super) const INVISIBILITY_PROJECTED_VERSION_OFFSET: u64 = 0x9E37_79B9_7F4A_7C15;

#[hotpath::measure]
pub(super) fn invisibility_projected_document(document: &Document, paragraph_ix: usize) -> Option<Document> {
  let paragraph = document.paragraphs.get(paragraph_ix)?;
  if !matches!(paragraph.style, ParagraphStyle::Normal) {
    return None;
  }

  let source = paragraph_text(document, paragraph_ix);
  let mut byte = 0usize;
  let mut text = String::new();
  let mut runs = Vec::new();

  for run in &paragraph.runs {
    let start = byte;
    let end = start + run.len;
    byte = end;
    if !run_is_visible(run.styles) {
      continue;
    }
    let piece = source.get(start..end).unwrap_or("");
    if piece.is_empty() {
      continue;
    }
    let prefix = if text.is_empty() { "" } else { " " };
    if !prefix.is_empty() {
      text.push_str(prefix);
      runs.push(TextRun {
        len: prefix.len(),
        styles: RunStyles::default(),
      });
    }
    text.push_str(piece);
    runs.push(TextRun {
      len: piece.len(),
      styles: run.styles,
    });
  }

  if text.is_empty() {
    return None;
  }

  let paragraph = Paragraph {
    style: ParagraphStyle::Normal,
    byte_range: 0..text.len(),
    runs,
    // Give the projected paragraph a distinct cache key from the source
    // paragraph so invisible-mode layout cannot reuse a full-text layout.
    version: paragraph.version.wrapping_add(INVISIBILITY_PROJECTED_VERSION_OFFSET),
  };
  let paragraphs = Arc::new(vec![paragraph.clone()]);
  Some(Document {
    text: Rope::from(text),
    blocks: Arc::new(vec![Block::Paragraph(paragraph)]),
    paragraphs: paragraphs.clone(),
    assets: document.assets.clone(),
    offset_index: ParagraphOffsetIndex::new(&paragraphs),
    theme: document.theme.clone(),
  })
}

#[hotpath::measure]
pub(super) fn run_is_visible(styles: RunStyles) -> bool {
  styles.semantic == RunSemanticStyle::Cite || matches!(styles.highlight, Some(HighlightStyle::Spoken | HighlightStyle::Alternative))
}
