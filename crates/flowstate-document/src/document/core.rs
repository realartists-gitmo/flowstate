use std::{ops::Range, sync::Arc};

use crop::Rope;
use gpui::{Hsla, Pixels, SharedString, black, px, rgb};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

// `paragraph_widths` and `paragraph_width` are free helpers that still live in
// the parent module. `ParagraphOffsetIndex`'s methods invoke them.
use super::{paragraph_text_len, paragraph_width, paragraph_widths};

pub const SOFT_LINE_BREAK: char = '\u{2028}';
pub const SOFT_LINE_BREAK_STR: &str = "\u{2028}";

// -- Clipboard fragment ---------------------------------------------------

/// Internal clipboard fragment used to round-trip rich text via the system
/// clipboard. The `format` field acts as a magic string so we can distinguish
/// our payloads from anything else stored in the clipboard's metadata slot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RichClipboardFragment {
  pub format: String,
  #[serde(default)]
  pub paragraphs: Vec<InputParagraph>,
  #[serde(default)]
  pub blocks: Vec<InputBlock>,
  #[serde(default)]
  pub assets: Vec<InputAsset>,
}

// -- Document and paragraphs ---------------------------------------------

#[derive(Clone, Debug)]
pub struct Document {
  pub text: Rope,
  pub paragraphs: Arc<Vec<Paragraph>>,
  pub blocks: Arc<Vec<Block>>,
  pub assets: AssetStore,
  // Auxiliary Fenwick-tree index over per-paragraph byte widths. Kept in sync
  // with `paragraphs` by the edit helpers in `edit_ops`. Not part of the
  // public API.
  pub offset_index: ParagraphOffsetIndex,
  pub theme: DocumentTheme,
}

pub fn paragraphs_mut(document: &mut Document) -> &mut Vec<Paragraph> {
  Arc::make_mut(&mut document.paragraphs)
}

pub fn paragraph_blocks_from_paragraphs(paragraphs: &[Paragraph]) -> Vec<Block> {
  paragraphs.iter().cloned().map(Block::Paragraph).collect()
}

pub fn block_ix_for_paragraph(document: &Document, target_paragraph_ix: usize) -> Option<usize> {
  if document.blocks.len() == document.paragraphs.len()
    && document
      .blocks
      .get(target_paragraph_ix)
      .is_some_and(|block| matches!(block, Block::Paragraph(_)))
  {
    return Some(target_paragraph_ix);
  }

  let mut paragraph_ix = 0;
  for (block_ix, block) in document.blocks.iter().enumerate() {
    if matches!(block, Block::Paragraph(_)) {
      if paragraph_ix == target_paragraph_ix {
        return Some(block_ix);
      }
      paragraph_ix += 1;
    }
  }
  None
}

pub fn document_position_for_offset(document: &Document, offset: DocumentOffset) -> Option<DocumentPosition> {
  let paragraph = document.paragraphs.get(offset.paragraph)?;
  if offset.byte > paragraph_text_len(paragraph) {
    return None;
  }
  Some(DocumentPosition::Text {
    block_ix: block_ix_for_paragraph(document, offset.paragraph)?,
    byte: offset.byte,
  })
}

pub fn document_offset_for_position(document: &Document, position: &DocumentPosition) -> Option<DocumentOffset> {
  match position {
    DocumentPosition::Text { block_ix, byte } => {
      if document.blocks.len() == document.paragraphs.len()
        && let Some(Block::Paragraph(paragraph)) = document.blocks.get(*block_ix)
      {
        if *byte <= paragraph_text_len(paragraph) {
          return Some(DocumentOffset {
            paragraph: *block_ix,
            byte: *byte,
          });
        }
        return None;
      }

      let mut paragraph_ix = 0usize;
      for (ix, block) in document.blocks.iter().enumerate() {
        match block {
          Block::Paragraph(paragraph) => {
            if ix == *block_ix {
              if *byte <= paragraph_text_len(paragraph) {
                return Some(DocumentOffset {
                  paragraph: paragraph_ix,
                  byte: *byte,
                });
              }
              return None;
            }
            paragraph_ix += 1;
          },
          Block::Image(_) | Block::Equation(_) | Block::Table(_) => {
            if ix == *block_ix {
              return None;
            }
          },
        }
      }
      None
    },
    DocumentPosition::Object { .. } | DocumentPosition::TableCell { .. } => None,
  }
}

