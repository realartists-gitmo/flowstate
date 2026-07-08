// §act-four M3 migration — `BlockSeq`: the tree-backed block sequence that
// replaces `Arc<Vec<Block>>` on `DocumentProjection`.
//
// Read sites keep their shape — `document.blocks[i]`, `.get(i)`, `.iter()`,
// `.len()`, `.first()` all resolve through the persistent `BlockTree` and hand
// back `&Block` (zero copy). Mutation sites go through the `O(log N)` `splice`
// / `set` / `insert` / `remove` fast paths, or — for the few complex sites that
// run several `Vec` ops at once — the `BlockSeq::make_mut` guard (materialize →
// mutate → rebuild). Cloning is `O(1)` (the tree root is one `Arc`), which is
// exactly what lets a version's blocks be a pointer to restore on undo instead
// of a rebuilt `Vec`.

use crate::block_tree::BlockTree;

/// A persistent, tree-backed block sequence. Drop-in for the old
/// `Arc<Vec<Block>>` field: `O(1)` clone, `O(log N)` random access + byte
/// queries, structural sharing across document versions.
#[derive(Clone, Debug, Default)]
pub struct BlockSeq {
  tree: BlockTree,
}

impl BlockSeq {
  #[must_use]
  pub fn from_vec(blocks: Vec<Block>) -> Self {
    Self {
      tree: BlockTree::from_blocks(blocks),
    }
  }

  #[must_use]
  pub fn from_tree(tree: BlockTree) -> Self {
    Self { tree }
  }

  /// The backing tree — for the version graph (store a root) and byte queries.
  #[must_use]
  pub fn tree(&self) -> &BlockTree {
    &self.tree
  }

  #[must_use]
  pub fn len(&self) -> usize {
    self.tree.len()
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.tree.is_empty()
  }

  /// The block at `index`, by reference. `O(log N)`.
  #[must_use]
  pub fn get(&self, index: usize) -> Option<&Block> {
    self.tree.get_ref(index)
  }

  #[must_use]
  pub fn first(&self) -> Option<&Block> {
    self.tree.first()
  }

  #[must_use]
  pub fn last(&self) -> Option<&Block> {
    self.tree.last()
  }

