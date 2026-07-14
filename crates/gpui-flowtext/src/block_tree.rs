//! §act-four M3 — the persistent, monoid-annotated block tree.
//!
//! This is the read model's spine: a **persistent** (immutable, structurally
//! shared) sequence of document blocks that replaces `Arc<Vec<Block>>` +
//! `Arc<Vec<Paragraph>>` + the Fenwick offset index with ONE structure. It is an
//! **implicit treap** (a Cartesian tree keyed by position, heap-ordered by a
//! well-distributed random priority per node), which gives:
//!
//! - `O(1)` clone — a new version shares every unchanged node (one `Arc` bump);
//! - `O(log N)` `split` / `join` / `splice` — a mutation path-copies only the
//!   `O(log N)` nodes root-to-cut and shares the rest, so a new version costs
//!   `O(change · log N)` and an old version costs `O(1)` to keep (undo = restore
//!   a root);
//! - `O(log N)` monoid queries — each node caches its subtree [`Summary`]
//!   (`blocks` count + `bytes` = the document-text byte measure), so
//!   `offset ↔ block` and `block → byte offset` are logarithmic descents with
//!   NO separate index.
//!
//! Leaves hold `Arc<Block>` (shared across versions that didn't touch the
//! block; M4 turns these into lazily-materialized handles). The tree is generic
//! only in spirit — it is concrete to `Block` here so the byte measure and the
//! editor's accessors line up exactly.

use std::sync::Arc;

use crate::{Block, Paragraph, paragraph_runs_len};

/// The monoid carried by every node. All four components add componentwise
/// (identity `{0,0,0,0}`, associative), so subtree summaries compose bottom-up:
///
/// - `blocks` — block count (positional split space);
/// - `paragraphs` — paragraph-block count (paragraph-rank space);
/// - `para_text_bytes` — `Σ runs_len` over paragraph blocks only (objects
///   contribute 0) — the `document.text` paragraph-rope coordinate, from which
///   `paragraph_start` is derived (subsuming the Fenwick `ParagraphOffsetIndex`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Summary {
  pub blocks: usize,
  pub paragraphs: usize,
  pub para_text_bytes: usize,
}

impl Summary {
  #[must_use]
  pub fn of_block(block: &Block) -> Self {
    let (paragraphs, para_text_bytes) = match block {
      Block::Paragraph(paragraph) => (1, paragraph_runs_len(paragraph)),
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => (0, 0),
    };
    Self {
      blocks: 1,
      paragraphs,
      para_text_bytes,
    }
  }

  #[must_use]
  fn combine(self, other: Self) -> Self {
    Self {
      blocks: self.blocks + other.blocks,
      paragraphs: self.paragraphs + other.paragraphs,
      para_text_bytes: self.para_text_bytes + other.para_text_bytes,
    }
  }
}

/// A persistent treap node. Caches subtree size (positional split) and
/// [`Summary`] (offset queries); `priority` heap-orders the treap (balance).
/// `Clone` backs the copy-on-write in-place maps (`update_at`):
/// `Arc::make_mut` clones a node only when a retained version still shares it,
/// so an owned tree mutates in place.
#[derive(Clone, Debug)]
struct Node {
  value: Arc<Block>,
  left: Option<Arc<Node>>,
  right: Option<Arc<Node>>,
  priority: u64,
  size: usize,
  summary: Summary,
}

#[allow(
  clippy::ref_option,
  reason = "the &Option form threads cleanly through the recursive treap walk; per-call .as_ref() adds noise without benefit"
)]
#[must_use]
fn size(node: &Option<Arc<Node>>) -> usize {
  node.as_ref().map_or(0, |node| node.size)
}

#[allow(
  clippy::ref_option,
  reason = "the &Option form threads cleanly through the recursive treap walk; per-call .as_ref() adds noise without benefit"
)]
#[must_use]
fn summary(node: &Option<Arc<Node>>) -> Summary {
  node
    .as_ref()
    .map_or(Summary::default(), |node| node.summary)
}

