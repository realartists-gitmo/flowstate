#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum DropPreviewKind {
  ToolkitText,
  ExternalPaths,
  /// B-S11: an object block being moved by pointer drag.
  BlockMove,
}

/// B-S11: the pointer-drag payload for moving an object block. Dropping
/// lands ONE `MoveBlock` intent at the insertion point the preview shows.
/// Public so host-level headless tests can drive the drop-target math.
#[derive(Clone)]
pub struct BlockDrag {
  pub block_ix: usize,
  pub block_id: BlockId,
  pub label: SharedString,
}

/// The ghost riding the cursor during a block drag.
pub(super) struct BlockDragGhost {
  pub label: SharedString,
}

impl gpui::Render for BlockDragGhost {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .px_2()
      .py_1()
      .rounded(px(4.0))
      .bg(cx.theme().secondary)
      .border_1()
      .border_color(cx.theme().border)
      .text_size(px(12.0))
      .text_color(cx.theme().foreground)
      .child(self.label.clone())
  }
}

#[derive(Clone)]
enum DropPreviewContent {
  DocumentProjection(Box<DocumentProjection>),
  ExternalPaths { label: SharedString },
}

#[derive(Clone)]
struct DropPreview {
  kind: DropPreviewKind,
  fingerprint: u64,
  insert_block_ix: usize,
  suppressed_block_ix: Option<usize>,
  is_first: bool,
  is_last: bool,
  width: Pixels,
  height: Pixels,
  content: DropPreviewContent,
}

#[derive(Clone, Copy)]
struct DropPreviewPlacement {
  insert_block_ix: usize,
  suppressed_block_ix: Option<usize>,
}

#[hotpath::measure_all]
impl RichTextEditor {
  fn clear_drop_preview(&mut self) {
    self.drop_preview = None;
  }

  fn on_toolkit_text_drag_move(&mut self, event: &DragMoveEvent<ToolkitTextDrag>, window: &mut Window, cx: &mut Context<Self>) {
    if !event.bounds.contains(&event.event.position) {
      self.clear_drop_preview();
      cx.notify();
      return;
    }

    let paragraphs = event.drag(cx).paragraphs.clone();
    self.place_block_insertion_from_point(event.event.position, window, cx);
    let paragraphs = non_empty_input_paragraphs(paragraphs);
    self.set_document_drop_preview(DropPreviewKind::ToolkitText, paragraphs, window, cx);
    cx.notify();
  }

  fn on_external_paths_drag_move(&mut self, event: &DragMoveEvent<ExternalPaths>, window: &mut Window, cx: &mut Context<Self>) {
    if !event.bounds.contains(&event.event.position) {
      self.clear_drop_preview();
      cx.notify();
      return;
    }

    let paths = event.drag(cx).paths().to_vec();
    if paths.is_empty() {
      self.clear_drop_preview();
      cx.notify();
      return;
    }

    self.place_block_insertion_from_point(event.event.position, window, cx);
    self.set_external_paths_drop_preview(&paths, window, cx);
    cx.notify();
  }

  fn set_document_drop_preview(
    &mut self,
    kind: DropPreviewKind,
    paragraphs: Vec<InputParagraph>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    if paragraphs.is_empty() {
      self.clear_drop_preview();
      return;
    }

    let fingerprint = hash_input_paragraphs(&paragraphs);
    let placement = self.drop_preview_block_placement();
    let (is_first, is_last) = self.drop_preview_document_edges(placement);
    if let Some(preview) = &mut self.drop_preview
      && preview.kind == kind
      && preview.fingerprint == fingerprint
      && preview.is_first == is_first
      && preview.is_last == is_last
      && matches!(&preview.content, DropPreviewContent::DocumentProjection(_))
    {
      preview.insert_block_ix = placement.insert_block_ix;
      preview.suppressed_block_ix = placement.suppressed_block_ix;
      return;
    }

    let document = self.drop_preview_document_from_paragraphs(paragraphs, is_first, is_last);
    let width = self.current_layout_width();
    let height = drop_preview_document_height(&document, width, self.invisibility_mode, window, cx);
    self.drop_preview = Some(DropPreview {
      kind,
      fingerprint,
      insert_block_ix: placement.insert_block_ix,
      suppressed_block_ix: placement.suppressed_block_ix,
      is_first,
      is_last,
      width,
      height,
      content: DropPreviewContent::DocumentProjection(Box::new(document)),
    });
  }

