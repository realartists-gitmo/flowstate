//! Local projection drift detection.

use std::hash::{Hash, Hasher};

use gpui_flowtext::{Block, Document, paragraph_text};
use twox_hash::XxHash64;

#[must_use]
pub fn projection_hash(document: &Document) -> u64 {
  let mut hasher = XxHash64::default();
  let mut paragraph_ix = 0;
  for block in document.blocks.iter() {
    match block {
      Block::Paragraph(paragraph) => {
        paragraph.style.hash(&mut hasher);
        for run in &paragraph.runs {
          run.len.hash(&mut hasher);
          run.styles.hash(&mut hasher);
        }
        paragraph_text(document, paragraph_ix)
          .as_bytes()
          .hash(&mut hasher);
        "p".hash(&mut hasher);
        paragraph_ix += 1;
      },
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => {
        std::mem::discriminant(block).hash(&mut hasher);
        format!("{block:?}").hash(&mut hasher);
      },
    }
  }
  hasher.finish()
}
