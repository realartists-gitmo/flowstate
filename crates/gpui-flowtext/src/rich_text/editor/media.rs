#[hotpath::measure_all]
impl RichTextEditor {
  pub fn set_selected_image_alignment(&mut self, alignment: BlockAlignment, cx: &mut Context<Self>) {
    let Some(BlockSelection::Image(block_ix)) = self.selected_block else {
      return;
    };
    self.edit_selected_image(block_ix, cx, |image| {
      image.alignment = alignment;
      image.version = image.version.wrapping_add(1);
    });
  }

  pub fn set_selected_image_fit_width(&mut self, cx: &mut Context<Self>) {
    let Some(BlockSelection::Image(block_ix)) = self.selected_block else {
      return;
    };
    self.edit_selected_image(block_ix, cx, |image| {
      image.sizing = ImageSizing::FitWidth;
      image.version = image.version.wrapping_add(1);
    });
  }

  pub fn set_selected_image_intrinsic_size(&mut self, cx: &mut Context<Self>) {
    let Some(BlockSelection::Image(block_ix)) = self.selected_block else {
      return;
    };
    self.edit_selected_image(block_ix, cx, |image| {
      image.sizing = ImageSizing::Intrinsic;
      image.version = image.version.wrapping_add(1);
    });
  }

  pub fn widen_selected_image(&mut self, cx: &mut Context<Self>) {
    self.adjust_selected_image_width(48, cx);
  }

  pub fn narrow_selected_image(&mut self, cx: &mut Context<Self>) {
    self.adjust_selected_image_width(-48, cx);
  }

  fn adjust_selected_image_width(&mut self, delta_px: i32, cx: &mut Context<Self>) {
    let Some(BlockSelection::Image(block_ix)) = self.selected_block else {
      return;
    };
    let current_width = self
      .document
      .blocks
      .get(block_ix)
      .and_then(|block| match block {
        Block::Image(image) => Some(match image.sizing {
          ImageSizing::Fixed { width_px, .. } => width_px as i32,
          ImageSizing::Intrinsic => self
            .document
            .assets
            .assets
            .get(&image.asset_id)
            .and_then(image_asset_intrinsic_size)
            .map(|(width, _)| {
              let width: f32 = width.into();
              width as i32
            })
            .unwrap_or(320),
          ImageSizing::FitWidth => {
            let available_width = (self.current_layout_width() - self.document.theme.pageless_inset_x * 2.0).max(px(1.0));
            let available_width: f32 = available_width.into();
            available_width as i32
          },
        }),
        _ => None,
      })
      .unwrap_or(320);
    self.edit_selected_image(block_ix, cx, |image| {
      image.sizing = ImageSizing::Fixed {
        width_px: (current_width + delta_px).clamp(32, 2400) as u32,
        height_px: None,
      };
      image.version = image.version.wrapping_add(1);
    });
  }

  fn start_image_resize_drag(
    &mut self,
    block_ix: usize,
    handle: ImageResizeHandle,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    let Some(Block::Image(image)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    window.focus(&self.focus_handle);
    self.selected_block = Some(BlockSelection::Image(block_ix));
    self.table_cell_block_ix = 0;
    self.table_cell_caret = 0;
    self.image_resize_drag = Some(ImageResizeDrag {
      block_ix,
      start_position: position,
      start_width: self.image_rendered_width(&image),
      handle,
      before: image,
    });
    self.selecting = false;
    self.pending_text_drag = None;
    self.active_text_drag = None;
    self.goal_x = None;
    window.prevent_default();
    cx.stop_propagation();
    cx.notify();
  }

  fn update_image_resize_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
    let Some(drag) = self.image_resize_drag.clone() else {
      return false;
    };
    let delta: f32 = (position.x - drag.start_position.x).into();
    let delta = delta * drag.handle.horizontal_sign();
    let start_width: f32 = drag.start_width.into();
    let max_width: f32 = (self.current_layout_width() - self.document.theme.pageless_inset_x * 2.0)
      .max(px(32.0))
      .into();
    let width_px = (start_width + delta)
      .clamp(32.0, max_width.max(32.0))
      .round() as u32;
    let Some(Block::Image(image)) = Arc::make_mut(&mut self.document.blocks).get_mut(drag.block_ix) else {
      self.image_resize_drag = None;
      return true;
    };
    if image.sizing == (ImageSizing::Fixed { width_px, height_px: None }) {
      return true;
    }
    image.sizing = ImageSizing::Fixed { width_px, height_px: None };
    image.version = drag.before.version.wrapping_add(1);
    self.invalidate_document_layout_caches();
    cx.notify();
    true
  }

