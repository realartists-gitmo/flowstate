#[hotpath::measure]
#[must_use]
pub fn paragraph_text(document: &DocumentProjection, paragraph_ix: usize) -> String {
  document_text_slice(document, paragraph_byte_range(document, paragraph_ix))
}

#[hotpath::measure]
#[must_use]
pub fn paragraph_text_len(paragraph: &Paragraph) -> usize {
  paragraph_runs_len(paragraph)
}

#[hotpath::measure]
#[must_use]
pub fn document_text_slice(document: &DocumentProjection, range: Range<usize>) -> String {
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
pub fn push_document_text_slice(document: &DocumentProjection, range: Range<usize>, text: &mut String) {
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
pub fn paragraph_char_count(document: &DocumentProjection, paragraph_ix: usize, needle: char) -> usize {
  document_text_slice_char_count(document, paragraph_byte_range(document, paragraph_ix), needle)
}

#[hotpath::measure]
#[must_use]
pub fn document_text_slice_char_count(document: &DocumentProjection, range: Range<usize>, needle: char) -> usize {
  document
    .text
    .byte_slice(range)
    .chunks()
    .map(|chunk| chunk.matches(needle).count())
    .sum()
}

#[hotpath::measure]
#[must_use]
pub fn capture_document_span(document: &DocumentProjection, range: Range<usize>) -> DocumentSpan {
  let start = range.start.min(document.paragraphs.len());
  let end = range.end.min(document.paragraphs.len()).max(start);
  let text = if start < end {
    let byte_range = paragraph_span_byte_range(document, start, end - start);
    document_text_slice(document, byte_range)
  } else {
    String::new()
  };
  // Capture the durable ids the span currently owns (one paragraph id and one
  // paragraph-block id per paragraph) so a later replacement can force the
  // optimistic, predicted, and canonical apply to preserve the same identities.
  let paragraph_ids = (start..end)
    .filter_map(|paragraph_ix| document.ids.paragraph_ids.get(paragraph_ix).copied())
    .collect::<Vec<_>>();
  let block_ids = (start..end)
    .filter_map(|paragraph_ix| {
      block_ix_for_paragraph(document, paragraph_ix).and_then(|block_ix| document.ids.block_ids.get(block_ix).copied())
    })
    .collect::<Vec<_>>();
  DocumentSpan {
    start_paragraph: start,
    paragraphs: document.paragraphs[start..end].to_vec(),
    paragraph_ids,
    block_ids,
    text,
  }
}

#[hotpath::measure]
pub fn apply_document_span_replacement(document: &mut DocumentProjection, current: &DocumentSpan, replacement: &DocumentSpan) {
  debug_assert_eq!(
    replacement.paragraph_ids.len(),
    replacement.paragraphs.len(),
    "DocumentSpan replacement must carry exactly one paragraph id per paragraph",
  );
  debug_assert_eq!(
    replacement.block_ids.len(),
    replacement.paragraphs.len(),
    "DocumentSpan replacement must carry exactly one block id per paragraph",
  );
  let byte_range = paragraph_span_byte_range(document, current.start_paragraph, current.paragraphs.len());
  document.text.delete(byte_range.clone());
  document.text.insert(byte_range.start, &replacement.text);
  let paragraph_end = current
    .start_paragraph
    .saturating_add(current.paragraphs.len())
    .min(document.paragraphs.len());
  let before_count = paragraph_end.saturating_sub(current.start_paragraph);
  // Splice the editor-captured durable ids verbatim rather than re-deriving them
  // positionally (keep-first/mint-rest). Using the exact ids the command carried
  // is what keeps this optimistic replay, the runtime prediction, and the
  // canonical Loro apply from disagreeing on which paragraph/block id survives a
  // merge (e.g. a join dropping a middle boundary) — a disagreement that would
  // strand later pending edits referencing the dropped id and lose text.
  let paragraph_id_end = paragraph_end.min(document.ids.paragraph_ids.len());
  document
    .ids
    .paragraph_ids
    .splice(current.start_paragraph.min(paragraph_id_end)..paragraph_id_end, replacement.paragraph_ids.iter().copied());
  paragraphs_mut(document).splice(current.start_paragraph..paragraph_end, replacement.paragraphs.clone());
  replace_paragraph_blocks(document, current.start_paragraph, before_count, &replacement.paragraphs);
  // `replace_paragraph_blocks` derives the span's block ids positionally; overwrite
  // them with the editor-captured block ids so block identities match canonical
  // too. The replacement paragraph-blocks are contiguous from `block_start`.
  if let Some(block_start) = block_ix_for_paragraph(document, current.start_paragraph) {
    let block_end = block_start
      .saturating_add(replacement.block_ids.len())
      .min(document.ids.block_ids.len());
    document
      .ids
      .block_ids
      .splice(block_start.min(block_end)..block_end, replacement.block_ids.iter().copied());
  }
  // `replace_paragraph_blocks` already rebuilt the section outline; only the
  // byte-offset index still needs refreshing after the splice.
  rebuild_document_offset_index(document);
}

#[hotpath::measure]
#[must_use]
pub fn paragraph_span_byte_range(document: &DocumentProjection, start_paragraph: usize, paragraph_count: usize) -> Range<usize> {
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
pub fn full_document_text(document: &DocumentProjection) -> String {
  document_text_slice(document, 0..document.text.byte_len())
}

#[hotpath::measure]
pub fn document_end(document: &DocumentProjection) -> DocumentOffset {
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
pub fn global_byte(document: &DocumentProjection, offset: DocumentOffset) -> usize {
  paragraph_byte_range(document, offset.paragraph).start + offset.byte
}

#[allow(dead_code, reason = "Global-to-document offset conversion is retained for file/search integrations.")]
#[hotpath::measure]
#[must_use]
pub fn global_to_document_offset(document: &DocumentProjection, byte: usize) -> DocumentOffset {
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
pub fn find_text_ranges(document: &DocumentProjection, query: &str) -> Vec<Range<DocumentOffset>> {
  find_text_ranges_with_case(document, query, true)
}

#[hotpath::measure]
#[must_use]
pub fn find_text_ranges_with_case(document: &DocumentProjection, query: &str, case_sensitive: bool) -> Vec<Range<DocumentOffset>> {
  find_text_ranges_with_options(document, query, case_sensitive, false)
}

#[hotpath::measure]
#[must_use]
pub fn find_text_ranges_with_options(document: &DocumentProjection, query: &str, case_sensitive: bool, whole_words: bool) -> Vec<Range<DocumentOffset>> {
  if query.is_empty() {
    return Vec::new();
  }
  let text = full_document_text(document);
  // §perf: filter+map the match_indices iterator straight into the result instead of
  // collecting a throwaway intermediate Vec<Range> and re-iterating it. ASCII-lowercase
  // preserves byte offsets, so whole-word checks still use the original `text`.
  let to_offsets =
    |range: Range<usize>| global_to_document_offset(document, range.start)..global_to_document_offset(document, range.end);
  if case_sensitive {
    text
      .match_indices(query)
      .map(|(start, matched)| start..start + matched.len())
      .filter(|range| !whole_words || is_whole_word_match(&text, range.clone()))
      .map(to_offsets)
      .collect()
  } else {
    let lower_text = text.to_ascii_lowercase();
    let lower_query = query.to_ascii_lowercase();
    lower_text
      .match_indices(lower_query.as_str())
      .map(|(start, matched)| start..start + matched.len())
      .filter(|range| !whole_words || is_whole_word_match(&text, range.clone()))
      .map(to_offsets)
      .collect()
  }
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
pub fn selected_plain_text(document: &DocumentProjection, range: Range<DocumentOffset>) -> String {
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