/// Well-distributed priorities from a global counter via splitmix64 — no
/// wall-clock/RNG dependency, so tests are reproducible, yet the values are
/// scrambled enough to keep the treap `O(log N)` in expectation.
fn next_priority() -> u64 {
  use std::sync::atomic::{AtomicU64, Ordering};
  static COUNTER: AtomicU64 = AtomicU64::new(0);
  let mut z = COUNTER
    .fetch_add(0x9E37_79B9_7F4A_7C15, Ordering::Relaxed)
    .wrapping_add(0x9E37_79B9_7F4A_7C15);
  z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
  z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
  z ^ (z >> 31)
}

/// Assemble a node from a value + priority + subtrees, recomputing the cached
/// size/summary. The caller preserves each existing node's priority through
/// split/merge so the heap order (and thus balance) is stable.
#[must_use]
fn node(value: Arc<Block>, priority: u64, left: Option<Arc<Node>>, right: Option<Arc<Node>>) -> Arc<Node> {
  let value_summary = Summary::of_block(&value);
  Arc::new(Node {
    size: 1 + size(&left) + size(&right),
    summary: summary(&left)
      .combine(value_summary)
      .combine(summary(&right)),
    priority,
    value,
    left,
    right,
  })
}

#[must_use]
fn singleton(value: Arc<Block>) -> Arc<Node> {
  node(value, next_priority(), None, None)
}

/// Merge two subtrees (every element of `left` precedes every element of
/// `right`) into a valid treap. `O(log N)` expected.
fn merge(left: Option<Arc<Node>>, right: Option<Arc<Node>>) -> Option<Arc<Node>> {
  match (left, right) {
    (None, right) => right,
    (left, None) => left,
    (Some(l), Some(r)) => {
      if l.priority >= r.priority {
        let merged = merge(l.right.clone(), Some(r));
        Some(node(l.value.clone(), l.priority, l.left.clone(), merged))
      } else {
        let merged = merge(Some(l), r.left.clone());
        Some(node(r.value.clone(), r.priority, merged, r.right.clone()))
      }
    },
  }
}

#[allow(
  clippy::ref_option,
  reason = "the &Option form threads cleanly through the recursive treap walk; per-call .as_ref() adds noise without benefit"
)]
/// Split a subtree so the left part holds the first `k` elements. `O(log N)`.
fn split(root: &Option<Arc<Node>>, k: usize) -> (Option<Arc<Node>>, Option<Arc<Node>>) {
  let Some(current) = root else {
    return (None, None);
  };
  let left_size = size(&current.left);
  if k <= left_size {
    let (ll, lr) = split(&current.left, k);
    (ll, Some(node(current.value.clone(), current.priority, lr, current.right.clone())))
  } else {
    let (rl, rr) = split(&current.right, k - left_size - 1);
    (Some(node(current.value.clone(), current.priority, current.left.clone(), rl)), rr)
  }
}

/// Sum of `para_text_bytes` over the first `k` PARAGRAPH blocks of the subtree
/// (objects are skipped — they hold 0 paragraphs). Descends by paragraph-rank
/// using the cached `paragraphs` counts. `O(log N)`.
#[allow(
  clippy::ref_option,
  reason = "recursive treap walk over &Option children, consistent with split/size/summary"
)]
fn prefix_para_text_bytes(node: &Option<Arc<Node>>, k: usize) -> usize {
  let Some(current) = node else {
    return 0;
  };
  if k == 0 {
    return 0;
  }
  let left_paragraphs = summary(&current.left).paragraphs;
  if k <= left_paragraphs {
    return prefix_para_text_bytes(&current.left, k);
  }
  let mut acc = summary(&current.left).para_text_bytes;
  let mut remaining = k - left_paragraphs;
  let value = Summary::of_block(&current.value);
  if value.paragraphs == 1 {
    // The current block is the (left_paragraphs + 1)-th paragraph; include it.
    acc += value.para_text_bytes;
    remaining -= 1;
  }
  acc + prefix_para_text_bytes(&current.right, remaining)
}

/// Recompute a node's cached `summary` from its (possibly just-mutated)
/// children and value. `size` is unaffected — these maps never restructure.
fn refresh_summary(node: &mut Node) {
  node.summary = summary(&node.left)
    .combine(Summary::of_block(&node.value))
    .combine(summary(&node.right));
}

