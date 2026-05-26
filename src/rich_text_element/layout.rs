use std::{
  borrow::Cow,
  hash::{Hash, Hasher},
  ops::Range,
};

use gpui::{
  App, Bounds, FontStyle, FontWeight, Hsla, Pixels, Point, ShapedLine, SharedString, Size, TextRun as GpuiTextRun, Window, font, point, px, size,
};
use rustc_hash::{FxHashMap, FxHasher};

use super::*;

#[derive(Clone)]
pub(super) struct LayoutState {
  pub(super) paragraphs: Vec<LaidOutParagraph>,
  pub(super) blocks: Vec<LaidOutBlock>,
  pub(super) paragraph_to_block: Vec<usize>,
  #[allow(dead_code)]
  pub(super) block_to_paragraph: Vec<Option<usize>>,
  pub(super) bounds: Option<Bounds<Pixels>>,
  pub(super) size: Size<Pixels>,
  pub(super) width: Pixels,
  pub(super) snap_underline_rules_to_pixels: bool,
}

impl LayoutState {
  pub(super) fn block_count(&self) -> usize {
    self.blocks.len()
  }

  pub(super) fn paragraph_block_ix(&self, paragraph_ix: usize) -> Option<usize> {
    self.paragraph_to_block.get(paragraph_ix).copied()
  }

  #[allow(dead_code)]
  pub(super) fn block_paragraph_ix(&self, block_ix: usize) -> Option<usize> {
    self.block_to_paragraph.get(block_ix).copied().flatten()
  }

  pub(super) fn hit_test(&self, position: Point<Pixels>) -> DocumentOffset {
    let position = match self.bounds {
      Some(bounds) => position - bounds.origin,
      None => position,
    };
    self.hit_test_unpositioned(position)
  }

  pub(super) fn hit_test_at_bounds(&self, position: Point<Pixels>, bounds: Bounds<Pixels>) -> DocumentOffset {
    self.hit_test_unpositioned(position - bounds.origin)
  }

  fn hit_test_unpositioned(&self, position: Point<Pixels>) -> DocumentOffset {
    let paragraph_ix = first_paragraph_with_bottom_at_or_after(&self.paragraphs, position.y);
    if let Some(paragraph) = self.paragraphs.get(paragraph_ix) {
      if position.y < paragraph.top {
        return DocumentOffset {
          paragraph: paragraph.index,
          byte: paragraph.byte_range.start,
        };
      }
      return paragraph.hit_test(position);
    }
    let Some(last) = self.paragraphs.last() else {
      return DocumentOffset::default();
    };
    DocumentOffset {
      paragraph: last.index,
      byte: last.byte_range.end.min(last.len),
    }
  }
}

