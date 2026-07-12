#[hotpath::measure_all]
impl RichTextEditor {
  /// Plain-text paste, Loro-first (spec §5): a single line is ONE `InsertText`
  /// intent; multi-line text converts to ONE `InsertRichFragment` intent (one
  /// gate hold, one commit, one undo unit). No direct projection mutation.
  ///
  /// Returns true when this path owned the paste. With an object block
  /// selected the caller's block-selection flows own placement instead.
  fn insert_plain_text_paste_at_caret(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    if self.selected_block.is_some() {
      return false;
    }
    self.write_plain_text_paste(text, cx)
  }

  /// Shared plain-text paste body (also the `insert_plain_text_fragment`
  /// compatibility surface, which historically ignored `selected_block`).
  /// Selection replacement + undo grouping are the write helpers' law.
  pub(super) fn write_plain_text_paste(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.is_empty() {
      return false;
    }
    if !normalized.contains('\n') {
      // Single line, no structure: plain insert. Style inheritance is Loro's
      // job (expand-`After` marks, spec §9) — no restated run styles.
      self.write_insert_text_at_caret(&normalized, cx);
      return true;
    }
    // Multi-line: one rich fragment so the whole paste is one intent and one
    // undo unit. Lines keep the caret paragraph's style and the caret's run
    // styles (previous paste behavior).
    let start = self.selection.normalized().start;
    let paragraph_style = self
      .document
      .paragraphs
      .get(start.paragraph)
      .map(|paragraph| paragraph.style)
      .unwrap_or(ParagraphStyle::Normal);
    let styles = self.styles_at_caret();
    let blocks = normalized
      .split('\n')
      .map(|line| {
        FragmentBlock::Paragraph(InputParagraph {
          style: paragraph_style,
          runs: if line.is_empty() {
            Vec::new()
          } else {
            vec![InputRun {
              text: line.to_string(),
              styles,
            }]
          },
        })
      })
      .collect::<Vec<_>>();
    self.write_insert_rich_fragment_at_caret(blocks, cx);
    true
  }

  /// Rich-fragment paste (the JSON-metadata clipboard path), Loro-first: the
  /// fragment's paragraphs + objects convert to `FragmentBlock`s in order and
  /// commit as ONE `InsertRichFragment` intent (a lone object commits as the
  /// precise `InsertObject` intent instead). Caret placement comes from the
  /// commit's `selection_after`, never from editor arithmetic.
  pub fn insert_rich_fragment_paste_at_caret(&mut self, fragment: &RichClipboardFragment, cx: &mut Context<Self>) -> bool {
    if self.selected_block.is_some() {
      return false;
    }
    self.write_clipboard_fragment_at_caret(fragment, cx)
  }

  /// Convert a clipboard fragment into intent vocabulary and commit it through
  /// the write authority. Empty fragments are a handled no-op (true) rather
  /// than an `EmptyIntent` round-trip through the authority.
  pub(super) fn write_clipboard_fragment_at_caret(&mut self, fragment: &RichClipboardFragment, cx: &mut Context<Self>) -> bool {
    let blocks = fragment_blocks_from_clipboard(fragment);
    if clipboard_fragment_is_empty(&blocks) {
      return true;
    }
    self.adopt_clipboard_assets(&fragment.assets);
    if let [FragmentBlock::Object(block)] = blocks.as_slice() {
      let block = block.clone();
      self.write_object_block_replacing_selection(block, cx);
      return true;
    }
    self.write_insert_rich_fragment_at_caret(blocks, cx);
    true
  }

  fn selected_table_cell_fragment(&self) -> Option<RichClipboardFragment> {
    let cell = self.selected_table_cell()?;
    if let (Some(range), Some(paragraph)) = (self.table_cell_selection_range(), self.selected_table_cell_paragraph()) {
      return Some(RichClipboardFragment {
        format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
        paragraphs: vec![input_paragraph_from_table_cell_range(paragraph, range)],
        blocks: Vec::new(),
        assets: Vec::new(),
      });
    }
    let paragraphs = cell
      .blocks
      .iter()
      .filter_map(|block| match block {
        TableCellBlock::Paragraph(paragraph) => Some(input_paragraph_from_table_cell_paragraph(paragraph)),
        TableCellBlock::Table(_) => None,
      })
      .collect::<Vec<_>>();
    (!paragraphs.is_empty()).then_some(RichClipboardFragment {
      format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
      paragraphs,
      blocks: Vec::new(),
      assets: Vec::new(),
    })
  }

