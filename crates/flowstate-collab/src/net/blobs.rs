use std::{collections::VecDeque, num::NonZeroUsize};

use crate::ids::BlobId;

pub const DEFAULT_MAX_BLOBS: usize = 64;
pub const DEFAULT_MAX_BYTES: usize = 16 * 1024 * 1024;

#[derive(Debug)]
pub struct BlobOutbox {
  max_blobs: NonZeroUsize,
  max_bytes: usize,
  total_bytes: usize,
  entries: VecDeque<(BlobId, Vec<u8>)>,
}

impl Default for BlobOutbox {
  fn default() -> Self {
    Self {
      max_blobs: NonZeroUsize::new(DEFAULT_MAX_BLOBS).expect("constant is non-zero"),
      max_bytes: DEFAULT_MAX_BYTES,
      total_bytes: 0,
      entries: VecDeque::new(),
    }
  }
}

impl BlobOutbox {
  #[must_use]
  pub fn new(max_blobs: NonZeroUsize, max_bytes: usize) -> Self {
    Self {
      max_blobs,
      max_bytes,
      total_bytes: 0,
      entries: VecDeque::new(),
    }
  }

  #[must_use]
  pub fn insert(&mut self, bytes: Vec<u8>) -> BlobId {
    let id = BlobId::new();
    self.insert_with_id(id, bytes);
    id
  }

  pub fn insert_with_id(&mut self, id: BlobId, bytes: Vec<u8>) {
    if let Some((_, existing)) = self
      .entries
      .iter_mut()
      .find(|(candidate, _)| *candidate == id)
    {
      self.total_bytes = self
        .total_bytes
        .saturating_sub(existing.len())
        .saturating_add(bytes.len());
      *existing = bytes;
      self.trim();
      return;
    }

    self.total_bytes = self.total_bytes.saturating_add(bytes.len());
    self.entries.push_back((id, bytes));
    self.trim();
  }

  #[must_use]
  pub fn get(&self, id: BlobId) -> Option<&[u8]> {
    self
      .entries
      .iter()
      .find_map(|(candidate, bytes)| (*candidate == id).then_some(bytes.as_slice()))
  }

  #[must_use]
  pub fn len(&self) -> usize {
    self.entries.len()
  }

  #[must_use]
  pub const fn total_bytes(&self) -> usize {
    self.total_bytes
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self.entries.is_empty()
  }

  fn trim(&mut self) {
    while self.entries.len() > self.max_blobs.get() || self.total_bytes > self.max_bytes {
      let Some((_, bytes)) = self.entries.pop_front() else {
        self.total_bytes = 0;
        return;
      };
      self.total_bytes = self.total_bytes.saturating_sub(bytes.len());
    }
  }
}

#[cfg(test)]
mod tests {
  use std::num::NonZeroUsize;

  use crate::BlobId;

  use super::BlobOutbox;

  #[test]
  fn insert_with_id_replaces_existing_payload_and_total() {
    let mut outbox = BlobOutbox::new(NonZeroUsize::new(4).expect("non-zero max blobs"), 1024);
    let id = BlobId(7);

    outbox.insert_with_id(id, vec![1, 2, 3]);
    outbox.insert_with_id(id, vec![4, 5]);

    assert_eq!(outbox.len(), 1);
    assert_eq!(outbox.total_bytes(), 2);
    assert_eq!(outbox.get(id), Some([4, 5].as_slice()));
  }
}
