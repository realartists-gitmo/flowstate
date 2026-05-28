fn render_image_block(
  document: &Document,
  image: &ImageBlock,
  block_ix: usize,
  row_size: Size<Pixels>,
  selected_block: Option<BlockSelection>,
  editor: Entity<RichTextEditor>,
) -> gpui::AnyElement {
  let selected = selected_block == Some(BlockSelection::Image(block_ix));
  let Some(asset) = document.assets.assets.get(&image.asset_id) else {
    return reserved_object_frame(document, row_size, selected)
      .child("Missing image")
      .into_any_element();
  };
  let Some(format) = ImageFormat::from_mime_type(asset.mime_type.as_ref()) else {
    return reserved_object_frame(document, row_size, selected)
      .child("Unsupported image")
      .into_any_element();
  };
  let gpui_image = Image::from_bytes(format, asset.bytes.as_ref().clone());
  image_object_frame(document, image, asset, row_size, selected)
    .child(
      img(Arc::new(gpui_image))
        .size_full()
        .object_fit(gpui::ObjectFit::Contain)
        .with_loading(|| div().size_full().bg(rgb(0xffffff)).into_any_element())
        .with_fallback(|| {
          div()
            .size_full()
            .bg(rgb(0xffffff))
            .child("Image unavailable")
            .into_any_element()
        }),
    )
    .when(selected, |this| this.children(image_resize_handles(editor, block_ix)))
    .into_any_element()
}

fn image_resize_handles(editor: Entity<RichTextEditor>, block_ix: usize) -> Vec<gpui::AnyElement> {
  [
    ImageResizeHandle::TopLeft,
    ImageResizeHandle::TopRight,
    ImageResizeHandle::Left,
    ImageResizeHandle::Right,
    ImageResizeHandle::BottomLeft,
    ImageResizeHandle::BottomRight,
  ]
  .into_iter()
  .map(|handle| image_resize_handle(editor.clone(), block_ix, handle))
  .collect()
}

fn image_resize_handle(editor: Entity<RichTextEditor>, block_ix: usize, handle: ImageResizeHandle) -> gpui::AnyElement {
  let cursor = match handle {
    ImageResizeHandle::Left | ImageResizeHandle::Right => CursorStyle::ResizeLeftRight,
    ImageResizeHandle::TopLeft | ImageResizeHandle::BottomRight => CursorStyle::ResizeUpLeftDownRight,
    ImageResizeHandle::TopRight | ImageResizeHandle::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
  };
  div()
    .absolute()
    .when(
      matches!(
        handle,
        ImageResizeHandle::Left | ImageResizeHandle::TopLeft | ImageResizeHandle::BottomLeft
      ),
      |this| this.left(px(-4.0)),
    )
    .when(
      matches!(
        handle,
        ImageResizeHandle::Right | ImageResizeHandle::TopRight | ImageResizeHandle::BottomRight
      ),
      |this| this.right(px(-4.0)),
    )
    .when(matches!(handle, ImageResizeHandle::TopLeft | ImageResizeHandle::TopRight), |this| {
      this.top(px(-4.0))
    })
    .when(matches!(handle, ImageResizeHandle::BottomLeft | ImageResizeHandle::BottomRight), |this| {
      this.bottom(px(-4.0))
    })
    .when(handle == ImageResizeHandle::Left || handle == ImageResizeHandle::Right, |this| {
      this.top(px(24.0))
    })
    .size(px(9.0))
    .bg(rgb(0xffffff))
    .border_1()
    .border_color(rgb(0x0969da))
    .cursor(cursor)
    .on_mouse_down(MouseButton::Left, move |event, window, cx| {
      cx.stop_propagation();
      editor.update(cx, |editor, cx| {
        editor.start_image_resize_drag(block_ix, handle, event.position, window, cx);
      });
    })
    .into_any_element()
}

