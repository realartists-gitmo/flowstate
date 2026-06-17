//! Maintained paragraph -> body byte-offset index for the single-root body text.
//!
//! In the single-root model the whole document lives in one `LoroText` ("body")
//! whose paragraphs are separated by `'\n'`. Mapping a paragraph ordinal to its
//! byte range in the body used to require stringifying the entire body and
//! linear-scanning for the Nth newline on every edit. [`BodyParagraphIndex`]
//! replaces that with a Fenwick (binary indexed) tree over per-paragraph body
//! widths, mirroring `gpui_flowtext::ParagraphOffsetIndex`, so paragraph -> byte
//! lookups are O(log n) and content edits update in O(log n).

use std::ops::Range;

use loro::LoroText;

use crate::schema::text_content;

/// Fenwick tree over per-paragraph body byte widths plus the raw paragraph byte
/// lengths.
///
/// `width(k) = len(k) + (1 if k is not the last paragraph else 0)` — the trailing
/// `'\n'` separator is charged to every paragraph except the last, so the prefix
/// sum of widths over the paragraphs before `k` is exactly `k`'s start byte in the
/// body.
#[derive(Clone, Debug, Default)]
pub struct BodyParagraphIndex {
  /// UTF-8 byte length of each paragraph's text (no separator newline).
  lens: Vec<usize>,
  /// 1-indexed Fenwick tree over paragraph widths.
  tree: Vec<usize>,
}

impl BodyParagraphIndex {
  /// Builds an index from per-paragraph UTF-8 byte lengths (in body order).
  #[must_use]
  pub fn from_lens(lens: Vec<usize>) -> Self {
    let mut index = Self {
      tree: vec![0; lens.len() + 1],
      lens,
    };
    for ordinal in 0..index.lens.len() {
      let width = index.width(ordinal);
      index.add(ordinal, width as isize);
    }
    index
  }

  /// Number of paragraphs tracked by the index.
  #[must_use]
  pub fn len(&self) -> usize {
    self.lens.len()
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.lens.is_empty()
  }

  /// Byte offset where paragraph `ordinal` starts in the body text.
  #[must_use]
  pub fn paragraph_start(&self, ordinal: usize) -> usize {
    self.prefix_sum(ordinal)
  }

  /// UTF-8 byte length of paragraph `ordinal` (excluding the separator newline).
  #[must_use]
  pub fn paragraph_len(&self, ordinal: usize) -> usize {
    self.lens.get(ordinal).copied().unwrap_or(0)
  }

  /// Byte range `[start, start + len)` of paragraph `ordinal` in the body text.
  #[must_use]
  pub fn paragraph_range(&self, ordinal: usize) -> Range<usize> {
    let start = self.paragraph_start(ordinal);
    start..start + self.paragraph_len(ordinal)
  }

  /// Total body length implied by the index (sum of widths).
  #[must_use]
  pub fn total_len(&self) -> usize {
    self.prefix_sum(self.lens.len())
  }

  /// Returns the ordinal of the paragraph that contains `byte` — i.e. the largest
  /// ordinal whose start is `<= byte`. A byte sitting on a separator newline maps
  /// to the paragraph it terminates.
  #[must_use]
  pub fn paragraph_ordinal_for_body_byte(&self, byte: usize) -> usize {
    if self.lens.is_empty() {
      return 0;
    }
    let mut low = 0usize;
    let mut high = self.lens.len() - 1;
    while low < high {
      let mid = low + (high - low).div_ceil(2);
      if self.paragraph_start(mid) <= byte {
        low = mid;
      } else {
        high = mid - 1;
      }
    }
    low
  }

  /// Updates one paragraph's byte length in O(log n). The paragraph count must be
  /// unchanged (use [`Self::insert_paragraph`] / [`Self::remove_paragraph`] for
  /// structural changes).
  pub fn set_paragraph_len(&mut self, ordinal: usize, new_len: usize) {
    let Some(old) = self.lens.get(ordinal).copied() else {
      return;
    };
    if old == new_len {
      return;
    }
    self.lens[ordinal] = new_len;
    self.add(ordinal, new_len as isize - old as isize);
  }

  /// Inserts a new paragraph of length `len` at `ordinal`, shifting later
  /// paragraphs right. O(n) (rebuilds the tree, matching the editor's structural
  /// path which is already O(n)).
  pub fn insert_paragraph(&mut self, ordinal: usize, len: usize) {
    let ordinal = ordinal.min(self.lens.len());
    self.lens.insert(ordinal, len);
    self.rebuild();
  }

  /// Removes the paragraph at `ordinal`. O(n).
  pub fn remove_paragraph(&mut self, ordinal: usize) {
    if ordinal >= self.lens.len() {
      return;
    }
    self.lens.remove(ordinal);
    self.rebuild();
  }

  /// Removes `count` paragraphs starting at `ordinal` in one rebuild. O(n).
  pub fn remove_paragraph_range(&mut self, ordinal: usize, count: usize) {
    let start = ordinal.min(self.lens.len());
    let end = start.saturating_add(count).min(self.lens.len());
    if start >= end {
      return;
    }
    self.lens.drain(start..end);
    self.rebuild();
  }

  fn rebuild(&mut self) {
    let lens = std::mem::take(&mut self.lens);
    *self = Self::from_lens(lens);
  }

  fn width(&self, ordinal: usize) -> usize {
    self.lens[ordinal] + usize::from(ordinal + 1 < self.lens.len())
  }

