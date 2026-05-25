use std::sync::Arc;

use crop::Rope;
use gpui::{Pixels, Size};
use std::rc::Rc;

use super::*;

pub(super) struct ItemSizesCache {
  pub(super) width: Pixels,
  pub(super) item_count: usize,
  pub(super) invisibility_mode: bool,
  pub(super) height_revision: u64,
  pub(super) visibility: VisibilityIndex,
  pub(super) sizes: Rc<Vec<Size<Pixels>>>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct VisibilityIndex {
  visible_blocks: Vec<bool>,
  block_to_paragraph: Vec<Option<usize>>,
}

impl VisibilityIndex {
  pub(super) fn build(document: &Document, invisibility_mode: bool) -> Self {
    let mut visible_blocks = Vec::with_capacity(document.blocks.len());
    let mut block_to_paragraph = Vec::with_capacity(document.blocks.len());
    let mut paragraph_ix = 0usize;

    for block in document.blocks.iter() {
      match block {
        Block::Paragraph(paragraph) => {
          block_to_paragraph.push(Some(paragraph_ix));
          paragraph_ix += 1;
          visible_blocks.push(!invisibility_mode || paragraph_is_visible(paragraph));
        },
        Block::Image(_) | Block::Equation(_) | Block::Table(_) => {
          block_to_paragraph.push(None);
          visible_blocks.push(!invisibility_mode);
        },
      }
    }

    Self {
      visible_blocks,
      block_to_paragraph,
    }
  }

  pub(super) fn is_visible(&self, block_ix: usize) -> bool {
    self.visible_blocks.get(block_ix).copied().unwrap_or(true)
  }

  pub(super) fn paragraph_ix_for_block(&self, block_ix: usize) -> Option<usize> {
    self
      .block_to_paragraph
      .get(block_ix)
      .and_then(|paragraph| *paragraph)
  }
}

pub(super) fn paragraph_is_visible(paragraph: &Paragraph) -> bool {
  matches!(
    paragraph.style,
    ParagraphStyle::Pocket | ParagraphStyle::Hat | ParagraphStyle::Block | ParagraphStyle::Tag | ParagraphStyle::Undertag
  ) || paragraph.runs.iter().any(|run| run_is_visible(run.styles))
}

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
    version: paragraph.version.wrapping_add(0x9E37_79B9_7F4A_7C15),
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

fn run_is_visible(styles: RunStyles) -> bool {
  styles.semantic == RunSemanticStyle::Cite || matches!(styles.highlight, Some(HighlightStyle::Spoken | HighlightStyle::Alternative))
}
