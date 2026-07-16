#[hotpath::measure_all]
impl RichTextEditor {
  pub fn set_selected_image_alignment(&mut self, alignment: BlockAlignment, cx: &mut Context<Self>) {
    let Some(BlockSelection::Image(block_ix)) = self.selected_block else {
      return;
    };
    let alignment = input_alignment_from_alignment(alignment);
    self.write_selected_image_layout(block_ix, cx, |image| {
      image.alignment = alignment;
    });
  }

  pub fn set_selected_image_fit_width(&mut self, cx: &mut Context<Self>) {
    let Some(BlockSelection::Image(block_ix)) = self.selected_block else {
      return;
    };
    self.write_selected_image_layout(block_ix, cx, |image| {
      image.sizing = InputImageSizing::FitWidth;
    });
  }

  pub fn set_selected_image_intrinsic_size(&mut self, cx: &mut Context<Self>) {
    let Some(BlockSelection::Image(block_ix)) = self.selected_block else {
      return;
    };
    self.write_selected_image_layout(block_ix, cx, |image| {
      image.sizing = InputImageSizing::Intrinsic;
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
    self.write_selected_image_layout(block_ix, cx, |image| {
      image.sizing = InputImageSizing::Fixed {
        width_px: (current_width + delta_px).clamp(32, 2400) as u32,
        height_px: None,
      };
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

  /// Track a live image-resize drag. Loro-first (spec §5): the drag never
  /// touches THE projection — it only accumulates the target width (rebased
  /// into `start_width`/`start_position` each move), and the commit at drag
  /// end is one typed `SetImageLayout` intent.
  fn update_image_resize_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
    if self.image_resize_drag.is_none() {
      return false;
    }
    let max_width: f32 = (self.current_layout_width() - self.document.theme.pageless_inset_x * 2.0)
      .max(px(32.0))
      .into();
    let Some(drag) = self.image_resize_drag.as_mut() else {
      return false;
    };
    let delta: f32 = (position.x - drag.start_position.x).into();
    let delta = delta * drag.handle.horizontal_sign();
    let start_width: f32 = drag.start_width.into();
    let width = (start_width + delta).clamp(32.0, max_width.max(32.0)).round();
    drag.start_width = px(width);
    drag.start_position = position;
    cx.notify();
    true
  }

  /// Commit the image resize at drag end through the ONE write path: one
  /// typed `SetImageLayout` intent addressed by the image's durable identity.
  /// The runtime's patches advance THE projection — no direct write, no
  /// history snapshot. A drag that ends at its starting width is a no-op.
  fn finish_image_resize_drag(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(drag) = self.image_resize_drag.take() else {
      return false;
    };
    let final_width: f32 = drag.start_width.into();
    let width_px = final_width.round() as u32;
    let initial_width: f32 = self.image_rendered_width(&drag.before).into();
    if width_px == initial_width.round() as u32 {
      cx.notify();
      return true;
    }
    self.write_selected_image_layout(drag.block_ix, cx, |image| {
      image.sizing = InputImageSizing::Fixed { width_px, height_px: None };
    });
    cx.notify();
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
    let Some(Block::Image(image)) = self.document.blocks.get(block_ix) else {
      return;
    };
    if image.alt_text == alt_text {
      return;
    }
    let Some(image_id) = self.identity_map.block_id(block_ix) else {
      tracing::warn!(block_ix, "refusing image alt-text edit: projection block has no durable id, so no intent can address it");
      return;
    };
    self.write_intent(
      LocalIntent::ReplaceImageAltText(crate::local_intents::ReplaceImageAltTextIntent {
        image: image_id,
        text: alt_text.to_string(),
      }),
      cx,
    );
  }

  /// Route an image layout change through the ONE write path: convert the
  /// current projection image to its input shape, apply the change, and
  /// commit one typed `SetImageLayout` intent addressed by the image's
  /// durable identity. Unchanged layouts never reach the authority.
  fn write_selected_image_layout(&mut self, block_ix: usize, cx: &mut Context<Self>, update: impl FnOnce(&mut InputImageBlock)) {
    let Some(block @ Block::Image(_)) = self.document.blocks.get(block_ix) else {
      return;
    };
    let InputBlock::Image(current) = input_block_from_block(block) else {
      return;
    };
    let mut updated = current.clone();
    update(&mut updated);
    if updated.sizing == current.sizing && updated.alignment == current.alignment {
      return;
    }
    let Some(image_id) = self.identity_map.block_id(block_ix) else {
      tracing::warn!(block_ix, "refusing image layout edit: projection block has no durable id, so no intent can address it");
      return;
    };
    self.write_intent(
      LocalIntent::SetImageLayout(crate::local_intents::SetImageLayoutIntent {
        image: image_id,
        sizing: updated.sizing,
        alignment: updated.alignment,
      }),
      cx,
    );
  }

  pub fn insert_equation(&mut self, source: impl Into<SharedString>, cx: &mut Context<Self>) {
    // Loro-first: the equation enters through an InsertObject intent (identity
    // minted by the write path), matching the converted table insertion.
    self.write_insert_object_at_caret(
      InputBlock::Equation(InputEquationBlock {
        source: source.into().to_string(),
        syntax: InputEquationSyntax::Latex,
        display: InputEquationDisplay::Display,
      }),
      cx,
    );
  }

  pub fn insert_image_block(&mut self, asset: AssetRecord, alt_text: impl Into<SharedString>, cx: &mut Context<Self>) {
    self.insert_image_assets(vec![(asset, alt_text.into())], cx);
  }

  /// Loro-first: asset bytes are content-addressed sideband state (same
  /// pattern as clipboard adoption and the remote `AssetArrived` path); the
  /// image BLOCKS commit as intents through the write authority.
  fn insert_image_assets(&mut self, assets: Vec<(AssetRecord, SharedString)>, cx: &mut Context<Self>) {
    if assets.is_empty() {
      return;
    }
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
        external_url: None,
        version: 0,
      }));
    }
    self.insert_blocks_after_caret(blocks, cx);
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
      editor
        .update(cx, |editor, cx| {
          if editor.disposed {
            return;
          }
          match image_asset {
            Ok((asset, alt_text)) => editor.insert_image_block(asset, alt_text, cx),
            // B-S1: the refusal reaches the host (activity zone) instead of
            // vanishing.
            Err(message) => cx.emit(EditorEvent::Refused { message: message.into() }),
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
      let results = cx
        .background_executor()
        .spawn(async move { paths.iter().map(|path| image_asset_from_path(path)).collect::<Vec<_>>() })
        .await;
      let mut image_assets = Vec::new();
      let mut refusals = Vec::new();
      for result in results {
        match result {
          Ok(asset) => image_assets.push(asset),
          Err(message) => refusals.push(message),
        }
      }
      let _ = window_handle.update(cx, |_, window, cx| {
        let _ = editor.update(cx, |editor, cx| {
          if editor.disposed {
            return;
          }
          for message in refusals {
            // B-S1: every dropped file that can't insert says why.
            cx.emit(EditorEvent::Refused { message: message.into() });
          }
          if image_assets.is_empty() {
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
    self.selection = EditorSelection::collapsed(offset);
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
