//! Lightweight projection index for fast paragraph/block lookups.
//!
//! This index sits alongside the `Document` projection and provides O(1)
//! lookups from paragraph ID / block ID to their respective indices. It is
//! updated incrementally when projection windows are patched, avoiding the
//! need to scan all blocks/paragraphs on every edit.

use std::collections::HashMap;

use crate::{Block, BlockId, Document, ParagraphId, RichBlockIdentity, TableCellBlockIdentity, TableIdentity};

use super::ProjectionImpact;

/// A lightweight index over the editor projection that accelerates coordinate
/// lookups needed during incremental patching and cursor resolution.
///
/// The index is cheap to build and cheap to update incrementally. It avoids
/// repeated O(n) scans when resolving paragraph IDs to projection indices.
#[derive(Clone, Debug)]
pub struct Db8ProjectionIndex {
  /// Maps paragraph ID to its index in `Document.ids.paragraph_ids`.
  pub paragraph_to_ix: HashMap<ParagraphId, usize>,
  /// Reverse mapping: paragraph index to paragraph ID (for O(1) index→ID lookup).
  pub paragraph_ix_to_id: Vec<ParagraphId>,
  /// Maps block ID to its index in `Document.blocks`.
  #[allow(dead_code, reason = "reserved for incremental block-level lookup during windowed patching")]
  pub block_to_ix: HashMap<BlockId, usize>,
  /// For each block index, the paragraph index of the first paragraph at or
  /// after this block. Used for fast paragraph-range lookups.
  #[allow(dead_code, reason = "reserved for range-based paragraph lookup during incremental patching")]
  pub block_to_paragraph_start: Vec<usize>,
  rich_node_to_root: HashMap<u128, BlockId>,
}

impl Db8ProjectionIndex {
  /// Build the index from a Document projection.
  #[must_use]
  pub fn build(document: &Document) -> Self {
    let count = document.ids.paragraph_ids.len();
    let mut paragraph_to_ix = HashMap::with_capacity(count);
    let mut paragraph_ix_to_id = Vec::with_capacity(count);
    for (ix, id) in document.ids.paragraph_ids.iter().enumerate() {
      paragraph_to_ix.insert(*id, ix);
      paragraph_ix_to_id.push(*id);
    }

    let mut block_to_ix = HashMap::with_capacity(document.ids.block_ids.len());
    for (ix, id) in document.ids.block_ids.iter().enumerate() {
      block_to_ix.insert(*id, ix);
    }

    let mut block_to_paragraph_start = Vec::with_capacity(document.blocks.len());
    let mut paragraph_count = 0;
    for block in document.blocks.iter() {
      block_to_paragraph_start.push(paragraph_count);
      if matches!(block, Block::Paragraph(_)) {
        paragraph_count += 1;
      }
    }
    let mut rich_node_to_root = HashMap::new();
    for (root, identity) in &document.ids.rich_block_ids {
      index_rich_identity(*root, identity, &mut rich_node_to_root);
    }

    Self {
      paragraph_to_ix,
      paragraph_ix_to_id,
      block_to_ix,
      block_to_paragraph_start,
      rich_node_to_root,
    }
  }

  pub fn apply_patch(&mut self, document: &Document, impact: &ProjectionImpact) {
    let paragraph_before = impact.affected_paragraphs_before.clone();
    let paragraph_after = impact.affected_paragraphs_after.clone();
    let block_before = impact.replaced_blocks_before.clone();
    let block_after = impact.replacement_blocks_after.clone();
    if paragraph_before.start != paragraph_after.start
      || block_before.start != block_after.start
      || paragraph_before.len() != paragraph_after.len()
      || block_before.len() != block_after.len()
    {
      *self = Self::build(document);
      return;
    }
    for id in &self.paragraph_ix_to_id[paragraph_before.clone()] {
      self.paragraph_to_ix.remove(id);
    }
    self
      .paragraph_ix_to_id
      .splice(paragraph_before.clone(), document.ids.paragraph_ids[paragraph_after.clone()].iter().copied());
    for ix in paragraph_after.clone() {
      self.paragraph_to_ix.insert(self.paragraph_ix_to_id[ix], ix);
    }

    let removed = self
      .block_to_ix
      .iter()
      .filter_map(|(id, ix)| block_before.contains(ix).then_some(*id))
      .collect::<Vec<_>>();
    for id in removed {
      self.block_to_ix.remove(&id);
    }
    for ix in block_after.clone() {
      self.block_to_ix.insert(document.ids.block_ids[ix], ix);
    }

    self
      .block_to_paragraph_start
      .splice(block_before, std::iter::repeat_n(0, block_after.len()));
    let mut paragraph = if block_after.start == 0 {
      0
    } else {
      let previous = block_after.start - 1;
      self.block_to_paragraph_start[previous] + usize::from(matches!(document.blocks[previous], Block::Paragraph(_)))
    };
    for block_ix in block_after.start..document.blocks.len() {
      self.block_to_paragraph_start[block_ix] = paragraph;
      paragraph += usize::from(matches!(document.blocks[block_ix], Block::Paragraph(_)));
    }
    self.rich_node_to_root.clear();
    for (root, identity) in &document.ids.rich_block_ids {
      index_rich_identity(*root, identity, &mut self.rich_node_to_root);
    }
  }

