#[hotpath::measure]
fn reserved_object_frame(document: &DocumentProjection, row_size: Size<Pixels>, selected: bool) -> gpui::Div {
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
fn image_object_frame(document: &DocumentProjection, image: &ImageBlock, asset: &AssetRecord, row_size: Size<Pixels>, selected: bool) -> gpui::Div {
  let available_width = (row_size.width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  let intrinsic = image_asset_intrinsic_size(asset);
  let loading = asset.is_loading_placeholder();
  let object_width = if loading {
    px(IMAGE_LOADING_PLACEHOLDER_WIDTH_PX).min(available_width)
  } else {
    match image.sizing {
      ImageSizing::Fixed { width_px, .. } => px(width_px as f32).min(available_width),
      ImageSizing::FitWidth => available_width,
      ImageSizing::Intrinsic => intrinsic
        .map(|(width, _)| width.min(available_width))
        .unwrap_or(available_width),
    }
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
    .bg(if loading { rgb(0xf3f4f6) } else { rgb(0xffffff) })
    .border_1()
    .border_color(if selected {
      rgb(0x0969da)
    } else if loading {
      rgb(0xd0d7de)
    } else {
      rgb(0xffffff)
    })
}

#[hotpath::measure]
fn image_asset_intrinsic_size(asset: &AssetRecord) -> Option<(Pixels, Pixels)> {
  if asset.is_loading_placeholder() {
    return None;
  }
  // B-S2: the stored dimensions (CRDT asset map / intake sniff) are the fast
  // path; the byte-header sniff survives only as a legacy-record fallback.
  let (width, height) = match asset.dimensions {
    Some(dimensions) => dimensions,
    None => {
      let size = imagesize::blob_size(asset.bytes.as_ref()).ok()?;
      (size.width as u32, size.height as u32)
    },
  };
  if width == 0 || height == 0 {
    return None;
  }
  Some((px(width as f32), px(height as f32)))
}

#[hotpath::measure]
fn image_asset_from_path(path: &Path) -> Result<(AssetRecord, SharedString), String> {
  // B-S1: refusals carry a REASON — oversized and unsupported files used to
  // vanish silently on insert (picker and drag-drop both).
  // B-S2: the cap equals the collaboration blob transport's per-blob limit
  // (flowstate-collab net/blobs.rs). It was 25 MiB against a 16 MiB transport
  // — an 18 MiB image inserted fine and was permanently un-syncable, peers
  // stuck on "Loading image" forever.
  const MAX_IMAGE_BYTES: u64 = 16 * 1024 * 1024;
  let name = path.file_name().map(|name| name.to_string_lossy().to_string()).unwrap_or_default();
  let metadata = fs::metadata(path).map_err(|error| format!("Couldn't read {name}: {error}"))?;
  if metadata.len() > MAX_IMAGE_BYTES {
    return Err(format!(
      "{name} is {} MB — images over 16 MB can't be inserted (the collaboration transfer limit).",
      metadata.len() / (1024 * 1024)
    ));
  }
  let bytes = fs::read(path).map_err(|error| format!("Couldn't read {name}: {error}"))?;
  let format = image_format_for_path(path)
    .ok_or_else(|| format!("{name} isn't a supported image format (png, jpeg, webp, gif, svg, bmp, tiff)."))?;
  let original_name = path
    .file_name()
    .map(|name| name.to_string_lossy().to_string());
  let alt_text: SharedString = original_name.clone().unwrap_or_default().into();
  let content_hash = AssetRecord::stable_content_hash(&bytes);
  Ok((
    AssetRecord {
      id: AssetId(uuid::Uuid::new_v4().as_u128()),
      mime_type: format.mime_type().into(),
      original_name: original_name.map(Into::into),
      content_hash,
      // B-S2: sniff ONCE at intake; layout reads the stored dimensions.
      dimensions: imagesize::blob_size(&bytes)
        .ok()
        .map(|size| (size.width as u32, size.height as u32)),
      bytes: Arc::new(bytes),
      render_image: Arc::default(),
    },
    alt_text,
  ))
}

#[hotpath::measure]
fn image_asset_from_image(image: Image) -> (AssetRecord, SharedString) {
  let asset_id = AssetId(uuid::Uuid::new_v4().as_u128());
  let content_hash = AssetRecord::stable_content_hash(&image.bytes);
  let dimensions = imagesize::blob_size(&image.bytes)
    .ok()
    .map(|size| (size.width as u32, size.height as u32));
  (
    AssetRecord {
      id: asset_id,
      mime_type: image.format.mime_type().into(),
      original_name: None,
      content_hash,
      dimensions,
      bytes: Arc::new(image.bytes),
      render_image: Arc::default(),
    },
    // B-S1: pasted images used to ship the literal string "Pasted image" as
    // their exported accessibility description. Empty means "no description"
    // — the DOCX exporter skips empty descr.
    SharedString::default(),
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
