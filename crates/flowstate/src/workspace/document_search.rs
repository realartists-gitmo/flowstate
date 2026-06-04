use memchr::memmem::Finder;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct DocumentSearchMatch {
  pub start: usize,
  pub end: usize,
}

impl DocumentSearchMatch {
  #[must_use]
  pub const fn len(self) -> usize {
    self.end.saturating_sub(self.start)
  }

  #[must_use]
  pub const fn is_empty(self) -> bool {
    self.end <= self.start
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentSearchSnapshot {
  text: String,
  line_starts: Vec<usize>,
}

impl DocumentSearchSnapshot {
  #[must_use]
  pub fn new(text: String) -> Self {
    let mut line_starts = vec![0];
    for (byte_index, byte) in text.bytes().enumerate() {
      if byte == b'\n' {
        line_starts.push(byte_index + 1);
      }
    }

    Self { text, line_starts }
  }

  #[must_use]
  pub fn text(&self) -> &str {
    &self.text
  }

  #[must_use]
  pub fn line_starts(&self) -> &[usize] {
    &self.line_starts
  }

  #[must_use]
  pub fn find_literal(&self, needle: &str) -> Vec<DocumentSearchMatch> {
    find_literal_ranges(&self.text, needle)
  }

  #[must_use]
  pub fn line_for_byte(&self, byte_index: usize) -> usize {
    self
      .line_starts
      .partition_point(|line_start| *line_start <= byte_index)
      .saturating_sub(1)
  }
}

#[must_use]
pub fn find_literal_ranges(haystack: &str, needle: &str) -> Vec<DocumentSearchMatch> {
  if needle.is_empty() {
    return Vec::new();
  }

  let finder = Finder::new(needle);
  finder
    .find_iter(haystack.as_bytes())
    .map(|start| DocumentSearchMatch {
      start,
      end: start + needle.len(),
    })
    .collect()
}

#[cfg(test)]
mod tests {
  use super::{DocumentSearchMatch, DocumentSearchSnapshot, find_literal_ranges};

  #[test]
  fn finds_literal_byte_ranges() {
    assert_eq!(
      find_literal_ranges("policy policy", "policy"),
      vec![DocumentSearchMatch { start: 0, end: 6 }, DocumentSearchMatch { start: 7, end: 13 },]
    );
  }

  #[test]
  fn empty_needle_returns_no_matches() {
    assert!(find_literal_ranges("policy", "").is_empty());
  }

  #[test]
  fn tracks_line_starts() {
    let snapshot = DocumentSearchSnapshot::new("a\nb\nc".to_string());

    assert_eq!(snapshot.line_starts(), &[0, 2, 4]);
    assert_eq!(snapshot.line_for_byte(0), 0);
    assert_eq!(snapshot.line_for_byte(2), 1);
    assert_eq!(snapshot.line_for_byte(4), 2);
  }
}
