#[hotpath::measure_all]
impl RichTextEditor {
  pub fn insert_toolkit_text_at_caret(&mut self, paragraphs: Vec<InputParagraph>, cx: &mut Context<Self>) {
    let paragraphs = non_empty_input_paragraphs(paragraphs);
    if paragraphs.is_empty() {
      return;
    }
    let fragment = RichClipboardFragment {
      format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
      paragraphs,
      blocks: Vec::new(),
      assets: Vec::new(),
    };
    if self.insert_rich_fragment_into_selected_table_cell(&fragment, cx) {
      return;
    }
    if self.insert_rich_fragment_paste_at_caret(&fragment, cx) {
      return;
    }
    self.apply_document_edit(cx, |editor, cx| editor.insert_rich_fragment(fragment, cx));
  }

  pub fn insert_toolkit_paragraphs_as_blocks(&mut self, paragraphs: Vec<InputParagraph>, cx: &mut Context<Self>) {
    let blocks = non_empty_input_paragraphs(paragraphs)
      .into_iter()
      .map(InputBlock::Paragraph)
      .collect::<Vec<_>>();
    if blocks.is_empty() {
      return;
    }
    let fragment = RichClipboardFragment {
      format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
      paragraphs: Vec::new(),
      blocks,
      assets: Vec::new(),
    };
    self.insert_block_fragment(fragment, cx);
  }

  fn insert_rich_fragment(&mut self, fragment: RichClipboardFragment, cx: &mut Context<Self>) {
    if !fragment.blocks.is_empty() {
      self.insert_block_fragment(fragment, cx);
      return;
    }
    if fragment.paragraphs.is_empty() {
      return;
    }
    if !self.selection.is_caret() {
      self.delete_selection_internal();
    }
    let caret = insert_rich_fragment_at(&mut self.document, self.selection.head, &fragment);
    self.selection = EditorSelection { anchor: caret, head: caret };
    self.emit_selection_changed(cx);
    self.after_text_mutation(cx);
  }

  fn insert_block_fragment(&mut self, fragment: RichClipboardFragment, cx: &mut Context<Self>) {
    if fragment.blocks.is_empty() {
      return;
    }
    let before_document = self.document.clone();
    let before_selection = self.selection.clone();
    for asset in fragment.assets {
      self.document.assets.assets.insert(
        asset.id,
        AssetRecord {
          id: asset.id,
          mime_type: asset.mime_type.into(),
          original_name: asset.original_name.map(Into::into),
          content_hash: asset.content_hash,
          bytes: Arc::new(asset.bytes),
        },
      );
    }
    let inserted_range = self.insert_ordered_block_fragment_after_caret(&fragment.blocks, cx);
    let semantic_commands = self.insert_block_semantic_commands(&before_document, &before_selection, inserted_range);
    self.push_document_snapshot_history(
      before_document,
      before_selection,
      semantic_commands.unwrap_or_default(),
      cx,
    );
  }