fn render_equation_block(
  document: &Document,
  equation: &EquationBlock,
  block_ix: usize,
  row_size: Size<Pixels>,
  selected: bool,
  source_selection: Option<EquationSourceSelection>,
) -> gpui::AnyElement {
  let _ = block_ix;
  let frame = reserved_object_frame(document, row_size, selected);
  let equation_width = {
    let source_width = equation.source.len().max(4) as f32 * 26.0;
    let max_width: f32 = (row_size.width - document.theme.pageless_inset_x * 2.0)
      .max(px(240.0))
      .into();
    px(source_width.clamp(240.0, max_width))
  };
  let source_strip = || {
    div()
      .w_full()
      .px_2()
      .py_1()
      .text_xs()
      .line_height(relative(1.15))
      .font_family("Consolas")
      .text_color(rgb(0x000000))
      .bg(rgb(0xf6f8fa))
      .relative()
      .h(px(22.0))
      .flex()
      .flex_row()
      .items_center()
      .children(equation_source_text_elements(&equation.source, source_selection))
      .into_any_element()
  };
  match EquationRenderer::png_bytes(equation) {
    Ok(png) => {
      let image = Image::from_bytes(ImageFormat::Png, png.as_ref().clone());
      frame
        .child(
          div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .child(
              img(Arc::new(image))
                .w(equation_width)
                .h(px(60.0))
                .object_fit(gpui::ObjectFit::ScaleDown)
                .with_loading(|| div().size_full().bg(rgb(0xffffff)).into_any_element())
                .with_fallback(|| {
                  div()
                    .size_full()
                    .bg(rgb(0xffffff))
                    .child("Equation unavailable")
                    .into_any_element()
                }),
            )
            .when(selected, |this| this.child(source_strip())),
        )
        .into_any_element()
    },
    Err(error) => frame
      .child(
        div()
          .size_full()
          .flex()
          .flex_col()
          .items_center()
          .justify_center()
          .gap_1()
          .font_family("Cambria Math")
          .text_size(px(18.0))
          .text_color(rgb(0x000000))
          .child(div().text_xs().text_color(rgb(0xa40000)).child(error))
          .child(source_strip()),
      )
      .into_any_element(),
  }
}

fn equation_source_text_elements(source: &str, selection: Option<EquationSourceSelection>) -> Vec<gpui::AnyElement> {
  let range = selection.and_then(|selection| {
    if selection.anchor == selection.caret {
      None
    } else {
      Some(selection.anchor.min(selection.caret)..selection.anchor.max(selection.caret))
    }
  });
  let caret = selection.map(|selection| selection.caret.min(source.len()));
  let caret_visible = selection
    .map(|selection| selection.caret_visible)
    .unwrap_or(false);
  let mut children = Vec::new();
  const SOURCE_CHAR_WIDTH: f32 = 7.0;
  for (byte, ch) in source.char_indices() {
    let end = byte + ch.len_utf8();
    let selected = range
      .as_ref()
      .is_some_and(|range| byte < range.end && end > range.start);
    children.push(
      div()
        .w(px(SOURCE_CHAR_WIDTH))
        .h_full()
        .flex_none()
        .flex()
        .items_center()
        .when(selected, |this| this.bg(rgb(0x0969da)).text_color(rgb(0xffffff)))
        .child(ch.to_string())
        .into_any_element(),
    );
  }
  if let Some(caret) = caret
    && caret_visible
  {
    let caret_ix = char_index_for_byte(source, caret);
    children.push(
      div()
        .absolute()
        .top(px(3.0))
        .bottom(px(3.0))
        .left(px(caret_ix as f32 * SOURCE_CHAR_WIDTH))
        .w(px(1.0))
        .bg(rgb(0x000000))
        .text_color(rgb(0x000000))
        .into_any_element(),
    );
  }
  children
}

fn byte_for_char_index(text: &str, char_ix: usize) -> usize {
  text
    .char_indices()
    .nth(char_ix)
    .map(|(byte, _)| byte)
    .unwrap_or(text.len())
}

fn char_index_for_byte(text: &str, byte: usize) -> usize {
  text
    .char_indices()
    .take_while(|(char_byte, _)| *char_byte < byte)
    .count()
}

type EquationCacheKey = (SharedString, bool);
type EquationRenderCache = FxHashMap<EquationCacheKey, Result<Arc<Vec<u8>>, String>>;

static EQUATION_SVG_CACHE: OnceLock<Mutex<EquationRenderCache>> = OnceLock::new();
static EQUATION_PNG_CACHE: OnceLock<Mutex<EquationRenderCache>> = OnceLock::new();

