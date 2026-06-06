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
  let len = document.text.byte_len();
  let start = range.start.min(len);
  let end = range.end.min(len);
  if start >= end {
    return String::new();
  }
  let mut text = String::with_capacity(end - start);
  push_document_text_slice(document, start..end, &mut text);
  text
}

#[hotpath::measure]
pub fn push_document_text_slice(document: &Document, range: Range<usize>, text: &mut String) {
  let len = document.text.byte_len();
  let start = range.start.min(len);
  let end = range.end.min(len);
  if start >= end {
    return;
  }
  for chunk in document.text.byte_slice(start..end).chunks() {
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
pub fn apply_document_span_replacement(document: &mut Document, current: &DocumentSpan, replacement: &DocumentSpan) {
  let byte_range = paragraph_span_byte_range(document, current.start_paragraph, current.paragraphs.len());
  document.text.delete(byte_range.clone());
  document.text.insert(byte_range.start, &replacement.text);
  let paragraph_end = current
    .start_paragraph
    .saturating_add(current.paragraphs.len())
    .min(document.paragraphs.len());
  remove_paragraph_ids(document, current.start_paragraph..paragraph_end);
  paragraphs_mut(document).splice(current.start_paragraph..paragraph_end, replacement.paragraphs.clone());
  for relative_ix in 0..replacement.paragraphs.len() {
    insert_paragraph_id(document, current.start_paragraph + relative_ix);
  }
  replace_paragraph_blocks(
    document,
    current.start_paragraph,
    paragraph_end.saturating_sub(current.start_paragraph),
    &replacement.paragraphs,
  );
  rebuild_document_offset_index(document);
  rebuild_document_sections(document);
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
  let end_paragraph = start_paragraph
    .saturating_add(paragraph_count.saturating_sub(1))
    .min(document.paragraphs.len() - 1);
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
  find_text_ranges_with_case(document, query, true)
}

#[hotpath::measure]
#[must_use]
pub fn find_text_ranges_with_case(document: &Document, query: &str, case_sensitive: bool) -> Vec<Range<DocumentOffset>> {
  find_text_ranges_with_options(document, query, case_sensitive, false)
}

#[hotpath::measure]
#[must_use]
pub fn find_text_ranges_with_options(document: &Document, query: &str, case_sensitive: bool, whole_words: bool) -> Vec<Range<DocumentOffset>> {
  if query.is_empty() {
    return Vec::new();
  }
  let text = full_document_text(document);
  let matches = if case_sensitive {
    text
      .match_indices(query)
      .map(|(start, matched)| start..start + matched.len())
      .collect::<Vec<_>>()
  } else {
    let lower_text = text.to_ascii_lowercase();
    let lower_query = query.to_ascii_lowercase();
    lower_text
      .match_indices(lower_query.as_str())
      .map(|(start, matched)| start..start + matched.len())
      .collect::<Vec<_>>()
  };

  matches
    .into_iter()
    .filter(|range| !whole_words || is_whole_word_match(&text, range.clone()))
    .map(|range| global_to_document_offset(document, range.start)..global_to_document_offset(document, range.end))
    .collect()
}

#[hotpath::measure]
fn is_whole_word_match(text: &str, range: Range<usize>) -> bool {
  !previous_char_is_word_like(text, range.start) && !next_char_is_word_like(text, range.end)
}

#[hotpath::measure]
fn previous_char_is_word_like(text: &str, byte: usize) -> bool {
  text[..byte.min(text.len())]
    .chars()
    .next_back()
    .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
}

#[hotpath::measure]
fn next_char_is_word_like(text: &str, byte: usize) -> bool {
  text
    .get(byte.min(text.len())..)
    .and_then(|suffix| suffix.chars().next())
    .is_some_and(|ch| ch.is_alphanumeric() || ch == '_')
}

#[hotpath::measure]
#[must_use]
pub fn selected_plain_text(document: &Document, range: Range<DocumentOffset>) -> String {
  if document.paragraphs.is_empty() {
    return String::new();
  }

  let mut start = range.start;
  let mut end = range.end;
  if (end.paragraph, end.byte) < (start.paragraph, start.byte) {
    std::mem::swap(&mut start, &mut end);
  }

  let last_paragraph = document.paragraphs.len() - 1;
  start.paragraph = start.paragraph.min(last_paragraph);
  end.paragraph = end.paragraph.min(last_paragraph);
  start.byte = start
    .byte
    .min(paragraph_text_len(&document.paragraphs[start.paragraph]));
  end.byte = end
    .byte
    .min(paragraph_text_len(&document.paragraphs[end.paragraph]));

  if start.paragraph == end.paragraph {
    if start.byte >= end.byte {
      return String::new();
    }
    let paragraph_range = paragraph_byte_range(document, start.paragraph);
    return clipboard_plain_text(document_text_slice(
      document,
      paragraph_range.start + start.byte..paragraph_range.start + end.byte,
    ));
  }

  let mut text = String::new();
  for paragraph_ix in start.paragraph..=end.paragraph {
    if paragraph_ix > start.paragraph {
      text.push('\n');
    }
    let paragraph = &document.paragraphs[paragraph_ix];
    let start_byte = if paragraph_ix == start.paragraph { start.byte } else { 0 };
    let end_byte = if paragraph_ix == end.paragraph {
      end.byte
    } else {
      paragraph_text_len(paragraph)
    };
    if start_byte >= end_byte {
      continue;
    }
    let paragraph_range = paragraph_byte_range(document, paragraph_ix);
    text.push_str(&clipboard_plain_text(document_text_slice(
      document,
      paragraph_range.start + start_byte..paragraph_range.start + end_byte,
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
