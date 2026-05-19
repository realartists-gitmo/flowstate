use super::*;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct MouseSelectionOptions {
  pub smart_word_selection: bool,
  pub exact: bool,
}

pub(super) fn expand_mouse_selection(
  document: &Document,
  anchor: DocumentOffset,
  head: DocumentOffset,
  granularity: SelectionGranularity,
  options: MouseSelectionOptions,
) -> EditorSelection {
  if granularity != SelectionGranularity::Character {
    return expand_drag_selection(document, anchor, head, granularity);
  }

  let exact = EditorSelection { anchor, head };
  if options.exact || !options.smart_word_selection || same_word_fragment(document, anchor, head) {
    return exact;
  }

  smart_word_selection(document, anchor, head)
}

fn smart_word_selection(document: &Document, anchor: DocumentOffset, head: DocumentOffset) -> EditorSelection {
  if head < anchor {
    EditorSelection {
      anchor: smart_word_selection_end(document, anchor),
      head: smart_word_selection_start(document, head),
    }
  } else {
    EditorSelection {
      anchor: smart_word_selection_start(document, anchor),
      head: smart_word_selection_end(document, head),
    }
  }
}

fn smart_word_selection_start(document: &Document, offset: DocumentOffset) -> DocumentOffset {
  let Some(text) = paragraph_text_for_offset(document, offset) else {
    return DocumentOffset::default();
  };
  let byte = offset.byte.min(text.len());
  DocumentOffset {
    paragraph: offset.paragraph.min(document.paragraphs.len().saturating_sub(1)),
    byte: if is_debate_word_byte(&text, byte) {
      previous_debate_word_boundary_in_paragraph_text(&text, byte)
    } else {
      next_word_start_at_or_after(&text, byte)
    },
  }
}

fn smart_word_selection_end(document: &Document, offset: DocumentOffset) -> DocumentOffset {
  let Some(text) = paragraph_text_for_offset(document, offset) else {
    return DocumentOffset::default();
  };
  let byte = offset.byte.min(text.len());
  DocumentOffset {
    paragraph: offset.paragraph.min(document.paragraphs.len().saturating_sub(1)),
    byte: if is_debate_word_byte(&text, byte) {
      next_debate_word_boundary_in_paragraph_text(&text, byte)
    } else {
      previous_word_end_at_or_before(&text, byte)
    },
  }
}

fn paragraph_text_for_offset(document: &Document, offset: DocumentOffset) -> Option<String> {
  if document.paragraphs.is_empty() {
    return None;
  }
  let paragraph = offset.paragraph.min(document.paragraphs.len() - 1);
  Some(paragraph_text(document, paragraph))
}

fn next_word_start_at_or_after(text: &str, mut byte: usize) -> usize {
  byte = byte.min(text.len());
  while byte < text.len() {
    if is_debate_word_byte(text, byte) {
      return previous_debate_word_boundary_in_paragraph_text(text, byte);
    }
    byte = next_char_boundary(text, byte);
  }
  text.len()
}

fn previous_word_end_at_or_before(text: &str, mut byte: usize) -> usize {
  byte = byte.min(text.len());
  while byte > 0 {
    let prev = previous_char_boundary(text, byte);
    if is_debate_word_byte(text, prev) {
      return next_debate_word_boundary_in_paragraph_text(text, prev);
    }
    byte = prev;
  }
  0
}

pub(super) fn offset_is_in_same_word_as(document: &Document, anchor: DocumentOffset, offset: DocumentOffset) -> bool {
  if anchor.paragraph != offset.paragraph || document.paragraphs.is_empty() {
    return false;
  }
  let paragraph = anchor.paragraph.min(document.paragraphs.len() - 1);
  let text = paragraph_text(document, paragraph);
  let anchor_byte = anchor.byte.min(text.len());
  let offset_byte = offset.byte.min(text.len());
  let word_start = previous_debate_word_boundary_in_paragraph_text(&text, anchor_byte);
  let word_end = next_debate_word_boundary_in_paragraph_text(&text, anchor_byte);
  word_start < word_end && (word_start..=word_end).contains(&offset_byte)
}

fn same_word_fragment(document: &Document, anchor: DocumentOffset, head: DocumentOffset) -> bool {
  if anchor.paragraph != head.paragraph || anchor == head {
    return false;
  }
  let paragraph = anchor.paragraph.min(document.paragraphs.len().saturating_sub(1));
  let text = paragraph_text(document, paragraph);
  let start = anchor.byte.min(head.byte).min(text.len());
  let end = anchor.byte.max(head.byte).min(text.len());
  if start == end {
    return false;
  }

  let word_start = previous_debate_word_boundary_in_paragraph_text(&text, start);
  let word_end = next_debate_word_boundary_in_paragraph_text(&text, start);
  all_word_bytes(&text, start, end) && start > word_start && end < word_end
}

fn all_word_bytes(text: &str, start: usize, end: usize) -> bool {
  let mut byte = start;
  while byte < end {
    if !is_debate_word_byte(text, byte) {
      return false;
    }
    byte = next_char_boundary(text, byte);
  }
  true
}
