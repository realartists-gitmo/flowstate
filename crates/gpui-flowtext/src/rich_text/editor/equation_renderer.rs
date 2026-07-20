use typst::diag::FileError;
use typst::foundations::{Bytes, Datetime};
use typst::layout::PagedDocument;
use typst::syntax::{FileId, Source};
use typst::text::{Font, FontBook};
use typst::utils::LazyHash;
use typst::LibraryExt;
use typst::World;

struct EquationRenderer;

/// B-S2: layout-facing intrinsic size (logical px) for an equation block.
pub(crate) fn equation_intrinsic_size(equation: &EquationBlock) -> Result<(f32, f32), String> {
  EquationRenderer::intrinsic_size(equation)
}

/// B-S8: the composer's live preview — render arbitrary (uncommitted) source
/// through the same cached pipeline the document uses. Returns the shared
/// image handle + intrinsic logical size; `Err` carries the pipeline's
/// diagnostic (mitex/typst parse errors), which the composer shows inline.
pub fn equation_preview_image(source: &str, display: bool) -> Result<(std::sync::Arc<gpui::Image>, (f32, f32)), String> {
  let equation = EquationBlock {
    source: source.to_string().into(),
    syntax: crate::EquationSyntax::Latex,
    display: if display { EquationDisplay::Display } else { EquationDisplay::InlineLikeParagraph },
    version: 0,
  };
  let image = EquationRenderer::render_image(&equation)?;
  let size = EquationRenderer::intrinsic_size(&equation)?;
  Ok((image, size))
}

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

    if let Some(cache) = EQUATION_IMAGE_CACHE.get()
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
    let result = render_typst_equation(key.0.as_ref(), display)
      .map(|svg| Arc::new(pad_svg_viewbox(&svg).into_bytes()))
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

  /// B-S2: the equation's INTRINSIC render size in logical pixels (the raster
  /// runs at [`EQUATION_RASTER_SCALE`]×). Layout and paint size the box from
  /// this instead of the old fixed 60px height + `len × 26px` width guess.
  fn intrinsic_size(equation: &EquationBlock) -> Result<(f32, f32), String> {
    let png = Self::png_bytes(equation)?;
    let size = imagesize::blob_size(png.as_ref()).map_err(|error| error.to_string())?;
    Ok((
      size.width as f32 / EQUATION_RASTER_SCALE,
      size.height as f32 / EQUATION_RASTER_SCALE,
    ))
  }

  /// B-S2: the shared gpui image handle — building `Image::from_bytes` cloned
  /// the full PNG on every paint of every visible equation.
  fn render_image(equation: &EquationBlock) -> Result<Arc<gpui::Image>, String> {
    let display = matches!(equation.display, EquationDisplay::Display);
    let key = (equation.source.clone(), display);
    let cache = EQUATION_IMAGE_CACHE.get_or_init(|| Mutex::new(FxHashMap::default()));
    if let Some(cached) = cache.lock().ok().and_then(|cache| cache.get(&key).cloned()) {
      return cached;
    }
    let result = Self::png_bytes(equation).map(|png| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Png, png.as_ref().clone())));
    if let Ok(mut cache) = cache.lock() {
      cache.insert(key, result.clone());
    }
    result
  }
}

fn render_typst_equation(latex: &str, display: bool) -> Result<String, String> {
  if latex.trim().is_empty() {
    return Ok(r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 1 1" width="1" height="1"/>"#.to_string());
  }
  let typst_math =
    mitex::convert_math(latex, None).map_err(|e| format!("LaTeX conversion failed: {e}"))?;

  let (math_kind, padding) = if display { ("display", "\n") } else { ("inline", "") };

  let typst_source = format!(
    "#set page(width: auto, height: auto, margin: 0pt)\n\
     #set text(size: 14pt)\n\
     ${padding}{math_kind}({typst_math}){padding}$\n"
  );

  let world = EquationWorld::new(&typst_source);
  let warned = typst::compile::<PagedDocument>(&world);
  let document = warned.output.map_err(|errors| {
    errors
      .into_iter()
      .map(|e| format!("{e:?}"))
      .collect::<Vec<_>>()
      .join("; ")
  })?;

  let Some(page) = document.pages.first() else {
    return Err("Typst produced no output".to_string());
  };

  Ok(typst_svg::svg(page))
}

struct EquationWorld {
  library: LazyHash<typst_library::Library>,
  book: LazyHash<FontBook>,
  main_source: Source,
  fonts: Vec<Font>,
}

impl EquationWorld {
  fn new(source: &str) -> Self {
    let library = LazyHash::new(<typst_library::Library as LibraryExt>::builder().build());

    let (fonts, book) = Self::global_fonts();
    let book = LazyHash::new(book.clone());
    let fonts = fonts.clone();

    let main_source = Source::detached(source);

    Self { library, book, main_source, fonts }
  }

  fn global_fonts() -> &'static (Vec<Font>, FontBook) {
    static FONTS: OnceLock<(Vec<Font>, FontBook)> = OnceLock::new();
    FONTS.get_or_init(|| {
      let fonts: Vec<Font> = typst_assets::fonts()
        .flat_map(|data| Font::iter(Bytes::new(data)))
        .collect();
      let book = FontBook::from_fonts(&fonts);
      (fonts, book)
    })
  }
}

impl World for EquationWorld {
  fn library(&self) -> &LazyHash<typst_library::Library> {
    &self.library
  }

  fn book(&self) -> &LazyHash<FontBook> {
    &self.book
  }

  fn main(&self) -> FileId {
    self.main_source.id()
  }

  fn source(&self, id: FileId) -> Result<Source, FileError> {
    if id == self.main_source.id() {
      Ok(self.main_source.clone())
    } else {
      Err(FileError::NotFound(PathBuf::new()))
    }
  }

  fn file(&self, _id: FileId) -> Result<Bytes, FileError> {
    Err(FileError::NotFound(PathBuf::new()))
  }

  fn font(&self, index: usize) -> Option<Font> {
    self.fonts.get(index).cloned()
  }

  fn today(&self, _offset: Option<i64>) -> Option<Datetime> {
    None
  }
}

/// Raster oversampling factor — headroom for zoomed display without re-render.
const EQUATION_RASTER_SCALE: f32 = 4.0;

#[hotpath::measure]
fn rasterize_svg_to_png(svg: &[u8]) -> Result<Vec<u8>, String> {
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
fn pad_svg_viewbox(svg: &str) -> String {
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