pub fn update_paragraph_block(document: &mut Document, paragraph_ix: usize) {
  let Some(paragraph) = document.paragraphs.get(paragraph_ix).cloned() else {
    return;
  };
  if let Some(block_ix) = block_ix_for_paragraph(document, paragraph_ix)
    && let Some(block) = Arc::make_mut(&mut document.blocks).get_mut(block_ix)
  {
    *block = Block::Paragraph(paragraph);
  }
}

pub fn replace_paragraph_blocks(document: &mut Document, start_paragraph: usize, old_count: usize, replacements: &[Paragraph]) {
  let mut paragraph_ix = 0;
  let mut output = Vec::with_capacity(document.blocks.len() + replacements.len());
  let mut inserted_replacements = false;

  for block in document.blocks.iter() {
    match block {
      Block::Paragraph(_) if paragraph_ix >= start_paragraph && paragraph_ix < start_paragraph + old_count => {
        if !inserted_replacements {
          output.extend(replacements.iter().cloned().map(Block::Paragraph));
          inserted_replacements = true;
        }
        paragraph_ix += 1;
      },
      Block::Paragraph(paragraph) => {
        if !inserted_replacements && paragraph_ix >= start_paragraph {
          output.extend(replacements.iter().cloned().map(Block::Paragraph));
          inserted_replacements = true;
        }
        output.push(Block::Paragraph(paragraph.clone()));
        paragraph_ix += 1;
      },
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => output.push(block.clone()),
    }
  }

  if !inserted_replacements {
    output.extend(replacements.iter().cloned().map(Block::Paragraph));
  }
  if output.is_empty()
    && let Some(paragraph) = document.paragraphs.first()
  {
    output.push(Block::Paragraph(paragraph.clone()));
  }

  document.blocks = Arc::new(output);
}

/// Fenwick-tree (binary indexed tree) over the byte widths of each paragraph,
/// plus the raw widths. Lets us compute the absolute byte offset of any
/// paragraph in O(log N) and update it incrementally as the document is
/// edited.
#[derive(Clone, Debug)]
pub struct ParagraphOffsetIndex {
  pub widths: Vec<usize>,
  pub tree: Vec<usize>,
}

impl ParagraphOffsetIndex {
  pub fn new(paragraphs: &[Paragraph]) -> Self {
    let mut index = Self {
      widths: paragraph_widths(paragraphs),
      tree: vec![0; paragraphs.len() + 1],
    };
    for ix in 0..index.widths.len() {
      index.add(ix, index.widths[ix] as isize);
    }
    index
  }

  pub fn rebuild(&mut self, paragraphs: &[Paragraph]) {
    *self = Self::new(paragraphs);
  }

  pub fn paragraph_start(&self, paragraph_ix: usize) -> usize {
    self.prefix_sum(paragraph_ix)
  }

  pub fn update_paragraph_width(&mut self, paragraph_ix: usize, paragraphs: &[Paragraph]) {
    let Some(width) = paragraph_width(paragraphs, paragraph_ix) else {
      return;
    };
    let old_width = self.widths[paragraph_ix];
    if old_width == width {
      return;
    }
    self.widths[paragraph_ix] = width;
    self.add(paragraph_ix, width as isize - old_width as isize);
  }

  fn add(&mut self, paragraph_ix: usize, delta: isize) {
    if delta == 0 {
      return;
    }
    let mut ix = paragraph_ix + 1;
    while ix < self.tree.len() {
      self.tree[ix] = self.tree[ix].saturating_add_signed(delta);
      ix += ix & (!ix + 1);
    }
  }

  fn prefix_sum(&self, paragraph_count: usize) -> usize {
    let mut ix = paragraph_count.min(self.widths.len());
    let mut sum = 0;
    while ix > 0 {
      sum += self.tree[ix];
      ix &= ix - 1;
    }
    sum
  }
}
