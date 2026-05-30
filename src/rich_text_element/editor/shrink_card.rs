#[hotpath::measure_all]
impl RichTextEditor {
  pub(super) fn apply_semantic_style_to_card_span(&mut self, semantic: RunSemanticStyle, cx: &mut Context<Self>) {
    if !matches!(semantic, RunSemanticStyle::Condensed | RunSemanticStyle::Ultracondensed) {
      return;
    }
    let Some(target_block_ix) = condensed_card_target_block_ix(&self.document, self.current_block_ix_for_condensed_card()) else {
      return;
    };
    let Some(span) = condensed_card_block_span(&self.document, target_block_ix) else {
      return;
    };

    let before_document = self.document.clone();
    let before_selection = self.selection.clone();
    let clear_semantic = condensed_card_span_all_eligible_runs_have_semantic(&self.document, span.clone(), semantic);
    let mut paragraph_ix = 0usize;
    for block_ix in 0..self.document.blocks.len() {
      let in_span = span.contains(&block_ix);
      match self.document.blocks.get(block_ix) {
        Some(Block::Paragraph(_)) => {
          if in_span {
            let changed = {
              let Some(paragraph) = paragraphs_mut(&mut self.document).get_mut(paragraph_ix) else {
                paragraph_ix += 1;
                continue;
              };
              apply_condensed_semantic_to_paragraph(paragraph, semantic, clear_semantic)
            };
            if changed {
              update_paragraph_block(&mut self.document, paragraph_ix);
            }
          }
          paragraph_ix += 1;
        },
        Some(Block::Table(_)) if in_span => {
          if let Some(Block::Table(table)) = Arc::make_mut(&mut self.document.blocks).get_mut(block_ix)
            && apply_condensed_semantic_to_table(table, semantic, clear_semantic)
          {
            table.version = table.version.wrapping_add(1);
          }
        },
        Some(Block::Image(_) | Block::Equation(_) | Block::Table(_)) | None => {},
      }
    }

    self.push_replace_document_history(before_document, before_selection, cx);
  }

  fn current_block_ix_for_condensed_card(&self) -> Option<usize> {
    match self.selected_block {
      Some(BlockSelection::TableCell { block_ix, .. }) | Some(BlockSelection::Table(block_ix)) => Some(block_ix),
      Some(BlockSelection::Equation(block_ix) | BlockSelection::Image(block_ix)) => Some(block_ix),
      None => block_ix_for_paragraph(&self.document, self.selection.head.paragraph),
    }
  }
}

#[hotpath::measure]
fn condensed_card_target_block_ix(document: &Document, start_block_ix: Option<usize>) -> Option<usize> {
  let mut block_ix = start_block_ix?.min(document.blocks.len().saturating_sub(1));
  while block_ix < document.blocks.len() {
    if condensed_card_block_is_eligible(&document.blocks[block_ix]) {
      return Some(block_ix);
    }
    block_ix += 1;
  }
  None
}

#[hotpath::measure]
fn condensed_card_block_span(document: &Document, target_block_ix: usize) -> Option<Range<usize>> {
  if !document
    .blocks
    .get(target_block_ix)
    .is_some_and(condensed_card_block_is_eligible)
  {
    return None;
  }

  let mut start = target_block_ix;
  while start > 0 && condensed_card_block_is_eligible(&document.blocks[start - 1]) {
    start -= 1;
  }

  let mut end = target_block_ix + 1;
  while end < document.blocks.len() && condensed_card_block_is_eligible(&document.blocks[end]) {
    end += 1;
  }

  Some(start..end)
}

#[hotpath::measure]
fn condensed_card_block_is_eligible(block: &Block) -> bool {
  match block {
    Block::Paragraph(paragraph) => condensed_card_paragraph_is_eligible(paragraph),
    Block::Table(table) => !table_contains_cite(table),
    Block::Image(_) | Block::Equation(_) => false,
  }
}

#[hotpath::measure]
fn condensed_card_paragraph_is_eligible(paragraph: &Paragraph) -> bool {
  paragraph.style == ParagraphStyle::Normal && !paragraph_contains_cite(paragraph)
}

#[hotpath::measure]
fn paragraph_contains_cite(paragraph: &Paragraph) -> bool {
  paragraph
    .runs
    .iter()
    .any(|run| run.styles.semantic == RunSemanticStyle::Cite)
}

