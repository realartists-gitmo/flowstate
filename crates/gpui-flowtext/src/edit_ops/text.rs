use crate::{ParagraphId, new_paragraph_id};

#[hotpath::measure]
#[must_use]
pub fn paragraph_text(document: &Document, paragraph_ix: usize) -> String {
  document_text_slice(document, paragraph_byte_range(document, paragraph_ix))
}

#[hotpath::measure]
#[must_use]
pub fn paragraph_text_len(paragraph: &Paragraph) -> usize {
  paragraph_runs_len(paragraph)
}

#[hotpath::measure]
#[must_use]
pub fn document_text_slice(document: &Document, range: Range<usize>) -> String {
  let mut text = String::with_capacity(range.end - range.start);
  push_document_text_slice(document, range, &mut text);
  text
}

#[hotpath::measure]
pub fn push_document_text_slice(document: &Document, range: Range<usize>, text: &mut String) {
  for chunk in document.text.byte_slice(range).chunks() {
    text.push_str(chunk);
  }
}

#[hotpath::measure]
#[must_use]
pub fn paragraph_char_count(document: &Document, paragraph_ix: usize, needle: char) -> usize {
  document_text_slice_char_count(document, paragraph_byte_range(document, paragraph_ix), needle)
}

#[hotpath::measure]
#[must_use]
pub fn document_text_slice_char_count(document: &Document, range: Range<usize>, needle: char) -> usize {
  document
    .text
    .byte_slice(range)
    .chunks()
    .map(|chunk| chunk.matches(needle).count())
    .sum()
}

#[hotpath::measure]
#[must_use]
pub fn capture_document_span(document: &Document, range: Range<usize>) -> DocumentSpan {
  let start = range.start.min(document.paragraphs.len());
  let end = range.end.min(document.paragraphs.len()).max(start);
  let text = if start < end {
    let byte_range = paragraph_span_byte_range(document, start, end - start);
    document_text_slice(document, byte_range)
  } else {
    String::new()
  };
  DocumentSpan {
    start_paragraph: start,
    paragraphs: document.paragraphs[start..end].to_vec(),
    text,
  }
}

#[hotpath::measure]
pub fn apply_document_span_replacement(document: &mut Document, current: &DocumentSpan, replacement: &DocumentSpan) -> bool {
  if current.start_paragraph > document.paragraphs.len() {
    return false;
  }
  let paragraph_end = current
    .start_paragraph
    .saturating_add(current.paragraphs.len())
    .min(document.paragraphs.len());
  let byte_range = paragraph_span_byte_range(document, current.start_paragraph, paragraph_end.saturating_sub(current.start_paragraph));
  document.text.delete(byte_range.clone());
  document.text.insert(byte_range.start, &replacement.text);
  let replacement_ids = paragraph_replacement_ids(document, current.start_paragraph, paragraph_end, replacement.paragraphs.len());
  paragraphs_mut(document).splice(current.start_paragraph..paragraph_end, replacement.paragraphs.clone());
  document
    .ids
    .paragraph_ids
    .splice(current.start_paragraph..paragraph_end, replacement_ids);
  replace_paragraph_blocks(
    document,
    current.start_paragraph,
    paragraph_end.saturating_sub(current.start_paragraph),
    &replacement.paragraphs,
  );
  rebuild_document_offset_index(document);
  rebuild_document_sections(document);
  true
}

fn paragraph_replacement_ids(document: &Document, start: usize, end: usize, replacement_len: usize) -> Vec<ParagraphId> {
  if replacement_len == 0 {
    return Vec::new();
  }
  let existing = document.ids.paragraph_ids[start.min(document.ids.paragraph_ids.len())..end.min(document.ids.paragraph_ids.len())].to_vec();
  if existing.len() == replacement_len {
    return existing;
  }
  let mut ids = Vec::with_capacity(replacement_len);
  if let Some(first) = existing.first().copied() {
    ids.push(first);
  }
  while ids.len() < replacement_len {
    ids.push(new_paragraph_id());
  }
  ids
}

#[hotpath::measure]
#[must_use]
pub fn paragraph_span_byte_range(document: &Document, start_paragraph: usize, paragraph_count: usize) -> Range<usize> {
  if paragraph_count == 0 || start_paragraph >= document.paragraphs.len() {
    let byte = document
      .paragraphs
      .get(start_paragraph)
      .map_or_else(|| document.text.byte_len(), |_| paragraph_byte_range(document, start_paragraph).start);
    return byte..byte;
  }
  let end_paragraph = (start_paragraph + paragraph_count - 1).min(document.paragraphs.len() - 1);
  paragraph_byte_range(document, start_paragraph).start..paragraph_byte_range(document, end_paragraph).end
}