  fn finish_image_resize_drag(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(drag) = self.image_resize_drag.take() else {
      return false;
    };
    let Some(Block::Image(after)) = self.document.blocks.get(drag.block_ix).cloned() else {
      cx.notify();
      return true;
    };
    if after == drag.before {
      cx.notify();
      return true;
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let after_block = Block::Image(after.clone());
    let semantic_commands = match (self.semantic_block_id(drag.block_ix), input_block_from_block(&after_block)) {
      (Some(image_id), InputBlock::Image(image)) => vec![SemanticEditCommand::SetImageLayout {
        image: image_id,
        sizing: image.sizing,
        alignment: image.alignment,
      }],
      (None, _) => {
        eprintln!(
          "skipping image resize semantic command because projection block {} has no durable id",
          drag.block_ix
        );
        Vec::new()
      },
      (Some(image_id), _) => {
        eprintln!(
          "skipping image resize semantic command because projection block {} ({image_id:?}) is no longer an image",
          drag.block_ix
        );
        Vec::new()
      },
    };
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock {
        block_ix: drag.block_ix,
        before: Block::Image(drag.before),
        after: Block::Image(after),
      }],
      semantic_commands: semantic_commands.clone(),
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed_with_ops(after_generation, true, Some(&semantic_commands), cx);
    true
  }

  fn image_rendered_width(&self, image: &ImageBlock) -> Pixels {
    let available_width = (self.current_layout_width() - self.document.theme.pageless_inset_x * 2.0).max(px(1.0));
    match image.sizing {
      ImageSizing::Fixed { width_px, .. } => px(width_px as f32).min(available_width),
      ImageSizing::FitWidth => available_width,
      ImageSizing::Intrinsic => self
        .document
        .assets
        .assets
        .get(&image.asset_id)
        .and_then(image_asset_intrinsic_size)
        .map(|(width, _)| width.min(available_width))
        .unwrap_or(available_width),
    }
  }

  pub fn set_selected_image_alt_text(&mut self, alt_text: impl Into<SharedString>, cx: &mut Context<Self>) {
    let Some(BlockSelection::Image(block_ix)) = self.selected_block else {
      return;
    };
    let alt_text = alt_text.into();
    let semantic_alt_text = alt_text.to_string();
    self.edit_selected_image_with_semantic(block_ix, cx, |image| {
      image.alt_text = alt_text;
      image.version = image.version.wrapping_add(1);
    }, |editor, block_ix, _after| {
      if let Some(image_id) = editor.semantic_block_id(block_ix) {
        vec![SemanticEditCommand::ReplaceImageAltText {
          image: image_id,
          text: semantic_alt_text,
        }]
      } else {
        eprintln!("skipping image alt-text semantic command because projection block {block_ix} has no durable id");
        Vec::new()
      }
    });
  }

  fn edit_selected_image(&mut self, block_ix: usize, cx: &mut Context<Self>, update: impl FnOnce(&mut ImageBlock)) {
    self.edit_selected_image_with_semantic(block_ix, cx, update, |editor, block_ix, after| {
      let Some(image_id) = editor.semantic_block_id(block_ix) else {
        eprintln!("skipping image layout semantic command because projection block {block_ix} has no durable id");
        return Vec::new();
      };
      let InputBlock::Image(image) = input_block_from_block(after) else {
        eprintln!("skipping image layout semantic command because projection block {block_ix} ({image_id:?}) is no longer an image");
        return Vec::new();
      };
      vec![SemanticEditCommand::SetImageLayout {
        image: image_id,
        sizing: image.sizing,
        alignment: image.alignment,
      }]
    });
  }