  fn insert_plain_text_into_selected_table_cell(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.is_empty() {
      return matches!(self.selected_block, Some(BlockSelection::TableCell { .. }));
    }
    let styles = self
      .selected_table_cell_paragraph()
      .map(|paragraph| table_cell_styles_at(paragraph, self.table_cell_caret))
      .unwrap_or_default();
    let paragraphs = normalized
      .split('\n')
      .map(|line| InputParagraph {
        style: ParagraphStyle::Normal,
        runs: if line.is_empty() {
          Vec::new()
        } else {
          vec![InputRun {
            text: line.to_string(),
            styles,
          }]
        },
      })
      .collect::<Vec<_>>();
    self.insert_paragraphs_into_selected_table_cell(&paragraphs, cx)
  }

  fn insert_rich_fragment_into_selected_table_cell(&mut self, fragment: &RichClipboardFragment, cx: &mut Context<Self>) -> bool {
    if fragment
      .blocks
      .iter()
      .any(|block| !matches!(block, InputBlock::Paragraph(_)))
    {
      return false;
    }
    if !fragment.blocks.is_empty() {
      let paragraphs = fragment
        .blocks
        .iter()
        .filter_map(|block| match block {
          InputBlock::Paragraph(paragraph) => Some(paragraph.clone()),
          InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_) => None,
        })
        .collect::<Vec<_>>();
      return self.insert_paragraphs_into_selected_table_cell(&paragraphs, cx);
    }
    self.insert_paragraphs_into_selected_table_cell(&fragment.paragraphs, cx)
  }

  fn insert_paragraphs_into_selected_table_cell(&mut self, paragraphs: &[InputParagraph], cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell {
      block_ix,
      row_ix,
      cell_ix,
      ..
    }) = self.selected_block
    else {
      return false;
    };
    if paragraphs.is_empty() {
      return true;
    }
    let current_paragraph_ix = self.table_cell_block_ix;
    let caret = self.table_cell_caret;
    let mut new_caret = None;
    self.edit_table_cell(block_ix, row_ix, cell_ix, cx, |cell| {
      new_caret = insert_table_cell_paragraphs_at(cell, current_paragraph_ix, caret, paragraphs);
    });
    if let Some((paragraph_ix, byte)) = new_caret {
      self.table_cell_block_ix = paragraph_ix;
      self.table_cell_caret = byte;
      cx.notify();
    }
    true
  }

  fn selected_table_cell_text(&self) -> Option<String> {
    self
      .selected_table_cell_paragraph()
      .map(|paragraph| paragraph.text.clone())
  }

  fn selected_equation_source(&self) -> Option<String> {
    let BlockSelection::Equation(block_ix) = self.selected_block? else {
      return None;
    };
    let Block::Equation(equation) = self.document.blocks.get(block_ix)? else {
      return None;
    };
    Some(equation.source.to_string())
  }

  fn equation_source_selection_range(&self) -> Option<Range<usize>> {
    if self.equation_source_anchor == self.equation_source_caret {
      return None;
    }
    Some(self.equation_source_anchor.min(self.equation_source_caret)..self.equation_source_anchor.max(self.equation_source_caret))
  }

  fn selected_equation_source_text(&self) -> Option<String> {
    let source = self.selected_equation_source()?;
    let range = self.equation_source_selection_range()?;
    Some(source.get(range).unwrap_or("").to_string())
  }

  fn equation_source_selection_for_render(&self, block_ix: usize) -> Option<EquationSourceSelection> {
    if self.selected_block != Some(BlockSelection::Equation(block_ix)) {
      return None;
    }
    Some(EquationSourceSelection {
      anchor: self.equation_source_anchor,
      caret: self.equation_source_caret,
      caret_visible: self.caret_visible,
    })
  }

  pub(super) fn table_cell_selection_range(&self) -> Option<Range<usize>> {
    if self.table_cell_anchor == self.table_cell_caret {
      return None;
    }
    Some(self.table_cell_anchor.min(self.table_cell_caret)..self.table_cell_anchor.max(self.table_cell_caret))
  }

  fn selected_table_cell_paragraph(&self) -> Option<&TableCellParagraph> {
    let cell = self.selected_table_cell()?;
    let paragraph_ix = table_cell_paragraph_block_ix(cell, self.table_cell_block_ix)?;
    let TableCellBlock::Paragraph(paragraph) = cell.blocks.get(paragraph_ix)? else {
      return None;
    };
    Some(paragraph)
  }

  fn selected_table_cell(&self) -> Option<&TableCell> {
    let BlockSelection::TableCell { block_ix, row_ix, cell_ix, .. } = self.selected_block? else {
      return None;
    };
    let Block::Table(table) = self.document.blocks.get(block_ix)? else {
      return None;
    };
    table.rows.get(row_ix)?.cells.get(cell_ix)
  }

  fn adjacent_selected_table_cell_paragraph(&self, forward: bool) -> Option<(usize, usize)> {
    let cell = self.selected_table_cell()?;
    let current_ix = table_cell_paragraph_block_ix(cell, self.table_cell_block_ix)?;
    let paragraph_ix = if forward {
      next_table_cell_paragraph_block_ix(cell, current_ix)?
    } else {
      previous_table_cell_paragraph_block_ix(cell, current_ix)?
    };
    let TableCellBlock::Paragraph(paragraph) = cell.blocks.get(paragraph_ix)? else {
      return None;
    };
    Some((paragraph_ix, paragraph.text.len()))
  }

  fn clear_selected_table_cell(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix, .. }) = self.selected_block else {
      return false;
    };
    self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
      paragraph.text.clear();
      paragraph.paragraph.runs.clear();
      paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
    });
    true
  }

  fn move_selected_table_cell(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix, .. }) = self.selected_block else {
      return false;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix) else {
      return false;
    };
    let mut positions = Vec::new();
    for (row, table_row) in table.rows.iter().enumerate() {
      for cell in 0..table_row.cells.len() {
        positions.push((row, cell));
      }
    }
    let Some(current) = positions
      .iter()
      .position(|&(row, cell)| row == row_ix && cell == cell_ix)
    else {
      return false;
    };
    let next = if forward { current + 1 } else { current.saturating_sub(1) };
    let Some(&(row_ix, cell_ix)) = positions.get(next) else {
      return false;
    };
    let (row_id, column_id) = table_cell_ids_at(&self.document, block_ix, row_ix, cell_ix);
    self.selected_block = Some(BlockSelection::TableCell {
      block_ix,
      row_ix,
      cell_ix,
      row_id,
      column_id,
    });
    self.table_cell_block_ix = 0;
    self.table_cell_caret = self
      .selected_table_cell_text()
      .map(|text| text.len())
      .unwrap_or(0);
    cx.notify();
    true
  }

}

/// Order-preserving conversion of a clipboard fragment into the intent
/// vocabulary: block-shaped fragments already carry paragraphs and objects in
/// document order; paragraph-only fragments are just paragraphs.
fn fragment_blocks_from_clipboard(fragment: &RichClipboardFragment) -> Vec<FragmentBlock> {
  if fragment.blocks.is_empty() {
    fragment
      .paragraphs
      .iter()
      .cloned()
      .map(FragmentBlock::Paragraph)
      .collect()
  } else {
    fragment
      .blocks
      .iter()
      .map(|block| match block {
        InputBlock::Paragraph(paragraph) => FragmentBlock::Paragraph(paragraph.clone()),
        block => FragmentBlock::Object(block.clone()),
      })
      .collect()
  }
}

/// True when the fragment would insert nothing: no blocks, or exactly one
/// paragraph with no text. Multiple empty paragraphs still insert paragraph
/// boundaries and are NOT empty.
fn clipboard_fragment_is_empty(blocks: &[FragmentBlock]) -> bool {
  match blocks {
    [] => true,
    [FragmentBlock::Paragraph(paragraph)] => paragraph.runs.iter().all(|run| run.text.is_empty()),
    _ => false,
  }
}
