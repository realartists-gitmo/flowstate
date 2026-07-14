// §act-four M3 Slice 4 — `ParagraphSeq`: the persistent, structurally-shared
// paragraph sequence that replaces `Arc<Vec<Paragraph>>` on
// `DocumentProjection`.
//
// Backed by a persistent RRB vector (`im::Vector`): `O(1)` clone with
// structural sharing (so a retained version's paragraphs cost `O(change)`, not
// `O(document)`), `O(log N)` random access + splice. Read sites keep their
// shape — `document.paragraphs[i]`, `.get(i)`, `.iter()`, `.len()`, `.first()`
// all hand back `&Paragraph` tied to the field (a field, not a temporary view,
// so borrows live as long as the projection). The complex mutation sites go
// through `paragraphs_mut(...)`'s materialize→mutate→rebuild guard; hot single-
// paragraph edits use the `O(log N)` `set`/`insert`/`remove` fast paths.

/// A persistent, tree-backed paragraph sequence. Drop-in for the old
/// `Arc<Vec<Paragraph>>` field.
#[derive(Clone, Debug, Default)]
pub struct ParagraphSeq {
  paragraphs: im::Vector<Paragraph>,
}

impl ParagraphSeq {
  #[must_use]
  pub fn from_vec(paragraphs: Vec<Paragraph>) -> Self {
    Self {
      paragraphs: paragraphs.into_iter().collect(),
    }
  }

  #[must_use]
  pub fn len(&self) -> usize {
    self.paragraphs.len()
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.paragraphs.is_empty()
  }

  #[must_use]
  pub fn get(&self, index: usize) -> Option<&Paragraph> {
    self.paragraphs.get(index)
  }

  #[must_use]
  pub fn first(&self) -> Option<&Paragraph> {
    self.paragraphs.front()
  }

  #[must_use]
  pub fn last(&self) -> Option<&Paragraph> {
    self.paragraphs.back()
  }

  /// In-order iteration by reference — backs `document.paragraphs.iter()`.
  pub fn iter(&self) -> im::vector::Iter<'_, Paragraph> {
    self.paragraphs.iter()
  }

  #[must_use]
  pub fn to_vec(&self) -> Vec<Paragraph> {
    self.paragraphs.iter().cloned().collect()
  }

  /// Owned copy of the paragraphs in `range` (replaces `paragraphs[a..b].to_vec()`).
  #[must_use]
  pub fn range_to_vec(&self, range: std::ops::Range<usize>) -> Vec<Paragraph> {
    let len = self.len();
    let start = range.start.min(len);
    let end = range.end.min(len).max(start);
    self.paragraphs.iter().skip(start).take(end - start).cloned().collect()
  }

  // -- `O(log N)` fast-path mutations (path-copy, share the rest) -----------

  /// Replace the paragraph at `index` (no-op if out of range). `O(log N)`.
  pub fn set(&mut self, index: usize, paragraph: Paragraph) {
    if index < self.len() {
      self.paragraphs.set(index, paragraph);
    }
  }

  /// Insert `paragraph` before `index` (clamped to `len`). `O(log N)`.
  pub fn insert(&mut self, index: usize, paragraph: Paragraph) {
    self.paragraphs.insert(index.min(self.len()), paragraph);
  }

  /// Remove the paragraph at `index` (no-op if out of range). `O(log N)`.
  pub fn remove(&mut self, index: usize) {
    if index < self.len() {
      self.paragraphs.remove(index);
    }
  }

  /// Append `paragraph`. `O(log N)`.
  pub fn push(&mut self, paragraph: Paragraph) {
    self.paragraphs.push_back(paragraph);
  }

  /// Copy-on-write mutable access to one paragraph — clones the shared node
  /// only if a retained version still holds it. `O(log N)`. Borrows only
  /// `self`, so callers can interleave a following block edit (NLL releases the
  /// borrow at last use — no `Drop` guard to pin `&mut document`).
  pub fn get_mut(&mut self, index: usize) -> Option<&mut Paragraph> {
    self.paragraphs.get_mut(index)
  }

  /// Copy-on-write mutable iteration over the paragraphs. `O(N)`.
  pub fn iter_mut(&mut self) -> im::vector::IterMut<'_, Paragraph> {
    self.paragraphs.iter_mut()
  }

  /// Replace `range` with `replacement` (the `Vec::splice` shape used by a few
  /// mutation sites). `O(log N + |replacement|)`.
  pub fn splice(&mut self, range: std::ops::Range<usize>, replacement: Vec<Paragraph>) {
    let len = self.len();
    let start = range.start.min(len);
    let end = range.end.min(len).max(start);
    let tail = self.paragraphs.split_off(end);
    let _removed = self.paragraphs.split_off(start);
    for paragraph in replacement {
      self.paragraphs.push_back(paragraph);
    }
    self.paragraphs.append(tail);
  }

  /// Mutable access as a `Vec` for the complex sites that run several ops at
  /// once: materialize now, rebuild the persistent vector on the guard's
  /// `Drop`. `O(N)` — hot single-paragraph sites should prefer the fast paths.
  #[must_use]
  pub fn make_mut(&mut self) -> ParagraphSeqMut<'_> {
    let vec = self.to_vec();
    ParagraphSeqMut { seq: self, vec }
  }
}

