//! §act-four M4 — lazy, version-keyed leaf materialization (core).
//!
//! A leaf of the read-model tree is not a `Block`; it is a *promise* of one.
//! Spec §4:
//!
//! ```text
//! LazyLeaf = { key: LeafKey { block_id, version }, cell: OnceCell<Arc<Block>> }
//! ```
//!
//! - **[`BlockVersion`]** is a *content-stable* version of a single block — it
//!   changes iff an op touched that block's range, and is identical across
//!   unrelated edits elsewhere. Two roots that didn't touch block `b` carry
//!   leaves with the *same* [`LeafKey`] → the *same* `Arc<Block>` → materialized
//!   once, ever.
//! - **[`LeafCache`]** maps `(block_id, version) → Arc<Block>`, LRU-bounded by
//!   entry count. A read walks `O(viewport)` leaves and materializes only the
//!   misses.
//!
//! This is the core data structure + its correctness gate (materialize-once,
//! version-keyed sharing, LRU bound); wiring it as the tree's leaf type is the
//! follow-on, exactly as [`crate::BlockTree`] was built before its migration.

use std::cell::OnceCell;
use std::collections::VecDeque;
use std::sync::Arc;

use rustc_hash::FxHashMap;

use crate::{Block, BlockId};

/// A content-stable version of a single block: it advances iff an op touched
/// that block's range, and is identical across unrelated edits elsewhere.
pub type BlockVersion = u64;

/// The identity of a materialized block content: its durable id plus its
/// content-stable version. Equal keys denote byte-identical block content, so a
/// materialization is shared across every tree root that carries this leaf.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct LeafKey {
  pub block_id: BlockId,
  pub version: BlockVersion,
}

impl LeafKey {
  #[must_use]
  pub fn new(block_id: BlockId, version: BlockVersion) -> Self {
    Self { block_id, version }
  }
}

/// A leaf: a promise of a `Block` keyed by `(block_id, version)`. Either already
/// resolved (the common in-memory case — a splice that produced a known block)
/// or deferred (materialized on first read via the §6-R regional law, narrowed
/// to this one leaf).
#[derive(Debug, Default)]
pub struct LazyLeaf {
  key: LeafKey,
  cell: OnceCell<Arc<Block>>,
}

impl Default for LeafKey {
  fn default() -> Self {
    Self {
      block_id: BlockId(0),
      version: 0,
    }
  }
}

impl LazyLeaf {
  /// A leaf whose content is already known (`O(1)`, no deferred work).
  #[must_use]
  pub fn resolved(key: LeafKey, block: Arc<Block>) -> Self {
    let cell = OnceCell::new();
    let _ = cell.set(block);
    Self { key, cell }
  }

  /// A leaf to be materialized on first read.
  #[must_use]
  pub fn deferred(key: LeafKey) -> Self {
    Self {
      key,
      cell: OnceCell::new(),
    }
  }

  #[must_use]
  pub fn key(&self) -> LeafKey {
    self.key
  }

  /// The materialized block, materializing it via `materialize` on first read
  /// and caching it for every subsequent read. `materialize` runs **at most
  /// once** for the lifetime of this leaf.
  pub fn get_or_materialize(&self, materialize: impl FnOnce() -> Arc<Block>) -> &Arc<Block> {
    self.cell.get_or_init(materialize)
  }

  /// The block if it has already been materialized, without forcing it.
  #[must_use]
  pub fn peek(&self) -> Option<&Arc<Block>> {
    self.cell.get()
  }

  #[must_use]
  pub fn is_materialized(&self) -> bool {
    self.cell.get().is_some()
  }
}

/// The process-wide, `(block_id, version)`-keyed leaf cache. A block content is
/// materialized once ever, then shared as an `Arc<Block>` across every version
/// and every leaf that carries the same key. LRU-bounded by entry count so
/// deep history / large documents stay within a memory budget; an evicted entry
/// simply re-materializes on its next miss.
#[derive(Debug)]
pub struct LeafCache {
  map: FxHashMap<LeafKey, Arc<Block>>,
  // Newest-at-back recency queue for LRU eviction.
  order: VecDeque<LeafKey>,
  capacity: usize,
}

impl LeafCache {
  #[must_use]
  pub fn with_capacity(capacity: usize) -> Self {
    Self {
      map: FxHashMap::default(),
      order: VecDeque::new(),
      capacity: capacity.max(1),
    }
  }

  #[must_use]
  pub fn len(&self) -> usize {
    self.map.len()
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.map.is_empty()
  }

  #[must_use]
  pub fn contains(&self, key: &LeafKey) -> bool {
    self.map.contains_key(key)
  }

  /// Return the cached block for `key`, or materialize it via `materialize`
  /// (called **at most once** per distinct key while it remains resident),
  /// insert, and return it. Marks `key` most-recently-used; evicts the LRU
  /// entry when over capacity.
  pub fn get_or_insert(&mut self, key: LeafKey, materialize: impl FnOnce() -> Arc<Block>) -> Arc<Block> {
    if let Some(block) = self.map.get(&key) {
      let block = block.clone();
      self.touch(key);
      return block;
    }
    let block = materialize();
    self.map.insert(key, block.clone());
    self.order.push_back(key);
    self.evict_over_capacity();
    block
  }

