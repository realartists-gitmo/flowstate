struct EquationRenderer;

#[hotpath::measure_all]
impl EquationRenderer {
  fn clear_entries(keys: impl IntoIterator<Item = (SharedString, bool)>) {
    let keys: Vec<_> = keys.into_iter().collect();
    if keys.is_empty() {
      return;
    }

    if let Some(cache) = EQUATION_SVG_CACHE.get()
      && let Ok(mut cache) = cache.lock()
    {
      for key in &keys {
        cache.remove(key);
      }
    }

    if let Some(cache) = EQUATION_PNG_CACHE.get()
      && let Ok(mut cache) = cache.lock()
    {
      for key in &keys {
        cache.remove(key);
      }
    }
  }

  fn svg_bytes(equation: &EquationBlock) -> Result<Arc<Vec<u8>>, String> {
    let display = matches!(equation.display, EquationDisplay::Display);
    let key = (equation.source.clone(), display);
    let cache = EQUATION_SVG_CACHE.get_or_init(|| Mutex::new(FxHashMap::default()));
    if let Some(cached) = cache.lock().ok().and_then(|cache| cache.get(&key).cloned()) {
      return cached;
    }
    let result = if display {
      mathjax_svg::convert_to_svg(key.0.as_ref())
    } else {
      mathjax_svg::convert_to_svg_inline(key.0.as_ref())
    }
    .map(|svg| Arc::new(pad_mathjax_svg_viewbox(&svg).into_bytes()))
    .map_err(|error| error.to_string());
    if let Ok(mut cache) = cache.lock() {
      cache.insert(key, result.clone());
    }
    result
  }

  fn png_bytes(equation: &EquationBlock) -> Result<Arc<Vec<u8>>, String> {
    let display = matches!(equation.display, EquationDisplay::Display);
    let key = (equation.source.clone(), display);
    let cache = EQUATION_PNG_CACHE.get_or_init(|| Mutex::new(FxHashMap::default()));
    if let Some(cached) = cache.lock().ok().and_then(|cache| cache.get(&key).cloned()) {
      return cached;
    }
    let result = Self::svg_bytes(equation)
      .and_then(|svg| rasterize_svg_to_png(svg.as_ref()))
      .map(Arc::new);
    if let Ok(mut cache) = cache.lock() {
      cache.insert(key, result.clone());
    }
    result
  }
}

#[hotpath::measure]
fn rasterize_svg_to_png(svg: &[u8]) -> Result<Vec<u8>, String> {
  const EQUATION_RASTER_SCALE: f32 = 4.0;
  let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default()).map_err(|error| error.to_string())?;
  let svg_size = tree.size();
  let width = (svg_size.width() * EQUATION_RASTER_SCALE).ceil().max(1.0) as u32;
  let height = (svg_size.height() * EQUATION_RASTER_SCALE).ceil().max(1.0) as u32;
  let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height).ok_or_else(|| "equation SVG has invalid raster size".to_string())?;
  resvg::render(
    &tree,
    resvg::tiny_skia::Transform::from_scale(EQUATION_RASTER_SCALE, EQUATION_RASTER_SCALE),
    &mut pixmap.as_mut(),
  );

  pixmap.encode_png().map_err(|error| error.to_string())
}

#[hotpath::measure]
fn pad_mathjax_svg_viewbox(svg: &str) -> String {
  let Some(viewbox_start) = svg.find("viewBox=\"") else {
    return svg.to_string();
  };
  let values_start = viewbox_start + "viewBox=\"".len();
  let Some(values_end) = svg[values_start..]
    .find('"')
    .map(|offset| values_start + offset)
  else {
    return svg.to_string();
  };
  let values = &svg[values_start..values_end];
  let mut parts = values
    .split_whitespace()
    .filter_map(|part| part.parse::<f32>().ok());
  let (Some(x), Some(y), Some(width), Some(height)) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
    return svg.to_string();
  };
  let top_pad = height * 0.08;
  let bottom_pad = height * 0.18;
  let replacement = format!("{} {} {} {}", x, y - top_pad, width, height + top_pad + bottom_pad);
  let mut output = String::with_capacity(svg.len() + replacement.len());
  output.push_str(&svg[..values_start]);
  output.push_str(&replacement);
  output.push_str(&svg[values_end..]);
  output
}