  /// B-S11: track the block drag — caret/selection follows the pointer
  /// (the same placement law the toolkit and file drops use), the edges
  /// auto-scroll, and the indicator paints through the shared preview.
  fn on_block_drag_move(&mut self, event: &DragMoveEvent<BlockDrag>, window: &mut Window, cx: &mut Context<Self>) {
    if !event.bounds.contains(&event.event.position) {
      self.clear_drop_preview();
      cx.notify();
      return;
    }
    self.autoscroll_for_drag(event.event.position);
    self.ensure_drag_autoscroll_task(cx);
    let (fingerprint, label) = {
      let drag = event.drag(cx);
      (drag.block_id.0 as u64, drag.label.clone())
    };
    self.place_block_insertion_from_point(event.event.position, window, cx);
    let placement = self.drop_preview_block_placement();
    if let Some(preview) = &mut self.drop_preview
      && preview.kind == DropPreviewKind::BlockMove
      && preview.fingerprint == fingerprint
    {
      preview.insert_block_ix = placement.insert_block_ix;
      preview.suppressed_block_ix = placement.suppressed_block_ix;
      cx.notify();
      return;
    }
    let width = self.current_layout_width();
    self.drop_preview = Some(DropPreview {
      kind: DropPreviewKind::BlockMove,
      fingerprint,
      insert_block_ix: placement.insert_block_ix,
      suppressed_block_ix: placement.suppressed_block_ix,
      is_first: false,
      is_last: false,
      width,
      height: drop_preview_external_paths_height(width, window, cx),
      content: DropPreviewContent::ExternalPaths { label },
    });
    cx.notify();
  }

  /// B-S11: the drop — one identity-addressed `MoveBlock` through the write
  /// authority; the block stays selected at its new home.
  pub fn on_block_drop(&mut self, drag: &BlockDrag, _window: &mut Window, cx: &mut Context<Self>) {
    self.clear_drop_preview();
    let insert_ix = self.drop_preview_block_placement().insert_block_ix;
    // Dropping onto its own footprint is a no-op, not an intent.
    if insert_ix == drag.block_ix || insert_ix == drag.block_ix + 1 {
      cx.notify();
      return;
    }
    let before = if insert_ix < self.document.blocks.len() {
      match self.semantic_block_id(insert_ix) {
        Some(id) => Some(id),
        None => {
          cx.emit(EditorEvent::Refused {
            message: "That spot can't take the block yet — its neighbor has no durable identity.".into(),
          });
          return;
        },
      }
    } else {
      None
    };
    let moved = self
      .write_intent(
        LocalIntent::MoveBlock(crate::local_intents::MoveBlockIntent {
          block: drag.block_id,
          before,
        }),
        cx,
      )
      .is_some();
    if moved {
      // Re-select the block at its post-move index.
      let new_ix = (0..self.document.blocks.len()).find(|ix| self.semantic_block_id(*ix) == Some(drag.block_id));
      if let Some(new_ix) = new_ix
        && let Some(selection) = self.selection_for_object_block(new_ix)
      {
        self.select_block(selection, cx);
      }
    }
    cx.notify();
  }

  fn set_external_paths_drop_preview(&mut self, paths: &[PathBuf], window: &mut Window, cx: &mut Context<Self>) {
    let fingerprint = hash_external_paths(paths);
    let placement = self.drop_preview_block_placement();
    if let Some(preview) = &mut self.drop_preview
      && preview.kind == DropPreviewKind::ExternalPaths
      && preview.fingerprint == fingerprint
    {
      preview.insert_block_ix = placement.insert_block_ix;
      preview.suppressed_block_ix = placement.suppressed_block_ix;
      return;
    }

    let width = self.current_layout_width();
    self.drop_preview = Some(DropPreview {
      kind: DropPreviewKind::ExternalPaths,
      fingerprint,
      insert_block_ix: placement.insert_block_ix,
      suppressed_block_ix: placement.suppressed_block_ix,
      is_first: false,
      is_last: false,
      width,
      height: drop_preview_external_paths_height(width, window, cx),
      content: DropPreviewContent::ExternalPaths {
        label: external_paths_preview_label(paths),
      },
    });
  }

