const RICH_TEXT_CLIPBOARD_FORMAT: &str = "flowstate.rich-text-fragment.v1";

#[hotpath::measure_all]
impl RichTextEditor {
  pub fn insert_toolkit_text_at_caret(&mut self, paragraphs: Vec<InputParagraph>, cx: &mut Context<Self>) {
    if self.block_local_mutation(cx) {
      return;
    }
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
    if self.block_local_mutation(cx) {
      return;
    }
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
    if self.block_local_mutation(cx) {
      return;
    }
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
    self.after_text_mutation(cx);
  }

  fn insert_block_fragment(&mut self, fragment: RichClipboardFragment, cx: &mut Context<Self>) {
    if self.block_local_mutation(cx) {
      return;
    }
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
    self.insert_ordered_block_fragment_after_caret(&fragment.blocks);
    self.push_replace_document_history(before_document, before_selection, cx);
  }

  fn insert_ordered_block_fragment_after_caret(&mut self, input_blocks: &[InputBlock]) {
    let insert_ix = self.prepare_block_insertion_index();
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
  }

  fn insert_blocks_after_caret(&mut self, blocks: Vec<Block>, cx: &mut Context<Self>) {
    if self.block_local_mutation(cx) {
      return;
    }
    if blocks.is_empty() {
      return;
    }
    let before_document = self.document.clone();
    let before_selection = self.selection.clone();
    self.insert_blocks_after_caret_without_history(blocks);
    self.push_replace_document_history(before_document, before_selection, cx);
  }

  fn insert_blocks_after_caret_without_history(&mut self, blocks: Vec<Block>) {
    if blocks.is_empty() {
      return;
    }
    let insert_ix = self.prepare_block_insertion_index();
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
  }

  fn prepare_block_insertion_index(&mut self) -> usize {
    if let Some(
      BlockSelection::Image(block_ix)
      | BlockSelection::Equation(block_ix)
      | BlockSelection::Table(block_ix)
      | BlockSelection::TableCell { block_ix, .. },
    ) = self.selected_block
    {
      return (block_ix + 1).min(self.document.blocks.len());
    }

    if let Some(insert_ix) = self.remove_empty_caret_paragraph_for_block_insertion() {
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

  fn remove_empty_caret_paragraph_for_block_insertion(&mut self) -> Option<usize> {
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

  fn push_replace_document_history(&mut self, before_document: Document, before_selection: EditorSelection, cx: &mut Context<Self>) {
    if self.block_local_mutation(cx) {
      return;
    }
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
    self.undo_stack.push(EditRecord {
      before_selection,
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceDocument {
        before: Box::new(before_document),
        after: Box::new(self.document.clone()),
      }],
      canonical_operations: vec![CanonicalOperation::ReplaceDocument],
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed(after_generation, cx);
  }

  fn insert_plain_text_fragment(&mut self, text: &str, cx: &mut Context<Self>) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.is_empty() {
      return;
    }
    let paragraph_style = self.document.paragraphs[self.selection.head.paragraph].style;
    let styles = self.styles_at_caret();
    let fragment = RichClipboardFragment {
      format: "flowstate.rich-text-fragment.v1".to_string(),
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