#[derive(Clone)]
pub(super) struct LaidOutParagraph {
  pub(super) index: usize,
  pub(super) cache_key: ParagraphCacheKey,
  pub(super) len: usize,
  pub(super) byte_range: Range<usize>,
  pub(super) top: Pixels,
  pub(super) bottom: Pixels,
  pub(super) lines: Vec<LaidOutLine>,
  pub(super) borders: Vec<RunRect>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(super) enum LaidOutBlock {
  Paragraph(LaidOutParagraph),
  Image(LaidOutObjectBlock),
  Equation(LaidOutObjectBlock),
  Table(LaidOutTable),
}

#[derive(Clone)]
#[allow(dead_code)]
pub(super) struct LaidOutObjectBlock {
  pub(super) block_ix: usize,
  pub(super) top: Pixels,
  pub(super) bottom: Pixels,
  pub(super) bounds: Bounds<Pixels>,
  pub(super) render_ready: bool,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(super) struct LaidOutTable {
  pub(super) block_ix: usize,
  pub(super) top: Pixels,
  pub(super) bottom: Pixels,
  pub(super) bounds: Bounds<Pixels>,
  pub(super) rows: Vec<LaidOutTableRow>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(super) struct LaidOutTableRow {
  pub(super) top: Pixels,
  pub(super) bottom: Pixels,
  pub(super) cells: Vec<LaidOutTableCell>,
}

#[derive(Clone)]
#[allow(dead_code)]
pub(super) struct LaidOutTableCell {
  pub(super) bounds: Bounds<Pixels>,
  pub(super) blocks: Vec<LaidOutBlock>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct ParagraphCacheKey {
  pub(super) fingerprint: u64,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) struct ParagraphHeightCacheEntry {
  pub(super) key: ParagraphCacheKey,
  pub(super) width: Pixels,
  pub(super) invisibility_mode: bool,
  pub(super) height: Pixels,
}

pub(super) fn paragraph_cache_key(_document: &Document, paragraph: &Paragraph) -> ParagraphCacheKey {
  let mut hasher = FxHasher::default();
  paragraph.style.hash(&mut hasher);
  paragraph.version.hash(&mut hasher);
  ParagraphCacheKey {
    fingerprint: hasher.finish(),
  }
}

impl LaidOutParagraph {
  pub(super) fn shift_y(&mut self, new_top: Pixels) {
    let delta = new_top - self.top;
    self.top += delta;
    self.bottom += delta;
    for line in &mut self.lines {
      line.origin.y += delta;
    }
    for border in &mut self.borders {
      border.bounds.origin.y += delta;
    }
  }

  pub(super) fn hit_test(&self, position: Point<Pixels>) -> DocumentOffset {
    let line_ix = first_line_with_bottom_at_or_after(&self.lines, position.y);
    if let Some(line) = self.lines.get(line_ix) {
      return DocumentOffset {
        paragraph: self.index,
        byte: line.hit_test_x(position.x - line.origin.x),
      };
    }
    DocumentOffset {
      paragraph: self.index,
      byte: self.byte_range.end.min(self.len),
    }
  }

  pub(super) fn contains_byte(&self, byte: usize) -> bool {
    if self.byte_range.start == self.byte_range.end {
      return byte == self.byte_range.start;
    }

    (byte >= self.byte_range.start && byte < self.byte_range.end) || (byte == self.byte_range.end && self.byte_range.end == self.len)
  }
}

#[derive(Clone)]
pub(super) struct LaidOutLine {
  pub(super) origin: Point<Pixels>,
  pub(super) line_height: Pixels,
  pub(super) ascent: Pixels,
  pub(super) descent: Pixels,
  pub(super) width: Pixels,
  pub(super) start_byte: usize,
  pub(super) end_byte: usize,
  pub(super) segments: Vec<LaidOutSegment>,
  pub(super) rects: Vec<RunRect>,
  pub(super) underlines: Vec<Decoration>,
  pub(super) strikethroughs: Vec<Decoration>,
}

impl LaidOutLine {
  pub(super) fn baseline_y(&self) -> Pixels {
    ((self.line_height - self.ascent - self.descent) / 2.0) + self.ascent
  }

  pub(super) fn hit_test_x(&self, x: Pixels) -> usize {
    for segment in &self.segments {
      if x <= segment.x + segment.width {
        let local_x = (x - segment.x).max(px(0.0));
        return segment.start_byte + segment.shaped.closest_index_for_x(local_x);
      }
    }
    self.end_byte
  }
}

#[derive(Clone)]
pub(super) struct LaidOutSegment {
  pub(super) shaped: ShapedLine,
  pub(super) format: EffectiveRunFormat,
  pub(super) x: Pixels,
  pub(super) width: Pixels,
  pub(super) ascent: Pixels,
  pub(super) descent: Pixels,
  pub(super) font_size: Pixels,
  pub(super) start_byte: usize,
}

#[derive(Clone)]
pub(super) struct RunRect {
  pub(super) bounds: Bounds<Pixels>,
  pub(super) color: Hsla,
  pub(super) snap: RuleSnap,
}

#[derive(Clone, Copy)]
pub(super) enum RuleSnap {
  None,
  Horizontal,
  Vertical,
}

#[derive(Clone)]
pub(super) struct Decoration {
  pub(super) bounds: Bounds<Pixels>,
  pub(super) color: Hsla,
}

#[derive(Clone)]
pub(super) struct EffectiveParagraphFormat {
  pub(super) font_size: Pixels,
  pub(super) font_family: SharedString,
  pub(super) bold: bool,
  pub(super) italic: bool,
  pub(super) color: Hsla,
  pub(super) align: ParagraphAlign,
  pub(super) spacing_before: Pixels,
  pub(super) spacing_after: Pixels,
  pub(super) line_spacing: f32,
  pub(super) border: Option<ParagraphBorder>,
  pub(super) underline: UnderlineKind,
}

#[derive(Clone, Copy)]
pub(super) struct ParagraphBorder {
  width: Pixels,
  space_x: Pixels,
  space_y: Pixels,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum ParagraphAlign {
  Left,
  Center,
}

#[derive(Clone, Copy, PartialEq)]
pub(super) enum UnderlineKind {
  None,
  Single,
  Double,
}

impl From<ThemeUnderline> for UnderlineKind {
  fn from(value: ThemeUnderline) -> Self {
    match value {
      ThemeUnderline::None => UnderlineKind::None,
      ThemeUnderline::Single => UnderlineKind::Single,
      ThemeUnderline::Double => UnderlineKind::Double,
    }
  }
}

#[derive(Clone)]
pub(super) struct EffectiveRunFormat {
  pub(super) font_size: Pixels,
  pub(super) font_family: SharedString,
  pub(super) bold: bool,
  pub(super) italic: bool,
  pub(super) color: Hsla,
  pub(super) underline: UnderlineKind,
  pub(super) strikethrough: bool,
  pub(super) highlight: Option<Hsla>,
  pub(super) border_width: Pixels,
}

pub(super) fn paragraph_format(document: &Document, style: ParagraphStyle) -> EffectiveParagraphFormat {
  let theme = &document.theme;
  let normal = EffectiveParagraphFormat {
    font_size: theme.body_font_size,
    font_family: theme.default_font_family.clone(),
    bold: theme.normal_bold,
    italic: theme.normal_italic,
    color: theme.default_text_color,
    align: ParagraphAlign::Left,
    spacing_before: px(0.0),
    spacing_after: theme.paragraph_after,
    line_spacing: theme.line_spacing,
    border: None,
    underline: theme.normal_underline.into(),
  };

  match style {
    ParagraphStyle::Normal => normal,
    ParagraphStyle::Pocket => EffectiveParagraphFormat {
      font_size: theme.pocket_font_size,
      color: theme.pocket_color,
      bold: theme.pocket_bold,
      italic: theme.pocket_italic,
      align: ParagraphAlign::Center,
      spacing_before: theme.pocket_before,
      spacing_after: px(0.0),
      border: Some(ParagraphBorder {
        width: theme.pocket_border_width,
        space_x: theme.pocket_border_space_x,
        space_y: theme.pocket_border_space_y,
      }),
      underline: theme.pocket_underline.into(),
      ..normal
    },
    ParagraphStyle::Hat => EffectiveParagraphFormat {
      font_size: theme.hat_font_size,
      color: theme.hat_color,
      bold: theme.hat_bold,
      italic: theme.hat_italic,
      align: ParagraphAlign::Center,
      spacing_before: theme.hat_before,
      spacing_after: px(0.0),
      underline: theme.hat_underline.into(),
      ..normal
    },
    ParagraphStyle::Block => EffectiveParagraphFormat {
      font_size: theme.block_font_size,
      color: theme.block_color,
      bold: theme.block_bold,
      italic: theme.block_italic,
      align: ParagraphAlign::Center,
      spacing_before: theme.block_before,
      spacing_after: px(0.0),
      underline: theme.block_underline.into(),
      ..normal
    },
    ParagraphStyle::Tag => EffectiveParagraphFormat {
      font_size: theme.tag_font_size,
      color: theme.tag_color,
      bold: theme.tag_bold,
      italic: theme.tag_italic,
      underline: theme.tag_underline.into(),
      spacing_before: theme.tag_before,
      spacing_after: px(0.0),
      ..normal
    },
    ParagraphStyle::Analytic => EffectiveParagraphFormat {
      font_size: theme.tag_font_size,
      bold: theme.analytic_bold,
      italic: theme.analytic_italic,
      color: theme.analytic_color,
      underline: theme.analytic_underline.into(),
      spacing_before: theme.tag_before,
      spacing_after: px(0.0),
      ..normal
    },
    ParagraphStyle::Undertag => EffectiveParagraphFormat {
      font_size: theme.undertag_font_size,
      font_family: theme.default_font_family.clone(),
      bold: theme.undertag_bold,
      italic: theme.undertag_italic,
      color: theme.undertag_color,
      underline: theme.undertag_underline.into(),
      spacing_after: px(0.0),
      ..normal
    },
  }
}

pub(super) fn run_format(document: &Document, paragraph: EffectiveParagraphFormat, styles: RunStyles) -> EffectiveRunFormat {
  let theme = &document.theme;
  let mut format = EffectiveRunFormat {
    font_size: paragraph.font_size,
    font_family: paragraph.font_family,
    bold: paragraph.bold,
    italic: paragraph.italic,
    color: paragraph.color,
    underline: paragraph.underline,
    strikethrough: styles.strikethrough,
    highlight: styles.highlight.map(|highlight| match highlight {
      HighlightStyle::Spoken => theme.highlight_spoken,
      HighlightStyle::Insert => theme.highlight_insert,
      HighlightStyle::Alternative => theme.highlight_alternative,
    }),
    border_width: px(0.0),
  };

  match styles.semantic {
    RunSemanticStyle::Plain => {},
    RunSemanticStyle::Underline => {
      format.font_size = theme.body_font_size;
      format.color = theme.underline_color;
      format.bold = theme.underline_bold;
      format.italic = theme.underline_italic;
      format.underline = theme.underline_underline.into();
    },
    RunSemanticStyle::Cite => {
      format.font_size = theme.cite_font_size;
      format.color = theme.cite_color;
      format.bold = theme.cite_bold;
      format.italic = theme.cite_italic;
      format.underline = theme.cite_underline.into();
    },
    RunSemanticStyle::Emphasis => {
      format.font_family = theme.default_font_family.clone();
      format.font_size = theme.cite_font_size;
      format.color = theme.emphasis_color;
      format.bold = theme.emphasis_bold;
      format.italic = theme.emphasis_italic;
      format.underline = theme.emphasis_underline.into();
      format.border_width = theme.emphasis_border_width;
    },
    RunSemanticStyle::Condensed => {
      format.font_size = theme.condensed_font_size;
      format.color = theme.condensed_color;
      format.bold = theme.condensed_bold;
      format.italic = theme.condensed_italic;
      format.underline = theme.condensed_underline.into();
    },
    RunSemanticStyle::Ultracondensed => {
      format.font_size = theme.ultracondensed_font_size;
      format.color = theme.ultracondensed_color;
      format.bold = theme.ultracondensed_bold;
      format.italic = theme.ultracondensed_italic;
      format.underline = theme.ultracondensed_underline.into();
    },
  };
  if styles.direct_underline {
    format.underline = UnderlineKind::Single;
  }

  format
}

pub(super) fn build_layout(
  document: &Document,
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
  log_timing(
    "build layout",
    timing,
    format!(
      "blocks={} paragraphs={} shaped={shaped_count} reused={reused_count}",
      layout.block_count(),
      layout.paragraphs.len()
    ),
  );
  layout
}

pub(super) fn build_single_paragraph_layout_with_visibility(
  document: &Document,
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
      .is_some_and(|paragraph| !paragraph_is_visible(paragraph))
  {
    return LayoutState {
      blocks: vec![LaidOutBlock::Paragraph(LaidOutParagraph {
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
      })],
      paragraph_to_block: vec![0],
      block_to_paragraph: vec![Some(paragraph_ix)],
      paragraphs: Vec::new(),
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
  let previous_paragraph = previous_layout.and_then(|layout| paragraph_layout(layout, paragraph_ix));
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
  log_timing(
    "build visible paragraph",
    timing,
    format!("paragraph={paragraph_ix} shaped={} reused={}", usize::from(!reused), usize::from(reused)),
  );
  layout
}

#[allow(dead_code)]
pub(super) fn build_structural_block_layout(
  document: &Document,
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

fn structural_block_bounds(document: &Document, width: Pixels, y: Pixels, height: Pixels) -> Bounds<Pixels> {
  let left = document.theme.pageless_inset_x;
  let block_width = (width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  Bounds::new(point(left, y), size(block_width, height.max(px(1.0))))
}

fn image_placeholder_height(document: &Document, image: &ImageBlock, width: Pixels) -> Pixels {
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

#[cfg(test)]
pub(super) fn image_layout_height_for_test(document: &Document, image: &ImageBlock, width: Pixels) -> Pixels {
  image_placeholder_height(document, image, width)
}

fn image_intrinsic_size(document: &Document, image: &ImageBlock) -> Option<(Pixels, Pixels)> {
  let asset = document.assets.assets.get(&image.asset_id)?;
  let size = imagesize::blob_size(asset.bytes.as_ref()).ok()?;
  if size.width == 0 || size.height == 0 {
    return None;
  }
  Some((px(size.width as f32), px(size.height as f32)))
}

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

fn equation_placeholder_height(document: &Document, equation: &EquationBlock) -> Pixels {
  match equation.display {
    EquationDisplay::Display => (document.theme.body_font_size * 3.7).max(px(72.0)),
    EquationDisplay::InlineLikeParagraph => (document.theme.body_font_size * 2.75).max(px(56.0)),
  }
}

pub(super) fn layout_structural_block_at(
  document: &Document,
  block_ix: usize,
  width: Pixels,
  y: Pixels,
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
    Block::Table(table) => Some(LaidOutBlock::Table(layout_table_block(document, block_ix, table, width, y, window, cx))),
  }
}

pub(super) fn structural_block_height(block: &LaidOutBlock) -> Pixels {
  match block {
    LaidOutBlock::Paragraph(paragraph) => paragraph.bottom - paragraph.top,
    LaidOutBlock::Image(object) | LaidOutBlock::Equation(object) => object.bottom - object.top,
    LaidOutBlock::Table(table) => table.bottom - table.top,
  }
}

fn layout_table_block(
  document: &Document,
  block_ix: usize,
  table: &TableBlock,
  width: Pixels,
  y: Pixels,
  window: &mut Window,
  cx: &mut App,
) -> LaidOutTable {
  let table_left = document.theme.pageless_inset_x;
  let table_width = (width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  let column_count = table
    .column_widths
    .len()
    .max(
      table
        .rows
        .iter()
        .map(|row| row.cells.len())
        .max()
        .unwrap_or(1),
    )
    .max(1);
  let column_widths = resolved_table_column_widths(table, table_width, column_count);
  let mut row_top = y;
  let mut rows = Vec::with_capacity(table.rows.len());

  for row in &table.rows {
    let row_height = table_row_height(document, row, &column_widths, window, cx);
    let mut x = table_left;
    let mut cells = Vec::with_capacity(row.cells.len());
    let mut column_ix = 0;
    for cell in &row.cells {
      let span = cell.col_span.max(1) as usize;
      let cell_width = spanned_column_width(&column_widths, column_ix, span);
      let cell_bounds = Bounds::new(point(x, row_top), size(cell_width, row_height));
      cells.push(LaidOutTableCell {
        bounds: cell_bounds,
        blocks: layout_table_cell_blocks(document, cell, cell_bounds, window, cx),
      });
      x += cell_width;
      column_ix += span;
    }
    rows.push(LaidOutTableRow {
      top: row_top,
      bottom: row_top + row_height,
      cells,
    });
    row_top += row_height;
  }

  LaidOutTable {
    block_ix,
    top: y,
    bottom: row_top,
    bounds: Bounds::new(point(table_left, y), size(table_width, (row_top - y).max(px(1.0)))),
    rows,
  }
}

fn table_row_height(document: &Document, row: &TableRow, column_widths: &[Pixels], window: &mut Window, cx: &mut App) -> Pixels {
  let mut column_ix = 0;
  row
    .cells
    .iter()
    .map(|cell| {
      let span = cell.col_span.max(1) as usize;
      let width = spanned_column_width(column_widths, column_ix, span);
      column_ix += span;
      table_cell_height(document, cell, width, window, cx)
    })
    .fold(px(28.0), Pixels::max)
}

fn resolved_table_column_widths(table: &TableBlock, table_width: Pixels, column_count: usize) -> Vec<Pixels> {
  let mut fixed_total = px(0.0);
  let mut fraction_total = 0u32;
  let mut auto_count = 0usize;
  for ix in 0..column_count {
    match table
      .column_widths
      .get(ix)
      .unwrap_or(&TableColumnWidth::Fraction(1))
    {
      TableColumnWidth::FixedPx(width) => fixed_total += px(*width as f32),
      TableColumnWidth::Fraction(fraction) => fraction_total = fraction_total.saturating_add((*fraction).max(1)),
      TableColumnWidth::Auto => auto_count += 1,
    }
  }
  let remaining = (table_width - fixed_total).max(px(1.0));
  let denominator = fraction_total.saturating_add(auto_count as u32).max(1);
  (0..column_count)
    .map(|ix| {
      match table
        .column_widths
        .get(ix)
        .unwrap_or(&TableColumnWidth::Fraction(1))
      {
        TableColumnWidth::FixedPx(width) => px(*width as f32).max(px(8.0)),
        TableColumnWidth::Fraction(fraction) => remaining * ((*fraction).max(1) as f32 / denominator as f32),
        TableColumnWidth::Auto => remaining * (1.0 / denominator as f32),
      }
    })
    .collect()
}

fn spanned_column_width(column_widths: &[Pixels], column_ix: usize, span: usize) -> Pixels {
  let end = column_ix.saturating_add(span).min(column_widths.len());
  let width = column_widths
    .get(column_ix..end)
    .unwrap_or(&[])
    .iter()
    .copied()
    .fold(px(0.0), |sum, width| sum + width);
  width.max(px(1.0))
}

fn table_cell_height(document: &Document, cell: &TableCell, width: Pixels, window: &mut Window, cx: &mut App) -> Pixels {
  let padding = table_cell_padding();
  let content_width = (width - padding * 2.0).max(px(1.0));
  let mut y = padding;
  if cell.blocks.is_empty() {
    return px(28.0);
  }
  for block in &cell.blocks {
    match block {
      TableCellBlock::Paragraph(paragraph) => {
        let laid_out = layout_table_cell_paragraph(document, paragraph, 0, content_width, padding, y, window, cx);
        y = laid_out.bottom + px(2.0);
      },
      TableCellBlock::Table(table) => {
        let laid_out = layout_table_block(document, 0, table, content_width + document.theme.pageless_inset_x * 2.0, y, window, cx);
        y = laid_out.bottom + px(2.0);
      },
    }
  }
  (y + padding).max(px(28.0))
}

fn layout_table_cell_blocks(
  document: &Document,
  cell: &TableCell,
  bounds: Bounds<Pixels>,
  window: &mut Window,
  cx: &mut App,
) -> Vec<LaidOutBlock> {
  let padding = table_cell_padding();
  let content_width = (bounds.size.width - padding * 2.0).max(px(1.0));
  let mut y = bounds.origin.y + padding;
  let mut blocks = Vec::with_capacity(cell.blocks.len());
  for (ix, block) in cell.blocks.iter().enumerate() {
    match block {
      TableCellBlock::Paragraph(paragraph) => {
        let laid_out = layout_table_cell_paragraph(document, paragraph, ix, content_width, bounds.origin.x + padding, y, window, cx);
        y = laid_out.bottom + px(2.0);
        blocks.push(LaidOutBlock::Paragraph(laid_out));
      },
      TableCellBlock::Table(table) => {
        let laid_out = layout_table_block(document, 0, table, content_width + document.theme.pageless_inset_x * 2.0, y, window, cx);
        y = laid_out.bottom + px(2.0);
        blocks.push(LaidOutBlock::Table(laid_out));
      },
    }
  }
  blocks
}

fn layout_table_cell_paragraph(
  document: &Document,
  cell_paragraph: &TableCellParagraph,
  index: usize,
  width: Pixels,
  x: Pixels,
  y: Pixels,
  window: &mut Window,
  cx: &mut App,
) -> LaidOutParagraph {
  let paragraph = &cell_paragraph.paragraph;
  let p_format = paragraph_format(document, paragraph.style);
  let cache_key = paragraph_cache_key(document, paragraph);
  let lines = wrap_lines(document, paragraph, p_format.clone(), &cell_paragraph.text, width, window, cx);
  let mut laid_out_lines = Vec::with_capacity(lines.len());
  let mut line_y = y;
  for mut line in lines {
    line.origin.x = x
      + match p_format.align {
        ParagraphAlign::Left => px(0.0),
        ParagraphAlign::Center => (width - line.width).max(px(0.0)) / 2.0,
      };
    line.origin.y = line_y;
    line_y += line.line_height;
    laid_out_lines.push(line);
  }
  LaidOutParagraph {
    index,
    cache_key,
    len: cell_paragraph.text.len(),
    byte_range: 0..cell_paragraph.text.len(),
    top: y,
    bottom: line_y,
    lines: laid_out_lines,
    borders: Vec::new(),
  }
}

pub(super) fn table_cell_padding() -> Pixels {
  px(5.0)
}

pub(super) fn layout_paragraph_at(
  document: &Document,
  paragraph_ix: usize,
  width: Pixels,
  previous_bottom: Pixels,
  previous_paragraph: Option<&LaidOutParagraph>,
  window: &mut Window,
  cx: &mut App,
) -> (LaidOutParagraph, Pixels, Pixels, bool) {
  let paragraph = &document.paragraphs[paragraph_ix];
  let p_format = paragraph_format(document, paragraph.style);
  let y = previous_bottom + p_format.spacing_before;
  let cache_key = paragraph_cache_key(document, paragraph);

  if let Some(cached) = previous_paragraph.filter(|cached| cached.cache_key == cache_key) {
    let mut laid_out_paragraph = cached.clone();
    laid_out_paragraph.shift_y(y);
    let max_width = laid_out_paragraph
      .lines
      .iter()
      .map(|line| line.origin.x + line.width)
      .fold(width, Pixels::max);
    let next_y = laid_out_paragraph.bottom + p_format.spacing_after;
    return (laid_out_paragraph, next_y, max_width, true);
  }

  let pageless_left = document.theme.pageless_inset_x;
  let pageless_width = (width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  let border = p_format.border;
  let border_inset = border.map_or(px(0.0), |border| border.width + border.space_x);
  let content_left = pageless_left + border_inset;
  let content_top = border.map_or(px(0.0), |border| border.width + border.space_y);
  let content_width = (pageless_width - border_inset * 2.0).max(px(1.0));
  let paragraph_text = paragraph_text(document, paragraph_ix);
  let lines = wrap_lines(document, paragraph, p_format.clone(), &paragraph_text, content_width, window, cx);

  let mut max_width = width;
  let mut laid_out_lines = Vec::with_capacity(lines.len());
  let mut line_y = y + content_top;
  for mut line in lines {
    line.origin.x = content_left
      + match p_format.align {
        ParagraphAlign::Left => px(0.0),
        ParagraphAlign::Center => (content_width - line.width).max(px(0.0)) / 2.0,
      };
    line.origin.y = line_y;
    line_y += line.line_height;
    max_width = max_width.max(line.origin.x + line.width);
    laid_out_lines.push(line);
  }

  let bottom = line_y + content_top;
  let mut borders = Vec::new();
  if let Some(border) = border {
    push_box_rules(
      &mut borders,
      Bounds::new(point(pageless_left, y), size(pageless_width, bottom - y)),
      border.width,
      document.theme.default_text_color,
    );
  }

  (
    LaidOutParagraph {
      index: paragraph_ix,
      cache_key,
      len: paragraph_text.len(),
      byte_range: 0..paragraph_text.len(),
      top: y,
      bottom,
      lines: laid_out_lines,
      borders,
    },
    bottom + p_format.spacing_after,
    max_width,
    false,
  )
}

pub(super) const DEFAULT_PARAGRAPH_CHUNK_TARGET_LINES: usize = 48;

pub(super) struct ParagraphChunkBuildResult {
  pub(super) layout: LayoutState,
  pub(super) start_byte: usize,
  pub(super) next_byte: usize,
  pub(super) complete: bool,
}

pub(super) fn build_paragraph_chunk_layout_with_visibility(
  document: &Document,
  paragraph_ix: usize,
  width: Pixels,
  start_byte: usize,
  target_lines: usize,
  invisibility_mode: bool,
  paragraph_text_override: Option<&str>,
  wrap_break_ends_override: Option<&[usize]>,
  window: &mut Window,
  cx: &mut App,
) -> Option<ParagraphChunkBuildResult> {
  if invisibility_mode
    && document
      .paragraphs
      .get(paragraph_ix)
      .is_some_and(|paragraph| !paragraph_is_visible(paragraph))
  {
    return None;
  }
  let projected_document = invisibility_mode
    .then(|| invisibility_projected_document(document, paragraph_ix))
    .flatten();
  let layout_document = projected_document.as_ref().unwrap_or(document);
  let layout_paragraph_ix = if projected_document.is_some() { 0 } else { paragraph_ix };
  let is_first_document_paragraph = paragraph_ix == 0;
  let is_last_document_paragraph = paragraph_ix + 1 == document.paragraphs.len();
  let mut result = layout_paragraph_chunk_at(
    layout_document,
    layout_paragraph_ix,
    paragraph_ix,
    width,
    start_byte,
    target_lines,
    is_first_document_paragraph,
    is_last_document_paragraph,
    paragraph_text_override.filter(|_| projected_document.is_none()),
    wrap_break_ends_override.filter(|_| projected_document.is_none()),
    window,
    cx,
  )?;
  if projected_document.is_some()
    && let Some(paragraph) = result.layout.paragraphs.first_mut()
  {
    paragraph.index = paragraph_ix;
  }
  Some(result)
}

fn layout_paragraph_chunk_at(
  document: &Document,
  layout_paragraph_ix: usize,
  display_paragraph_ix: usize,
  width: Pixels,
  start_byte: usize,
  target_lines: usize,
  is_first_document_paragraph: bool,
  is_last_document_paragraph: bool,
  paragraph_text_override: Option<&str>,
  wrap_break_ends_override: Option<&[usize]>,
  window: &mut Window,
  cx: &mut App,
) -> Option<ParagraphChunkBuildResult> {
  let paragraph = document.paragraphs.get(layout_paragraph_ix)?;
  let paragraph_text = paragraph_text_override
    .map(Cow::Borrowed)
    .unwrap_or_else(|| Cow::Owned(paragraph_text(document, layout_paragraph_ix)));
  let paragraph_text = paragraph_text.as_ref();
  let len = paragraph_text.len();
  let start_byte = clamp_to_char_boundary(&paragraph_text, start_byte.min(len));
  let p_format = paragraph_format(document, paragraph.style);
  let cache_key = paragraph_cache_key(document, paragraph);
  let pageless_left = document.theme.pageless_inset_x;
  let pageless_width = (width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  let border = p_format.border;
  let border_inset = border.map_or(px(0.0), |border| border.width + border.space_x);
  let content_left = pageless_left + border_inset;
  let content_width = (pageless_width - border_inset * 2.0).max(px(1.0));
  let is_first_chunk = start_byte == 0;
  let chunk_target_lines = target_lines.max(1);
  let mut shape_cache = FragmentShapeCache::default();
  let (lines, next_byte, complete) = wrap_lines_limited(
    document,
    paragraph,
    p_format.clone(),
    &paragraph_text,
    start_byte,
    chunk_target_lines,
    content_width,
    wrap_break_ends_override,
    &mut shape_cache,
    window,
    cx,
  );

  let paragraph_top = if is_first_chunk {
    let mut top = p_format.spacing_before;
    if is_first_document_paragraph {
      top += document.theme.pageless_inset_top;
    }
    top
  } else {
    px(0.0)
  };
  let content_top = if is_first_chunk {
    border.map_or(px(0.0), |border| border.width + border.space_y)
  } else {
    px(0.0)
  };
  let mut max_width = width;
  let mut laid_out_lines = Vec::with_capacity(lines.len());
  let mut line_y = paragraph_top + content_top;
  for mut line in lines {
    line.origin.x = content_left
      + match p_format.align {
        ParagraphAlign::Left => px(0.0),
        ParagraphAlign::Center => (content_width - line.width).max(px(0.0)) / 2.0,
      };
    line.origin.y = line_y;
    line_y += line.line_height;
    max_width = max_width.max(line.origin.x + line.width);
    laid_out_lines.push(line);
  }

  let tail_space = if complete {
    let mut tail = border.map_or(px(0.0), |border| border.width + border.space_y) + p_format.spacing_after;
    if is_last_document_paragraph {
      tail += document.theme.pageless_inset_bottom;
    }
    tail
  } else {
    px(0.0)
  };
  let row_bottom = line_y + tail_space;
  let byte_range_end = if complete { len } else { next_byte.min(len) };
  let mut borders = Vec::new();
  if let Some(border) = border {
    push_chunk_box_rules(
      &mut borders,
      Bounds::new(
        point(pageless_left, paragraph_top),
        size(pageless_width, (row_bottom - paragraph_top).max(px(1.0))),
      ),
      border.width,
      document.theme.default_text_color,
      is_first_chunk,
      complete,
    );
  }

  let paragraph = LaidOutParagraph {
    index: display_paragraph_ix,
    cache_key,
    len,
    byte_range: start_byte..byte_range_end,
    top: paragraph_top,
    bottom: row_bottom,
    lines: laid_out_lines,
    borders,
  };
  let layout = LayoutState {
    blocks: vec![LaidOutBlock::Paragraph(paragraph.clone())],
    paragraph_to_block: vec![0],
    block_to_paragraph: vec![Some(display_paragraph_ix)],
    paragraphs: vec![paragraph],
    bounds: None,
    size: size(max_width.max(width), row_bottom.max(px(1.0))),
    width,
    snap_underline_rules_to_pixels: document.theme.snap_underline_rules_to_pixels,
  };
  Some(ParagraphChunkBuildResult {
    layout,
    start_byte,
    next_byte: byte_range_end,
    complete,
  })
}

fn clamp_to_char_boundary(text: &str, mut byte: usize) -> usize {
  byte = byte.min(text.len());
  while byte > 0 && !text.is_char_boundary(byte) {
    byte -= 1;
  }
  byte
}

fn ceil_char_boundary(text: &str, mut byte: usize) -> usize {
  byte = byte.min(text.len());
  while byte < text.len() && !text.is_char_boundary(byte) {
    byte += 1;
  }
  byte
}

fn push_chunk_box_rules(
  rects: &mut Vec<RunRect>,
  bounds: Bounds<Pixels>,
  thickness: Pixels,
  color: Hsla,
  include_top: bool,
  include_bottom: bool,
) {
  if include_top {
    rects.push(RunRect {
      bounds: Bounds::new(bounds.origin, size(bounds.size.width, thickness)),
      color,
      snap: RuleSnap::Horizontal,
    });
  }
  if include_bottom {
    rects.push(RunRect {
      bounds: Bounds::new(
        point(bounds.origin.x, bounds.origin.y + bounds.size.height - thickness),
        size(bounds.size.width, thickness),
      ),
      color,
      snap: RuleSnap::Horizontal,
    });
  }
  rects.push(RunRect {
    bounds: Bounds::new(bounds.origin, size(thickness, bounds.size.height)),
    color,
    snap: RuleSnap::Vertical,
  });
  rects.push(RunRect {
    bounds: Bounds::new(
      point(bounds.origin.x + bounds.size.width - thickness, bounds.origin.y),
      size(thickness, bounds.size.height),
    ),
    color,
    snap: RuleSnap::Vertical,
  });
}

pub(super) fn estimate_paragraph_item_height(document: &Document, paragraph_ix: usize, width: Pixels) -> Pixels {
  estimate_paragraph_item_height_with_visibility(document, paragraph_ix, width, false)
}

pub(super) fn estimate_paragraph_item_height_with_visibility(
  document: &Document,
  paragraph_ix: usize,
  width: Pixels,
  invisibility_mode: bool,
) -> Pixels {
  if invisibility_mode
    && document
      .paragraphs
      .get(paragraph_ix)
      .is_some_and(|paragraph| !paragraph_is_visible(paragraph))
  {
    return px(0.0);
  }
  let projected_document = invisibility_mode
    .then(|| invisibility_projected_document(document, paragraph_ix))
    .flatten();
  let estimate_document = projected_document.as_ref().unwrap_or(document);
  let estimate_paragraph_ix = if projected_document.is_some() { 0 } else { paragraph_ix };
  let paragraph = &estimate_document.paragraphs[estimate_paragraph_ix];
  let p_format = paragraph_format(estimate_document, paragraph.style);
  let border = p_format.border;
  let border_inset = border.map_or(px(0.0), |border| border.width + border.space_x);
  let content_top = border.map_or(px(0.0), |border| border.width + border.space_y);
  let content_width = (width - estimate_document.theme.pageless_inset_x * 2.0 - border_inset * 2.0).max(px(1.0));
  let avg_char_width = (p_format.font_size * 0.52).max(px(1.0));
  let chars_per_line = ((content_width / avg_char_width).floor() as usize).max(1);
  let text_len = paragraph_text_len(paragraph);
  let forced_line_count = paragraph_text(estimate_document, estimate_paragraph_ix)
    .matches(SOFT_LINE_BREAK)
    .count();
  let estimated_lines = (text_len / chars_per_line)
    .saturating_add(1)
    .saturating_add(forced_line_count)
    .max(1);
  let line_gap = p_format.font_size * estimate_document.theme.line_gap_fraction;
  let line_height = (p_format.font_size + line_gap) * p_format.line_spacing;
  let mut height = p_format.spacing_before + content_top + line_height * estimated_lines as f32 + content_top + p_format.spacing_after;
  if paragraph_ix == 0 {
    height += document.theme.pageless_inset_top;
  }
  if paragraph_ix + 1 == document.paragraphs.len() {
    height += document.theme.pageless_inset_bottom;
  }
  height.max(line_height)
}

pub(super) fn estimate_structural_block_item_height(document: &Document, block_ix: usize, width: Pixels) -> Pixels {
  let Some(block) = document.blocks.get(block_ix) else {
    return px(1.0);
  };
  match block {
    Block::Paragraph(_) => {
      let paragraph_ix = document
        .blocks
        .iter()
        .take(block_ix + 1)
        .filter(|block| matches!(block, Block::Paragraph(_)))
        .count()
        .saturating_sub(1);
      estimate_paragraph_item_height(document, paragraph_ix, width)
    },
    Block::Image(image) => image_placeholder_height(document, image, width) + document.theme.paragraph_after,
    Block::Equation(equation) => equation_placeholder_height(document, equation) + document.theme.paragraph_after,
    Block::Table(table) => table_placeholder_height(document, table, width) + document.theme.paragraph_after,
  }
}

fn table_placeholder_height(document: &Document, table: &TableBlock, width: Pixels) -> Pixels {
  let line_height = (document.theme.body_font_size * document.theme.line_spacing).max(px(16.0));
  let column_count = table
    .column_widths
    .len()
    .max(
      table
        .rows
        .iter()
        .map(|row| row.cells.len())
        .max()
        .unwrap_or(1),
    )
    .max(1);
  let content_width = (width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  let column_widths = resolved_table_column_widths(table, content_width, column_count);
  let height = table
    .rows
    .iter()
    .map(|row| {
      let mut column_ix = 0;
      row
        .cells
        .iter()
        .map(|cell| {
          let span = cell.col_span.max(1) as usize;
          let _column_width = spanned_column_width(&column_widths, column_ix, span);
          column_ix += span;
          let paragraph_count = cell
            .blocks
            .iter()
            .filter(|block| matches!(block, TableCellBlock::Paragraph(_)))
            .count()
            .max(1);
          line_height * paragraph_count as f32 + table_cell_padding() * 2.0
        })
        .fold(px(28.0), Pixels::max)
    })
    .fold(px(0.0), |height, row_height| height + row_height);
  if height > px(0.0) {
    return height;
  }
  let laid_out = layout_table_block_without_text(document, table, width, px(0.0));
  if laid_out.rows.is_empty() {
    return (document.theme.body_font_size * document.theme.line_spacing).max(px(24.0));
  }
  laid_out.bottom - laid_out.top
}

fn layout_table_block_without_text(document: &Document, table: &TableBlock, width: Pixels, y: Pixels) -> LaidOutTable {
  let table_left = document.theme.pageless_inset_x;
  let table_width = (width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  let column_count = table
    .column_widths
    .len()
    .max(
      table
        .rows
        .iter()
        .map(|row| row.cells.len())
        .max()
        .unwrap_or(1),
    )
    .max(1);
  let column_widths = resolved_table_column_widths(table, table_width, column_count);
  let mut row_top = y;
  let mut rows = Vec::with_capacity(table.rows.len());
  for row in &table.rows {
    let row_height = px(28.0);
    let mut x = table_left;
    let mut cells = Vec::with_capacity(row.cells.len());
    let mut column_ix = 0;
    for cell in &row.cells {
      let span = cell.col_span.max(1) as usize;
      let cell_width = spanned_column_width(&column_widths, column_ix, span);
      cells.push(LaidOutTableCell {
        bounds: Bounds::new(point(x, row_top), size(cell_width, row_height)),
        blocks: Vec::new(),
      });
      x += cell_width;
      column_ix += span;
    }
    rows.push(LaidOutTableRow {
      top: row_top,
      bottom: row_top + row_height,
      cells,
    });
    row_top += row_height;
  }
  LaidOutTable {
    block_ix: 0,
    top: y,
    bottom: row_top,
    bounds: Bounds::new(point(table_left, y), size(table_width, (row_top - y).max(px(1.0)))),
    rows,
  }
}

pub(super) fn wrap_lines(
  document: &Document,
  paragraph: &Paragraph,
  p_format: EffectiveParagraphFormat,
  text: &str,
  max_width: Pixels,
  window: &mut Window,
  cx: &mut App,
) -> Vec<LaidOutLine> {
  let mut shape_cache = FragmentShapeCache::default();
  if text.is_empty() {
    return vec![shape_line(document, paragraph, p_format, text, 0..0, &mut shape_cache, window, cx)];
  }
  if text.contains(SOFT_LINE_BREAK) {
    let mut lines = Vec::new();
    let mut segment_start = 0;
    for (break_ix, ch) in text.char_indices().filter(|(_, ch)| *ch == SOFT_LINE_BREAK) {
      push_wrapped_soft_segment(
        &mut lines,
        document,
        paragraph,
        p_format.clone(),
        text,
        segment_start..break_ix,
        max_width,
        &mut shape_cache,
        window,
        cx,
      );
      segment_start = break_ix + ch.len_utf8();
    }
    push_wrapped_soft_segment(
      &mut lines,
      document,
      paragraph,
      p_format,
      text,
      segment_start..text.len(),
      max_width,
      &mut shape_cache,
      window,
      cx,
    );
    return lines;
  }

  wrap_text_segment(
    document,
    paragraph,
    p_format,
    text,
    0..text.len(),
    max_width,
    &mut shape_cache,
    window,
    cx,
  )
}

fn wrap_lines_limited(
  document: &Document,
  paragraph: &Paragraph,
  p_format: EffectiveParagraphFormat,
  text: &str,
  start_byte: usize,
  max_lines: usize,
  max_width: Pixels,
  wrap_break_ends_override: Option<&[usize]>,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
  cx: &mut App,
) -> (Vec<LaidOutLine>, usize, bool) {
  let max_lines = max_lines.max(1);
  let start_byte = clamp_to_char_boundary(text, start_byte.min(text.len()));
  if text.is_empty() {
    return (
      vec![shape_line(document, paragraph, p_format, text, 0..0, shape_cache, window, cx)],
      0,
      true,
    );
  }
  if start_byte >= text.len() {
    return (Vec::new(), text.len(), true);
  }

  let mut lines = Vec::new();
  let mut segment_start = start_byte;
  while segment_start < text.len() && lines.len() < max_lines {
    let soft_break = text[segment_start..]
      .char_indices()
      .find_map(|(offset, ch)| (ch == SOFT_LINE_BREAK).then_some((segment_start + offset, ch.len_utf8())));
    let (segment_end, break_len, has_break) = soft_break
      .map(|(byte, len)| (byte, len, true))
      .unwrap_or((text.len(), 0, false));
    let remaining = max_lines - lines.len();
    if segment_start == segment_end {
      lines.push(shape_line(
        document,
        paragraph,
        p_format.clone(),
        "",
        segment_start..segment_start,
        shape_cache,
        window,
        cx,
      ));
      segment_start = segment_end + break_len;
      if lines.len() >= max_lines {
        return (lines, segment_start.min(text.len()), segment_start >= text.len());
      }
      continue;
    }

    let (mut segment_lines, next_byte, segment_complete) = wrap_text_segment_limited(
      document,
      paragraph,
      p_format.clone(),
      text,
      segment_start..segment_end,
      max_width,
      remaining,
      wrap_break_ends_override,
      shape_cache,
      window,
      cx,
    );
    lines.append(&mut segment_lines);
    if !segment_complete {
      return (lines, next_byte, false);
    }

    segment_start = if has_break { segment_end + break_len } else { segment_end };
    if !has_break {
      return (lines, text.len(), true);
    }
  }

  (lines, segment_start.min(text.len()), segment_start >= text.len())
}

fn wrap_text_segment_limited(
  document: &Document,
  paragraph: &Paragraph,
  p_format: EffectiveParagraphFormat,
  text: &str,
  segment: Range<usize>,
  max_width: Pixels,
  max_lines: usize,
  wrap_break_ends_override: Option<&[usize]>,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
  cx: &mut App,
) -> (Vec<LaidOutLine>, usize, bool) {
  if segment.is_empty() {
    return (
      vec![shape_line(document, paragraph, p_format, "", segment.clone(), shape_cache, window, cx)],
      segment.end,
      true,
    );
  }

  let max_lines = max_lines.max(1);
  let mut lines = Vec::new();
  let mut start = segment.start;
  let computed_break_ends;
  let break_ends = if let Some(break_ends) = wrap_break_ends_override {
    break_ends
  } else {
    computed_break_ends = wrap_break_ends(&text[segment.clone()])
      .into_iter()
      .map(|byte| segment.start + byte)
      .collect::<Vec<_>>();
    computed_break_ends.as_slice()
  };

  while start < segment.end {
    let break_cursor = first_break_after(&break_ends, start);
    let break_limit = first_break_after(&break_ends, segment.end);
    let last_break = if break_cursor < break_limit {
      if let Some(over_ix) = first_break_over_width(
        document,
        paragraph,
        &p_format,
        text,
        start,
        &break_ends,
        break_cursor..break_limit,
        max_width,
        shape_cache,
        window,
      ) {
        let line_end = if over_ix > break_cursor {
          break_ends[over_ix - 1]
        } else {
          first_overflow_line_end(
            document,
            paragraph,
            &p_format,
            text,
            start,
            break_ends[over_ix],
            max_width,
            shape_cache,
            window,
          )
        };
        lines.push(shape_line(
          document,
          paragraph,
          p_format.clone(),
          text[start..line_end].trim_end(),
          start..line_end,
          shape_cache,
          window,
          cx,
        ));
        start = skip_leading_whitespace(text, line_end);
        if lines.len() >= max_lines {
          return (lines, start.min(segment.end), start >= segment.end);
        }
        continue;
      }
      break_ends.get(break_limit - 1).copied()
    } else {
      None
    };

    if break_cursor == break_limit {
      let line_end = first_overflow_line_end(document, paragraph, &p_format, text, start, segment.end, max_width, shape_cache, window);
      if line_end < segment.end {
        lines.push(shape_line(
          document,
          paragraph,
          p_format.clone(),
          text[start..line_end].trim_end(),
          start..line_end,
          shape_cache,
          window,
          cx,
        ));
        start = skip_leading_whitespace(text, line_end);
        if lines.len() >= max_lines {
          return (lines, start.min(segment.end), start >= segment.end);
        }
        continue;
      }
      lines.push(shape_line(
        document,
        paragraph,
        p_format,
        &text[start..segment.end],
        start..segment.end,
        shape_cache,
        window,
        cx,
      ));
      return (lines, segment.end, true);
    }

    let Some(last_break) = last_break else {
      continue;
    };

    let remaining_width = measure_line_width(
      document,
      paragraph,
      &p_format,
      text,
      start..segment.end,
      segment.end - start,
      shape_cache,
      window,
    );
    if remaining_width <= max_width {
      lines.push(shape_line(
        document,
        paragraph,
        p_format,
        &text[start..segment.end],
        start..segment.end,
        shape_cache,
        window,
        cx,
      ));
      return (lines, segment.end, true);
    }

    let line_end = last_break;
    lines.push(shape_line(
      document,
      paragraph,
      p_format.clone(),
      text[start..line_end].trim_end(),
      start..line_end,
      shape_cache,
      window,
      cx,
    ));
    start = skip_leading_whitespace(text, line_end);
    if lines.len() >= max_lines {
      return (lines, start.min(segment.end), start >= segment.end);
    }
  }

  (lines, segment.end, true)
}

fn push_wrapped_soft_segment(
  lines: &mut Vec<LaidOutLine>,
  document: &Document,
  paragraph: &Paragraph,
  p_format: EffectiveParagraphFormat,
  text: &str,
  segment: Range<usize>,
  max_width: Pixels,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
  cx: &mut App,
) {
  if segment.is_empty() {
    lines.push(shape_line(document, paragraph, p_format, "", segment, shape_cache, window, cx));
  } else {
    lines.extend(wrap_text_segment(
      document,
      paragraph,
      p_format,
      text,
      segment,
      max_width,
      shape_cache,
      window,
      cx,
    ));
  }
}

fn wrap_text_segment(
  document: &Document,
  paragraph: &Paragraph,
  p_format: EffectiveParagraphFormat,
  text: &str,
  segment: Range<usize>,
  max_width: Pixels,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
  cx: &mut App,
) -> Vec<LaidOutLine> {
  if segment.is_empty() {
    return vec![shape_line(document, paragraph, p_format, "", segment, shape_cache, window, cx)];
  }

  let mut lines = Vec::new();
  let mut start = segment.start;
  let break_ends = wrap_break_ends(&text[segment.clone()])
    .into_iter()
    .map(|byte| segment.start + byte)
    .collect::<Vec<_>>();

  while start < segment.end {
    let break_cursor = first_break_after(&break_ends, start);
    let break_limit = first_break_after(&break_ends, segment.end);
    let last_break = if break_cursor < break_limit {
      if let Some(over_ix) = first_break_over_width(
        document,
        paragraph,
        &p_format,
        text,
        start,
        &break_ends,
        break_cursor..break_limit,
        max_width,
        shape_cache,
        window,
      ) {
        let line_end = if over_ix > break_cursor {
          break_ends[over_ix - 1]
        } else {
          first_overflow_line_end(
            document,
            paragraph,
            &p_format,
            text,
            start,
            break_ends[over_ix],
            max_width,
            shape_cache,
            window,
          )
        };
        lines.push(shape_line(
          document,
          paragraph,
          p_format.clone(),
          text[start..line_end].trim_end(),
          start..line_end,
          shape_cache,
          window,
          cx,
        ));
        start = skip_leading_whitespace(text, line_end);
        continue;
      }
      break_ends.get(break_limit - 1).copied()
    } else {
      None
    };

    if break_cursor == break_limit {
      let line_end = first_overflow_line_end(document, paragraph, &p_format, text, start, segment.end, max_width, shape_cache, window);
      if line_end < segment.end {
        lines.push(shape_line(
          document,
          paragraph,
          p_format.clone(),
          text[start..line_end].trim_end(),
          start..line_end,
          shape_cache,
          window,
          cx,
        ));
        start = skip_leading_whitespace(text, line_end);
        continue;
      }
      lines.push(shape_line(
        document,
        paragraph,
        p_format,
        &text[start..segment.end],
        start..segment.end,
        shape_cache,
        window,
        cx,
      ));
      break;
    }

    let Some(last_break) = last_break else {
      continue;
    };

    let remaining_width = measure_line_width(
      document,
      paragraph,
      &p_format,
      text,
      start..segment.end,
      segment.end - start,
      shape_cache,
      window,
    );
    if remaining_width <= max_width {
      lines.push(shape_line(
        document,
        paragraph,
        p_format,
        &text[start..segment.end],
        start..segment.end,
        shape_cache,
        window,
        cx,
      ));
      break;
    }

    let line_end = last_break;
    lines.push(shape_line(
      document,
      paragraph,
      p_format.clone(),
      text[start..line_end].trim_end(),
      start..line_end,
      shape_cache,
      window,
      cx,
    ));
    start = skip_leading_whitespace(text, line_end);
  }

  lines
}

fn first_break_after(break_ends: &[usize], byte: usize) -> usize {
  let mut low = 0usize;
  let mut high = break_ends.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if break_ends[mid] <= byte {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  low
}

#[allow(clippy::too_many_arguments)]
fn first_break_over_width(
  document: &Document,
  paragraph: &Paragraph,
  p_format: &EffectiveParagraphFormat,
  text: &str,
  start: usize,
  break_ends: &[usize],
  range: Range<usize>,
  max_width: Pixels,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
) -> Option<usize> {
  let mut low = range.start;
  let mut high = range.end;
  while low < high {
    let mid = low + (high - low) / 2;
    let break_at = break_ends[mid];
    let candidate_width = measure_line_width(
      document,
      paragraph,
      p_format,
      text,
      start..break_at,
      break_at - start,
      shape_cache,
      window,
    );
    if candidate_width > max_width {
      high = mid;
    } else {
      low = mid + 1;
    }
  }
  (low < range.end).then_some(low)
}

pub(super) fn wrap_break_ends(text: &str) -> Vec<usize> {
  text
    .char_indices()
    .filter_map(|(byte_ix, ch)| is_wrap_break(ch).then_some(byte_ix + ch.len_utf8()))
    .collect()
}

pub(super) fn is_wrap_break(ch: char) -> bool {
  ch.is_whitespace() || matches!(ch, '-' | '/' | ',' | ';' | ':')
}

pub(super) fn skip_leading_whitespace(text: &str, mut byte: usize) -> usize {
  while byte < text.len() && text[byte..].chars().next().is_some_and(char::is_whitespace) {
    byte += text[byte..].chars().next().unwrap().len_utf8();
  }
  byte
}

pub(super) fn first_overflow_line_end(
  document: &Document,
  paragraph: &Paragraph,
  p_format: &EffectiveParagraphFormat,
  text: &str,
  start: usize,
  limit: usize,
  max_width: Pixels,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
) -> usize {
  let chars: Vec<_> = text[start..limit]
    .char_indices()
    .map(|(relative_byte, ch)| {
      let byte_ix = start + relative_byte;
      (byte_ix, byte_ix + ch.len_utf8(), ch)
    })
    .collect();
  if chars.is_empty() {
    return limit;
  }

  let mut low = 0;
  let mut high = chars.len();
  while low < high {
    let mid = (low + high) / 2;
    let end = chars[mid].1;
    let width = measure_line_width(document, paragraph, p_format, text, start..end, end - start, shape_cache, window);
    if width > max_width {
      high = mid;
    } else {
      low = mid + 1;
    }
  }

  let Some((byte_ix, end, ch)) = chars.get(low).copied() else {
    return limit;
  };
  if is_wrap_break(ch) || byte_ix == start { end } else { byte_ix }
}

pub(super) fn measure_line_width(
  document: &Document,
  paragraph: &Paragraph,
  p_format: &EffectiveParagraphFormat,
  paragraph_text: &str,
  source_range: Range<usize>,
  rendered_len: usize,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
) -> Pixels {
  let mut width = px(0.0);
  let rendered_start = clamp_to_char_boundary(paragraph_text, source_range.start);
  let rendered_end = clamp_to_char_boundary(
    paragraph_text,
    source_range
      .start
      .saturating_add(rendered_len)
      .min(source_range.end)
      .min(paragraph_text.len()),
  )
  .max(rendered_start);
  let rendered_range = rendered_start..rendered_end;
  let rendered_text = &paragraph_text[rendered_range.clone()];
  let measure_key = LineMeasureCacheKey {
    start: rendered_range.start,
    end: rendered_range.end,
  };
  if let Some(width) = shape_cache.line_widths.get(&measure_key) {
    return *width;
  }
  for fragment in fragments_for_range(paragraph, &rendered_range, rendered_text) {
    let text = &rendered_text[fragment.line_range.clone()];
    if text.is_empty() {
      continue;
    }
    let format = run_format(document, p_format.clone(), fragment.styles);
    let shaped = shape_fragment_cached(window, text, format.clone(), fragment.source_start, fragment.styles, shape_cache);
    if format.border_width > px(0.0) {
      width += document.theme.box_padding_left;
    }
    width += shaped.width;
    if format.border_width > px(0.0) {
      width += document.theme.box_padding_right;
    }
  }
  shape_cache.line_widths.insert(measure_key, width);
  width
}

pub(super) fn shape_line(
  document: &Document,
  paragraph: &Paragraph,
  p_format: EffectiveParagraphFormat,
  line_text: &str,
  source_range: Range<usize>,
  shape_cache: &mut FragmentShapeCache,
  window: &mut Window,
  cx: &mut App,
) -> LaidOutLine {
  let fragments = fragments_for_range(paragraph, &source_range, line_text);
  let mut x = px(0.0);
  let mut segments = Vec::with_capacity(fragments.len().max(1));
  let mut ascent = px(0.0);
  let mut descent = px(0.0);

  for fragment in fragments {
    let text = &line_text[fragment.line_range.clone()];
    if text.is_empty() {
      continue;
    }
    let format = run_format(document, p_format.clone(), fragment.styles);
    let shaped = shape_fragment_cached(window, text, format.clone(), fragment.source_start, fragment.styles, shape_cache);
    let width = shaped.width;
    let box_margin_left = if format.border_width > px(0.0) {
      document.theme.box_padding_left
    } else {
      px(0.0)
    };
    let box_margin_right = if format.border_width > px(0.0) {
      document.theme.box_padding_right
    } else {
      px(0.0)
    };
    let segment_ascent = shaped.ascent;
    let segment_descent = shaped.descent;
    ascent = ascent.max(segment_ascent);
    descent = descent.max(segment_descent);
    x += box_margin_left;
    segments.push(LaidOutSegment {
      shaped,
      x,
      width,
      format: format.clone(),
      ascent: segment_ascent,
      descent: segment_descent,
      font_size: format.font_size,
      start_byte: fragment.source_start,
    });
    x += width + box_margin_right;
  }

  if segments.is_empty() {
    let format = run_format(document, p_format.clone(), RunStyles::default());
    let shaped = shape_fragment(window, "", format.clone());
    #[cfg(target_os = "linux")]
    let (segment_ascent, segment_descent) = {
      let (font_ascent, font_descent) = font_metrics_for_format(&format, cx);
      (shaped.ascent.max(font_ascent), shaped.descent.max(font_descent))
    };
    #[cfg(not(target_os = "linux"))]
    let (segment_ascent, segment_descent) = (shaped.ascent, shaped.descent);
    segments.push(LaidOutSegment {
      shaped,
      format: format.clone(),
      x: px(0.0),
      width: px(0.0),
      ascent: segment_ascent,
      descent: segment_descent,
      font_size: format.font_size,
      start_byte: source_range.start,
    });
  }

  ascent = segments
    .iter()
    .map(|segment| segment.ascent)
    .fold(px(0.0), Pixels::max);
  descent = segments
    .iter()
    .map(|segment| segment.descent)
    .fold(px(0.0), Pixels::max);

  let max_font_size = segments
    .iter()
    .map(|segment| segment.font_size)
    .fold(p_format.font_size, Pixels::max);
  let line_gap = max_font_size * document.theme.line_gap_fraction;
  let line_height = (ascent + descent + line_gap) * p_format.line_spacing;
  let mut line = LaidOutLine {
    origin: point(px(0.0), px(0.0)),
    line_height,
    ascent,
    descent,
    width: x,
    start_byte: source_range.start,
    end_byte: source_range.end,
    segments,
    rects: Vec::new(),
    underlines: Vec::new(),
    strikethroughs: Vec::new(),
  };
  line.rects = rects_for_line(document, &line);
  line.underlines = underlines_for_line(document, &line, cx);
  line.strikethroughs = strikethroughs_for_line(document, &line);
  line
}

#[derive(Default)]
pub(super) struct FragmentShapeCache {
  shapes: FxHashMap<FragmentShapeCacheKey, ShapedLine>,
  line_widths: FxHashMap<LineMeasureCacheKey, Pixels>,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub(super) struct FragmentShapeCacheKey {
  source_start: usize,
  len: usize,
  styles: RunStyles,
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
struct LineMeasureCacheKey {
  start: usize,
  end: usize,
}

pub(super) fn shape_fragment_cached(
  window: &mut Window,
  text: &str,
  format: EffectiveRunFormat,
  source_start: usize,
  styles: RunStyles,
  cache: &mut FragmentShapeCache,
) -> ShapedLine {
  let key = FragmentShapeCacheKey {
    source_start,
    len: text.len(),
    styles,
  };
  if let Some(shaped) = cache.shapes.get(&key) {
    return shaped.clone();
  }
  let shaped = shape_fragment(window, text, format);
  cache.shapes.insert(key, shaped.clone());
  shaped
}

pub(super) fn shape_fragment(window: &mut Window, text: &str, format: EffectiveRunFormat) -> ShapedLine {
  let mut run_font = font(format.font_family);
  run_font.weight = if format.bold { FontWeight::BOLD } else { FontWeight::NORMAL };
  run_font.style = if format.italic { FontStyle::Italic } else { FontStyle::Normal };
  let run = GpuiTextRun {
    len: text.len(),
    font: run_font,
    color: format.color,
    background_color: None,
    underline: None,
    strikethrough: None,
  };
  window
    .text_system()
    .shape_line(SharedString::new(text), format.font_size, &[run], None)
}

#[cfg(target_os = "linux")]
fn font_metrics_for_format(format: &EffectiveRunFormat, cx: &mut App) -> (Pixels, Pixels) {
  let mut run_font = font(format.font_family.clone());
  run_font.weight = if format.bold { FontWeight::BOLD } else { FontWeight::NORMAL };
  run_font.style = if format.italic { FontStyle::Italic } else { FontStyle::Normal };
  let font_id = cx.text_system().resolve_font(&run_font);
  (
    cx.text_system().ascent(font_id, format.font_size),
    cx.text_system().descent(font_id, format.font_size),
  )
}

#[derive(Clone)]
pub(super) struct VisualFragment {
  pub(super) styles: RunStyles,
  pub(super) line_range: Range<usize>,
  pub(super) source_start: usize,
}

pub(super) fn fragments_for_range(paragraph: &Paragraph, range: &Range<usize>, rendered_text: &str) -> Vec<VisualFragment> {
  let mut byte_offset = 0;
  let rendered_len = rendered_text.len();
  let mut fragments = Vec::with_capacity(paragraph.runs.len());
  for run in &paragraph.runs {
    let run_start = byte_offset;
    let run_end = byte_offset + run.len;
    byte_offset = run_end;
    let start = run_start.max(range.start);
    let end = run_end.min(range.end);
    if start >= end || rendered_len == 0 {
      continue;
    }
    let line_start = ceil_char_boundary(rendered_text, start.saturating_sub(range.start).min(rendered_len));
    let line_end = ceil_char_boundary(rendered_text, end.saturating_sub(range.start).min(rendered_len));
    if line_start >= line_end {
      continue;
    }
    fragments.push(VisualFragment {
      styles: run.styles,
      line_range: line_start..line_end,
      source_start: range.start + line_start,
    });
  }
  fragments
}

pub(super) fn rects_for_line(document: &Document, line: &LaidOutLine) -> Vec<RunRect> {
  let mut backgrounds = Vec::new();
  let mut borders = Vec::new();
  let text_top = line.baseline_y() - line.ascent;
  let text_bottom = line.baseline_y() + line.descent;
  let max_font_size = line
    .segments
    .iter()
    .map(|segment| segment.font_size)
    .fold(px(0.0), Pixels::max);
  let bottom_pad = max_font_size * document.theme.highlight_bottom_extra_fraction;
  // Highlights share the same theoretical top as Word's inline run border:
  // even when no border is painted, the highlight should look like it fills
  // the box that would be drawn for the run.
  let paint_top = text_top - document.theme.box_padding_top;
  let paint_height = (text_bottom + bottom_pad - paint_top).max(px(1.0));

  for segment in &line.segments {
    let highlight_pad_left = if segment.format.border_width > px(0.0) {
      document.theme.box_padding_left
    } else {
      document.theme.highlight_pad_x
    };
    let highlight_pad_right = if segment.format.border_width > px(0.0) {
      document.theme.box_padding_right
    } else {
      document.theme.highlight_pad_x
    };
    let paint_box = Bounds::new(
      point(segment.x - highlight_pad_left, paint_top),
      size((segment.width + highlight_pad_left + highlight_pad_right).max(px(1.0)), paint_height),
    );

    if let Some(background) = segment.format.highlight {
      backgrounds.push(RunRect {
        bounds: paint_box,
        color: background,
        snap: RuleSnap::None,
      });
    }
    if segment.format.border_width > px(0.0) {
      let box_bounds = Bounds::new(
        point(segment.x - document.theme.box_padding_left, text_top - document.theme.box_padding_top),
        size(
          (segment.width + document.theme.box_padding_left + document.theme.box_padding_right).max(px(1.0)),
          (text_bottom - text_top + document.theme.box_padding_top + document.theme.box_padding_bottom).max(px(1.0)),
        ),
      );
      push_merged_box(&mut borders, box_bounds);
    }
  }
  let border_color = document.theme.default_text_color;
  let border_thickness = document.theme.emphasis_border_paint_width;
  let borders = borders
    .into_iter()
    .flat_map(|bounds| box_rules(bounds, border_thickness, border_color))
    .collect::<Vec<_>>();
  // Word paints fills before border rules. Keeping all run borders after all
  // run highlights prevents a following highlighted run from hiding the right
  // edge of the previous boxed run.
  backgrounds.extend(borders);
  backgrounds
}

fn push_merged_box(boxes: &mut Vec<Bounds<Pixels>>, bounds: Bounds<Pixels>) {
  const EPSILON: f32 = 0.5;
  if let Some(last) = boxes.last_mut() {
    let same_band = (f32::from(last.origin.y) - f32::from(bounds.origin.y)).abs() <= EPSILON
      && (f32::from(last.size.height) - f32::from(bounds.size.height)).abs() <= EPSILON;
    let touching = f32::from(bounds.origin.x) <= f32::from(last.origin.x + last.size.width) + EPSILON;
    if same_band && touching {
      let right = (last.origin.x + last.size.width).max(bounds.origin.x + bounds.size.width);
      last.size.width = right - last.origin.x;
      return;
    }
  }
  boxes.push(bounds);
}

fn box_rules(bounds: Bounds<Pixels>, thickness: Pixels, color: Hsla) -> [RunRect; 4] {
  [
    RunRect {
      bounds: Bounds::new(bounds.origin, size(bounds.size.width, thickness)),
      color,
      snap: RuleSnap::Horizontal,
    },
    RunRect {
      bounds: Bounds::new(
        point(bounds.origin.x, bounds.origin.y + bounds.size.height - thickness),
        size(bounds.size.width, thickness),
      ),
      color,
      snap: RuleSnap::Horizontal,
    },
    RunRect {
      bounds: Bounds::new(bounds.origin, size(thickness, bounds.size.height)),
      color,
      snap: RuleSnap::Vertical,
    },
    RunRect {
      bounds: Bounds::new(
        point(bounds.origin.x + bounds.size.width - thickness, bounds.origin.y),
        size(thickness, bounds.size.height),
      ),
      color,
      snap: RuleSnap::Vertical,
    },
  ]
}

pub(super) fn push_box_rules(rects: &mut Vec<RunRect>, bounds: Bounds<Pixels>, thickness: Pixels, color: Hsla) {
  rects.push(RunRect {
    bounds: Bounds::new(bounds.origin, size(bounds.size.width, thickness)),
    color,
    snap: RuleSnap::Horizontal,
  });
  rects.push(RunRect {
    bounds: Bounds::new(
      point(bounds.origin.x, bounds.origin.y + bounds.size.height - thickness),
      size(bounds.size.width, thickness),
    ),
    color,
    snap: RuleSnap::Horizontal,
  });
  rects.push(RunRect {
    bounds: Bounds::new(bounds.origin, size(thickness, bounds.size.height)),
    color,
    snap: RuleSnap::Vertical,
  });
  rects.push(RunRect {
    bounds: Bounds::new(
      point(bounds.origin.x + bounds.size.width - thickness, bounds.origin.y),
      size(thickness, bounds.size.height),
    ),
    color,
    snap: RuleSnap::Vertical,
  });
}

pub(super) fn underlines_for_line(document: &Document, line: &LaidOutLine, cx: &mut App) -> Vec<Decoration> {
  let mut underlines = Vec::new();
  let baseline = line.baseline_y();
  for (segment_ix, segment) in line.segments.iter().enumerate() {
    match segment.format.underline {
      UnderlineKind::None => {},
      UnderlineKind::Single => {
        let (offset, thickness) = single_underline_metrics_for_segment(segment, document, cx);
        underlines.push(DecorationSource {
          segment_ix,
          x: segment.x,
          width: segment.width,
          y: baseline + offset,
          thickness,
          color: document.theme.default_text_color,
          boxed: segment.format.border_width > px(0.0),
        });
      },
      UnderlineKind::Double => {
        let (offset, thickness) = double_underline_metrics_for_segment(document);
        let y = baseline + offset;
        underlines.push(DecorationSource {
          segment_ix,
          x: segment.x,
          width: segment.width,
          y,
          thickness,
          color: document.theme.default_text_color,
          boxed: segment.format.border_width > px(0.0),
        });
        underlines.push(DecorationSource {
          segment_ix,
          x: segment.x,
          width: segment.width,
          y: y + thickness + document.theme.double_underline_gap,
          thickness,
          color: document.theme.default_text_color,
          boxed: segment.format.border_width > px(0.0),
        });
      },
    }
  }
  build_inline_decorations(underlines, document.theme.box_padding_left, document.theme.box_padding_right)
}

pub(super) fn strikethroughs_for_line(document: &Document, line: &LaidOutLine) -> Vec<Decoration> {
  let baseline = line.baseline_y();
  let decorations = line
    .segments
    .iter()
    .enumerate()
    .filter(|(_, segment)| segment.format.strikethrough)
    .map(|(segment_ix, segment)| {
      let thickness = document.theme.underline_rule_thickness.max(px(1.0));
      let y = baseline - segment.font_size * 0.30;
      DecorationSource {
        segment_ix,
        x: segment.x,
        width: segment.width,
        y,
        thickness,
        color: document.theme.default_text_color,
        boxed: segment.format.border_width > px(0.0),
      }
    })
    .collect();
  build_inline_decorations(decorations, document.theme.box_padding_left, document.theme.box_padding_right)
}

#[derive(Clone, Copy)]
pub(super) struct DecorationSource {
  pub(super) segment_ix: usize,
  pub(super) x: Pixels,
  pub(super) width: Pixels,
  pub(super) y: Pixels,
  pub(super) thickness: Pixels,
  pub(super) color: Hsla,
  pub(super) boxed: bool,
}

pub(super) fn build_inline_decorations(
  sources: Vec<DecorationSource>,
  boxed_bridge_left: Pixels,
  boxed_bridge_right: Pixels,
) -> Vec<Decoration> {
  let mut decorations = Vec::with_capacity(sources.len());
  for (source_ix, source) in sources.iter().enumerate() {
    let mut x = source.x;
    let mut width = source.width.max(px(1.0));
    if has_matching_previous_boxed_source(&sources, source_ix, source) {
      x -= boxed_bridge_left;
      width += boxed_bridge_left;
    }
    if has_matching_next_boxed_source(&sources, source_ix, source) {
      width += boxed_bridge_right;
    }
    decorations.push(Decoration {
      bounds: Bounds::new(point(x, source.y), size(width, source.thickness)),
      color: source.color,
    });
  }
  merge_inline_decorations(decorations)
}

fn has_matching_previous_boxed_source(sources: &[DecorationSource], source_ix: usize, source: &DecorationSource) -> bool {
  if !source.boxed || source.segment_ix == 0 {
    return false;
  }
  for candidate in sources[..source_ix].iter().rev() {
    if candidate.segment_ix + 1 < source.segment_ix {
      break;
    }
    if candidate.segment_ix + 1 == source.segment_ix && matching_boxed_decoration_source(source, candidate) {
      return true;
    }
  }
  false
}

fn has_matching_next_boxed_source(sources: &[DecorationSource], source_ix: usize, source: &DecorationSource) -> bool {
  if !source.boxed {
    return false;
  }
  for candidate in sources[source_ix + 1..].iter() {
    if candidate.segment_ix > source.segment_ix + 1 {
      break;
    }
    if candidate.segment_ix == source.segment_ix + 1 && matching_boxed_decoration_source(source, candidate) {
      return true;
    }
  }
  false
}

fn matching_boxed_decoration_source(a: &DecorationSource, b: &DecorationSource) -> bool {
  b.boxed
    && same_color(a.color, b.color)
    && (f32::from(a.y) - f32::from(b.y)).abs() <= 0.25
    && (f32::from(a.thickness) - f32::from(b.thickness)).abs() <= 0.25
}

pub(super) fn merge_inline_decorations(decorations: Vec<Decoration>) -> Vec<Decoration> {
  let mut merged: Vec<Decoration> = Vec::with_capacity(decorations.len());
  for decoration in decorations {
    push_merged_decoration(&mut merged, decoration);
  }
  merged
}

fn push_merged_decoration(decorations: &mut Vec<Decoration>, decoration: Decoration) {
  for existing in decorations.iter_mut().rev() {
    if !same_decoration_band(existing, &decoration) {
      continue;
    }
    const EPSILON: f32 = 0.75;
    let existing_left = f32::from(existing.bounds.origin.x);
    let existing_right = f32::from(existing.bounds.origin.x + existing.bounds.size.width);
    let decoration_left = f32::from(decoration.bounds.origin.x);
    let decoration_right = f32::from(decoration.bounds.origin.x + decoration.bounds.size.width);
    if decoration_left <= existing_right + EPSILON && decoration_right + EPSILON >= existing_left {
      let right = (existing.bounds.origin.x + existing.bounds.size.width).max(decoration.bounds.origin.x + decoration.bounds.size.width);
      existing.bounds.origin.x = existing.bounds.origin.x.min(decoration.bounds.origin.x);
      existing.bounds.size.width = right - existing.bounds.origin.x;
      return;
    }
    break;
  }
  decorations.push(decoration);
}

fn same_decoration_band(a: &Decoration, b: &Decoration) -> bool {
  const EPSILON: f32 = 0.25;
  same_color(a.color, b.color)
    && (f32::from(a.bounds.origin.y) - f32::from(b.bounds.origin.y)).abs() <= EPSILON
    && (f32::from(a.bounds.size.height) - f32::from(b.bounds.size.height)).abs() <= EPSILON
}

fn same_color(a: Hsla, b: Hsla) -> bool {
  a.h == b.h && a.s == b.s && a.l == b.l && a.a == b.a
}

pub(super) fn single_underline_metrics_for_segment(segment: &LaidOutSegment, document: &Document, cx: &mut App) -> (Pixels, Pixels) {
  // GPUI exposes glyph bounds in font coordinates. For Calibri, the
  // underscore bbox is below the baseline. The origin is the lower
  // edge of the glyph box on this metric path; Word positions an
  // underline at the top of the underscore glyph, so subtract the
  // glyph height from the baseline-to-origin distance.
  //
  // On Linux, GPUI's `typographic_bounds` is a stub returning
  // `origin = (0, 0)` with the advance box as the size (see gpui's
  // platform/linux/text_system.rs). That makes the formula collapse to 0
  // and paint the underline at the baseline, cutting through descenders.
  // So on Linux we skip the glyph-derived path entirely and use the
  // theme's Word-derived fallback constant.
  #[cfg(target_os = "linux")]
  let offset = {
    let _ = (segment, cx); // silence unused warnings on linux
    document.theme.underline_fallback_top_from_baseline
  };
  #[cfg(not(target_os = "linux"))]
  let offset = regular_underscore_bounds(segment, cx)
    .map(|bounds| (bounds.origin.y.abs() - bounds.size.height).max(px(0.0)))
    .unwrap_or(document.theme.underline_fallback_top_from_baseline);
  (offset, document.theme.underline_rule_thickness)
}

pub(super) fn double_underline_metrics_for_segment(document: &Document) -> (Pixels, Pixels) {
  (document.theme.double_underline_top_from_baseline, document.theme.underline_rule_thickness)
}

#[cfg(not(target_os = "linux"))]
pub(super) fn regular_underscore_bounds(segment: &LaidOutSegment, cx: &mut App) -> Option<Bounds<Pixels>> {
  let mut underline_font = font(segment.format.font_family.clone());
  // Word's underline metric follows the regular face's underscore metrics;
  // bold text remains bold, but the underline itself does not get bolded.
  underline_font.weight = FontWeight::NORMAL;
  underline_font.style = if segment.format.italic { FontStyle::Italic } else { FontStyle::Normal };
  let font_id = cx.text_system().resolve_font(&underline_font);
  cx.text_system()
    .typographic_bounds(font_id, segment.font_size, '_')
    .ok()
}

pub(super) fn first_paragraph_with_bottom_at_or_after(paragraphs: &[LaidOutParagraph], y: Pixels) -> usize {
  let mut low = 0;
  let mut high = paragraphs.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if paragraphs[mid].bottom < y {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  low
}

pub(super) fn first_paragraph_with_top_after(paragraphs: &[LaidOutParagraph], y: Pixels) -> usize {
  let mut low = 0;
  let mut high = paragraphs.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if paragraphs[mid].top <= y {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  low
}

pub(super) fn first_line_with_bottom_at_or_after(lines: &[LaidOutLine], y: Pixels) -> usize {
  let mut low = 0;
  let mut high = lines.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if lines[mid].origin.y + lines[mid].line_height < y {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  low
}

pub(super) fn caret_bounds(layout: &LayoutState, offset: DocumentOffset, origin: Point<Pixels>) -> Option<Bounds<Pixels>> {
  // Use locate_line so the caret is drawn on the same visual line that
  // Up/Down/Home/End navigate from — in particular the wrap-seam bias
  // (byte at end of line k == start of line k+1 → paint on k+1) must be
  // identical in both paths, otherwise the caret appears at the wrong
  // position after the cursor reaches a soft-wrap boundary.
  let (p_ix, l_ix) = locate_line(layout, offset)?;
  let line = layout.paragraphs[p_ix].lines.get(l_ix)?;
  let x = x_for_byte(line, offset.byte);
  Some(Bounds::new(origin + line.origin + point(x, px(0.0)), size(px(1.0), line.line_height)))
}

pub(super) fn caret_bounds_in_paragraph(paragraph: &LaidOutParagraph, byte: usize, origin: Point<Pixels>) -> Option<Bounds<Pixels>> {
  let line_ix = line_ix_for_byte(paragraph, byte)?;
  let line = paragraph.lines.get(line_ix)?;
  let x = x_for_byte(line, byte);
  Some(Bounds::new(origin + line.origin + point(x, px(0.0)), size(px(1.0), line.line_height)))
}

pub(super) fn x_for_byte(line: &LaidOutLine, byte: usize) -> Pixels {
  for segment in &line.segments {
    let segment_end = segment.start_byte + segment.shaped.len();
    if byte <= segment_end {
      return segment.x
        + segment
          .shaped
          .x_for_index(byte.saturating_sub(segment.start_byte));
    }
  }
  line.width
}

fn line_ix_for_byte(paragraph: &LaidOutParagraph, byte: usize) -> Option<usize> {
  let mut low = 0;
  let mut high = paragraph.lines.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if paragraph.lines[mid].end_byte < byte {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  if let Some(line) = paragraph.lines.get(low)
    && byte >= line.start_byte
    && byte <= line.end_byte
  {
    if byte == line.end_byte && low + 1 < paragraph.lines.len() && paragraph.lines[low + 1].start_byte == byte {
      return Some(low + 1);
    }
    return Some(low);
  }
  paragraph.lines.len().checked_sub(1)
}

// Locate the `LaidOutLine` containing the given offset. Returns
// `(paragraph_layout_index, line_index)`. When the byte sits exactly on a
// soft-wrap seam (== end_byte of line k and start_byte of line k+1), we bias
// to the next line — matching Word's "caret-at-start-of-next-line"
// convention. This is exactly the disambiguation called out in the plan.
pub(super) fn locate_line(layout: &LayoutState, off: DocumentOffset) -> Option<(usize, usize)> {
  let p_ix = paragraph_layout_index_for_offset(layout, off)?;
  let para = &layout.paragraphs[p_ix];
  let mut low = 0;
  let mut high = para.lines.len();
  while low < high {
    let mid = low + (high - low) / 2;
    if para.lines[mid].end_byte < off.byte {
      low = mid + 1;
    } else {
      high = mid;
    }
  }
  if let Some(line) = para.lines.get(low)
    && off.byte >= line.start_byte
    && off.byte <= line.end_byte
  {
    // Bias to next line at exact wrap seam.
    if off.byte == line.end_byte && low + 1 < para.lines.len() && para.lines[low + 1].start_byte == off.byte {
      return Some((p_ix, low + 1));
    }
    return Some((p_ix, low));
  }
  // Fall back to last line of the paragraph (e.g. byte == para.len after a
  // soft-wrapped trailing whitespace strip).
  let last = para.lines.len().checked_sub(1)?;
  Some((p_ix, last))
}

pub(super) fn paragraph_layout(layout: &LayoutState, paragraph: usize) -> Option<&LaidOutParagraph> {
  let layout_ix = paragraph_layout_index(layout, paragraph)?;
  layout.paragraphs.get(layout_ix)
}

pub(super) fn paragraph_layout_index_for_offset(layout: &LayoutState, offset: DocumentOffset) -> Option<usize> {
  layout
    .paragraphs
    .iter()
    .enumerate()
    .find(|(_, paragraph)| paragraph.index == offset.paragraph && paragraph.contains_byte(offset.byte))
    .map(|(ix, _)| ix)
    .or_else(|| paragraph_layout_index(layout, offset.paragraph))
}

pub(super) fn paragraph_layout_index(layout: &LayoutState, paragraph: usize) -> Option<usize> {
  let _ = layout.paragraph_block_ix(paragraph);
  if layout
    .paragraphs
    .get(paragraph)
    .is_some_and(|layout_paragraph| layout_paragraph.index == paragraph)
  {
    Some(paragraph)
  } else {
    let mut low = 0;
    let mut high = layout.paragraphs.len();
    while low < high {
      let mid = low + (high - low) / 2;
      if layout.paragraphs[mid].index < paragraph {
        low = mid + 1;
      } else {
        high = mid;
      }
    }
    layout
      .paragraphs
      .get(low)
      .is_some_and(|layout_paragraph| layout_paragraph.index == paragraph)
      .then_some(low)
  }
}

// Step to the previous visual line. If we're already on the first line of a
// paragraph, jump to the last line of the previous paragraph.
pub(super) fn find_line_above(layout: &LayoutState, p_ix: usize, line_ix: usize) -> Option<(usize, usize)> {
  if line_ix > 0 {
    return Some((p_ix, line_ix - 1));
  }
  if p_ix == 0 {
    return None;
  }
  let prev = p_ix - 1;
  let last = layout.paragraphs[prev].lines.len().checked_sub(1)?;
  Some((prev, last))
}

pub(super) fn find_line_below(layout: &LayoutState, p_ix: usize, line_ix: usize) -> Option<(usize, usize)> {
  if line_ix + 1 < layout.paragraphs[p_ix].lines.len() {
    return Some((p_ix, line_ix + 1));
  }
  if p_ix + 1 < layout.paragraphs.len() {
    return Some((p_ix + 1, 0));
  }
  None
}