  fn insert_ordered_block_fragment_after_caret(&mut self, input_blocks: &[InputBlock], cx: &mut Context<Self>) -> Range<usize> {
    let remove_empty_caret_paragraph = !input_blocks.iter().any(|block| matches!(block, InputBlock::Paragraph(_)));
    let insert_ix = self.prepare_block_insertion_index(remove_empty_caret_paragraph, cx);
    let insert_paragraph_ix = self
      .document
      .blocks
      .iter()
      .take(insert_ix)
      .filter(|block| matches!(block, Block::Paragraph(_)))
      .count();
    let inserted_paragraph_inputs = input_blocks
      .iter()
      .filter_map(|block| match block {
        InputBlock::Paragraph(paragraph) => Some(paragraph.clone()),
        InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_) => None,
      })
      .collect::<Vec<_>>();
    let inserted_paragraphs = insert_standalone_paragraphs_into_projection(&mut self.document, insert_paragraph_ix, &inserted_paragraph_inputs);
    let mut inserted_paragraph_ix = 0;
    let inserted_blocks = input_blocks
      .iter()
      .map(|block| match block {
        InputBlock::Paragraph(_) => {
          let paragraph = inserted_paragraphs
            .get(inserted_paragraph_ix)
            .cloned()
            .unwrap_or_else(|| Paragraph {
              style: ParagraphStyle::Normal,
              byte_range: 0..0,
              runs: Vec::new(),
              version: 0,
            });
          inserted_paragraph_ix += 1;
          Block::Paragraph(paragraph)
        },
        InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_) => block_from_input_block(block),
      })
      .collect::<Vec<_>>();
    let old_blocks = self.document.blocks.as_ref().clone();
    let old_block_ids = self.document.ids.block_ids.clone();
    let mut paragraph_ix = 0;
    let mut output = Vec::with_capacity(old_blocks.len() + inserted_blocks.len());
    let mut output_block_ids = Vec::with_capacity(old_blocks.len() + inserted_blocks.len());
    for (block_ix, block) in old_blocks.iter().enumerate() {
      if block_ix == insert_ix {
        output.extend(inserted_blocks.iter().cloned());
        output_block_ids.extend((0..inserted_blocks.len()).map(|_| new_block_id()));
      }
      match block {
        Block::Paragraph(_) => {
          if let Some(paragraph) = self.document.paragraphs.get(paragraph_ix) {
            output.push(Block::Paragraph(paragraph.clone()));
            output_block_ids.push(old_block_ids.get(block_ix).copied().unwrap_or_else(new_block_id));
          }
          paragraph_ix += 1;
        },
        Block::Image(_) | Block::Equation(_) | Block::Table(_) => {
          output.push(block.clone());
          output_block_ids.push(old_block_ids.get(block_ix).copied().unwrap_or_else(new_block_id));
        },
      }
    }
    if insert_ix >= old_blocks.len() {
      output.extend(inserted_blocks);
      output_block_ids.extend((0..input_blocks.len()).map(|_| new_block_id()));
    }
    self.document.blocks = Arc::new(output);
    self.document.ids.block_ids = output_block_ids;
    rebuild_document_sections(&mut self.document);
    self.selected_block = None;
    self.clear_layout_work_caches();
    self.item_sizes_cache = None;
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
    insert_ix..insert_ix + input_blocks.len()
  }

  fn insert_blocks_after_caret(&mut self, blocks: Vec<Block>, cx: &mut Context<Self>) {
    if blocks.is_empty() {
      return;
    }
    let before_document = self.document.clone();
    let before_selection = self.selection.clone();
    let inserted_range = self.insert_blocks_after_caret_without_history(blocks, cx);
    let semantic_commands = self.insert_block_semantic_commands(&before_document, &before_selection, inserted_range);
    self.push_document_snapshot_history(
      before_document,
      before_selection,
      semantic_commands.unwrap_or_default(),
      cx,
    );
  }

  fn insert_blocks_after_caret_without_history(&mut self, blocks: Vec<Block>, cx: &mut Context<Self>) -> Range<usize> {
    if blocks.is_empty() {
      return 0..0;
    }
    let remove_empty_caret_paragraph = !blocks.iter().any(|block| matches!(block, Block::Paragraph(_)));
    let insert_ix = self.prepare_block_insertion_index(remove_empty_caret_paragraph, cx);
    let inserted_count = blocks.len();
    Arc::make_mut(&mut self.document.blocks).splice(insert_ix..insert_ix, blocks);
    for relative_ix in 0..inserted_count {
      insert_block_id(&mut self.document, insert_ix + relative_ix);
    }
    self.append_missing_paragraph_blocks();
    rebuild_document_sections(&mut self.document);
    self.selected_block = None;
    self.clear_layout_work_caches();
    self.item_sizes_cache = None;
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
    insert_ix..insert_ix + inserted_count
  }

  fn prepare_block_insertion_index(&mut self, remove_empty_caret_paragraph: bool, cx: &mut Context<Self>) -> usize {
    if let Some(
      BlockSelection::Image(block_ix)
      | BlockSelection::Equation(block_ix)
      | BlockSelection::Table(block_ix)
      | BlockSelection::TableCell { block_ix, .. },
    ) = self.selected_block
    {
      return (block_ix + 1).min(self.document.blocks.len());
    }

    if remove_empty_caret_paragraph
      && let Some(insert_ix) = self.remove_empty_caret_paragraph_for_block_insertion(cx)
    {
      return insert_ix;
    }

    if !self.selection.is_caret() {
      let range = self.selection.normalized();
      let object_indices = self.object_block_indices_in_text_range(range);
      if !object_indices.is_empty() {
        {
          let blocks = Arc::make_mut(&mut self.document.blocks);
          for block_ix in object_indices.iter().copied().rev() {
            if block_ix < blocks.len() {
              blocks.remove(block_ix);
            }
          }
        }
        for block_ix in object_indices.into_iter().rev() {
          remove_block_ids(&mut self.document, block_ix..block_ix + 1);
        }
      }
      self.delete_selection_internal();
    }

    if let Some(position) = document_position_for_offset(&self.document, self.selection.head) {
      debug_assert_eq!(document_offset_for_position(&self.document, &position), Some(self.selection.head));
      if let DocumentPosition::Text { block_ix, .. } = position {
        return (block_ix + 1).min(self.document.blocks.len());
      }
    }
    self.document.blocks.len()
  }

  fn remove_empty_caret_paragraph_for_block_insertion(&mut self, cx: &mut Context<Self>) -> Option<usize> {
    if !self.selection.is_caret() {
      return None;
    }
    let paragraph_ix = self.selection.head.paragraph;
    let paragraph = self.document.paragraphs.get(paragraph_ix)?;
    if self.selection.head.byte != 0 || paragraph_text_len(paragraph) != 0 {
      return None;
    }
    let block_ix = self.block_ix_for_paragraph(paragraph_ix)?;
    let paragraph_count = self.document.paragraphs.len();
    {
      let blocks = Arc::make_mut(&mut self.document.blocks);
      if block_ix < blocks.len() {
        blocks.remove(block_ix);
      }
    }
    remove_block_ids(&mut self.document, block_ix..block_ix + 1);

    if paragraph_count > 1 {
      let range = paragraph_byte_range(&self.document, paragraph_ix);
      if paragraph_ix + 1 < paragraph_count {
        self.document.text.delete(range.start..range.start + 1);
      } else if range.start > 0 {
        self.document.text.delete(range.start - 1..range.start);
      }
      paragraphs_mut(&mut self.document).remove(paragraph_ix);
      remove_paragraph_ids(&mut self.document, paragraph_ix..paragraph_ix + 1);
      rebuild_document_offset_index(&mut self.document);
      rebuild_document_sections(&mut self.document);
      let new_paragraph_ix = paragraph_ix.min(self.document.paragraphs.len().saturating_sub(1));
      self.selection = EditorSelection {
        anchor: DocumentOffset {
          paragraph: new_paragraph_ix,
          byte: 0,
        },
        head: DocumentOffset {
          paragraph: new_paragraph_ix,
          byte: 0,
        },
      };
      self.emit_selection_changed(cx);
    }
    Some(block_ix)
  }

  fn append_missing_paragraph_blocks(&mut self) {
    let existing = self
      .document
      .blocks
      .iter()
      .filter(|block| matches!(block, Block::Paragraph(_)))
      .count();
    if existing >= self.document.paragraphs.len() {
      return;
    }
    let inserted_count = self.document.paragraphs.len() - existing;
    {
      let blocks = Arc::make_mut(&mut self.document.blocks);
      for paragraph in self.document.paragraphs.iter().skip(existing) {
        blocks.push(Block::Paragraph(paragraph.clone()));
      }
    }
    self.document.ids.block_ids.extend((0..inserted_count).map(|_| new_block_id()));
    rebuild_document_sections(&mut self.document);
  }

  fn insert_block_semantic_commands(
    &self,
    before_document: &DocumentProjection,
    before_selection: &EditorSelection,
    inserted_range: Range<usize>,
  ) -> Option<Vec<SemanticEditCommand>> {
    if inserted_range.is_empty() {
      return None;
    }

    let inserted_blocks = self.document.blocks.get(inserted_range.clone())?;
    let inserted_paragraph_count = inserted_blocks
      .iter()
      .filter(|block| matches!(block, Block::Paragraph(_)))
      .count();
    let inserted_object_count = inserted_blocks.len() - inserted_paragraph_count;
    let deleted_object_ids = if before_selection.is_caret() {
      Vec::new()
    } else {
      object_block_ids_in_text_range(before_document, before_selection.normalized())?
    };
    let mut commands = Vec::with_capacity(
      deleted_object_ids.len() + usize::from(inserted_paragraph_count > 0 || !before_selection.is_caret()) + inserted_object_count,
    );
    commands.extend(
      deleted_object_ids
        .into_iter()
        .map(|block| SemanticEditCommand::DeleteBlock { block }),
    );

    if before_selection.is_caret() && inserted_paragraph_count > 0 {
      let before_paragraph_count = before_document.paragraphs.len();
      if before_paragraph_count == 0 {
        return None;
      }
      let insert_paragraph_ix = self
        .document
        .blocks
        .iter()
        .take(inserted_range.start)
        .filter(|block| matches!(block, Block::Paragraph(_)))
        .count();
      let span_start = insert_paragraph_ix.min(before_paragraph_count.saturating_sub(1));
      let before_span = if insert_paragraph_ix < before_paragraph_count {
        capture_document_span(before_document, insert_paragraph_ix..insert_paragraph_ix + 1)
      } else {
        capture_document_span(before_document, span_start..span_start + 1)
      };
      let after_count = inserted_paragraph_count + 1;
      let after_span = capture_document_span(&self.document, span_start..span_start + after_count);
      commands.push(SemanticEditCommand::ReplaceParagraphSpan {
        start: Some(DocumentOffset {
          paragraph: before_span.start_paragraph,
          byte: 0,
        }),
        before: before_span,
        after: after_span,
      });
    } else if !before_selection.is_caret() {
      let before_range = before_selection.normalized();
      let before_paragraph_count = before_document.paragraphs.len();
      if before_paragraph_count == 0 || before_range.start.paragraph >= before_paragraph_count {
        return None;
      }
      let before_span = capture_document_span(
        before_document,
        before_range.start.paragraph..before_range.end.paragraph.saturating_add(1).min(before_paragraph_count),
      );
      let paragraph_delta = self.document.paragraphs.len() as isize - before_paragraph_count as isize;
      let after_count = before_span
        .paragraphs
        .len()
        .saturating_add_signed(paragraph_delta)
        .min(
          self
            .document
            .paragraphs
            .len()
            .saturating_sub(before_span.start_paragraph),
        );
      let after_span = capture_document_span(&self.document, before_span.start_paragraph..before_span.start_paragraph + after_count);
      if before_span != after_span {
        commands.push(SemanticEditCommand::ReplaceParagraphSpan {
          start: Some(DocumentOffset {
            paragraph: before_span.start_paragraph,
            byte: 0,
          }),
          before: before_span,
          after: after_span,
        });
      }
    }

    for block_ix in inserted_range {
      if matches!(self.document.blocks.get(block_ix), Some(Block::Paragraph(_))) {
        continue;
      }
      let block = self.document.ids.block_ids.get(block_ix).copied().or_else(|| {
        eprintln!("skipping inserted object semantic command because projection block {block_ix} has no durable id");
        None
      })?;
      let after = self
        .document
        .blocks
        .get(block_ix)
        .map(input_block_from_block)?;
      commands.push(SemanticEditCommand::InsertBlock {
        block,
        block_ix,
        after,
      });
    }
    (!commands.is_empty()).then_some(commands)
  }

  #[cfg(test)]
  pub(super) fn insert_block_fragment_for_test(&mut self, fragment: RichClipboardFragment, cx: &mut Context<Self>) {
    self.insert_block_fragment(fragment, cx);
  }

  fn push_document_snapshot_history(
    &mut self,
    before_document: DocumentProjection,
    before_selection: EditorSelection,
    semantic_commands: Vec<SemanticEditCommand>,
    cx: &mut Context<Self>,
  ) {
    if before_document.text == self.document.text
      && before_document.paragraphs == self.document.paragraphs
      && before_document.blocks == self.document.blocks
      && before_document.assets == self.document.assets
    {
      return;
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let runtime_owned = self.suppress_collab_capture == 0
      && !semantic_commands.is_empty()
      && (self.collab_capture || self.runtime_capture);
    if runtime_owned {
      self.pending_projection_rollback = Some(before_document.clone());
    }
    self.undo_stack.push(EditRecord {
      before_selection,
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: if runtime_owned {
        Vec::new()
      } else {
        vec![EditOperation::RestoreProjectionSnapshot {
          before: Box::new(before_document),
          after: Box::new(self.document.clone()),
        }]
      },
      semantic_commands: semantic_commands.clone(),
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed_with_ops(after_generation, true, Some(&semantic_commands), cx);
  }

  fn insert_plain_text_fragment(&mut self, text: &str, cx: &mut Context<Self>) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.is_empty() {
      return;
    }
    let Some(paragraph) = self.document.paragraphs.get(self.selection.head.paragraph) else {
      return;
    };
    let paragraph_style = paragraph.style;
    let styles = self.styles_at_caret();
    let fragment = RichClipboardFragment {
      format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
      paragraphs: normalized
        .split('\n')
        .map(|line| InputParagraph {
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
        .collect(),
      blocks: Vec::new(),
      assets: Vec::new(),
    };
    self.insert_rich_fragment(fragment, cx);
  }

}

fn non_empty_input_paragraphs(paragraphs: Vec<InputParagraph>) -> Vec<InputParagraph> {
  paragraphs
    .into_iter()
    .filter(|paragraph| !paragraph.runs.is_empty())
    .collect()
}

fn object_block_ids_in_text_range(document: &DocumentProjection, range: Range<DocumentOffset>) -> Option<Vec<BlockId>> {
  let start_block = block_ix_for_paragraph(document, range.start.paragraph)?;
  let end_block = block_ix_for_paragraph(document, range.end.paragraph)?;
  let mut object_ids = Vec::new();
  for block_ix in (start_block + 1)..end_block {
    if document
      .blocks
      .get(block_ix)
      .is_some_and(|block| !matches!(block, Block::Paragraph(_)))
    {
      object_ids.push(*document.ids.block_ids.get(block_ix)?);
    }
  }
  Some(object_ids)
}