  /// In-order iteration by reference — backs `document.blocks.iter()`. `O(N)`.
  pub fn iter(&self) -> impl Iterator<Item = &Block> + '_ {
    self.tree.iter_blocks()
  }

  /// Iterate `range` by reference — backs the `blocks[a..b]` read sites. The
  /// range is clamped to `len()`.
  pub fn range(&self, range: std::ops::Range<usize>) -> impl Iterator<Item = &Block> + '_ {
    self.tree.range(range)
  }

  /// Bounds-checked range iteration mirroring slice `get(a..b)`: `None` if the
  /// range is inverted or past the end (preserves the `?`-bail read sites).
  pub fn get_range(&self, range: std::ops::Range<usize>) -> Option<impl Iterator<Item = &Block> + '_> {
    if range.start > range.end || range.end > self.len() {
      return None;
    }
    Some(self.tree.range(range))
  }

  /// Materialize the whole sequence into an owned `Vec` (equivalence checks,
  /// serialization, and the few sites that genuinely need a contiguous copy).
  #[must_use]
  pub fn to_vec(&self) -> Vec<Block> {
    self.tree.to_vec()
  }

  /// Cumulative document-text byte offset BEFORE block `index`. `O(log N)`.
  #[must_use]
  pub fn byte_offset(&self, index: usize) -> usize {
    self.tree.byte_offset(index)
  }

  /// `(block_index, byte_within_block)` for document byte `byte`. `O(log N)`.
  #[must_use]
  pub fn block_at_byte(&self, byte: usize) -> Option<(usize, usize)> {
    self.tree.block_at_byte(byte)
  }

  /// The number of paragraph blocks. `O(1)`.
  #[must_use]
  pub fn paragraph_count(&self) -> usize {
    self.tree.paragraph_count()
  }

  /// The `paragraph_ix`-th paragraph block by reference (objects skipped) — the
  /// zero-copy accessor behind a tree-derived `document.paragraphs` view
  /// (§act-four Slice 4). `O(log N)`.
  #[must_use]
  pub fn paragraph_ref(&self, paragraph_ix: usize) -> Option<&Paragraph> {
    self.tree.paragraph_ref(paragraph_ix)
  }

  /// Iterate the paragraph blocks in order, by reference. `O(N)`.
  pub fn paragraphs_iter(&self) -> impl Iterator<Item = &Paragraph> + '_ {
    self.tree.paragraphs_iter()
  }

  /// The `document.text` byte offset where paragraph `paragraph_ix` starts —
  /// the tree-native replacement for `ParagraphOffsetIndex::paragraph_start`.
  /// `O(log N)`.
  #[must_use]
  pub fn paragraph_start(&self, paragraph_ix: usize) -> usize {
    self.tree.paragraph_start(paragraph_ix)
  }

  // -- `O(log N)` fast-path mutations (path-copy, share the rest) -----------

  /// Replace `range` with `replacement`, sharing every untouched node.
  pub fn splice(&mut self, range: std::ops::Range<usize>, replacement: Vec<Block>) {
    self.tree = self.tree.splice(range, replacement);
  }

  /// Replace the single block at `index` (no-op if out of range). `O(log N)`.
  pub fn set(&mut self, index: usize, block: Block) {
    if index < self.len() {
      self.tree = self.tree.splice(index..index + 1, vec![block]);
    }
  }

  /// Insert `block` before `index` (clamped to `len`). `O(log N)`.
  pub fn insert(&mut self, index: usize, block: Block) {
    let at = index.min(self.len());
    self.tree = self.tree.splice(at..at, vec![block]);
  }

  /// Remove the block at `index` (no-op if out of range). `O(log N)`.
  pub fn remove(&mut self, index: usize) {
    if index < self.len() {
      self.tree = self.tree.splice(index..index + 1, Vec::new());
    }
  }

  /// Append `block`. `O(log N)`.
  pub fn push(&mut self, block: Block) {
    let len = self.len();
    self.tree = self.tree.splice(len..len, vec![block]);
  }

  /// Mutate one block in place (copy-on-write). `O(log N)`, no allocation on a
  /// uniquely-owned tree.
  pub fn update_at(&mut self, index: usize, edit: impl FnOnce(&mut Block)) {
    self.tree.update_at(index, edit);
  }

  /// Apply `edit` to every block from `start` onward, in place (copy-on-write)
  /// — the per-keystroke byte-range mirror's O(N)-in-place path (matches the
  /// old `Arc<Vec<Block>>` shift; no deep clone when owned).
  pub fn map_from_mut(&mut self, start: usize, edit: impl FnMut(&mut Block)) {
    self.tree.map_from_mut(start, edit);
  }

  /// Mutable access as a `Vec` for the complex sites that run several ops at
  /// once: materialize now, rebuild the tree on the guard's `Drop`. `O(N)` —
  /// hot single-block sites should prefer `set`/`insert`/`remove`/`splice`.
  #[must_use]
  pub fn make_mut(&mut self) -> BlockSeqMut<'_> {
    let vec = self.tree.to_vec();
    BlockSeqMut { seq: self, vec }
  }
}

impl std::ops::Index<usize> for BlockSeq {
  type Output = Block;
  fn index(&self, index: usize) -> &Block {
    self.get(index).expect("block index out of bounds")
  }
}

impl From<Vec<Block>> for BlockSeq {
  fn from(blocks: Vec<Block>) -> Self {
    Self::from_vec(blocks)
  }
}

impl FromIterator<Block> for BlockSeq {
  fn from_iter<I: IntoIterator<Item = Block>>(iter: I) -> Self {
    Self::from_vec(iter.into_iter().collect())
  }
}

