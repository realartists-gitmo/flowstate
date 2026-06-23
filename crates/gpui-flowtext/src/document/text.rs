#[derive(Clone, Debug, PartialEq, Eq)]
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

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DocumentSection {
  pub id: SectionId,
  pub parent_id: Option<SectionId>,
  pub kind: SectionKind,
  pub heading_paragraph: Option<ParagraphId>,
  pub start_paragraph: ParagraphId,
  pub end_paragraph_exclusive: Option<ParagraphId>,
  /// §11 page-structure payload for this section, when known.
  ///
  /// The heading-outline computation ([`document_sections`]) cannot derive page
  /// structure from paragraph styles, so it leaves this `None`. The canonical
  /// values live in Loro and are populated by the `flowstate-document`
  /// projector. `#[serde(default)]` keeps older cached projections (which had no
  /// such field) deserializable.
  #[serde(default)]
  pub page: Option<SectionPageAttrs>,
}

/// US Letter width in twips (8.5in x 1440). Mirrors the `flowstate-document`
/// Loro encoding so projection mapping is a trivial field-by-field copy (§11).
const DEFAULT_PAGE_WIDTH_TWIPS: i64 = 12_240;
/// US Letter height in twips (11in x 1440).
const DEFAULT_PAGE_HEIGHT_TWIPS: i64 = 15_840;
/// One-inch margin in twips.
const DEFAULT_MARGIN_TWIPS: i64 = 1_440;

/// §11 page-structure attributes carried by a [`DocumentSection`].
///
/// This is the gpui-flowtext-native projection mirror of
/// `flowstate_document::loro_schema::SectionPageAttrs`. gpui-flowtext must not
/// depend on `flowstate-document` (that crate depends on this one), so the type
/// is defined here and the projector maps Loro values onto it field-for-field.
/// All units/semantics match the canonical Loro encoding: lengths are in twips
/// (1/1440 inch), `columns` is a count, and header/footer flow ids reference
/// independent text flows.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionPageAttrs {
  pub page_size: SectionPageSize,
  pub margins: SectionMargins,
  pub columns: i64,
  pub orientation: SectionOrientation,
  pub page_numbering: SectionPageNumbering,
  pub header_flow_id: Option<String>,
  pub footer_flow_id: Option<String>,
}

impl Default for SectionPageAttrs {
  fn default() -> Self {
    Self {
      page_size: SectionPageSize::default(),
      margins: SectionMargins::default(),
      columns: 1,
      orientation: SectionOrientation::Portrait,
      page_numbering: SectionPageNumbering::default(),
      header_flow_id: None,
      footer_flow_id: None,
    }
  }
}

/// §11 page size in twips. Defaults to US Letter.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionPageSize {
  pub width_twips: i64,
  pub height_twips: i64,
}

impl Default for SectionPageSize {
  fn default() -> Self {
    Self {
      width_twips: DEFAULT_PAGE_WIDTH_TWIPS,
      height_twips: DEFAULT_PAGE_HEIGHT_TWIPS,
    }
  }
}

/// §11 section margins in twips. Defaults to one-inch margins.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionMargins {
  pub top_twips: i64,
  pub right_twips: i64,
  pub bottom_twips: i64,
  pub left_twips: i64,
}

impl Default for SectionMargins {
  fn default() -> Self {
    Self {
      top_twips: DEFAULT_MARGIN_TWIPS,
      right_twips: DEFAULT_MARGIN_TWIPS,
      bottom_twips: DEFAULT_MARGIN_TWIPS,
      left_twips: DEFAULT_MARGIN_TWIPS,
    }
  }
}

/// §11 section page orientation.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SectionOrientation {
  #[default]
  Portrait,
  Landscape,
}

/// §11 page-numbering descriptor for a section. Defaults to no numbering
/// starting at 1.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SectionPageNumbering {
  pub format: PageNumberFormat,
  pub start: i64,
}

impl Default for SectionPageNumbering {
  fn default() -> Self {
    Self {
      format: PageNumberFormat::None,
      start: 1,
    }
  }
}

/// §11 page-number rendering format for a section.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum PageNumberFormat {
  #[default]
  None,
  Decimal,
  LowerRoman,
  UpperRoman,
  LowerAlpha,
  UpperAlpha,
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DocumentSpan {
  pub start_paragraph: usize,
  pub paragraphs: Vec<Paragraph>,
  pub text: String,
}

/// Input-shape used by document builders (demo data, clipboard fragments).
/// Carries explicit run text instead of byte offsets so the higher-level
/// helpers can splice in arbitrary content.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputRun {
  pub text: String,
  pub styles: RunStyles,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum InputBlock {
  Paragraph(InputParagraph),
  Image(InputImageBlock),
  Equation(InputEquationBlock),
  Table(InputTableBlock),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputImageBlock {
  pub asset_id: AssetId,
  pub alt_text: String,
  pub caption: Option<InputParagraph>,
  pub sizing: InputImageSizing,
  pub alignment: InputBlockAlignment,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum InputImageSizing {
  Intrinsic,
  FitWidth,
  Fixed { width_px: u32, height_px: Option<u32> },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum InputBlockAlignment {
  Left,
  Center,
  Right,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputEquationBlock {
  pub source: String,
  pub syntax: InputEquationSyntax,
  pub display: InputEquationDisplay,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum InputEquationSyntax {
  Latex,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum InputEquationDisplay {
  Display,
  InlineLikeParagraph,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputTableBlock {
  pub rows: Vec<InputTableRow>,
  pub column_widths: Vec<InputTableColumnWidth>,
  pub style: InputTableStyle,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputTableRow {
  pub cells: Vec<InputTableCell>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct InputTableCell {
  pub blocks: Vec<InputTableCellBlock>,
  pub row_span: u16,
  pub col_span: u16,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum InputTableCellBlock {
  Paragraph(InputParagraph),
  Table(InputTableBlock),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum InputTableColumnWidth {
  Auto,
  FixedPx(u32),
  Fraction(u32),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
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