/// COW descent to `index`, applying `edit` to that block in place. `make_mut`
/// clones only nodes still shared with a retained version.
fn update_node_at(node: &mut Option<Arc<Node>>, index: usize, edit: impl FnOnce(&mut Block)) {
  let Some(arc) = node else {
    return;
  };
  let current = Arc::make_mut(arc);
  let left_size = size(&current.left);
  match index.cmp(&left_size) {
    std::cmp::Ordering::Less => update_node_at(&mut current.left, index, edit),
    std::cmp::Ordering::Equal => edit(Arc::make_mut(&mut current.value)),
    std::cmp::Ordering::Greater => update_node_at(&mut current.right, index - left_size - 1, edit),
  }
  refresh_summary(current);
}

/// Build a valid treap from a slice of blocks. `O(n log n)` via repeated merge
/// (bulk build is not on any hot path).
fn build(blocks: &[Arc<Block>]) -> Option<Arc<Node>> {
  let mut root = None;
  for block in blocks {
    root = merge(root, Some(singleton(block.clone())));
  }
  root
}

/// The persistent block sequence. Cloning is `O(1)`; mutations return a new
/// tree sharing all untouched structure.
#[derive(Clone, Debug, Default)]
pub struct BlockTree {
  root: Option<Arc<Node>>,
}

impl BlockTree {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  #[must_use]
  pub fn from_blocks(blocks: Vec<Block>) -> Self {
    let leaves: Vec<Arc<Block>> = blocks.into_iter().map(Arc::new).collect();
    Self { root: build(&leaves) }
  }

  #[must_use]
  pub fn len(&self) -> usize {
    size(&self.root)
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.root.is_none()
  }

  /// The block at `index`, or `None` if out of range. `O(log N)`.
  #[must_use]
  pub fn get(&self, index: usize) -> Option<Arc<Block>> {
    self.node_at(index).map(|node| node.value.clone())
  }

  /// A borrow of the block at `index`, tied to `&self` — the zero-copy form the
  /// editor's `document.blocks[i]` / `.get(i)` accessors want. `O(log N)`.
  #[must_use]
  pub fn get_ref(&self, index: usize) -> Option<&Block> {
    self.node_at(index).map(|node| &*node.value)
  }

  /// The first / last block by reference. `O(log N)`.
  #[must_use]
  pub fn first(&self) -> Option<&Block> {
    self.get_ref(0)
  }

  #[must_use]
  pub fn last(&self) -> Option<&Block> {
    let len = self.len();
    if len == 0 { None } else { self.get_ref(len - 1) }
  }

  /// Descend to the node holding `index` (shared by `get`/`get_ref`). `O(log N)`.
  #[must_use]
  fn node_at(&self, index: usize) -> Option<&Arc<Node>> {
    let mut node = self.root.as_ref()?;
    let mut index = index;
    if index >= node.size {
      return None;
    }
    loop {
      let left_size = size(&node.left);
      match index.cmp(&left_size) {
        std::cmp::Ordering::Less => node = node.left.as_ref()?,
        std::cmp::Ordering::Equal => return Some(node),
        std::cmp::Ordering::Greater => {
          index -= left_size + 1;
          node = node.right.as_ref()?;
        },
      }
    }
  }

  /// The number of paragraph blocks in the tree. `O(1)`.
  #[must_use]
  pub fn paragraph_count(&self) -> usize {
    summary(&self.root).paragraphs
  }

  /// A borrow of the `paragraph_ix`-th PARAGRAPH block (objects are skipped in
  /// paragraph-rank space). The zero-copy accessor behind a tree-derived
  /// `document.paragraphs` view (§act-four Slice 4). `O(log N)`.
  #[must_use]
  pub fn paragraph_ref(&self, paragraph_ix: usize) -> Option<&Paragraph> {
    let mut node = self.root.as_ref()?;
    let mut rank = paragraph_ix;
    loop {
      let left_paragraphs = summary(&node.left).paragraphs;
      if rank < left_paragraphs {
        node = node.left.as_ref()?;
        continue;
      }
      rank -= left_paragraphs;
      if let Block::Paragraph(paragraph) = &*node.value {
        if rank == 0 {
          return Some(paragraph);
        }
        rank -= 1;
      }
      node = node.right.as_ref()?;
    }
  }