/// A materialize→mutate→rebuild guard. Derefs to the underlying `Vec<Block>`
/// so existing `Arc::make_mut(&mut …blocks)` call sites keep their body
/// verbatim; on `Drop` the mutated `Vec` is rebuilt into the persistent tree.
pub struct BlockSeqMut<'a> {
  seq: &'a mut BlockSeq,
  vec: Vec<Block>,
}

impl std::ops::Deref for BlockSeqMut<'_> {
  type Target = Vec<Block>;
  fn deref(&self) -> &Vec<Block> {
    &self.vec
  }
}

impl std::ops::DerefMut for BlockSeqMut<'_> {
  fn deref_mut(&mut self) -> &mut Vec<Block> {
    &mut self.vec
  }
}

impl Drop for BlockSeqMut<'_> {
  fn drop(&mut self) {
    self.seq.tree = BlockTree::from_blocks(std::mem::take(&mut self.vec));
  }
}

#[cfg(test)]
mod block_seq_tests {
  use super::*;
  use crate::{ParagraphStyle, TextRun};

  fn para(byte_len: usize) -> Block {
    Block::Paragraph(Paragraph {
      style: ParagraphStyle::Normal,
      byte_range: 0..byte_len,
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

  fn seq_and_ref(lens: &[usize]) -> (BlockSeq, Vec<Block>) {
    let reference: Vec<Block> = lens.iter().map(|&l| para(l)).collect();
    (BlockSeq::from_vec(reference.clone()), reference)
  }

  #[test]
  fn read_api_matches_reference_vec() {
    let (seq, reference) = seq_and_ref(&[3, 5, 0, 8, 2]);
    assert_eq!(seq.len(), reference.len());
    assert!(!seq.is_empty());
    for ix in 0..reference.len() {
      assert_eq!(seq.get(ix), Some(&reference[ix]));
      assert_eq!(&seq[ix], &reference[ix]);
    }
    assert_eq!(seq.get(reference.len()), None);
    assert_eq!(seq.first(), reference.first());
    assert_eq!(seq.last(), reference.last());
    assert_eq!(seq.iter().collect::<Vec<_>>(), reference.iter().collect::<Vec<_>>());
    assert_eq!(seq.range(1..4).collect::<Vec<_>>(), reference[1..4].iter().collect::<Vec<_>>());
    assert_eq!(seq.to_vec(), reference);
  }

  #[test]
  fn fast_path_mutations_match_vec_semantics() {
    let (mut seq, mut reference) = seq_and_ref(&[1, 2, 3, 4]);
    seq.set(1, para(9));
    reference[1] = para(9);
    assert_eq!(seq.to_vec(), reference);
    seq.insert(2, para(7));
    reference.insert(2, para(7));
    assert_eq!(seq.to_vec(), reference);
    seq.remove(0);
    reference.remove(0);
    assert_eq!(seq.to_vec(), reference);
    seq.push(para(5));
    reference.push(para(5));
    assert_eq!(seq.to_vec(), reference);
    seq.splice(1..3, vec![para(6), para(6), para(6)]);
    reference.splice(1..3, vec![para(6), para(6), para(6)]);
    assert_eq!(seq.to_vec(), reference);
  }

  #[test]
  fn make_mut_guard_rebuilds_on_drop() {
    let (mut seq, mut reference) = seq_and_ref(&[1, 2, 3]);
    let mut guard = seq.make_mut();
    guard.insert(1, para(8));
    guard[0] = para(4);
    guard.remove(3);
    drop(guard);
    reference.insert(1, para(8));
    reference[0] = para(4);
    reference.remove(3);
    assert_eq!(seq.to_vec(), reference);
  }

  #[test]
  fn clone_shares_and_is_persistent() {
    let (seq, _) = seq_and_ref(&[1, 2, 3, 4, 5]);
    let mut edited = seq.clone();
    edited.set(2, para(99));
    // The clone diverges; the original is untouched (persistence → undo target).
    assert_eq!(seq.to_vec(), seq_and_ref(&[1, 2, 3, 4, 5]).1);
    assert_ne!(edited.to_vec(), seq.to_vec());
  }
}
