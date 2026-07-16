#[hotpath::measure]
fn render_image_block(
  document: &DocumentProjection,
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
  if asset.is_loading_placeholder() {
    return render_loading_image_placeholder(document, image, asset, row_size, selected, editor, block_ix);
  }
  let Some(format) = ImageFormat::from_mime_type(asset.mime_type.as_ref()) else {
    return reserved_object_frame(document, row_size, selected)
      .child("Unsupported image")
      .into_any_element();
  };
  // B-S2: the shared per-asset handle — `Image::from_bytes` cloned the whole
  // byte buffer on every paint of every visible image.
  let gpui_image = asset.render_image(format);
  image_object_frame(document, image, asset, row_size, selected)
    .child(
      img(gpui_image)
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

#[hotpath::measure]
fn render_loading_image_placeholder(
  document: &DocumentProjection,
  image: &ImageBlock,
  asset: &AssetRecord,
  row_size: Size<Pixels>,
  selected: bool,
  editor: Entity<RichTextEditor>,
  block_ix: usize,
) -> gpui::AnyElement {
  image_object_frame(document, image, asset, row_size, selected)
    .child(
      div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap_1()
        .text_sm()
        .text_color(rgb(0x6b7280))
        .child(div().text_xs().text_color(rgb(0x9ca3af)).child("..."))
        .child("Loading image"),
    )
    .when(selected, |this| this.children(image_resize_handles(editor, block_ix)))
    .into_any_element()
}

#[hotpath::measure]
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

#[hotpath::measure]
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

#[hotpath::measure]
fn render_equation_block(
  document: &DocumentProjection,
  equation: &EquationBlock,
  block_ix: usize,
  row_size: Size<Pixels>,
  selected: bool,
) -> gpui::AnyElement {
  let _ = block_ix;
  let frame = reserved_object_frame(document, row_size, selected);
  // B-S2: the box tracks the INTRINSIC math size × document zoom — the old
  // fixed 60px height + `len × 26px` width guess squished tall equations and
  // ignored zoom entirely. Falls back to the legacy guess until rendered.
  let zoom = document.theme.zoom_factor.max(0.01);
  let intrinsic = EquationRenderer::intrinsic_size(equation).ok();
  let max_width: f32 = (row_size.width - document.theme.pageless_inset_x * 2.0)
    .max(px(240.0))
    .into();
  let (equation_width, equation_height) = match intrinsic {
    Some((width, height)) => {
      let scaled_width = (width * zoom).clamp(24.0, max_width);
      let scale = if width * zoom > 0.0 { scaled_width / (width * zoom) } else { 1.0 };
      (px(scaled_width), px((height * zoom * scale).max(16.0)))
    },
    None => {
      let source_width = equation.source.len().max(4) as f32 * 26.0;
      (px(source_width.clamp(240.0, max_width)), px(60.0))
    },
  };
  match EquationRenderer::render_image(equation) {
    Ok(image) => {
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
              img(image)
                .w(equation_width)
                .h(equation_height)
                .object_fit(gpui::ObjectFit::ScaleDown)
                .with_loading(|| div().size_full().bg(rgb(0xffffff)).into_any_element())
                .with_fallback(|| {
                  div()
                    .size_full()
                    .bg(rgb(0xffffff))
                    .child("Equation unavailable")
                    .into_any_element()
                }),
            ),
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
          .child(div().text_xs().text_color(rgb(0xa40000)).child(error)),
      )
      .into_any_element(),
  }
}

type EquationCacheKey = (SharedString, bool);
type EquationRenderCache = FxHashMap<EquationCacheKey, Result<Arc<Vec<u8>>, String>>;

static EQUATION_SVG_CACHE: OnceLock<Mutex<EquationRenderCache>> = OnceLock::new();
static EQUATION_PNG_CACHE: OnceLock<Mutex<EquationRenderCache>> = OnceLock::new();
/// B-S2: shared gpui image handles per (source, display) — no per-paint clone.
type EquationImageCache = FxHashMap<EquationCacheKey, Result<Arc<gpui::Image>, String>>;
static EQUATION_IMAGE_CACHE: OnceLock<Mutex<EquationImageCache>> = OnceLock::new();
