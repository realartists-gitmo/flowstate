use std::{collections::HashMap, ops::Range, sync::Arc};

use crop::Rope;
use gpui::{Hsla, Pixels, SharedString, black, px, rgb};
use serde::{Deserialize, Serialize};

// `paragraph_widths` and `paragraph_width` are free helpers that still live in
// the parent module. `ParagraphOffsetIndex`'s methods invoke them.
use super::{paragraph_width, paragraph_widths};

pub(super) const SOFT_LINE_BREAK: char = '\u{2028}';
pub(super) const SOFT_LINE_BREAK_STR: &str = "\u{2028}";

// -- Clipboard fragment ---------------------------------------------------

/// Internal clipboard fragment used to round-trip rich text via the system
/// clipboard. The `format` field acts as a magic string so we can distinguish
/// our payloads from anything else stored in the clipboard's metadata slot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct RichClipboardFragment {
  pub(super) format: String,
  #[serde(default)]
  pub(super) paragraphs: Vec<InputParagraph>,
  #[serde(default)]
  pub(super) blocks: Vec<InputBlock>,
  #[serde(default)]
  pub(super) assets: Vec<InputAsset>,
}

// -- Document and paragraphs ---------------------------------------------

#[derive(Clone, Debug)]
pub struct Document {
  pub text: Rope,
  pub paragraphs: Arc<Vec<Paragraph>>,
  pub blocks: Arc<Vec<Block>>,
  pub assets: AssetStore,
  // Auxiliary Fenwick-tree index over per-paragraph byte widths. Kept in sync
  // with `paragraphs` by the edit helpers in `edit_ops`. Not part of the
  // public API.
  pub(super) offset_index: ParagraphOffsetIndex,
  pub theme: DocumentTheme,
}

pub(super) fn paragraphs_mut(document: &mut Document) -> &mut Vec<Paragraph> {
  Arc::make_mut(&mut document.paragraphs)
}

pub(super) fn paragraph_blocks_from_paragraphs(paragraphs: &[Paragraph]) -> Vec<Block> {
  paragraphs.iter().cloned().map(Block::Paragraph).collect()
}

pub(super) fn block_ix_for_paragraph(document: &Document, target_paragraph_ix: usize) -> Option<usize> {
  let mut paragraph_ix = 0;
  for (block_ix, block) in document.blocks.iter().enumerate() {
    if matches!(block, Block::Paragraph(_)) {
      if paragraph_ix == target_paragraph_ix {
        return Some(block_ix);
      }
      paragraph_ix += 1;
    }
  }
  None
}

pub(super) fn update_paragraph_block(document: &mut Document, paragraph_ix: usize) {
  let Some(paragraph) = document.paragraphs.get(paragraph_ix).cloned() else {
    return;
  };
  if let Some(block_ix) = block_ix_for_paragraph(document, paragraph_ix)
    && let Some(block) = Arc::make_mut(&mut document.blocks).get_mut(block_ix)
  {
    *block = Block::Paragraph(paragraph);
  }
}

pub(super) fn replace_paragraph_blocks(document: &mut Document, start_paragraph: usize, old_count: usize, replacements: &[Paragraph]) {
  let mut paragraph_ix = 0;
  let mut output = Vec::with_capacity(document.blocks.len() + replacements.len());
  let mut inserted_replacements = false;

  for block in document.blocks.iter() {
    match block {
      Block::Paragraph(_) if paragraph_ix >= start_paragraph && paragraph_ix < start_paragraph + old_count => {
        if !inserted_replacements {
          output.extend(replacements.iter().cloned().map(Block::Paragraph));
          inserted_replacements = true;
        }
        paragraph_ix += 1;
      },
      Block::Paragraph(paragraph) => {
        if !inserted_replacements && paragraph_ix >= start_paragraph {
          output.extend(replacements.iter().cloned().map(Block::Paragraph));
          inserted_replacements = true;
        }
        output.push(Block::Paragraph(paragraph.clone()));
        paragraph_ix += 1;
      },
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => output.push(block.clone()),
    }
  }

  if !inserted_replacements {
    output.extend(replacements.iter().cloned().map(Block::Paragraph));
  }
  if output.is_empty()
    && let Some(paragraph) = document.paragraphs.first()
  {
    output.push(Block::Paragraph(paragraph.clone()));
  }

  document.blocks = Arc::new(output);
}