impl std::ops::Index<usize> for ParagraphSeq {
  type Output = Paragraph;
  fn index(&self, index: usize) -> &Paragraph {
    &self.paragraphs[index]
  }
}

impl From<Vec<Paragraph>> for ParagraphSeq {
  fn from(paragraphs: Vec<Paragraph>) -> Self {
    Self::from_vec(paragraphs)
  }
}

impl FromIterator<Paragraph> for ParagraphSeq {
  fn from_iter<I: IntoIterator<Item = Paragraph>>(iter: I) -> Self {
    Self {
      paragraphs: iter.into_iter().collect(),
    }
  }
}

impl<'a> IntoIterator for &'a ParagraphSeq {
  type Item = &'a Paragraph;
  type IntoIter = im::vector::Iter<'a, Paragraph>;
  fn into_iter(self) -> Self::IntoIter {
    self.paragraphs.iter()
  }
}

impl<'a> IntoIterator for &'a mut ParagraphSeq {
  type Item = &'a mut Paragraph;
  type IntoIter = im::vector::IterMut<'a, Paragraph>;
  fn into_iter(self) -> Self::IntoIter {
    self.paragraphs.iter_mut()
  }
}

/// A materialize→mutate→rebuild guard. Derefs to the underlying `Vec<Paragraph>`
/// so existing `paragraphs_mut(...)` bodies keep their `Vec` operations
/// verbatim; on `Drop` the mutated `Vec` is rebuilt into the persistent vector.
pub struct ParagraphSeqMut<'a> {
  seq: &'a mut ParagraphSeq,
  vec: Vec<Paragraph>,
}

impl std::ops::Deref for ParagraphSeqMut<'_> {
  type Target = Vec<Paragraph>;
  fn deref(&self) -> &Vec<Paragraph> {
    &self.vec
  }
}

impl std::ops::DerefMut for ParagraphSeqMut<'_> {
  fn deref_mut(&mut self) -> &mut Vec<Paragraph> {
    &mut self.vec
  }
}

impl Drop for ParagraphSeqMut<'_> {
  fn drop(&mut self) {
    self.seq.paragraphs = std::mem::take(&mut self.vec).into_iter().collect();
  }
}

#[cfg(test)]
mod paragraph_seq_tests {
  use super::*;
  use crate::{ParagraphStyle, TextRun};

  fn para(byte_len: usize) -> Paragraph {
    Paragraph {
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
    }
  }

  fn seq_and_ref(lens: &[usize]) -> (ParagraphSeq, Vec<Paragraph>) {
    let reference: Vec<Paragraph> = lens.iter().map(|&l| para(l)).collect();
    (ParagraphSeq::from_vec(reference.clone()), reference)
  }

  #[test]
  fn read_api_matches_reference_vec() {
    let (seq, reference) = seq_and_ref(&[3, 5, 0, 8, 2]);
    assert_eq!(seq.len(), reference.len());
    assert!(!seq.is_empty());
    for (ix, expected) in reference.iter().enumerate() {
      assert_eq!(seq.get(ix), Some(expected));
      assert_eq!(&seq[ix], expected);
    }
    assert_eq!(seq.get(reference.len()), None);
    assert_eq!(seq.first(), reference.first());
    assert_eq!(seq.last(), reference.last());
    assert_eq!(seq.iter().collect::<Vec<_>>(), reference.iter().collect::<Vec<_>>());
    assert_eq!(seq.to_vec(), reference);
  }

  #[test]
  fn mutations_match_vec_semantics() {
    let (mut seq, mut reference) = seq_and_ref(&[1, 2, 3, 4]);
    seq.set(1, para(9));
    reference[1] = para(9);
    seq.insert(2, para(7));
    reference.insert(2, para(7));
    seq.remove(0);
    reference.remove(0);
    seq.push(para(5));
    reference.push(para(5));
    assert_eq!(seq.to_vec(), reference);
    let mut guard = seq.make_mut();
    guard.insert(1, para(8));
    guard[0] = para(4);
    guard.remove(2);
    drop(guard);
    reference.insert(1, para(8));
    reference[0] = para(4);
    reference.remove(2);
    assert_eq!(seq.to_vec(), reference);
  }

  #[test]
  fn clone_shares_and_is_persistent() {
    let (seq, original) = seq_and_ref(&[1, 2, 3, 4, 5]);
    let mut edited = seq.clone();
    edited.set(2, para(99));
    assert_eq!(seq.to_vec(), original);
    assert_ne!(edited.to_vec(), seq.to_vec());
  }
}
