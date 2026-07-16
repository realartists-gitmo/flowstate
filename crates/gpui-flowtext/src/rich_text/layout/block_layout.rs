#[hotpath::measure]
pub(super) fn build_layout(
  document: &DocumentProjection,
  width: Pixels,
  previous_layout: Option<&LayoutState>,
  window: &mut Window,
  cx: &mut App,
) -> LayoutState {
  let timing = Instant::now();
  let mut y = document.theme.pageless_inset_top;
  let mut paragraphs = Vec::with_capacity(document.paragraphs.len());
  let mut max_width = width;
  let mut shaped_count = 0;
  let mut reused_count = 0;
  let previous_layout = previous_layout.filter(|layout| layout.width == width);

  for paragraph_ix in 0..document.paragraphs.len() {
    let previous_paragraph = previous_layout.and_then(|layout| layout.paragraphs.get(paragraph_ix));
    let (paragraph, next_y, paragraph_max_width, reused) = layout_paragraph_at(document, paragraph_ix, width, y, previous_paragraph, window, cx);
    if reused {
      reused_count += 1;
    } else {
      shaped_count += 1;
    }
    max_width = max_width.max(paragraph_max_width);
    y = next_y;
    paragraphs.push(paragraph);
  }

  let layout = LayoutState {
    blocks: paragraphs
      .iter()
      .cloned()
      .map(LaidOutBlock::Paragraph)
      .collect(),
    paragraph_to_block: (0..paragraphs.len()).collect(),
    block_to_paragraph: (0..paragraphs.len()).map(Some).collect(),
    paragraphs,
    bounds: None,
    size: size(max_width, y + document.theme.pageless_inset_bottom),
    width,
    snap_underline_rules_to_pixels: document.theme.snap_underline_rules_to_pixels,
  };
  log_timing_lazy("build layout", timing, || {
    format!(
      "blocks={} paragraphs={} shaped={shaped_count} reused={reused_count}",
      layout.block_count(),
      layout.paragraphs.len()
    )
  });
  layout
}

#[hotpath::measure]
pub(super) fn build_layout_with_visibility(
  document: &DocumentProjection,
  width: Pixels,
  previous_layout: Option<&LayoutState>,
  invisibility_mode: bool,
  window: &mut Window,
  cx: &mut App,
) -> LayoutState {
  if !invisibility_mode {
    return build_layout(document, width, previous_layout, window, cx);
  }

  let timing = Instant::now();
  let mut y = document.theme.pageless_inset_top;
  let mut paragraphs = Vec::with_capacity(document.paragraphs.len());
  let mut max_width = width;
  let mut shaped_count = 0;
  let mut reused_count = 0;
  let previous_layout = previous_layout.filter(|layout| layout.width == width);

  for paragraph_ix in 0..document.paragraphs.len() {
    let Some(source_paragraph) = document.paragraphs.get(paragraph_ix) else {
      continue;
    };
    if !paragraph_is_visible(document, source_paragraph) {
      paragraphs.push(LaidOutParagraph {
        index: paragraph_ix,
        cache_key: paragraph_cache_key(document, source_paragraph),
        len: 0,
        byte_range: 0..0,
        top: y,
        bottom: y,
        lines: Vec::new(),
        borders: Vec::new(),
      });
      continue;
    }

    let projected_document = invisibility_projected_document(document, paragraph_ix);
    let layout_document = projected_document.as_ref().unwrap_or(document);
    let layout_paragraph_ix = if projected_document.is_some() { 0 } else { paragraph_ix };
    let previous_paragraph = previous_layout.and_then(|layout| paragraph_layout(layout, paragraph_ix));
    let (mut paragraph, next_y, paragraph_max_width, reused) =
      layout_paragraph_at(layout_document, layout_paragraph_ix, width, y, previous_paragraph, window, cx);
    paragraph.index = paragraph_ix;
    if reused {
      reused_count += 1;
    } else {
      shaped_count += 1;
    }
    max_width = max_width.max(paragraph_max_width);
    y = next_y;
    paragraphs.push(paragraph);
  }

  let layout = LayoutState {
    blocks: paragraphs
      .iter()
      .cloned()
      .map(LaidOutBlock::Paragraph)
      .collect(),
    paragraph_to_block: (0..paragraphs.len()).collect(),
    block_to_paragraph: (0..paragraphs.len()).map(Some).collect(),
    paragraphs,
    bounds: None,
    size: size(max_width, y + document.theme.pageless_inset_bottom),
    width,
    snap_underline_rules_to_pixels: document.theme.snap_underline_rules_to_pixels,
  };
  log_timing_lazy("build visible layout", timing, || {
    format!(
      "blocks={} paragraphs={} shaped={shaped_count} reused={reused_count}",
      layout.block_count(),
      layout.paragraphs.len()
    )
  });
  layout
}

