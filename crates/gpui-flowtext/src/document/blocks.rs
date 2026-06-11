
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
pub enum RichBlockIdentity {
  Image { caption: Option<ParagraphId> },
  Equation { source: ParagraphId },
  Table(TableIdentity),
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TableIdentity {
  pub rows: Vec<TableRowIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableRowIdentity {
  pub id: BlockId,
  pub cells: Vec<TableCellIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TableCellIdentity {
  pub id: BlockId,
  pub blocks: Vec<TableCellBlockIdentity>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum TableCellBlockIdentity {
  Paragraph(ParagraphId),
  Table { id: BlockId, identity: TableIdentity },
}

impl TableBlock {
  #[must_use]
  pub fn row_record_id(&self, block_id: u128, row_ix: usize) -> u128 {
    stable_table_record_id(block_id, 0x1f, row_ix as u128, self.rows.len() as u128)
  }

  #[must_use]
  pub fn cell_record_id(&self, block_id: u128, row_ix: usize, cell_ix: usize) -> u128 {
    let row = self.rows.get(row_ix).map(|row| row.cells.len() as u128).unwrap_or_default();
    stable_table_record_id(block_id, 0x2f, ((row_ix as u128) << 32) | cell_ix as u128, row)
  }

  #[must_use]
  pub fn row_order_ids(&self, block_id: u128) -> Vec<u128> {
    (0..self.rows.len()).map(|row_ix| self.row_record_id(block_id, row_ix)).collect()
  }

  #[must_use]
  pub fn cell_order_ids(&self, block_id: u128, row_ix: usize) -> Vec<u128> {
    self
      .rows
      .get(row_ix)
      .map(|row| (0..row.cells.len()).map(|cell_ix| self.cell_record_id(block_id, row_ix, cell_ix)).collect())
      .unwrap_or_default()
  }
}

fn stable_table_record_id(block_id: u128, salt: u128, a: u128, b: u128) -> u128 {
  let mut state = block_id ^ salt.rotate_left(17);
  state ^= a.wrapping_mul(0x9e37_79b9_7f4a_7c15);
  state = state.rotate_left(29) ^ b.wrapping_mul(0xbf58_476d_1ce4_e5b9);
  state ^ (state >> 64)
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn table_record_ids_are_stable_and_distinct() {
    let table = TableBlock {
      rows: vec![TableRow { cells: vec![TableCell { blocks: vec![], row_span: 1, col_span: 1 }] }],
      column_widths: vec![TableColumnWidth::Fraction(1)],
      style: TableStyle { header_row: false },
      version: 0,
    };
    let row_id_a = table.row_record_id(42, 0);
    let row_id_b = table.row_record_id(42, 0);
    let cell_id_a = table.cell_record_id(42, 0, 0);
    let cell_id_b = table.cell_record_id(42, 0, 0);
    assert_eq!(row_id_a, row_id_b);
    assert_eq!(cell_id_a, cell_id_b);
    assert_ne!(row_id_a, cell_id_a);
  }

  #[test]
  fn table_order_ids_follow_structure_deterministically() {
    let table = TableBlock {
      rows: vec![
        TableRow {
          cells: vec![
            TableCell { blocks: vec![], row_span: 1, col_span: 1 },
            TableCell { blocks: vec![], row_span: 1, col_span: 1 },
          ],
        },
        TableRow { cells: vec![TableCell { blocks: vec![], row_span: 1, col_span: 1 }] },
      ],
      column_widths: vec![TableColumnWidth::Fraction(1), TableColumnWidth::Fraction(1)],
      style: TableStyle { header_row: false },
      version: 0,
    };
    assert_eq!(table.row_order_ids(7), table.row_order_ids(7));
    assert_eq!(table.cell_order_ids(7, 0).len(), 2);
    assert_eq!(table.cell_order_ids(7, 1).len(), 1);
  }
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
impl AssetStore {
  #[must_use]
  pub fn contains(&self, asset_id: AssetId) -> bool {
    self.assets.contains_key(&asset_id)
  }
}

impl ImageBlock {
  #[must_use]
  pub fn has_valid_asset(&self, assets: &AssetStore) -> bool {
    assets.contains(self.asset_id)
  }
}
