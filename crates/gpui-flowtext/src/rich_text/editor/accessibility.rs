// Accessibility projection for the document body.
//
// The editor paints glyphs directly, so nothing about the document reaches
// assistive technology unless we build it. gpui gives a custom `Element` two
// hooks — `a11y_role()` and `a11y_synthetic_children()` — and the second can
// only push a FLAT list of leaf nodes under the element's own node
// (`A11ySubtreeBuilder::push_child` -> `push_leaf`). Nesting is impossible.
//
// So document STRUCTURE comes from the element tree (one node per paragraph,
// because a paragraph is already one `VirtualParagraphChunkElement`), and the
// text inside each paragraph comes from that element's synthetic `TextRun`
// children. That is also why the info below is computed per paragraph rather
// than for the document as a whole.
//
// Everything here is gated on `Window::is_a11y_active()` by the caller: the
// tree is rebuilt every frame and none of this is observable without assistive
// technology attached.

/// AccessKit's `word_starts`/`character_lengths` are `u8`-indexed, so a single
/// `TextRun` cannot describe more than 255 characters. Longer paragraphs are
/// split across several runs linked with `previous_on_line`/`next_on_line`.
pub(super) const MAX_CHARS_PER_TEXT_RUN: usize = 255;

/// What one paragraph exposes to assistive technology.
#[derive(Clone)]
pub(super) struct A11yParagraphInfo {
  pub(super) role: gpui::Role,
  /// Heading depth, when this paragraph is a heading. Maps to `aria_level`.
  pub(super) level: Option<usize>,
  /// The paragraph's plain text.
  pub(super) text: String,
  /// Selection within this paragraph, as CHARACTER indices (AccessKit counts
  /// characters; the editor stores UTF-8 byte offsets).
  pub(super) selection: Option<(usize, usize)>,
}

#[hotpath::measure_all]
impl RichTextEditor {
  /// Build the accessibility description of one paragraph, or `None` when the
  /// paragraph does not exist.
  ///
  /// Only chunk 0 of a paragraph reports itself. A long paragraph is split into
  /// several `VirtualParagraphChunkElement`s purely so the virtual list can
  /// size and recycle it, but that is a layout artifact — to a screen reader it
  /// is ONE paragraph. Letting every chunk report would either duplicate the
  /// text or expose arbitrary fragments as separate paragraphs.
  pub(super) fn a11y_paragraph_info(&self, paragraph_ix: usize, chunk_ix: usize) -> Option<A11yParagraphInfo> {
    if chunk_ix != 0 {
      return None;
    }
    let paragraph = self.document.paragraphs.get(paragraph_ix)?;

    // Heading level comes from the THEME, not the paragraph: `ParagraphStyle`
    // is an opaque slot and `section_level_and_kind` resolves it.
    let (role, level) = if paragraph_is_heading(&self.document, paragraph_ix) {
      let level = section_level_and_kind(&self.document, paragraph.style).map_or(1, |(level, _kind)| level.max(1));
      (gpui::Role::Heading, Some(level))
    } else {
      (gpui::Role::Paragraph, None)
    };

    let text = paragraph_text(&self.document, paragraph_ix);
    let selection = self.a11y_selection_in_paragraph(paragraph_ix, &text);

    Some(A11yParagraphInfo {
      role,
      level,
      text,
      selection,
    })
  }

  /// The selection clipped to `paragraph_ix`, converted from byte offsets to
  /// character indices. Returns `None` when the selection does not touch this
  /// paragraph, so only the paragraph the caret is in reports a text selection.
  fn a11y_selection_in_paragraph(&self, paragraph_ix: usize, text: &str) -> Option<(usize, usize)> {
    let range = self.selection.normalized();
    if paragraph_ix < range.start.paragraph || paragraph_ix > range.end.paragraph {
      return None;
    }
    let start_byte = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
    let end_byte = if paragraph_ix == range.end.paragraph { range.end.byte } else { text.len() };
    Some((char_index_for_byte(text, start_byte), char_index_for_byte(text, end_byte)))
  }
}

// Byte -> character index conversion reuses `char_index_for_byte` from
// `render_blocks.rs`: AccessKit indexes `character_lengths`, so every position
// handed to it must be a character count, and that helper already rounds a
// mid-codepoint byte offset down rather than panicking.

/// Split `text` into AccessKit `TextRun` nodes of at most
/// [`MAX_CHARS_PER_TEXT_RUN`] characters.
///
/// Always yields at least one run, even for an empty paragraph: the platform
/// text pattern needs a run to anchor a caret to, and a paragraph the caret can
/// sit in but that exposes no run would make the caret unreportable.
pub(super) fn a11y_text_runs(text: &str) -> Vec<String> {
  if text.is_empty() {
    return vec![String::new()];
  }
  let chars: Vec<char> = text.chars().collect();
  chars
    .chunks(MAX_CHARS_PER_TEXT_RUN)
    .map(|chunk| chunk.iter().collect())
    .collect()
}

/// Build the AccessKit node for one text run.
pub(super) fn a11y_text_run_node(chunk: &str) -> gpui::accesskit::Node {
  let mut node = gpui::accesskit::Node::new(gpui::accesskit::Role::TextRun);
  node.set_text_direction(gpui::accesskit::TextDirection::LeftToRight);
  node.set_value(chunk.to_string());
  // One entry per character, holding that character's UTF-8 length. This is
  // what lets a screen reader map its character cursor onto our bytes.
  node.set_character_lengths(
    chunk
      .chars()
      .map(|c| u8::try_from(c.len_utf8()).unwrap_or(1))
      .collect::<Vec<u8>>(),
  );
  // Word boundaries, as CHARACTER offsets within this run.
  let mut word_starts: Vec<u8> = Vec::new();
  let mut prev_was_space = true;
  for (char_ix, c) in chunk.chars().enumerate() {
    let is_space = c.is_whitespace();
    if prev_was_space
      && !is_space
      && let Ok(start) = u8::try_from(char_ix)
    {
      word_starts.push(start);
    }
    prev_was_space = is_space;
  }
  node.set_word_starts(word_starts);
  node
}