  /// The block-row index (objects INCLUDED) of the `paragraph_ix`-th paragraph
  /// block — the inverse of `paragraph_ref` in row space. `O(log N)`.
  ///
  /// §perf-heaven T7.11: the tree-native replacement for the O(blocks)
  /// `block_ix_for_paragraph` linear scan. It walks the same
  /// `summary().paragraphs` rank the read model already maintains (updated
  /// `O(change)` by `splice`/`update_at`), accumulating the total block count of
  /// fully-passed subtrees to land on the row. Returns `None` if `paragraph_ix`
  /// is past the last paragraph.
  #[must_use]
  pub fn block_row_for_paragraph_ix(&self, paragraph_ix: usize) -> Option<usize> {
    let mut node = self.root.as_ref()?;
    let mut rank = paragraph_ix;
    let mut row_base = 0usize;
    loop {
      let left_paragraphs = summary(&node.left).paragraphs;
      if rank < left_paragraphs {
        node = node.left.as_ref()?;
        continue;
      }
      rank -= left_paragraphs;
      row_base += size(&node.left);
      if let Block::Paragraph(_) = &*node.value {
        if rank == 0 {
          return Some(row_base);
        }
        rank -= 1;
      }
      row_base += 1;
      node = node.right.as_ref()?;
    }
  }

  /// The paragraph-rank of the paragraph block at block row `row`, i.e. the count
  /// of paragraph blocks strictly before `row`. `Some(rank)` iff the block at
  /// `row` is itself a paragraph; `None` for an object row or out of range.
  /// `O(log N)`.
  ///
  /// §perf-heaven T7.12/T7.13: the tree-native replacement for the object-doc
  /// scans in `document_offset_for_position` and `paragraph_ix_for_block_row`
  /// (both formerly counted `Block::Paragraph` before `row`).
  #[must_use]
  pub fn paragraph_ix_for_block_row(&self, row: usize) -> Option<usize> {
    let mut node = self.root.as_ref()?;
    let mut index = row;
    let mut paragraphs_before = 0usize;
    loop {
      let left_size = size(&node.left);
      if index < left_size {
        node = node.left.as_ref()?;
        continue;
      }
      paragraphs_before += summary(&node.left).paragraphs;
      index -= left_size;
      if index == 0 {
        return matches!(&*node.value, Block::Paragraph(_)).then_some(paragraphs_before);
      }
      if let Block::Paragraph(_) = &*node.value {
        paragraphs_before += 1;
      }
      index -= 1;
      node = node.right.as_ref()?;
    }
  }

  /// The count of paragraph blocks at rows strictly before `row` (any block
  /// kind at `row`; `row >= len` returns the total paragraph count). `O(log N)`.
  ///
  /// §act-ten A10.8: backs the editor's object-selection collapse queries
  /// (`paragraph_before_block`/`paragraph_after_block`), which formerly walked
  /// every block per call — O(visible items x blocks) per layout pass on
  /// object-heavy docs via `paragraph_range_for_item_range`.
  #[must_use]
  pub fn paragraphs_before_row(&self, row: usize) -> usize {
    let Some(mut node) = self.root.as_ref() else {
      return 0;
    };
    let mut index = row;
    let mut paragraphs_before = 0usize;
    loop {
      let left_size = size(&node.left);
      if index < left_size {
        match node.left.as_ref() {
          Some(left) => {
            node = left;
            continue;
          },
          None => return paragraphs_before,
        }
      }
      paragraphs_before += summary(&node.left).paragraphs;
      index -= left_size;
      if index == 0 {
        return paragraphs_before;
      }
      if let Block::Paragraph(_) = &*node.value {
        paragraphs_before += 1;
      }
      index -= 1;
      match node.right.as_ref() {
        Some(right) => node = right,
        None => return paragraphs_before,
      }
    }
  }

