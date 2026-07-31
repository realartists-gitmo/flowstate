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
  /// The same text, split at run-style boundaries (and the 255-char limit).
  pub(super) spans: Vec<A11yTextSpan>,
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
    let spans = a11y_spans_for_paragraph(&self.document, paragraph_ix);

    Some(A11yParagraphInfo {
      role,
      level,
      text,
      spans,
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

/// One table cell as assistive technology sees it.
///
/// Cells are FLAT children of the table node — synthetic children cannot nest,
/// so there is no `Role::Row` layer. That is fine: AccessKit conveys the grid
/// through each cell's row/column index, which is what a screen reader's table
/// navigation actually reads.
#[derive(Clone)]
pub(super) struct A11yTableCell {
  pub(super) row: usize,
  pub(super) column: usize,
  pub(super) text: String,
  /// True for cells in a header row, so AT announces them as headers and can
  /// repeat them while navigating the body.
  pub(super) is_header: bool,
}

/// What one structural block (image, equation, table) exposes.
#[derive(Clone)]
pub(super) struct A11yBlockInfo {
  pub(super) role: gpui::Role,
  /// Accessible name. For an image this is its alt text; for an equation, its
  /// source; for a table, a summary of its shape.
  pub(super) label: String,
  /// `(rows, columns)` for a table, so AT can announce its dimensions.
  pub(super) table_shape: Option<(usize, usize)>,
  pub(super) cells: Vec<A11yTableCell>,
}

/// Flatten a cell's blocks into one string.
///
/// Cell text does NOT live in `document.text` — it is carried inline on
/// `TableCellParagraph`, so the ordinary paragraph walk never sees it and a
/// table would otherwise be an empty grid to a screen reader. Nested tables are
/// summarised rather than recursed: a cell announcing an entire inner table
/// inline would bury the outer row.
fn a11y_cell_text(cell: &TableCell) -> String {
  let mut parts: Vec<String> = Vec::new();
  for block in &cell.blocks {
    match block {
      TableCellBlock::Paragraph(paragraph) => {
        let text = paragraph.text.trim();
        if !text.is_empty() {
          parts.push(text.to_string());
        }
      },
      TableCellBlock::Table(inner) => {
        parts.push(format!("nested table, {} rows, {} columns", inner.rows.len(), inner.columns.len()));
      },
    }
  }
  parts.join(" ")
}

#[hotpath::measure_all]
impl RichTextEditor {
  /// Accessibility description of a structural (non-paragraph) block.
  pub(super) fn a11y_block_info(&self, block_ix: usize) -> Option<A11yBlockInfo> {
    match self.document.blocks.get(block_ix)? {
      Block::Paragraph(_) => None,
      Block::Image(image) => Some(A11yBlockInfo {
        cells: Vec::new(),
        role: gpui::Role::Image,
        // Alt text is authored content and already the right name. An image
        // with none is announced as "image" with no description, which is the
        // honest outcome — inventing a name would be worse.
        label: image.alt_text.to_string(),
        table_shape: None,
      }),
      Block::Equation(equation) => Some(A11yBlockInfo {
        cells: Vec::new(),
        role: gpui::Role::Math,
        // The LaTeX source is the only textual form of an equation we hold.
        // It is not ideal prose, but it is lossless and lets a reader who knows
        // LaTeX follow the maths, which a bare "equation" does not.
        label: equation.source.to_string(),
        table_shape: None,
      }),
      Block::Table(table) => {
        let rows = table.rows.len();
        let columns = table.columns.len();
        // Column position comes from the row's own cell order rather than the
        // ColumnId, because a cell's index within its row is what the grid
        // actually renders; spans are reported so AT can account for them.
        let cells = table
          .rows
          .iter()
          .enumerate()
          .flat_map(|(row_ix, row)| {
            row.cells.iter().enumerate().map(move |(column_ix, cell)| A11yTableCell {
              row: row_ix,
              column: column_ix,
              text: a11y_cell_text(cell),
              is_header: table.style.header_row && row_ix == 0,
            })
          })
          .collect();
        Some(A11yBlockInfo {
          role: gpui::Role::Table,
          label: format!("Table, {rows} rows, {columns} columns"),
          table_shape: Some((rows, columns)),
          cells,
        })
      },
    }
  }
}

/// One accessible span of a paragraph: a stretch of text sharing one set of run
/// styles, already clipped to at most [`MAX_CHARS_PER_TEXT_RUN`] characters.
#[derive(Clone)]
pub(super) struct A11yTextSpan {
  pub(super) text: String,
  /// Presentation is RESOLVED here rather than stored as raw `RunStyles`:
  /// `Element::a11y_synthetic_children` gets no `&App`, so it cannot read the
  /// theme to turn a style slot into a colour.
  pub(super) underline: bool,
  pub(super) role_description: Option<&'static str>,
  pub(super) foreground: Option<gpui::accesskit::Color>,
  pub(super) background: Option<gpui::accesskit::Color>,
}

impl A11yTextSpan {
  fn plain(text: String) -> Self {
    Self {
      text,
      underline: false,
      role_description: None,
      foreground: None,
      background: None,
    }
  }
}

/// Split a paragraph into spans that break at BOTH run-style boundaries and the
/// 255-character AccessKit limit.
///
/// Splitting at style boundaries is what lets a citation, a highlight or an
/// emphasis be announced as such: those are conveyed on screen purely by colour
/// and weight, so without a per-span node they are invisible non-visually — a
/// screen reader would read a card and its citation as one undifferentiated
/// block.
pub(super) fn a11y_spans_for_paragraph(document: &DocumentProjection, paragraph_ix: usize) -> Vec<A11yTextSpan> {
  let Some(paragraph) = document.paragraphs.get(paragraph_ix) else {
    return vec![A11yTextSpan::plain(String::new())];
  };
  let text = paragraph_text(document, paragraph_ix);
  let mut spans = Vec::new();
  let mut byte = 0_usize;
  for run in &paragraph.runs {
    let end = (byte + run.len).min(text.len());
    if byte >= end {
      byte = end;
      continue;
    }
    // Run lengths are byte counts; snap to a char boundary defensively so a
    // malformed length cannot panic the whole projection.
    let slice = &text[snap_to_char_boundary(&text, byte)..snap_to_char_boundary(&text, end)];
    for chunk in split_chars(slice, MAX_CHARS_PER_TEXT_RUN) {
      spans.push(resolve_a11y_span(chunk, run.styles, document));
    }
    byte = end;
  }
  if spans.is_empty() {
    spans.push(A11yTextSpan::plain(String::new()));
  }
  spans
}

fn snap_to_char_boundary(text: &str, byte: usize) -> usize {
  let mut byte = byte.min(text.len());
  while byte > 0 && !text.is_char_boundary(byte) {
    byte -= 1;
  }
  byte
}

fn split_chars(text: &str, max: usize) -> Vec<String> {
  if text.is_empty() {
    return vec![String::new()];
  }
  text.chars().collect::<Vec<char>>().chunks(max).map(|c| c.iter().collect()).collect()
}

/// Resolve a run's styles into the properties AccessKit understands.
///
/// `Role::TextRun` is KEPT rather than swapping in `Role::Mark`/`Role::Comment`:
/// synthetic children are flat leaves, so a `Mark` node could not CONTAIN the
/// run, and changing the role would drop the span out of the text pattern that
/// makes caret tracking and review commands work. Style is conveyed through
/// properties instead, which is what those properties are for.
fn resolve_a11y_span(text: String, styles: RunStyles, document: &DocumentProjection) -> A11yTextSpan {
  let mut span = A11yTextSpan::plain(text);
  span.underline = styles.direct_underline;
  if styles.strikethrough {
    // AccessKit has no strikethrough property, and `role_description` is the
    // only channel that reaches the user. A struck span in a debate card is
    // material — it is text the speaker did NOT read.
    span.role_description = Some("struck through");
  }
  // Slot -> theme lookup mirrors `layout/format.rs`, including the `& 0x7f`
  // mask, so the accessible presentation matches what is painted.
  let theme = &document.theme;
  if let Some(HighlightStyle::Custom(slot)) = styles.highlight {
    let color = theme
      .custom_highlight_styles
      .get(&(slot & 0x7f))
      .map_or(theme.default_highlight_color, |style| style.color);
    span.background = Some(hsla_to_accesskit_color(color));
  }
  if let RunSemanticStyle::Custom(slot) = styles.semantic
    && let Some(style) = theme.custom_semantic_styles.get(&(slot & 0x7f))
  {
    if let Some(color) = style.color {
      span.foreground = Some(hsla_to_accesskit_color(color));
    }
    if !matches!(style.underline, None | Some(ThemeUnderline::None)) {
      span.underline = true;
    }
  }
  span
}

fn hsla_to_accesskit_color(color: gpui::Hsla) -> gpui::accesskit::Color {
  let rgba = gpui::Rgba::from(color);
  let byte = |v: f32| (v.clamp(0.0, 1.0) * 255.0).round() as u8;
  gpui::accesskit::Color {
    red: byte(rgba.r),
    green: byte(rgba.g),
    blue: byte(rgba.b),
    alpha: byte(rgba.a),
  }
}

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
