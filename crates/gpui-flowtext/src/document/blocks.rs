
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Block {
  Paragraph(Paragraph),
  Image(ImageBlock),
  Equation(EquationBlock),
  Table(TableBlock),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AssetStore {
  pub assets: FxHashMap<AssetId, AssetRecord>,
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

pub const IMAGE_LOADING_PLACEHOLDER_WIDTH_PX: f32 = 240.0;
pub const IMAGE_LOADING_PLACEHOLDER_HEIGHT_PX: f32 = 160.0;

impl AssetRecord {
  #[must_use]
  pub fn is_loading_placeholder(&self) -> bool {
    self.bytes.is_empty()
  }
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