  /// Iterate the paragraph blocks in order, by reference (skipping objects) —
  /// backs `document.paragraphs.iter()`. `O(N)`.
  pub fn paragraphs_iter(&self) -> impl Iterator<Item = &Paragraph> + '_ {
    self.iter_blocks().filter_map(|block| match block {
      Block::Paragraph(paragraph) => Some(paragraph),
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => None,
    })
  }

  /// The `document.text` (paragraph-rope) byte offset where paragraph
  /// `paragraph_ix` starts — the tree-native replacement for
  /// `ParagraphOffsetIndex::paragraph_start`. Paragraphs are joined by `\n`, so
  /// `start(p) = (Σ runs_len of paragraphs before p) + newlines_before(p)`,
  /// where `newlines_before(p) = min(p, paragraph_count - 1)` (every paragraph
  /// but the first is preceded by one separator; the last has no trailing one).
  /// `O(log N)`.
  #[must_use]
  pub fn paragraph_start(&self, paragraph_ix: usize) -> usize {
    let total = summary(&self.root).paragraphs;
    prefix_para_text_bytes(&self.root, paragraph_ix) + paragraph_ix.min(total.saturating_sub(1))
  }

  /// Mutate the block at `index` in place. Copy-on-write: `Arc::make_mut`
  /// clones only the `O(log N)` path nodes still shared with a retained
  /// version, so on a uniquely-owned tree this is an in-place edit with no
  /// allocation. Recomputes the path summaries. `O(log N)`.
  pub fn update_at(&mut self, index: usize, edit: impl FnOnce(&mut Block)) {
    if index < self.len() {
      update_node_at(&mut self.root, index, edit);
    }
  }

  /// Replace blocks `range` with `replacement`, returning a NEW tree that
  /// shares every node outside the cut. `O((log N) + |replacement|)`.
  #[must_use]
  pub fn splice(&self, range: std::ops::Range<usize>, replacement: Vec<Block>) -> Self {
    let len = self.len();
    let start = range.start.min(len);
    let end = range.end.min(len).max(start);
    let (left, rest) = split(&self.root, start);
    let (_removed, right) = split(&rest, end - start);
    let middle = build(&replacement.into_iter().map(Arc::new).collect::<Vec<_>>());
    let root = merge(merge(left, middle), right);
    Self { root }
  }

  /// Iterate the blocks in order by reference (in-order traversal, zero-copy).
  /// This is the workhorse behind `document.blocks.iter()`. `O(N)`.
  pub fn iter_blocks(&self) -> impl Iterator<Item = &Block> + '_ {
    let mut stack: Vec<&Node> = Vec::new();
    let mut current = self.root.as_deref();
    std::iter::from_fn(move || {
      while let Some(node) = current {
        stack.push(node);
        current = node.left.as_deref();
      }
      let node = stack.pop()?;
      current = node.right.as_deref();
      Some(&*node.value)
    })
  }

  /// Iterate the blocks in `range` by reference. `O(range.end)` (skips a prefix
  /// of the in-order walk); used by the handful of slice-index read sites. The
  /// range is clamped to `len()`.
  pub fn range(&self, range: std::ops::Range<usize>) -> impl Iterator<Item = &Block> + '_ {
    let len = self.len();
    let start = range.start.min(len);
    let end = range.end.min(len).max(start);
    self.iter_blocks().skip(start).take(end - start)
  }

  /// Iterate the blocks in order (in-order traversal). `O(N)`.
  pub fn iter(&self) -> impl Iterator<Item = Arc<Block>> + '_ {
    let mut stack = Vec::new();
    let mut current = self.root.clone();
    std::iter::from_fn(move || {
      while let Some(node) = current.clone() {
        stack.push(node.clone());
        current = node.left.clone();
      }
      let node = stack.pop()?;
      current = node.right.clone();
      Some(node.value.clone())
    })
  }

  /// Collect the blocks into a `Vec` (for equivalence checks / migration bridge).
  #[must_use]
  pub fn to_vec(&self) -> Vec<Block> {
    self.iter().map(|block| (*block).clone()).collect()
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Paragraph, ParagraphStyle, TextRun};

  fn paragraph(byte_len: usize) -> Block {
    Block::Paragraph(Paragraph {
      style: ParagraphStyle::Normal,
      runs: if byte_len == 0 {
        Vec::new()
      } else {
        vec![TextRun {
          len: byte_len,
          styles: crate::RunStyles::default(),
        }]
      },
      version: 0,
    })
  }

  fn blocks(lens: &[usize]) -> Vec<Block> {
    lens.iter().map(|&len| paragraph(len)).collect()
  }

  /// Check the treap invariants: heap order by priority, and cached
  /// size/summary equal the recomputed values. Also returns the height so the
  /// caller can assert it stays `O(log N)`.
  #[allow(clippy::ref_option, reason = "recursive tree walk over &Option children")]
  fn assert_invariants(node: &Option<Arc<Node>>) -> usize {
    match node {
      None => 0,
      Some(node) => {
        for child in [&node.left, &node.right].into_iter().flatten() {
          assert!(node.priority >= child.priority, "treap heap order violated");
        }
        assert_eq!(node.size, 1 + size(&node.left) + size(&node.right), "size annotation stale");
        let recomputed = summary(&node.left)
          .combine(Summary::of_block(&node.value))
          .combine(summary(&node.right));
        assert_eq!(node.summary, recomputed, "summary annotation stale");
        1 + assert_invariants(&node.left).max(assert_invariants(&node.right))
      },
    }
  }

  #[test]
  fn build_and_accessors() {
    let tree = BlockTree::from_blocks(blocks(&[3, 5, 2, 8]));
    assert_eq!(tree.len(), 4);
    assert_invariants(&tree.root);
  }

  #[test]
  fn borrowing_accessors_match_reference() {
    let reference = blocks(&[3, 5, 0, 8, 2]);
    let tree = BlockTree::from_blocks(reference.clone());
    // get_ref matches indexing the reference Vec.
    for (ix, block) in reference.iter().enumerate() {
      assert_eq!(tree.get_ref(ix), Some(block), "get_ref[{ix}]");
    }
    assert_eq!(tree.get_ref(reference.len()), None);
    assert_eq!(tree.first(), reference.first());
    assert_eq!(tree.last(), reference.last());
    // Borrowing in-order iteration matches the reference Vec exactly.
    assert_eq!(tree.iter_blocks().collect::<Vec<_>>(), reference.iter().collect::<Vec<_>>());
    // Range iteration matches slice iteration, incl. clamping.
    assert_eq!(tree.range(1..4).collect::<Vec<_>>(), reference[1..4].iter().collect::<Vec<_>>());
    assert_eq!(tree.range(3..100).collect::<Vec<_>>(), reference[3..].iter().collect::<Vec<_>>());
    assert_eq!(tree.range(2..2).count(), 0);
    // Empty tree.
    let empty = BlockTree::new();
    assert_eq!(empty.first(), None);
    assert_eq!(empty.last(), None);
    assert_eq!(empty.iter_blocks().count(), 0);
  }

  /// Reference `paragraph_start`, computed the `ParagraphOffsetIndex` way over
  /// the paragraph blocks only: `width(i) = runs_len(i) + separator`, cumulative.
  fn reference_paragraph_start(blocks: &[Block], paragraph_ix: usize) -> usize {
    let widths: Vec<usize> = blocks
      .iter()
      .filter_map(|block| match block {
        Block::Paragraph(paragraph) => Some(paragraph.runs.iter().map(|run| run.len).sum::<usize>()),
        _ => None,
      })
      .collect();
    let mut start = 0;
    for (i, width) in widths.iter().enumerate().take(paragraph_ix) {
      let separator = usize::from(i + 1 < widths.len());
      start += width + separator;
    }
    start
  }

  fn image_block() -> Block {
    Block::Image(crate::ImageBlock {
      asset_id: crate::AssetId(7),
      alt_text: "x".into(),
      caption: None,
      sizing: crate::ImageSizing::Intrinsic,
      alignment: crate::BlockAlignment::Center,
      external_url: None,
      version: 0,
    })
  }

  #[test]
  fn paragraph_view_accessors_skip_objects_and_match_reference() {
    let mixed = vec![paragraph(4), image_block(), paragraph(0), paragraph(6), image_block(), paragraph(2)];
    let tree = BlockTree::from_blocks(mixed.clone());
    let reference: Vec<&Paragraph> = mixed
      .iter()
      .filter_map(|block| match block {
        Block::Paragraph(paragraph) => Some(paragraph),
        _ => None,
      })
      .collect();
    assert_eq!(tree.paragraph_count(), reference.len());
    for (rank, expected) in reference.iter().enumerate() {
      assert_eq!(tree.paragraph_ref(rank), Some(*expected), "paragraph_ref[{rank}]");
    }
    assert_eq!(tree.paragraph_ref(reference.len()), None);
    assert_eq!(tree.paragraphs_iter().collect::<Vec<_>>(), reference);
  }

  #[test]
  fn paragraph_start_matches_offset_index_reference() {
    // Paragraph-only doc.
    let plain = blocks(&[4, 0, 7, 3, 5]);
    let tree = BlockTree::from_blocks(plain.clone());
    assert_eq!(tree.paragraph_count(), 5);
    for p in 0..=5 {
      assert_eq!(
        tree.paragraph_start(p),
        reference_paragraph_start(&plain, p),
        "plain paragraph_start[{p}]"
      );
    }
    // Object-interleaved doc: paragraph rank must skip images.
    let mixed = vec![paragraph(4), image_block(), paragraph(0), paragraph(6), image_block(), paragraph(2)];
    let tree = BlockTree::from_blocks(mixed.clone());
    assert_eq!(tree.paragraph_count(), 4);
    for p in 0..=4 {
      assert_eq!(
        tree.paragraph_start(p),
        reference_paragraph_start(&mixed, p),
        "mixed paragraph_start[{p}]"
      );
    }
    // Splices keep the paragraph monoid correct.
    let spliced = tree.splice(1..3, blocks(&[9, 9]));
    let spliced_ref = {
      let mut v = mixed.clone();
      v.splice(1..3, blocks(&[9, 9]));
      v
    };
    for p in 0..=spliced.paragraph_count() {
      assert_eq!(
        spliced.paragraph_start(p),
        reference_paragraph_start(&spliced_ref, p),
        "spliced paragraph_start[{p}]"
      );
    }
  }

  #[test]
  fn height_stays_logarithmic() {
    // A large tree built + spliced must keep O(log N) height (treap balance),
    // so every query/mutation is logarithmic, not linear.
    let n = 20_000usize;
    let mut tree = BlockTree::from_blocks(blocks(&(0..n).map(|i| 1 + i % 5).collect::<Vec<_>>()));
    let mut height = assert_invariants(&tree.root);
    // log2(20000) ≈ 14.3; a treap's expected height is ~1.4·log2(n). Allow 4×.
    let bound = 4 * (usize::BITS - (n as u32).leading_zeros()) as usize;
    assert!(height <= bound, "initial height {height} exceeds {bound}");
    // A pile of interior splices must not degrade balance.
    let mut rng = Rng::new(12345);
    for _ in 0..500 {
      let start = rng.below(tree.len());
      let end = (start + rng.below(5)).min(tree.len());
      tree = tree.splice(start..end, blocks(&[2, 3]));
    }
    height = assert_invariants(&tree.root);
    let bound = 4 * (usize::BITS - (tree.len() as u32).leading_zeros()) as usize;
    assert!(height <= bound, "post-splice height {height} exceeds {bound} for len {}", tree.len());
  }

  #[test]
  fn cow_maps_match_reference_and_preserve_persistence() {
    let base = BlockTree::from_blocks(blocks(&[1, 2, 3, 4, 5]));
    // Retain a version to prove COW persistence (it must NOT change).
    let retained = base.clone();
    let mut tree = base.clone();

    // update_at: replace one block.
    tree.update_at(2, |block| *block = paragraph(9));
    let mut reference = blocks(&[1, 2, 3, 4, 5]);
    reference[2] = paragraph(9);
    assert_eq!(tree.to_vec(), reference);
    assert_invariants(&tree.root);

    // The retained version is byte-for-byte unchanged (persistence held).
    assert_eq!(retained.to_vec(), blocks(&[1, 2, 3, 4, 5]));
  }

  #[test]
  fn splice_shares_and_matches_reference() {
    let mut reference: Vec<Block> = blocks(&[1, 2, 3, 4, 5]);
    let mut tree = BlockTree::from_blocks(reference.clone());
    // Replace [1,3) with two blocks.
    let repl = blocks(&[9, 8]);
    reference.splice(1..3, repl.clone());
    tree = tree.splice(1..3, repl);
    assert_eq!(tree.to_vec(), reference);
    assert_invariants(&tree.root);
  }

  #[test]
  fn clone_is_structural_and_undo_is_pointer_restore() {
    let v0 = BlockTree::from_blocks(blocks(&[1, 2, 3, 4, 5, 6, 7, 8]));
    let v1 = v0.splice(2..5, blocks(&[100]));
    // v0 is untouched (persistence): undo = keep v0.
    assert_eq!(v0.to_vec(), blocks(&[1, 2, 3, 4, 5, 6, 7, 8]));
    assert_eq!(v1.len(), 6);
    // Structural sharing: cloning v1 is O(1) — same root Arc.
    let v1b = v1.clone();
    assert!(Arc::ptr_eq(v1.root.as_ref().unwrap(), v1b.root.as_ref().unwrap()));
  }

  // ---- Property fuzz: the tree is behaviorally a persistent Vec -------------

  struct Rng(u64);
  impl Rng {
    fn new(seed: u64) -> Self {
      Self(seed.max(1))
    }
    fn next(&mut self) -> u64 {
      let mut x = self.0;
      x ^= x << 13;
      x ^= x >> 7;
      x ^= x << 17;
      self.0 = x;
      x
    }
    fn below(&mut self, bound: usize) -> usize {
      if bound == 0 { 0 } else { (self.next() % bound as u64) as usize }
    }
  }

  #[test]
  fn property_matches_reference_vec_under_random_splices() {
    for seed in 1..200u64 {
      let mut rng = Rng::new(seed);
      let mut reference: Vec<Block> = blocks(&[1, 2, 3]);
      let mut tree = BlockTree::from_blocks(reference.clone());
      // Retain a mid-history version to check persistence (it must not change).
      let mut snapshot: Option<(Vec<Block>, BlockTree)> = None;

      for step in 0..80 {
        let len = reference.len();
        let start = rng.below(len + 1);
        let end = (start + rng.below(len - start + 1)).min(len);
        let insert_count = rng.below(4);
        let repl = blocks(
          &(0..insert_count)
            .map(|i| 1 + ((step + i) % 7))
            .collect::<Vec<_>>(),
        );

        reference.splice(start..end, repl.clone());
        tree = tree.splice(start..end, repl);

        // (I1) content equals the reference Vec.
        assert_eq!(tree.to_vec(), reference, "seed {seed} step {step}: content diverged");
        // (I2) balance + annotation invariants hold.
        assert_invariants(&tree.root);
        // (I3) the paragraph-rope monoid agrees with a cumulative scan
        // (§act-ten A9.5: the block-space `bytes` monoid was removed — dead
        // outside the tree, maintained on every splice; `paragraph_start` is
        // the surviving load-bearing offset space).
        let paragraph_count = reference
          .iter()
          .filter(|block| matches!(block, Block::Paragraph(_)))
          .count();
        let mut cum = 0usize;
        let mut paragraph_rank = 0usize;
        for block in &reference {
          if let Block::Paragraph(paragraph) = block {
            let expected = cum + paragraph_rank.min(paragraph_count.saturating_sub(1));
            assert_eq!(
              tree.paragraph_start(paragraph_rank),
              expected,
              "seed {seed} step {step}: paragraph_start[{paragraph_rank}]"
            );
            cum += paragraph_runs_len(paragraph);
            paragraph_rank += 1;
          }
        }

        // (I4) persistence: a snapshot taken earlier is byte-for-byte unchanged.
        if let Some((snap_vec, snap_tree)) = &snapshot {
          assert_eq!(snap_tree.to_vec(), *snap_vec, "seed {seed} step {step}: snapshot mutated");
        }
        if step == 40 {
          snapshot = Some((reference.clone(), tree.clone()));
        }
      }
    }
  }
}