#[hotpath::measure]
fn table_contains_cite(table: &TableBlock) -> bool {
  table.rows.iter().any(|row| {
    row.cells.iter().any(|cell| {
      cell.blocks.iter().any(|block| match block {
        TableCellBlock::Paragraph(paragraph) => paragraph_contains_cite(&paragraph.paragraph),
        TableCellBlock::Table(table) => table_contains_cite(table),
      })
    })
  })
}

#[hotpath::measure]
fn condensed_card_span_all_eligible_runs_have_semantic(document: &Document, span: Range<usize>, semantic: RunSemanticStyle) -> bool {
  let mut saw_eligible = false;
  for block_ix in span {
    let Some(block) = document.blocks.get(block_ix) else {
      continue;
    };
    if !condensed_card_block_eligible_runs_have_semantic(block, semantic, &mut saw_eligible) {
      return false;
    }
  }
  saw_eligible
}

#[hotpath::measure]
fn condensed_card_block_eligible_runs_have_semantic(block: &Block, semantic: RunSemanticStyle, saw_eligible: &mut bool) -> bool {
  match block {
    Block::Paragraph(paragraph) => condensed_card_paragraph_eligible_runs_have_semantic(paragraph, semantic, saw_eligible),
    Block::Table(table) => condensed_card_table_eligible_runs_have_semantic(table, semantic, saw_eligible),
    Block::Image(_) | Block::Equation(_) => true,
  }
}

#[hotpath::measure]
fn condensed_card_paragraph_eligible_runs_have_semantic(
  paragraph: &Paragraph,
  semantic: RunSemanticStyle,
  saw_eligible: &mut bool,
) -> bool {
  for run in &paragraph.runs {
    if condensed_card_run_is_eligible(run.styles) {
      *saw_eligible = true;
      if run.styles.semantic != semantic {
        return false;
      }
    }
  }
  true
}

#[hotpath::measure]
fn condensed_card_table_eligible_runs_have_semantic(table: &TableBlock, semantic: RunSemanticStyle, saw_eligible: &mut bool) -> bool {
  for row in &table.rows {
    for cell in &row.cells {
      for block in &cell.blocks {
        let ok = match block {
          TableCellBlock::Paragraph(paragraph) => {
            condensed_card_paragraph_eligible_runs_have_semantic(&paragraph.paragraph, semantic, saw_eligible)
          },
          TableCellBlock::Table(table) => condensed_card_table_eligible_runs_have_semantic(table, semantic, saw_eligible),
        };
        if !ok {
          return false;
        }
      }
    }
  }
  true
}

#[hotpath::measure]
fn apply_condensed_semantic_to_paragraph(paragraph: &mut Paragraph, semantic: RunSemanticStyle, clear_semantic: bool) -> bool {
  let old_runs = paragraph.runs.clone();
  for run in &mut paragraph.runs {
    if condensed_card_run_is_eligible(run.styles) {
      run.styles.semantic = if clear_semantic { RunSemanticStyle::Plain } else { semantic };
    }
  }
  paragraph.runs = merge_adjacent_runs(std::mem::take(&mut paragraph.runs));
  let changed = paragraph.runs != old_runs;
  if changed {
    bump_paragraph_version(paragraph);
  }
  changed
}

#[hotpath::measure]
fn apply_condensed_semantic_to_table(table: &mut TableBlock, semantic: RunSemanticStyle, clear_semantic: bool) -> bool {
  let mut changed = false;
  for row in &mut table.rows {
    for cell in &mut row.cells {
      for block in &mut cell.blocks {
        match block {
          TableCellBlock::Paragraph(paragraph) => {
            changed |= apply_condensed_semantic_to_paragraph(&mut paragraph.paragraph, semantic, clear_semantic);
          },
          TableCellBlock::Table(table) => {
            changed |= apply_condensed_semantic_to_table(table, semantic, clear_semantic);
          },
        }
      }
    }
  }
  changed
}

#[hotpath::measure]
fn condensed_card_run_is_eligible(styles: RunStyles) -> bool {
  !styles.direct_underline && !matches!(styles.semantic, RunSemanticStyle::Underline | RunSemanticStyle::Emphasis)
}