  fn drop_preview_block_placement(&self) -> DropPreviewPlacement {
    if let Some(
      BlockSelection::Image(block_ix)
      | BlockSelection::Equation(block_ix)
      | BlockSelection::Table(block_ix)
      | BlockSelection::TableCell { block_ix, .. },
    ) = self.selected_block
    {
      return DropPreviewPlacement {
        insert_block_ix: (block_ix + 1).min(self.document.blocks.len()),
        suppressed_block_ix: None,
      };
    }

    if self.selection.is_caret() {
      let paragraph_ix = self.selection.head.paragraph;
      if let Some(paragraph) = self.document.paragraphs.get(paragraph_ix)
        && self.selection.head.byte == 0
        && paragraph_text_len(paragraph) == 0
        && let Some(block_ix) = self.block_ix_for_paragraph(paragraph_ix)
      {
        return DropPreviewPlacement {
          insert_block_ix: block_ix,
          suppressed_block_ix: Some(block_ix),
        };
      }
    }

    if let Some(position) = document_position_for_offset(&self.document, self.selection.head)
      && let DocumentPosition::Text { block_ix, .. } = position
    {
      return DropPreviewPlacement {
        insert_block_ix: (block_ix + 1).min(self.document.blocks.len()),
        suppressed_block_ix: None,
      };
    }

    DropPreviewPlacement {
      insert_block_ix: self.document.blocks.len(),
      suppressed_block_ix: None,
    }
  }

  fn drop_preview_document_from_paragraphs(&self, paragraphs: Vec<InputParagraph>, is_first: bool, is_last: bool) -> DocumentProjection {
    let mut theme = self.document.theme.clone();
    if !is_first {
      theme.pageless_inset_top = px(0.0);
    }
    if !is_last {
      theme.pageless_inset_bottom = px(0.0);
    }
    document_from_input(theme, paragraphs)
  }

  fn drop_preview_document_edges(&self, placement: DropPreviewPlacement) -> (bool, bool) {
    let block_count = self.document.blocks.len();
    let suppressed_before_insert = placement
      .suppressed_block_ix
      .is_some_and(|block_ix| block_ix < placement.insert_block_ix);
    let insert_after_suppression = placement
      .insert_block_ix
      .saturating_sub(usize::from(suppressed_before_insert));
    let final_block_count = block_count.saturating_sub(usize::from(placement.suppressed_block_ix.is_some()));
    (insert_after_suppression == 0, insert_after_suppression >= final_block_count)
  }