#[hotpath::measure]
pub(super) fn build_single_paragraph_layout_with_visibility(
  document: &DocumentProjection,
  paragraph_ix: usize,
  width: Pixels,
  previous_layout: Option<&LayoutState>,
  invisibility_mode: bool,
  window: &mut Window,
  cx: &mut App,
) -> LayoutState {
  let timing = Instant::now();
  let start_y = if paragraph_ix == 0 { document.theme.pageless_inset_top } else { px(0.0) };
  if invisibility_mode
    && document
      .paragraphs
      .get(paragraph_ix)
      .is_some_and(|paragraph| !paragraph_is_visible(document, paragraph))
  {
    let hidden = LaidOutParagraph {
      index: paragraph_ix,
      cache_key: document
        .paragraphs
        .get(paragraph_ix)
        .map(|paragraph| paragraph_cache_key(document, paragraph))
        .unwrap_or(ParagraphCacheKey { fingerprint: 0 }),
      len: 0,
      byte_range: 0..0,
      top: px(0.0),
      bottom: px(0.0),
      lines: Vec::new(),
      borders: Vec::new(),
    };
    return LayoutState {
      blocks: vec![LaidOutBlock::Paragraph(hidden.clone())],
      paragraph_to_block: vec![0],
      block_to_paragraph: vec![Some(paragraph_ix)],
      paragraphs: vec![hidden],
      bounds: None,
      size: size(width, px(0.0)),
      width,
      snap_underline_rules_to_pixels: document.theme.snap_underline_rules_to_pixels,
    };
  }
  let projected_document = invisibility_mode
    .then(|| invisibility_projected_document(document, paragraph_ix))
    .flatten();
  let layout_document = projected_document.as_ref().unwrap_or(document);
  let layout_paragraph_ix = if projected_document.is_some() { 0 } else { paragraph_ix };
  let previous_paragraph = previous_layout
    .filter(|layout| layout.width == width)
    .and_then(|layout| paragraph_layout(layout, paragraph_ix));
  let (mut paragraph, mut height, max_width, reused) =
    layout_paragraph_at(layout_document, layout_paragraph_ix, width, start_y, previous_paragraph, window, cx);
  paragraph.index = paragraph_ix;
  if paragraph_ix + 1 == document.paragraphs.len() {
    height += document.theme.pageless_inset_bottom;
  }
  let layout = LayoutState {
    blocks: vec![LaidOutBlock::Paragraph(paragraph.clone())],
    paragraph_to_block: vec![0],
    block_to_paragraph: vec![Some(paragraph_ix)],
    paragraphs: vec![paragraph],
    bounds: None,
    size: size(max_width.max(width), height),
    width,
    snap_underline_rules_to_pixels: document.theme.snap_underline_rules_to_pixels,
  };
  log_timing_lazy("build visible paragraph", timing, || {
    format!("paragraph={paragraph_ix} shaped={} reused={}", usize::from(!reused), usize::from(reused))
  });
  layout
}

#[allow(dead_code, reason = "Block layout helper is retained for incremental object-layout work.")]
#[hotpath::measure]
pub(super) fn build_structural_block_layout(
  document: &DocumentProjection,
  width: Pixels,
  previous_layout: Option<&LayoutState>,
  window: &mut Window,
  cx: &mut App,
) -> Vec<LaidOutBlock> {
  let mut y = document.theme.pageless_inset_top;
  let mut paragraph_ix = 0;
  let previous_layout = previous_layout.filter(|layout| layout.width == width);
  let mut blocks = Vec::with_capacity(document.blocks.len());

  for (block_ix, block) in document.blocks.iter().enumerate() {
    match block {
      Block::Paragraph(_) => {
        if paragraph_ix >= document.paragraphs.len() {
          continue;
        }
        let previous_paragraph = previous_layout.and_then(|layout| paragraph_layout(layout, paragraph_ix));
        let (paragraph, next_y, _, _) = layout_paragraph_at(document, paragraph_ix, width, y, previous_paragraph, window, cx);
        y = next_y;
        paragraph_ix += 1;
        blocks.push(LaidOutBlock::Paragraph(paragraph));
      },
      Block::Image(image) => {
        let height = image_placeholder_height(document, image, width);
        let bounds = structural_block_bounds(document, width, y, height);
        blocks.push(LaidOutBlock::Image(LaidOutObjectBlock {
          block_ix,
          top: y,
          bottom: y + height,
          bounds,
          render_ready: false,
        }));
        y += height + document.theme.paragraph_after;
      },
      Block::Equation(equation) => {
        let height = equation_placeholder_height(document, equation);
        let bounds = structural_block_bounds(document, width, y, height);
        blocks.push(LaidOutBlock::Equation(LaidOutObjectBlock {
          block_ix,
          top: y,
          bottom: y + height,
          bounds,
          render_ready: false,
        }));
        y += height + document.theme.paragraph_after;
      },
      Block::Table(table) => {
        let table = layout_table_block(document, block_ix, table, width, y, window, cx);
        y = table.bottom + document.theme.paragraph_after;
        blocks.push(LaidOutBlock::Table(table));
      },
    }
  }

  blocks
}

