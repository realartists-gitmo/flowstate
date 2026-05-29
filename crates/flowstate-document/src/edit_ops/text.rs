#[hotpath::measure]
pub fn paragraph_text(document: &Document, paragraph_ix: usize) -> String {
  document_text_slice(document, paragraph_byte_range(document, paragraph_ix))
}

#[hotpath::measure]
pub fn paragraph_text_len(paragraph: &Paragraph) -> usize {
  paragraph_runs_len(paragraph)
}

#[hotpath::measure]
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
pub fn paragraph_char_count(document: &Document, paragraph_ix: usize, needle: char) -> usize {
  document_text_slice_char_count(document, paragraph_byte_range(document, paragraph_ix), needle)
}

#[hotpath::measure]
pub fn document_text_slice_char_count(document: &Document, range: Range<usize>, needle: char) -> usize {
  document
    .text
    .byte_slice(range)
    .chunks()
    .map(|chunk| chunk.matches(needle).count())
    .sum()
}

#[hotpath::measure]
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
  paragraphs_mut(document).splice(current.start_paragraph..paragraph_end, replacement.paragraphs.clone());
  replace_paragraph_blocks(
    document,
    current.start_paragraph,
    paragraph_end.saturating_sub(current.start_paragraph),
    &replacement.paragraphs,
  );
  rebuild_document_offset_index(document);
}

#[hotpath::measure]
pub fn paragraph_span_byte_range(document: &Document, start_paragraph: usize, paragraph_count: usize) -> Range<usize> {
  if paragraph_count == 0 || start_paragraph >= document.paragraphs.len() {
    let byte = document
      .paragraphs
      .get(start_paragraph)
      .map(|_| paragraph_byte_range(document, start_paragraph).start)
      .unwrap_or_else(|| document.text.byte_len());
    return byte..byte;
  }
  let end_paragraph = (start_paragraph + paragraph_count - 1).min(document.paragraphs.len() - 1);
  paragraph_byte_range(document, start_paragraph).start..paragraph_byte_range(document, end_paragraph).end
}

#[allow(dead_code)]
#[hotpath::measure]
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
      .map(paragraph_text_len)
      .unwrap_or(0),
  }
}

#[allow(dead_code)]
#[hotpath::measure]
pub fn global_byte(document: &Document, offset: DocumentOffset) -> usize {
  paragraph_byte_range(document, offset.paragraph).start + offset.byte
}

#[allow(dead_code)]
#[hotpath::measure]
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