  fn edit_selected_image_with_semantic(
    &mut self,
    block_ix: usize,
    cx: &mut Context<Self>,
    update: impl FnOnce(&mut ImageBlock),
    semantic_commands: impl FnOnce(&Self, usize, &Block) -> Vec<SemanticEditCommand>,
  ) {
    let Some(Block::Image(image)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    let mut updated = image.clone();
    update(&mut updated);
    if updated == image {
      return;
    }
    let before = Block::Image(image);
    let after = Block::Image(updated);
    if let Some(block) = Arc::make_mut(&mut self.document.blocks).get_mut(block_ix) {
      *block = after.clone();
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let semantic_commands = semantic_commands(self, block_ix, &after);
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock { block_ix, before, after }],
      semantic_commands: semantic_commands.clone(),
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed_with_ops(after_generation, true, Some(&semantic_commands), cx);
  }

  pub fn insert_equation(&mut self, source: impl Into<SharedString>, cx: &mut Context<Self>) {
    self.insert_blocks_after_caret(
      vec![Block::Equation(EquationBlock {
        source: source.into(),
        syntax: EquationSyntax::Latex,
        display: EquationDisplay::Display,
        version: 0,
      })],
      cx,
    );
  }

  pub fn insert_image_block(&mut self, asset: AssetRecord, alt_text: impl Into<SharedString>, cx: &mut Context<Self>) {
    self.insert_image_assets(vec![(asset, alt_text.into())], cx);
  }

  fn insert_image_assets(&mut self, assets: Vec<(AssetRecord, SharedString)>, cx: &mut Context<Self>) {
    if assets.is_empty() {
      return;
    }
    let before_document = self.document.clone();
    let before_selection = self.selection.clone();
    let mut blocks = Vec::with_capacity(assets.len());
    for (asset, alt_text) in assets {
      let asset_id = asset.id;
      self.document.assets.assets.insert(asset_id, asset);
      blocks.push(Block::Image(ImageBlock {
        asset_id,
        alt_text,
        caption: None,
        sizing: ImageSizing::FitWidth,
        alignment: BlockAlignment::Center,
        version: 0,
      }));
    }
    let inserted_range = self.insert_blocks_after_caret_without_history(blocks, cx);
    let semantic_commands = self.insert_block_semantic_commands(&before_document, &before_selection, inserted_range);
    self.push_document_snapshot_history(
      before_document,
      before_selection,
      semantic_commands.unwrap_or_default(),
      cx,
    );
  }

  pub fn prompt_insert_image(&mut self, cx: &mut Context<Self>) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
      files: true,
      directories: false,
      multiple: false,
      prompt: Some("Insert image".into()),
    });
    cx.spawn(async move |editor, cx| {
      let Ok(Ok(Some(paths))) = paths.await else {
        return;
      };
      let Some(path) = paths.into_iter().next() else {
        return;
      };
      let image_asset = cx
        .background_executor()
        .spawn(async move { image_asset_from_path(&path) })
        .await;
      let Some((asset, alt_text)) = image_asset else {
        return;
      };
      editor
        .update(cx, |editor, cx| {
          if !editor.disposed {
            editor.insert_image_block(asset, alt_text, cx);
          }
        })
        .ok();
    })
    .detach();
  }

  fn on_file_drop(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
    self.clear_drop_preview();
    let paths = paths.paths().to_vec();
    if paths.is_empty() {
      return;
    }
    let position = window.mouse_position();
    let window_handle = window.window_handle();
    cx.spawn(async move |editor, cx| {
      let image_assets = cx
        .background_executor()
        .spawn(async move {
          paths
            .iter()
            .filter_map(|path| image_asset_from_path(path))
            .collect::<Vec<_>>()
        })
        .await;
      if image_assets.is_empty() {
        return;
      }
      let _ = window_handle.update(cx, |_, window, cx| {
        let _ = editor.update(cx, |editor, cx| {
          if editor.disposed {
            return;
          }
          editor.place_block_insertion_from_point(position, window, cx);
          editor.insert_image_assets(image_assets, cx);
        });
      });
    })
    .detach();
  }

  fn place_block_insertion_from_point(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
    let width = self.current_layout_width();
    self.ensure_exact_interaction_chunks(width, window, cx);
    let _ = self.paragraph_item_sizes(window, cx);
    let viewport = self.scroll_handle.bounds();
    let content_y = (position.y - viewport.top() - self.scroll_handle.offset().y).max(px(0.0));
    if let Some(cache) = &self.item_sizes_cache
      && self.height_prefix_index.len() == cache.item_count
    {
      let item_ix = self.height_prefix_index.lower_bound(content_y);
      let block_ix = match cache.items.get(item_ix) {
        Some(
          VirtualItem::HiddenBlock { block_ix }
          | VirtualItem::StructuralBlock { block_ix }
          | VirtualItem::ParagraphChunk { block_ix, .. }
          | VirtualItem::ParagraphRemainder { block_ix, .. },
        ) => *block_ix,
        None => return,
      };
      if let Some(selection) = self.selection_for_object_block(block_ix) {
        self.select_block(selection, cx);
        return;
      }
    }
    let offset = self.hit_test_document_position(position, window, cx);
    self.selection = EditorSelection {
      anchor: offset,
      head: offset,
    };
    self.clear_block_selection();
    self.goal_x = None;
    self.reset_caret_blink(cx);
  }

  fn insert_clipboard_image(&mut self, image: Image, cx: &mut Context<Self>) {
    cx.spawn(async move |editor, cx| {
      let (asset, alt_text) = cx
        .background_executor()
        .spawn(async move { image_asset_from_image(image) })
        .await;
      let _ = editor.update(cx, |editor, cx| {
        if !editor.disposed {
          editor.insert_image_block(asset, alt_text, cx);
        }
      });
    })
    .detach();
  }
}
