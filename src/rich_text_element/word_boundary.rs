use unicode_segmentation::GraphemeCursor;

use super::*;

fn is_word_char(ch: char) -> bool {
  ch.is_alphanumeric() || ch == '_'
}

fn debate_word_char_at(text: &str, byte: usize) -> Option<char> {
  text.get(byte..)?.chars().next()
}

fn is_debate_connector(text: &str, byte: usize) -> bool {
  let Some(ch) = debate_word_char_at(text, byte) else {
    return false;
  };
  if ch != '\'' && ch != '-' {
    return false;
  }
  let prev = text[..byte].chars().next_back();
  let next = text[byte + ch.len_utf8()..].chars().next();
  prev.is_some_and(is_word_char) && next.is_some_and(is_word_char)
}

pub(super) fn is_debate_word_byte(text: &str, byte: usize) -> bool {
  debate_word_char_at(text, byte).is_some_and(is_word_char) || is_debate_connector(text, byte)
}

pub(super) fn previous_char_boundary(text: &str, byte: usize) -> usize {
  text[..byte]
    .char_indices()
    .next_back()
    .map(|(ix, _)| ix)
    .unwrap_or(0)
}

pub(super) fn next_char_boundary(text: &str, byte: usize) -> usize {
  let Some(ch) = debate_word_char_at(text, byte) else {
    return text.len();
  };
  byte + ch.len_utf8()
}

fn previous_debate_word_boundary(text: &str, mut byte: usize) -> usize {
  byte = byte.min(text.len());
  while byte > 0 {
    let prev = previous_char_boundary(text, byte);
    if is_debate_word_byte(text, prev) {
      break;
    }
    byte = prev;
  }
  while byte > 0 {
    let prev = previous_char_boundary(text, byte);
    if !is_debate_word_byte(text, prev) {
      break;
    }
    byte = prev;
  }
  byte
}

pub(super) fn previous_debate_word_boundary_in_paragraph_text(text: &str, byte: usize) -> usize {
  previous_debate_word_boundary(text, byte)
}

fn next_debate_word_boundary(text: &str, mut byte: usize) -> usize {
  byte = byte.min(text.len());
  while byte < text.len() && !is_debate_word_byte(text, byte) {
    byte = next_char_boundary(text, byte);
  }
  while byte < text.len() && is_debate_word_byte(text, byte) {
    byte = next_char_boundary(text, byte);
  }
  byte
}

pub(super) fn next_debate_word_boundary_in_paragraph_text(text: &str, byte: usize) -> usize {
  next_debate_word_boundary(text, byte)
}

pub(super) fn previous_debate_word_boundary_in_document(document: &Document, offset: DocumentOffset) -> DocumentOffset {
  if document.paragraphs.is_empty() {
    return DocumentOffset::default();
  }

  let mut paragraph_ix = offset.paragraph.min(document.paragraphs.len() - 1);
  let mut byte = if paragraph_ix == offset.paragraph {
    offset
      .byte
      .min(paragraph_text_len(&document.paragraphs[paragraph_ix]))
  } else {
    paragraph_text_len(&document.paragraphs[paragraph_ix])
  };

  loop {
    let text = paragraph_text(document, paragraph_ix);
    let mut scan = byte.min(text.len());
    let mut found_word = false;
    while scan > 0 {
      let prev = previous_char_boundary(&text, scan);
      if is_debate_word_byte(&text, prev) {
        found_word = true;
        break;
      }
      scan = prev;
    }
    if found_word {
      return DocumentOffset {
        paragraph: paragraph_ix,
        byte: previous_debate_word_boundary(&text, byte),
      };
    }
    if paragraph_ix == 0 {
      return DocumentOffset { paragraph: 0, byte: 0 };
    }
    paragraph_ix -= 1;
    byte = paragraph_text_len(&document.paragraphs[paragraph_ix]);
  }
}

pub(super) fn next_debate_word_boundary_in_document(document: &Document, offset: DocumentOffset) -> DocumentOffset {
  if document.paragraphs.is_empty() {
    return DocumentOffset::default();
  }

  let mut paragraph_ix = offset.paragraph.min(document.paragraphs.len() - 1);
  let mut byte = if paragraph_ix == offset.paragraph {
    offset
      .byte
      .min(paragraph_text_len(&document.paragraphs[paragraph_ix]))
  } else {
    paragraph_text_len(&document.paragraphs[paragraph_ix])
  };

  loop {
    let text = paragraph_text(document, paragraph_ix);
    let mut scan = byte.min(text.len());
    while scan < text.len() && !is_debate_word_byte(&text, scan) {
      scan = next_char_boundary(&text, scan);
    }
    if scan < text.len() {
      return DocumentOffset {
        paragraph: paragraph_ix,
        byte: next_debate_word_boundary(&text, byte),
      };
    }
    if paragraph_ix + 1 >= document.paragraphs.len() {
      return document_end(document);
    }
    paragraph_ix += 1;
    byte = 0;
  }
}