#[allow(
  dead_code,
  reason = "Public text extraction helper is retained for planned clipboard/search integrations."
)]
#[hotpath::measure]
#[must_use]
pub fn full_document_text(document: &Document) -> String {
  document_text_slice(document, 0..document.text.byte_len())
}

#[hotpath::measure]
pub fn document_end(document: &Document) -> DocumentOffset {
  let paragraph = document.paragraphs.len().saturating_sub(1);
  DocumentOffset {
    paragraph,
    byte: document
      .paragraphs
      .get(paragraph)
      .map_or(0, paragraph_text_len),
  }
}

#[allow(
  dead_code,
  reason = "Global byte conversion is part of the editor offset API even when unused by current callers."
)]
#[hotpath::measure]
#[must_use]
pub fn global_byte(document: &Document, offset: DocumentOffset) -> usize {
  paragraph_byte_range(document, offset.paragraph).start + offset.byte
}

#[allow(dead_code, reason = "Global-to-document offset conversion is retained for file/search integrations.")]
#[hotpath::measure]
#[must_use]
pub fn global_to_document_offset(document: &Document, byte: usize) -> DocumentOffset {
  let byte = byte.min(document.text.byte_len());
  let mut low = 0;
  let mut high = document.paragraphs.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if paragraph_byte_range(document, mid).end < byte {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  let Some(paragraph) = document.paragraphs.get(low) else {
    return document_end(document);
  };
  DocumentOffset {
    paragraph: low,
    byte: byte
      .saturating_sub(paragraph_byte_range(document, low).start)
      .min(paragraph_text_len(paragraph)),
  }
}

#[hotpath::measure]
#[must_use]
pub fn find_text_ranges(document: &Document, query: &str) -> Vec<Range<DocumentOffset>> {
  if query.is_empty() {
    return Vec::new();
  }
  let text = full_document_text(document);
  text
    .match_indices(query)
    .map(|(start, matched)| global_to_document_offset(document, start)..global_to_document_offset(document, start + matched.len()))
    .collect()
}

#[hotpath::measure]
#[must_use]
pub fn selected_plain_text(document: &Document, range: Range<DocumentOffset>) -> String {
  if range.start.paragraph == range.end.paragraph {
    let paragraph_range = paragraph_byte_range(document, range.start.paragraph);
    return clipboard_plain_text(document_text_slice(
      document,
      paragraph_range.start + range.start.byte..paragraph_range.start + range.end.byte,
    ));
  }

  let mut text = String::new();
  for paragraph_ix in range.start.paragraph..=range.end.paragraph {
    if paragraph_ix > range.start.paragraph {
      text.push('\n');
    }
    let paragraph = &document.paragraphs[paragraph_ix];
    let start = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
    let end = if paragraph_ix == range.end.paragraph {
      range.end.byte
    } else {
      paragraph_text_len(paragraph)
    };
    let paragraph_range = paragraph_byte_range(document, paragraph_ix);
    text.push_str(&clipboard_plain_text(document_text_slice(
      document,
      paragraph_range.start + start..paragraph_range.start + end,
    )));
  }
  text
}

#[hotpath::measure]
fn clipboard_plain_text(text: String) -> String {
  if text.contains(SOFT_LINE_BREAK) {
    text.replace(SOFT_LINE_BREAK, "\n")
  } else {
    text
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use crate::{DocumentTheme, document_from_input, paragraph_id_at};

  #[test]
  fn span_replacement_preserves_paragraph_ids_when_shape_matches() {
    let mut document = document_from_input(
      DocumentTheme::default(),
      vec![
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "abc".to_string(),
            styles: RunStyles::default(),
          }],
        },
        InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![InputRun {
            text: "tail".to_string(),
            styles: RunStyles::default(),
          }],
        },
      ],
    );
    let first_id = paragraph_id_at(&document, 0);
    let second_id = paragraph_id_at(&document, 1);
    let before = capture_document_span(&document, 0..1);
    let mut edited = document.clone();

    assert!(insert_text_at(&mut edited, 0, 3, "!", RunStyles::default()));
    let after = capture_document_span(&edited, 0..1);
    apply_document_span_replacement(&mut document, &before, &after);

    assert_eq!(paragraph_id_at(&document, 0), first_id);
    assert_eq!(paragraph_id_at(&document, 1), second_id);
  }

  #[test]
  fn span_replacement_rejects_stale_start_without_panicking() {
    let mut document = document_from_input(
      DocumentTheme::default(),
      vec![InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![InputRun {
          text: "abc".to_string(),
          styles: RunStyles::default(),
        }],
      }],
    );
    let stale = DocumentSpan {
      start_paragraph: 4,
      paragraphs: Vec::new(),
      text: String::new(),
    };
    let replacement = stale.clone();

    assert!(!apply_document_span_replacement(&mut document, &stale, &replacement));
    assert_eq!(paragraph_text(&document, 0), "abc");
  }
}
