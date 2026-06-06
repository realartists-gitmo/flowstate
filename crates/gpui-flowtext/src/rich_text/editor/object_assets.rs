#[hotpath::measure]
fn reserved_object_frame(document: &Document, row_size: Size<Pixels>, selected: bool) -> gpui::Div {
  let object_height = (row_size.height - document.theme.paragraph_after).max(px(1.0));
  let object_width = (row_size.width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  div()
    .relative()
    .w(object_width)
    .h(object_height)
    .ml(document.theme.pageless_inset_x)
    .mr(document.theme.pageless_inset_x)
    .mb(document.theme.paragraph_after)
    .overflow_hidden()
    .bg(rgb(0xffffff))
    .border_1()
    .border_color(if selected { rgb(0x0969da) } else { rgb(0xffffff) })
}

#[hotpath::measure]
fn image_object_frame(document: &Document, image: &ImageBlock, asset: &AssetRecord, row_size: Size<Pixels>, selected: bool) -> gpui::Div {
  let available_width = (row_size.width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  let intrinsic = image_asset_intrinsic_size(asset);
  let object_width = match image.sizing {
    ImageSizing::Fixed { width_px, .. } => px(width_px as f32).min(available_width),
    ImageSizing::FitWidth => available_width,
    ImageSizing::Intrinsic => intrinsic
      .map(|(width, _)| width.min(available_width))
      .unwrap_or(available_width),
  };
  let object_height = (row_size.height - document.theme.paragraph_after).max(px(1.0));
  let left_margin = document.theme.pageless_inset_x
    + match image.alignment {
      BlockAlignment::Left => px(0.0),
      BlockAlignment::Center => (available_width - object_width).max(px(0.0)) / 2.0,
      BlockAlignment::Right => (available_width - object_width).max(px(0.0)),
    };
  div()
    .relative()
    .w(object_width)
    .h(object_height)
    .ml(left_margin)
    .mr(document.theme.pageless_inset_x)
    .mb(document.theme.paragraph_after)
    .overflow_hidden()
    .bg(rgb(0xffffff))
    .border_1()
    .border_color(if selected { rgb(0x0969da) } else { rgb(0xffffff) })
}

#[hotpath::measure]
fn image_asset_intrinsic_size(asset: &AssetRecord) -> Option<(Pixels, Pixels)> {
  let size = imagesize::blob_size(asset.bytes.as_ref()).ok()?;
  if size.width == 0 || size.height == 0 {
    return None;
  }
  Some((px(size.width as f32), px(size.height as f32)))
}

#[hotpath::measure]
fn image_asset_from_path(path: &Path) -> Option<(AssetRecord, SharedString)> {
  let bytes = fs::read(path).ok()?;
  let format = image_format_for_path(path)?;
  let original_name = path
    .file_name()
    .map(|name| name.to_string_lossy().to_string());
  let alt_text: SharedString = original_name.clone().unwrap_or_default().into();
  let mut hasher = DefaultHasher::new();
  bytes.hash(&mut hasher);
  Some((
    AssetRecord {
      id: AssetId(uuid::Uuid::new_v4().as_u128()),
      mime_type: format.mime_type().into(),
      original_name: original_name.map(Into::into),
      content_hash: hasher.finish(),
      bytes: Arc::new(bytes),
    },
    alt_text,
  ))
}

#[hotpath::measure]
fn image_asset_from_image(image: Image) -> (AssetRecord, SharedString) {
  let asset_id = AssetId(uuid::Uuid::new_v4().as_u128());
  let mut hasher = DefaultHasher::new();
  image.bytes.hash(&mut hasher);
  (
    AssetRecord {
      id: asset_id,
      mime_type: image.format.mime_type().into(),
      original_name: None,
      content_hash: hasher.finish(),
      bytes: Arc::new(image.bytes),
    },
    "Pasted image".into(),
  )
}

#[hotpath::measure]
fn image_format_for_path(path: &Path) -> Option<ImageFormat> {
  match path
    .extension()?
    .to_string_lossy()
    .to_ascii_lowercase()
    .as_str()
  {
    "png" => Some(ImageFormat::Png),
    "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
    "webp" => Some(ImageFormat::Webp),
    "gif" => Some(ImageFormat::Gif),
    "svg" => Some(ImageFormat::Svg),
    "bmp" => Some(ImageFormat::Bmp),
    "tif" | "tiff" => Some(ImageFormat::Tiff),
    _ => None,
  }
}