/// Fenwick-tree (binary indexed tree) over the byte widths of each paragraph,
/// plus the raw widths. Lets us compute the absolute byte offset of any
/// paragraph in O(log N) and update it incrementally as the document is
/// edited.
#[derive(Clone, Debug)]
pub(super) struct ParagraphOffsetIndex {
  pub(super) widths: Vec<usize>,
  pub(super) tree: Vec<usize>,
}

impl ParagraphOffsetIndex {
  pub(super) fn new(paragraphs: &[Paragraph]) -> Self {
    let mut index = Self {
      widths: paragraph_widths(paragraphs),
      tree: vec![0; paragraphs.len() + 1],
    };
    for ix in 0..index.widths.len() {
      index.add(ix, index.widths[ix] as isize);
    }
    index
  }

  pub(super) fn rebuild(&mut self, paragraphs: &[Paragraph]) {
    *self = Self::new(paragraphs);
  }

  pub(super) fn paragraph_start(&self, paragraph_ix: usize) -> usize {
    self.prefix_sum(paragraph_ix)
  }

  pub(super) fn update_paragraph_width(&mut self, paragraph_ix: usize, paragraphs: &[Paragraph]) {
    let Some(width) = paragraph_width(paragraphs, paragraph_ix) else {
      return;
    };
    let old_width = self.widths[paragraph_ix];
    if old_width == width {
      return;
    }
    self.widths[paragraph_ix] = width;
    self.add(paragraph_ix, width as isize - old_width as isize);
  }

  fn add(&mut self, paragraph_ix: usize, delta: isize) {
    if delta == 0 {
      return;
    }
    let mut ix = paragraph_ix + 1;
    while ix < self.tree.len() {
      self.tree[ix] = self.tree[ix].saturating_add_signed(delta);
      ix += ix & (!ix + 1);
    }
  }

