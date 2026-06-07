
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
    inner: Box<Self>,
  },
}

// -- Tiny unit-conversion helpers -----------------------------------------

/// Convert Word/PDF points to GPUI logical pixels (96 dpi).
#[hotpath::measure]
fn pt(value: f32) -> Pixels {
  px(value * 96.0 / 72.0)
}
