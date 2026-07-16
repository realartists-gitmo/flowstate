
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

#[derive(Clone, Debug)]
pub struct AssetRecord {
  pub id: AssetId,
  pub mime_type: SharedString,
  pub original_name: Option<SharedString>,
  pub content_hash: u64,
  pub bytes: Arc<Vec<u8>>,
  /// B-S2: intrinsic pixel dimensions, carried from the CRDT asset map (or
  /// sniffed once at intake). They were stored in Loro all along but
  /// discarded on load, so layout re-sniffed the byte header on the hot path.
  pub dimensions: Option<(u32, u32)>,
  /// B-S2: lazily-built render handle shared across clones — constructing
  /// `Image::from_bytes` cloned the full byte buffer on EVERY paint of every
  /// visible image. Derived data: excluded from equality.
  pub render_image: Arc<std::sync::OnceLock<Arc<gpui::Image>>>,
}

impl PartialEq for AssetRecord {
  fn eq(&self, other: &Self) -> bool {
    self.id == other.id
      && self.mime_type == other.mime_type
      && self.original_name == other.original_name
      && self.content_hash == other.content_hash
      && self.bytes == other.bytes
      && self.dimensions == other.dimensions
  }
}

impl Eq for AssetRecord {}

pub const IMAGE_LOADING_PLACEHOLDER_WIDTH_PX: f32 = 240.0;
pub const IMAGE_LOADING_PLACEHOLDER_HEIGHT_PX: f32 = 160.0;

impl AssetRecord {
  /// B-S2: the shared gpui image for painting — built once per asset, not
  /// once per frame.
  #[must_use]
  pub fn render_image(&self, format: gpui::ImageFormat) -> Arc<gpui::Image> {
    self
      .render_image
      .get_or_init(|| Arc::new(gpui::Image::from_bytes(format, self.bytes.as_ref().clone())))
      .clone()
  }

  #[must_use]
  pub fn stable_content_hash(bytes: &[u8]) -> u64 {
    let digest = blake3::hash(bytes);
    u64::from_le_bytes(
      digest.as_bytes()[..8]
        .try_into()
        .expect("BLAKE3 digest always contains at least eight bytes"),
    )
  }

  #[must_use]
  pub fn is_loading_placeholder(&self) -> bool {
    self.bytes.is_empty()
  }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImageBlock {
  pub asset_id: AssetId,
  pub alt_text: SharedString,
  pub sizing: ImageSizing,
  pub alignment: BlockAlignment,
  /// §A11.9: a genuinely-LINKED image's external target URL (`a:blip r:link` /
  /// VML `r:href` resolving to a `TargetMode="External"` relationship). Such an
  /// image has no embedded media part — `asset_id` is derived from the URL
  /// bytes and no [`AssetRecord`] carries bytes for it. `None` for embedded
  /// images (the overwhelmingly common case).
  pub external_url: Option<SharedString>,
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
  /// Ordered columns, each with its durable [`ColumnId`] and width (§P2b).
  /// Replaces the id-less `column_widths` list; read a width as
  /// `columns[i].width`.
  pub columns: Vec<TableColumn>,
  pub style: TableStyle,
  pub version: u64,
}

/// A table column carrying its durable identity and rendered width (§P2b).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableColumn {
  pub id: ColumnId,
  pub width: TableColumnWidth,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRow {
  pub id: RowId,
  pub cells: Vec<TableCell>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableCell {
  pub id: CellId,
  pub row_id: RowId,
  pub column_id: ColumnId,
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
