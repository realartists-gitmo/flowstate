#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Paragraph {
  pub style: ParagraphStyle,
  pub byte_range: Range<usize>,
  pub runs: Vec<TextRun>,
  pub version: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ParagraphId(pub u128);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct BlockId(pub u128);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SectionId(pub u128);

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct DocumentIds {
  pub document_id: u128,
  pub paragraph_ids: Vec<ParagraphId>,
  pub block_ids: Vec<BlockId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum SectionKind {
  Custom(u8),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DocumentSection {
  pub id: SectionId,
  pub parent_id: Option<SectionId>,
  pub kind: SectionKind,
  pub heading_paragraph: Option<ParagraphId>,
  pub start_paragraph: ParagraphId,
  pub end_paragraph_exclusive: Option<ParagraphId>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum ParagraphStyle {
  Normal,
  Custom(u8),
}

impl ParagraphStyle {
  #[must_use]
  pub const fn slot(self) -> u64 {
    match self {
      Self::Normal => 5,
      Self::Custom(slot) => 128 + slot as u64,
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct TextRun {
  pub len: usize,
  pub styles: RunStyles,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentRunInput {
  pub text: String,
  pub styles: RunStyles,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DocumentParagraphInput {
  pub style: ParagraphStyle,
  pub runs: Vec<DocumentRunInput>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DocumentSpan {
  pub start_paragraph: usize,
  pub paragraphs: Vec<Paragraph>,
  pub text: String,
}

/// Input-shape used by document builders (demo data, clipboard fragments).
/// Carries explicit run text instead of byte offsets so the higher-level
/// helpers can splice in arbitrary content.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputRun {
  pub text: String,
  pub styles: RunStyles,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputParagraph {
  pub style: ParagraphStyle,
  pub runs: Vec<InputRun>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputAsset {
  pub id: AssetId,
  pub mime_type: String,
  pub original_name: Option<String>,
  pub content_hash: u64,
  pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum InputBlock {
  Paragraph(InputParagraph),
  Image(InputImageBlock),
  Equation(InputEquationBlock),
  Table(InputTableBlock),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputImageBlock {
  pub asset_id: AssetId,
  pub alt_text: String,
  pub caption: Option<InputParagraph>,
  pub sizing: InputImageSizing,
  pub alignment: InputBlockAlignment,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum InputImageSizing {
  Intrinsic,
  FitWidth,
  Fixed { width_px: u32, height_px: Option<u32> },
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum InputBlockAlignment {
  Left,
  Center,
  Right,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputEquationBlock {
  pub source: String,
  pub syntax: InputEquationSyntax,
  pub display: InputEquationDisplay,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum InputEquationSyntax {
  Latex,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum InputEquationDisplay {
  Display,
  InlineLikeParagraph,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputTableBlock {
  pub rows: Vec<InputTableRow>,
  pub column_widths: Vec<InputTableColumnWidth>,
  pub style: InputTableStyle,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputTableRow {
  pub cells: Vec<InputTableCell>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputTableCell {
  pub blocks: Vec<InputTableCellBlock>,
  pub row_span: u16,
  pub col_span: u16,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum InputTableCellBlock {
  Paragraph(InputParagraph),
  Table(InputTableBlock),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub enum InputTableColumnWidth {
  Auto,
  FixedPx(u32),
  Fraction(u32),
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct InputTableStyle {
  pub header_row: bool,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct RunStyles {
  pub semantic: RunSemanticStyle,
  pub direct_underline: bool,
  pub strikethrough: bool,
  pub highlight: Option<HighlightStyle>,
}

impl RunStyles {
  #[must_use]
  pub fn is_plain(self) -> bool {
    self.semantic == RunSemanticStyle::Plain && !self.direct_underline && !self.strikethrough && self.highlight.is_none()
  }
}