  /// Look up the projection index of a paragraph by its stable ID.
  #[must_use]
  pub fn paragraph_index(&self, id: ParagraphId) -> Option<usize> {
    self.paragraph_to_ix.get(&id).copied()
  }

  /// Look up the paragraph ID by its projection index (reverse lookup).
  #[must_use]
  pub fn paragraph_index_to_id(&self, index: usize) -> Option<ParagraphId> {
    self.paragraph_ix_to_id.get(index).copied()
  }

  /// Look up the block index by its stable ID.
  #[must_use]
  pub fn block_index(&self, id: BlockId) -> Option<usize> {
    self.block_to_ix.get(&id).copied()
  }

  /// Get the paragraph index that corresponds to a given block index.
  /// Returns the count of paragraphs before this block.
  #[must_use]
  #[allow(dead_code, reason = "reserved for windowed patching and cursor resolution")]
  pub fn paragraph_start_at_block(&self, block_ix: usize) -> Option<usize> {
    self.block_to_paragraph_start.get(block_ix).copied()
  }

  #[must_use]
  pub fn rich_root_for_node(&self, raw: u128) -> Option<BlockId> {
    self.rich_node_to_root.get(&raw).copied()
  }
}

fn index_rich_identity(root: BlockId, identity: &RichBlockIdentity, index: &mut HashMap<u128, BlockId>) {
  index.insert(root.0, root);
  match identity {
    RichBlockIdentity::Image { caption } => {
      if let Some(caption) = caption {
        index.insert(caption.0, root);
      }
    },
    RichBlockIdentity::Equation { source } => {
      index.insert(source.0, root);
    },
    RichBlockIdentity::Table(table) => index_table_identity(root, table, index),
  }
}

fn index_table_identity(root: BlockId, table: &TableIdentity, index: &mut HashMap<u128, BlockId>) {
  for row in &table.rows {
    index.insert(row.id.0, root);
    for cell in &row.cells {
      index.insert(cell.id.0, root);
      for block in &cell.blocks {
        match block {
          TableCellBlockIdentity::Paragraph(paragraph) => {
            index.insert(paragraph.0, root);
          },
          TableCellBlockIdentity::Table { id, identity } => {
            index.insert(id.0, root);
            index_table_identity(root, identity, index);
          },
        }
      }
    }
  }
}

/// Configuration for retained update compaction in the durable outbox.
#[derive(Clone, Debug)]
pub struct RetainedUpdatePolicy {
  /// Maximum number of retained updates before compaction.
  pub max_retained_count: usize,
  /// Maximum total bytes of retained updates before compaction.
  pub max_retained_bytes: usize,
  /// Number of edits between automatic checkpoint snapshots.
  pub checkpoint_interval_edits: usize,
}

impl Default for RetainedUpdatePolicy {
  fn default() -> Self {
    Self {
      max_retained_count: 5_000,
      max_retained_bytes: 32 * 1024 * 1024, // 32 MB
      checkpoint_interval_edits: 500,
    }
  }
}

impl RetainedUpdatePolicy {
  /// Check whether the given retained updates exceed the policy thresholds.
  #[must_use]
  pub fn should_compact(&self, update_count: usize, total_bytes: usize) -> bool {
    update_count > self.max_retained_count || total_bytes > self.max_retained_bytes
  }

  /// Check whether the edit count since last checkpoint exceeds the
  /// configured checkpoint interval, suggesting a new checkpoint is due.
  #[must_use]
  pub fn should_compact_on_edit_count(&self, edit_count: usize) -> bool {
    edit_count >= self.checkpoint_interval_edits
  }
}