  fn prefix_sum(&self, paragraph_count: usize) -> usize {
    let mut ix = paragraph_count.min(self.widths.len());
    let mut sum = 0;
    while ix > 0 {
      sum += self.tree[ix];
      ix &= ix - 1;
    }
    sum
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Paragraph {
  pub style: ParagraphStyle,
  pub byte_range: Range<usize>,
  pub runs: Vec<TextRun>,
  pub version: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ParagraphStyle {
  Pocket,
  Hat,
  Block,
  Tag,
  Analytic,
  Normal,
  Undertag,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct TextRun {
  pub len: usize,
  pub styles: RunStyles,
}

/// Input-shape used by document builders (demo data, clipboard fragments).
/// Carries explicit run text instead of byte offsets so the higher-level
/// helpers can splice in arbitrary content.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct InputRun {
  pub(super) text: String,
  pub(super) styles: RunStyles,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct InputParagraph {
  pub(super) style: ParagraphStyle,
  pub(super) runs: Vec<InputRun>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct InputAsset {
  pub(super) id: AssetId,
  pub(super) mime_type: String,
  pub(super) original_name: Option<String>,
  pub(super) content_hash: u64,
  pub(super) bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum InputBlock {
  Paragraph(InputParagraph),
  Image(InputImageBlock),
  Equation(InputEquationBlock),
  Table(InputTableBlock),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct InputImageBlock {
  pub(super) asset_id: AssetId,
  pub(super) alt_text: String,
  pub(super) caption: Option<InputParagraph>,
  pub(super) sizing: InputImageSizing,
  pub(super) alignment: InputBlockAlignment,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum InputImageSizing {
  Intrinsic,
  FitWidth,
  Fixed { width_px: u32, height_px: Option<u32> },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(super) enum InputBlockAlignment {
  Left,
  Center,
  Right,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct InputEquationBlock {
  pub(super) source: String,
  pub(super) syntax: InputEquationSyntax,
  pub(super) display: InputEquationDisplay,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(super) enum InputEquationSyntax {
  Latex,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub(super) enum InputEquationDisplay {
  Display,
  InlineLikeParagraph,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct InputTableBlock {
  pub(super) rows: Vec<InputTableRow>,
  pub(super) column_widths: Vec<InputTableColumnWidth>,
  pub(super) style: InputTableStyle,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct InputTableRow {
  pub(super) cells: Vec<InputTableCell>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct InputTableCell {
  pub(super) blocks: Vec<InputTableCellBlock>,
  pub(super) row_span: u16,
  pub(super) col_span: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum InputTableCellBlock {
  Paragraph(InputParagraph),
  Table(InputTableBlock),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) enum InputTableColumnWidth {
  Auto,
  FixedPx(u32),
  Fraction(u32),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub(super) struct InputTableStyle {
  pub(super) header_row: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct RunStyles {
  pub semantic: RunSemanticStyle,
  pub direct_underline: bool,
  pub highlight: Option<HighlightStyle>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
  Paragraph(Paragraph),
  Image(ImageBlock),
  Equation(EquationBlock),
  Table(TableBlock),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssetStore {
  pub assets: HashMap<AssetId, AssetRecord>,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AssetId(pub u128);

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AssetRecord {
  pub id: AssetId,
  pub mime_type: SharedString,
  pub original_name: Option<SharedString>,
  pub content_hash: u64,
  pub bytes: Arc<Vec<u8>>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageBlock {
  pub asset_id: AssetId,
  pub alt_text: SharedString,
  pub caption: Option<Paragraph>,
  pub sizing: ImageSizing,
  pub alignment: BlockAlignment,
  pub version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageSizing {
  Intrinsic,
  FitWidth,
  Fixed { width_px: u32, height_px: Option<u32> },
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum BlockAlignment {
  #[default]
  Left,
  Center,
  Right,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EquationBlock {
  pub source: SharedString,
  pub syntax: EquationSyntax,
  pub display: EquationDisplay,
  pub version: u64,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EquationSyntax {
  #[default]
  Latex,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum EquationDisplay {
  #[default]
  Display,
  InlineLikeParagraph,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableBlock {
  pub rows: Vec<TableRow>,
  pub column_widths: Vec<TableColumnWidth>,
  pub style: TableStyle,
  pub version: u64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRow {
  pub cells: Vec<TableCell>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableCell {
  pub blocks: Vec<TableCellBlock>,
  pub row_span: u16,
  pub col_span: u16,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableCellBlock {
  Paragraph(TableCellParagraph),
  Table(TableBlock),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableCellParagraph {
  pub paragraph: Paragraph,
  pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableColumnWidth {
  Auto,
  FixedPx(u32),
  Fraction(u32),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableStyle {
  pub header_row: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum RunSemanticStyle {
  #[default]
  Plain,
  Cite,
  Emphasis,
  Underline,
  Condensed,
  Ultracondensed,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum HighlightStyle {
  Spoken,
  Insert,
  Alternative,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RunStyle {
  Plain,
  Cite,
  Underline,
  Emphasis,
  Condensed,
  Ultracondensed,
  HighlightSpoken,
  HighlightInsert,
  HighlightAlternative,
}

impl From<RunStyle> for RunStyles {
  fn from(style: RunStyle) -> Self {
    let mut styles = RunStyles::default();
    styles.apply(style);
    styles
  }
}

impl RunStyles {
  pub fn apply(&mut self, style: RunStyle) {
    match style {
      RunStyle::Plain => self.semantic = RunSemanticStyle::Plain,
      RunStyle::Cite => self.semantic = RunSemanticStyle::Cite,
      RunStyle::Underline => self.semantic = RunSemanticStyle::Underline,
      RunStyle::Emphasis => self.semantic = RunSemanticStyle::Emphasis,
      RunStyle::Condensed => self.semantic = RunSemanticStyle::Condensed,
      RunStyle::Ultracondensed => self.semantic = RunSemanticStyle::Ultracondensed,
      RunStyle::HighlightSpoken => self.highlight = Some(HighlightStyle::Spoken),
      RunStyle::HighlightInsert => self.highlight = Some(HighlightStyle::Insert),
      RunStyle::HighlightAlternative => self.highlight = Some(HighlightStyle::Alternative),
    }
  }

  pub fn with(mut self, style: RunStyle) -> Self {
    self.apply(style);
    self
  }

  pub fn with_direct_underline(mut self) -> Self {
    self.direct_underline = true;
    self
  }
}

// -- Theme ----------------------------------------------------------------

#[derive(Clone, Debug)]
pub struct DocumentTheme {
  pub default_font_family: SharedString,
  pub default_text_color: Hsla,
  pub pageless_inset_x: Pixels,
  pub pageless_inset_top: Pixels,
  pub pageless_inset_bottom: Pixels,
  pub body_font_size: Pixels,
  pub cite_font_size: Pixels,
  pub condensed_font_size: Pixels,
  pub ultracondensed_font_size: Pixels,
  pub pocket_font_size: Pixels,
  pub hat_font_size: Pixels,
  pub block_font_size: Pixels,
  pub tag_font_size: Pixels,
  pub undertag_font_size: Pixels,
  pub line_spacing: f32,
  pub line_gap_fraction: f32,
  pub paragraph_after: Pixels,
  pub pocket_before: Pixels,
  pub hat_before: Pixels,
  pub block_before: Pixels,
  pub tag_before: Pixels,
  pub pocket_border_width: Pixels,
  pub pocket_border_space_x: Pixels,
  pub pocket_border_space_y: Pixels,
  pub emphasis_border_width: Pixels,
  pub emphasis_border_paint_width: Pixels,
  pub box_padding_left: Pixels,
  pub box_padding_right: Pixels,
  pub box_padding_top: Pixels,
  pub box_padding_bottom: Pixels,
  pub highlight_pad_x: Pixels,
  pub highlight_top_extra_fraction: f32,
  pub highlight_bottom_extra_fraction: f32,
  pub underline_fallback_top_from_baseline: Pixels,
  pub underline_rule_thickness: Pixels,
  pub snap_underline_rules_to_pixels: bool,
  pub double_underline_top_from_baseline: Pixels,
  pub double_underline_gap: Pixels,
  pub highlight_spoken: Hsla,
  pub highlight_insert: Hsla,
  pub highlight_alternative: Hsla,
  pub pocket_color: Hsla,
  pub hat_color: Hsla,
  pub block_color: Hsla,
  pub tag_color: Hsla,
  pub analytic_color: Hsla,
  pub undertag_color: Hsla,
  pub cite_color: Hsla,
  pub underline_color: Hsla,
  pub emphasis_color: Hsla,
  pub condensed_color: Hsla,
  pub ultracondensed_color: Hsla,
  pub normal_bold: bool,
  pub normal_italic: bool,
  pub normal_underline: ThemeUnderline,
  pub pocket_bold: bool,
  pub pocket_italic: bool,
  pub pocket_underline: ThemeUnderline,
  pub hat_bold: bool,
  pub hat_italic: bool,
  pub hat_underline: ThemeUnderline,
  pub block_bold: bool,
  pub block_italic: bool,
  pub block_underline: ThemeUnderline,
  pub tag_bold: bool,
  pub tag_italic: bool,
  pub tag_underline: ThemeUnderline,
  pub analytic_bold: bool,
  pub analytic_italic: bool,
  pub analytic_underline: ThemeUnderline,
  pub undertag_bold: bool,
  pub undertag_italic: bool,
  pub undertag_underline: ThemeUnderline,
  pub cite_bold: bool,
  pub cite_italic: bool,
  pub cite_underline: ThemeUnderline,
  pub underline_bold: bool,
  pub underline_italic: bool,
  pub underline_underline: ThemeUnderline,
  pub emphasis_bold: bool,
  pub emphasis_italic: bool,
  pub emphasis_underline: ThemeUnderline,
  pub condensed_bold: bool,
  pub condensed_italic: bool,
  pub condensed_underline: ThemeUnderline,
  pub ultracondensed_bold: bool,
  pub ultracondensed_italic: bool,
  pub ultracondensed_underline: ThemeUnderline,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ThemeUnderline {
  #[default]
  None,
  Single,
  Double,
}

impl Default for DocumentTheme {
  fn default() -> Self {
    Self {
      default_font_family: "Carlito".into(),
      default_text_color: black(),
      // Word page margins are 1in = 96px at 96dpi. Pageless mode should
      // not use full page margins, but a proportional inset keeps content
      // from sitting on the viewport edge.
      pageless_inset_x: px(24.0),
      pageless_inset_top: px(16.0),
      pageless_inset_bottom: px(24.0),
      body_font_size: pt(11.0),
      cite_font_size: pt(13.0),
      condensed_font_size: pt(8.0),
      ultracondensed_font_size: pt(3.0),
      pocket_font_size: pt(26.0),
      hat_font_size: pt(22.0),
      block_font_size: pt(16.0),
      tag_font_size: pt(13.0),
      undertag_font_size: pt(12.0),
      line_spacing: 259.0 / 240.0,
      // GPUI exposes shaped ascent/descent but not Word/DirectWrite's
      // full line gap here. Add a Calibri-like internal leading term so
      // Word's 1.08 multiple is applied to a Word-like line box.
      line_gap_fraction: 0.18,
      paragraph_after: pt(8.0),
      pocket_before: pt(12.0),
      hat_before: pt(2.0),
      block_before: pt(2.0),
      tag_before: pt(2.0),
      pocket_border_width: border_eighth_points(24.0),
      pocket_border_space_x: pt(4.0),
      pocket_border_space_y: pt(1.0),
      emphasis_border_width: border_eighth_points(8.0),
      // DOCX stores this border as 1pt, but Word's display renderer
      // paints inline text borders as a screen hairline. Feed the snapper
      // a sub-pixel logical width so it resolves to one device pixel
      // instead of rounding up to a heavier two-pixel rule on scaled
      // displays.
      emphasis_border_paint_width: px(0.5),
      // Word run borders report zero DOCX spacing in our fixture, but
      // measured paint geometry shows a stable hidden inset around ink.
      // Keep this box-only; highlights continue using the highlight band.
      box_padding_left: pt(0.96),
      box_padding_right: pt(1.01),
      box_padding_top: pt(1.47),
      box_padding_bottom: pt(1.09),
      // These paint values come from layout-engine-handoff, whose PDF
      // measurements are in points. Keep the values in Word/PDF points,
      // then convert to GPUI logical px with pt().
      highlight_pad_x: pt(0.0),
      // Word highlights are paint rectangles, not ink boxes. The third
      // measurement pass has censored body-size rows because the analyzer
      // clipped at 12pt, but uncensored larger-size rows converge around
      // a 0.20-0.24em top expansion. Use that general rule so highlights
      // do not climb too far above the line.
      highlight_top_extra_fraction: 0.22,
      highlight_bottom_extra_fraction: 0.092,
      underline_fallback_top_from_baseline: pt(1.246),
      // GPUI paints to the screen in logical pixels. A PDF 0.25pt
      // hairline becomes subpixel-thin at 96dpi, so use a Word-like
      // one-pixel screen rule while keeping metric-based y placement.
      underline_rule_thickness: px(1.0),
      snap_underline_rules_to_pixels: true,
      double_underline_top_from_baseline: pt(17.79 - 16.5),
      double_underline_gap: pt(1.20),
      highlight_spoken: rgb(0x00ff00).into(),
      highlight_insert: rgb(0xd9d9d9).into(),
      highlight_alternative: rgb(0x00ffff).into(),
      pocket_color: black(),
      hat_color: black(),
      block_color: black(),
      tag_color: black(),
      analytic_color: rgb(0x1f3864).into(),
      undertag_color: rgb(0x385623).into(),
      cite_color: black(),
      underline_color: black(),
      emphasis_color: black(),
      condensed_color: black(),
      ultracondensed_color: black(),
      normal_bold: false,
      normal_italic: false,
      normal_underline: ThemeUnderline::None,
      pocket_bold: true,
      pocket_italic: false,
      pocket_underline: ThemeUnderline::None,
      hat_bold: true,
      hat_italic: false,
      hat_underline: ThemeUnderline::Double,
      block_bold: true,
      block_italic: false,
      block_underline: ThemeUnderline::Single,
      tag_bold: true,
      tag_italic: false,
      tag_underline: ThemeUnderline::None,
      analytic_bold: true,
      analytic_italic: false,
      analytic_underline: ThemeUnderline::None,
      undertag_bold: false,
      undertag_italic: true,
      undertag_underline: ThemeUnderline::None,
      cite_bold: true,
      cite_italic: false,
      cite_underline: ThemeUnderline::None,
      underline_bold: false,
      underline_italic: false,
      underline_underline: ThemeUnderline::Single,
      emphasis_bold: true,
      emphasis_italic: false,
      emphasis_underline: ThemeUnderline::Single,
      condensed_bold: false,
      condensed_italic: false,
      condensed_underline: ThemeUnderline::None,
      ultracondensed_bold: false,
      ultracondensed_italic: false,
      ultracondensed_underline: ThemeUnderline::None,
    }
  }
}

// -- Document offset ------------------------------------------------------

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Ord, PartialOrd)]
pub struct DocumentOffset {
  pub paragraph: usize,
  pub byte: usize,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ObjectAffinity {
  #[default]
  Before,
  After,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum DocumentPosition {
  Text {
    block_ix: usize,
    byte: usize,
  },
  Object {
    block_ix: usize,
    affinity: ObjectAffinity,
  },
  TableCell {
    table_block_ix: usize,
    row_ix: usize,
    cell_ix: usize,
    inner: Box<DocumentPosition>,
  },
}

// -- Tiny unit-conversion helpers -----------------------------------------

/// Convert Word/PDF points to GPUI logical pixels (96 dpi).
fn pt(value: f32) -> Pixels {
  px(value * 96.0 / 72.0)
}

/// Convert a DOCX border `w:sz` value (in eighths of a point) to logical px.
fn border_eighth_points(value: f32) -> Pixels {
  pt(value / 8.0)
}