  fn touch(&mut self, key: LeafKey) {
    if let Some(pos) = self.order.iter().position(|entry| *entry == key) {
      self.order.remove(pos);
    }
    self.order.push_back(key);
  }

  fn evict_over_capacity(&mut self) {
    while self.map.len() > self.capacity {
      if let Some(oldest) = self.order.pop_front() {
        self.map.remove(&oldest);
      } else {
        break;
      }
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Paragraph, ParagraphStyle};
  use std::cell::Cell;

  fn block(byte_len: usize) -> Arc<Block> {
    Arc::new(Block::Paragraph(Paragraph {
      style: ParagraphStyle::Normal,
      byte_range: 0..byte_len,
      runs: Vec::new(),
      version: 0,
    }))
  }

  #[test]
  fn lazy_leaf_materializes_at_most_once() {
    let calls = Cell::new(0usize);
    let leaf = LazyLeaf::deferred(LeafKey::new(BlockId(7), 3));
    assert!(!leaf.is_materialized());
    assert!(leaf.peek().is_none());

    let first = leaf.get_or_materialize(|| {
      calls.set(calls.get() + 1);
      block(5)
    });
    let first_ptr = Arc::as_ptr(first);
    // Repeated reads never re-run the materializer and return the SAME Arc.
    for _ in 0..4 {
      let again = leaf.get_or_materialize(|| {
        calls.set(calls.get() + 1);
        block(999)
      });
      assert_eq!(Arc::as_ptr(again), first_ptr);
    }
    assert_eq!(calls.get(), 1, "materialize ran exactly once");
    assert!(leaf.is_materialized());
    assert!(leaf.peek().is_some());
  }

  #[test]
  fn resolved_leaf_never_materializes() {
    let calls = Cell::new(0usize);
    let leaf = LazyLeaf::resolved(LeafKey::new(BlockId(1), 0), block(3));
    assert!(leaf.is_materialized());
    leaf.get_or_materialize(|| {
      calls.set(calls.get() + 1);
      block(3)
    });
    assert_eq!(calls.get(), 0, "a resolved leaf ignores the materializer");
  }

  #[test]
  fn cache_shares_by_key_and_materializes_misses_once() {
    let calls = Cell::new(0usize);
    let mut cache = LeafCache::with_capacity(8);
    let key = LeafKey::new(BlockId(42), 1);

    let a = cache.get_or_insert(key, || {
      calls.set(calls.get() + 1);
      block(4)
    });
    let b = cache.get_or_insert(key, || {
      calls.set(calls.get() + 1);
      block(4)
    });
    // Same key → same Arc, materialized exactly once.
    assert!(Arc::ptr_eq(&a, &b));
    assert_eq!(calls.get(), 1);

    // A different VERSION of the same block is a distinct key → new materialize.
    let _ = cache.get_or_insert(LeafKey::new(BlockId(42), 2), || {
      calls.set(calls.get() + 1);
      block(9)
    });
    assert_eq!(calls.get(), 2);
    assert_eq!(cache.len(), 2);
  }

  #[test]
  fn viewport_read_materializes_only_the_visible_leaves() {
    // The M4 headline property: a "document" of N deferred leaves; reading only
    // a viewport materializes only those leaves (byte-identical to eager),
    // leaving the rest unmaterialized. This is "open a 10k-block document,
    // materialize the ~30 on screen."
    const N: usize = 100;
    let eager: Vec<Arc<Block>> = (0..N).map(|i| block(i % 7)).collect();
    let leaves: Vec<LazyLeaf> = (0..N).map(|i| LazyLeaf::deferred(LeafKey::new(BlockId(i as u128), 0))).collect();
    let materializations = Cell::new(0usize);

    let viewport = 40..55;
    for i in viewport.clone() {
      let materialized = leaves[i].get_or_materialize(|| {
        materializations.set(materializations.get() + 1);
        eager[i].clone()
      });
      // Each materialized leaf is byte-identical to the eager block.
      assert_eq!(**materialized, *eager[i], "lazy materialization == eager block");
    }

    assert_eq!(materializations.get(), viewport.len(), "only the viewport leaves materialized");
    for (i, leaf) in leaves.iter().enumerate() {
      assert_eq!(leaf.is_materialized(), viewport.contains(&i), "only the viewport is resident (leaf {i})");
    }

    // Re-reading the viewport does not re-materialize (each leaf's cell holds).
    for i in viewport.clone() {
      leaves[i].get_or_materialize(|| {
        materializations.set(materializations.get() + 1);
        block(999)
      });
    }
    assert_eq!(materializations.get(), viewport.len(), "re-reading the viewport is free");
  }

  #[test]
  fn cache_evicts_least_recently_used() {
    let mut cache = LeafCache::with_capacity(2);
    let k1 = LeafKey::new(BlockId(1), 0);
    let k2 = LeafKey::new(BlockId(2), 0);
    let k3 = LeafKey::new(BlockId(3), 0);
    cache.get_or_insert(k1, || block(1));
    cache.get_or_insert(k2, || block(1));
    // Touch k1 so k2 becomes the LRU victim.
    cache.get_or_insert(k1, || block(1));
    cache.get_or_insert(k3, || block(1)); // over capacity → evict k2
    assert!(cache.contains(&k1));
    assert!(cache.contains(&k3));
    assert!(!cache.contains(&k2), "k2 was least-recently-used and evicted");
    assert_eq!(cache.len(), 2);
  }
}