  fn add(&mut self, ordinal: usize, delta: isize) {
    if delta == 0 {
      return;
    }
    let mut ix = ordinal + 1;
    while ix < self.tree.len() {
      self.tree[ix] = self.tree[ix].saturating_add_signed(delta);
      ix += ix & (!ix + 1);
    }
  }

  fn prefix_sum(&self, paragraph_count: usize) -> usize {
    let mut ix = paragraph_count.min(self.lens.len());
    let mut sum = 0;
    while ix > 0 {
      sum += self.tree[ix];
      ix &= ix - 1;
    }
    sum
  }
}

/// Scans the body text once and returns each paragraph's UTF-8 byte length (body
/// order). Used to (re)build [`BodyParagraphIndex`] and to debug-verify it.
///
/// This is O(body length); it is only meant for build / structural / debug paths,
/// never the per-keystroke hot path.
#[must_use]
pub fn paragraph_lens_from_body_text(text: &LoroText) -> Vec<usize> {
  text_content(text).split('\n').map(str::len).collect()
}

/// Length of the shared UTF-8 prefix of `left` and `right`, on a char boundary.
#[must_use]
pub fn common_prefix_bytes(left: &str, right: &str) -> usize {
  let mut prefix = 0usize;
  for (left_ch, right_ch) in left.chars().zip(right.chars()) {
    if left_ch != right_ch {
      break;
    }
    prefix += left_ch.len_utf8();
  }
  prefix
}

/// Length of the shared UTF-8 suffix of `left` and `right`, on a char boundary.
#[must_use]
pub fn common_suffix_bytes(left: &str, right: &str) -> usize {
  let mut suffix = 0usize;
  for (left_ch, right_ch) in left.chars().rev().zip(right.chars().rev()) {
    if left_ch != right_ch {
      break;
    }
    let len = left_ch.len_utf8();
    if suffix + len > left.len() || suffix + len > right.len() {
      break;
    }
    suffix += len;
  }
  suffix
}

/// Minimal byte splice turning `old` into `new`: the shared prefix length, the
/// length of the differing middle of `old` to delete, and the differing middle of
/// `new` to insert. Returns byte offsets relative to the start of `old`/`new`.
#[must_use]
pub fn minimal_utf8_splice(old: &str, new: &str) -> (usize, usize, Range<usize>) {
  let prefix = common_prefix_bytes(old, new);
  let suffix = common_suffix_bytes(&old[prefix..], &new[prefix..]);
  let old_middle = old.len().saturating_sub(prefix + suffix);
  let new_middle = prefix..new.len().saturating_sub(suffix);
  (prefix, old_middle, new_middle)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn fresh_start(lens: &[usize], ordinal: usize) -> usize {
    lens.iter().take(ordinal).map(|len| len + 1).sum()
  }

  #[test]
  fn starts_and_ranges_match_a_fresh_scan() {
    let lens = vec![3, 0, 5, 2, 7];
    let index = BodyParagraphIndex::from_lens(lens.clone());
    for ordinal in 0..lens.len() {
      assert_eq!(index.paragraph_start(ordinal), fresh_start(&lens, ordinal));
      assert_eq!(
        index.paragraph_range(ordinal),
        fresh_start(&lens, ordinal)..fresh_start(&lens, ordinal) + lens[ordinal]
      );
    }
    // Total length has no trailing newline after the last paragraph.
    let total: usize = lens.iter().sum::<usize>() + lens.len() - 1;
    assert_eq!(index.total_len(), total);
  }

  #[test]
  fn random_edits_keep_starts_consistent() {
    let mut lens = vec![4, 4, 4, 4];
    let mut index = BodyParagraphIndex::from_lens(lens.clone());

    index.set_paragraph_len(1, 10);
    lens[1] = 10;
    index.insert_paragraph(2, 3);
    lens.insert(2, 3);
    index.remove_paragraph(0);
    lens.remove(0);
    index.set_paragraph_len(index.len() - 1, 0);
    *lens.last_mut().unwrap() = 0;

    for ordinal in 0..lens.len() {
      assert_eq!(index.paragraph_start(ordinal), fresh_start(&lens, ordinal), "start mismatch at {ordinal}");
      assert_eq!(index.paragraph_len(ordinal), lens[ordinal]);
    }
  }

  #[test]
  fn ordinal_lookup_finds_containing_paragraph() {
    let lens = vec![2, 3, 4];
    let index = BodyParagraphIndex::from_lens(lens);
    // body: "aa\nbbb\ncccc" -> starts 0, 3, 7
    assert_eq!(index.paragraph_ordinal_for_body_byte(0), 0);
    assert_eq!(index.paragraph_ordinal_for_body_byte(2), 0); // newline after p0
    assert_eq!(index.paragraph_ordinal_for_body_byte(3), 1);
    assert_eq!(index.paragraph_ordinal_for_body_byte(6), 1); // newline after p1
    assert_eq!(index.paragraph_ordinal_for_body_byte(7), 2);
    assert_eq!(index.paragraph_ordinal_for_body_byte(100), 2);
  }

  #[test]
  fn minimal_splice_picks_changed_middle() {
    let (prefix, old_middle, new_middle) = minimal_utf8_splice("abcXYZdef", "abcQdef");
    assert_eq!(prefix, "abc".len());
    assert_eq!(old_middle, "XYZ".len());
    assert_eq!(new_middle, "abc".len().."abcQ".len());
  }
}