#[hotpath::measure]
fn structural_block_bounds(document: &DocumentProjection, width: Pixels, y: Pixels, height: Pixels) -> Bounds<Pixels> {
  let left = document.theme.pageless_inset_x;
  let block_width = (width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  Bounds::new(point(left, y), size(block_width, height.max(px(1.0))))
}

#[hotpath::measure]
fn image_placeholder_height(document: &DocumentProjection, image: &ImageBlock, width: Pixels) -> Pixels {
  if document
    .assets
    .assets
    .get(&image.asset_id)
    .is_some_and(AssetRecord::is_loading_placeholder)
  {
    return px(IMAGE_LOADING_PLACEHOLDER_HEIGHT_PX);
  }
  let available_width = (width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  let intrinsic = image_intrinsic_size(document, image);
  match image.sizing {
    ImageSizing::Fixed {
      height_px: Some(height_px), ..
    } => px(height_px as f32),
    ImageSizing::Fixed { width_px, height_px: None } => image_height_for_width(intrinsic, px(width_px as f32)).unwrap_or(px(160.0)),
    ImageSizing::FitWidth => image_height_for_width(intrinsic, available_width).unwrap_or((available_width * 0.5625).max(px(72.0))),
    ImageSizing::Intrinsic => intrinsic.map(|(_, height)| height).unwrap_or(px(160.0)),
  }
}

#[hotpath::measure]
fn image_intrinsic_size(document: &DocumentProjection, image: &ImageBlock) -> Option<(Pixels, Pixels)> {
  let asset = document.assets.assets.get(&image.asset_id)?;
  if asset.is_loading_placeholder() {
    return None;
  }
  // B-S2: stored dimensions first (CRDT asset map / intake sniff); the
  // byte-header sniff in THE LAYOUT HOT PATH survives only for legacy records.
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
fn image_height_for_width(intrinsic: Option<(Pixels, Pixels)>, width: Pixels) -> Option<Pixels> {
  let (intrinsic_width, intrinsic_height) = intrinsic?;
  let intrinsic_width: f32 = intrinsic_width.into();
  let intrinsic_height: f32 = intrinsic_height.into();
  if intrinsic_width <= 0.0 || intrinsic_height <= 0.0 {
    return None;
  }
  let width: f32 = width.into();
  Some(px(((width / intrinsic_width) * intrinsic_height).max(1.0)))
}

#[hotpath::measure]
fn equation_placeholder_height(document: &DocumentProjection, equation: &EquationBlock) -> Pixels {
  // B-S2: rendered equations size to their intrinsic math height × zoom; the
  // font-size heuristic below survives as the pre-render estimate.
  if let Ok((_, height)) = crate::rich_text::editor::equation_intrinsic_size(equation) {
    return px((height * document.theme.zoom_factor.max(0.01)).max(16.0)) + px(8.0);
  }
  match equation.display {
    EquationDisplay::Display => (document.theme.body_font_size * document.theme.zoom_factor.max(0.01) * 3.7).max(px(72.0)),
    EquationDisplay::InlineLikeParagraph => (document.theme.body_font_size * document.theme.zoom_factor.max(0.01) * 2.75).max(px(56.0)),
  }
}

#[hotpath::measure]
pub(super) fn layout_structural_block_at(
  document: &DocumentProjection,
  block_ix: usize,
  width: Pixels,
  y: Pixels,
  invisibility_mode: bool,
  window: &mut Window,
  cx: &mut App,
) -> Option<LaidOutBlock> {
  match document.blocks.get(block_ix)? {
    Block::Paragraph(_) => None,
    Block::Image(image) => {
      let height = image_placeholder_height(document, image, width);
      Some(LaidOutBlock::Image(LaidOutObjectBlock {
        block_ix,
        top: y,
        bottom: y + height,
        bounds: structural_block_bounds(document, width, y, height),
        render_ready: false,
      }))
    },
    Block::Equation(equation) => {
      let height = equation_placeholder_height(document, equation);
      Some(LaidOutBlock::Equation(LaidOutObjectBlock {
        block_ix,
        top: y,
        bottom: y + height,
        bounds: structural_block_bounds(document, width, y, height),
        render_ready: false,
      }))
    },
    Block::Table(table) => Some(LaidOutBlock::Table(layout_table_block_with_visibility(document, block_ix, table, width, y, invisibility_mode, window, cx))),
  }
}

#[hotpath::measure]
pub(super) fn structural_block_height(block: &LaidOutBlock) -> Pixels {
  match block {
    LaidOutBlock::Paragraph(paragraph) => paragraph.bottom - paragraph.top,
    LaidOutBlock::Image(object) | LaidOutBlock::Equation(object) => object.bottom - object.top,
    LaidOutBlock::Table(table) => table.bottom - table.top,
  }
}