  fn render_items_with_drop_preview(
    &mut self,
    base_items: Rc<Vec<VirtualItem>>,
    base_sizes: Rc<Vec<Size<Pixels>>>,
    width: Pixels,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> (RenderVirtualItems, Rc<Vec<Size<Pixels>>>) {
    if self.drop_preview.is_none() {
      return (RenderVirtualItems::DocumentProjection(base_items), base_sizes);
    }

    self.refresh_drop_preview_height(width, window, cx);

    let Some(preview) = &self.drop_preview else {
      return (RenderVirtualItems::DocumentProjection(base_items), base_sizes);
    };
    let Some(cache) = self.item_sizes_cache.as_ref() else {
      return (RenderVirtualItems::DocumentProjection(base_items), base_sizes);
    };

    let insert_item_ix = drop_preview_insert_item_ix(cache, preview.insert_block_ix);
    let suppressed_range = preview
      .suppressed_block_ix
      .and_then(|block_ix| cache.block_item_ranges.get(block_ix).cloned())
      .unwrap_or(0..0);
    let mut items = Vec::with_capacity(base_items.len() + 1);
    let mut sizes = Vec::with_capacity(base_sizes.len() + 1);
    for item_ix in 0..=base_items.len() {
      if item_ix == insert_item_ix {
        items.push(RenderVirtualItem::DropPreview);
        sizes.push(size(width, preview.height.max(px(1.0))));
      }
      if item_ix == base_items.len() {
        continue;
      }
      if suppressed_range.contains(&item_ix) {
        continue;
      }
      items.push(RenderVirtualItem::DocumentProjection(base_items[item_ix].clone()));
      sizes.push(base_sizes[item_ix]);
    }

    (RenderVirtualItems::WithDropPreview(Rc::new(items)), Rc::new(sizes))
  }

  fn refresh_drop_preview_height(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    let Some(preview) = &mut self.drop_preview else {
      return;
    };
    if preview.width == width {
      return;
    }
    preview.width = width;
    preview.height = match &preview.content {
      DropPreviewContent::DocumentProjection(document) => drop_preview_document_height(document, width, self.invisibility_mode, window, cx),
      DropPreviewContent::ExternalPaths { .. } => drop_preview_external_paths_height(width, window, cx),
    };
  }
}

fn render_drop_preview(preview: DropPreview, invisibility_mode: bool, cx: &mut Context<RichTextEditor>) -> impl IntoElement {
  let background = match &preview.content {
    DropPreviewContent::DocumentProjection(document) => document.theme.document_background_color.opacity(0.78),
    DropPreviewContent::ExternalPaths { .. } => cx.theme().background.opacity(0.78),
  };
  let border = cx.theme().drag_border.opacity(0.72);
  let text_color = cx.theme().foreground.opacity(0.72);
  let content = match preview.content {
    DropPreviewContent::DocumentProjection(document) => div()
      .w_full()
      .child(RichTextDocumentElement::new(*document).with_invisibility_mode(invisibility_mode))
      .into_any_element(),
    DropPreviewContent::ExternalPaths { label } => div()
      .w_full()
      .h(px(96.0))
      .flex()
      .items_center()
      .justify_center()
      .text_sm()
      .text_color(text_color)
      .child(label)
      .into_any_element(),
  };

  div()
    .id("rich-text-drop-preview")
    .size_full()
    .overflow_hidden()
    .opacity(0.58)
    .border_1()
    .border_color(border)
    .bg(background)
    .child(content)
}

fn drop_preview_insert_item_ix(cache: &ItemSizesCache, insert_block_ix: usize) -> usize {
  if insert_block_ix >= cache.block_item_ranges.len() {
    return cache.item_count;
  }
  cache
    .block_item_ranges
    .get(insert_block_ix)
    .map_or(cache.item_count, |range| range.start)
}

fn drop_preview_document_height(
  document: &DocumentProjection,
  width: Pixels,
  invisibility_mode: bool,
  window: &mut Window,
  cx: &mut Context<RichTextEditor>,
) -> Pixels {
  build_layout_with_visibility(document, width, None, invisibility_mode, window, cx).size.height
}

fn drop_preview_external_paths_height(_width: Pixels, _window: &mut Window, _cx: &mut Context<RichTextEditor>) -> Pixels {
  px(96.0)
}

fn hash_input_paragraphs(paragraphs: &[InputParagraph]) -> u64 {
  let mut hasher = DefaultHasher::new();
  hash_input_paragraph_slice(paragraphs, &mut hasher);
  hasher.finish()
}

fn hash_input_paragraph_slice(paragraphs: &[InputParagraph], hasher: &mut DefaultHasher) {
  paragraphs.len().hash(hasher);
  for paragraph in paragraphs {
    hash_input_paragraph(paragraph, hasher);
  }
}

fn hash_input_paragraph(paragraph: &InputParagraph, hasher: &mut DefaultHasher) {
  paragraph.style.hash(hasher);
  paragraph.runs.len().hash(hasher);
  for run in &paragraph.runs {
    run.text.hash(hasher);
    run.styles.hash(hasher);
  }
}

fn hash_external_paths(paths: &[PathBuf]) -> u64 {
  let mut hasher = DefaultHasher::new();
  paths.hash(&mut hasher);
  hasher.finish()
}

fn external_paths_preview_label(paths: &[PathBuf]) -> SharedString {
  match paths {
    [path] => path
      .file_name()
      .map(|name| SharedString::from(format!("Drop {}", name.to_string_lossy())))
      .unwrap_or_else(|| SharedString::from("Drop file")),
    paths => SharedString::from(format!("Drop {} files", paths.len())),
  }
}