pub(super) fn selection_for_word_at(document: &Document, offset: DocumentOffset) -> EditorSelection {
  let Some(paragraph) = document.paragraphs.get(offset.paragraph) else {
    return EditorSelection {
      anchor: DocumentOffset::default(),
      head: DocumentOffset::default(),
    };
  };
  let paragraph_len = paragraph_text_len(paragraph);
  if paragraph_len == 0 || offset.byte >= paragraph_len {
    return selection_for_paragraph_at(document, offset.paragraph);
  }
  EditorSelection {
    anchor: previous_debate_word_boundary_in_document(document, offset),
    head: next_debate_word_boundary_in_document(document, offset),
  }
}

pub(super) fn selection_for_paragraph_at(document: &Document, paragraph: usize) -> EditorSelection {
  let paragraph = paragraph.min(document.paragraphs.len().saturating_sub(1));
  EditorSelection {
    anchor: DocumentOffset { paragraph, byte: 0 },
    head: DocumentOffset {
      paragraph,
      byte: paragraph_text_len(&document.paragraphs[paragraph]),
    },
  }
}

pub(super) fn expand_drag_selection(
  document: &Document,
  anchor: DocumentOffset,
  head: DocumentOffset,
  granularity: SelectionGranularity,
) -> EditorSelection {
  match granularity {
    SelectionGranularity::Character => EditorSelection { anchor, head },
    SelectionGranularity::Word => {
      let anchor_range = selection_for_word_at(document, anchor).normalized();
      let head_range = selection_for_word_at(document, head).normalized();
      if head < anchor {
        EditorSelection {
          anchor: anchor_range.end,
          head: head_range.start,
        }
      } else {
        EditorSelection {
          anchor: anchor_range.start,
          head: head_range.end,
        }
      }
    },
    SelectionGranularity::Paragraph => {
      if head < anchor {
        EditorSelection {
          anchor: DocumentOffset {
            paragraph: anchor.paragraph,
            byte: paragraph_text_len(&document.paragraphs[anchor.paragraph]),
          },
          head: DocumentOffset {
            paragraph: head.paragraph,
            byte: 0,
          },
        }
      } else {
        EditorSelection {
          anchor: DocumentOffset {
            paragraph: anchor.paragraph,
            byte: 0,
          },
          head: DocumentOffset {
            paragraph: head.paragraph,
            byte: paragraph_text_len(&document.paragraphs[head.paragraph]),
          },
        }
      }
    },
  }
}

// Grapheme-cluster-aware step backwards. Handles combining marks and
// compound emoji correctly, so one keystroke deletes one visible character.
pub(super) fn prev_grapheme_boundary_in_paragraph(document: &Document, paragraph_ix: usize, byte: usize) -> usize {
  if byte == 0 {
    return 0;
  }
  // Fast path for the common ASCII case. ASCII bytes are always single-byte
  // grapheme clusters, so we can avoid allocating the paragraph text while
  // someone is holding an arrow/delete key down through ordinary prose.
  if paragraph_byte_at(document, paragraph_ix, byte - 1).is_some_and(|byte| byte.is_ascii()) {
    return byte - 1;
  }
  let text = paragraph_text(document, paragraph_ix);
  prev_grapheme_boundary(&text, byte)
}

pub(super) fn next_grapheme_boundary_in_paragraph(document: &Document, paragraph_ix: usize, byte: usize) -> usize {
  let paragraph = &document.paragraphs[paragraph_ix];
  let len = paragraph_text_len(paragraph);
  if byte >= len {
    return len;
  }
  if paragraph_byte_at(document, paragraph_ix, byte).is_some_and(|byte| byte.is_ascii()) {
    return byte + 1;
  }
  let text = paragraph_text(document, paragraph_ix);
  next_grapheme_boundary(&text, byte)
}

fn paragraph_byte_at(document: &Document, paragraph_ix: usize, byte: usize) -> Option<u8> {
  let paragraph = document.paragraphs.get(paragraph_ix)?;
  (byte < paragraph_text_len(paragraph)).then(|| {
    document
      .text
      .byte(paragraph_byte_range(document, paragraph_ix).start + byte)
  })
}

fn prev_grapheme_boundary(s: &str, byte: usize) -> usize {
  let mut cursor = GraphemeCursor::new(byte, s.len(), true);
  cursor.prev_boundary(s, 0).ok().flatten().unwrap_or(0)
}

fn next_grapheme_boundary(s: &str, byte: usize) -> usize {
  let mut cursor = GraphemeCursor::new(byte, s.len(), true);
  cursor.next_boundary(s, 0).ok().flatten().unwrap_or(s.len())
}
