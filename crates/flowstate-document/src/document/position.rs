
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
#[hotpath::measure]
fn pt(value: f32) -> Pixels {
  px(value * 96.0 / 72.0)
}

/// Convert a DOCX border `w:sz` value (in eighths of a point) to logical px.
#[hotpath::measure]
fn border_eighth_points(value: f32) -> Pixels {
  pt(value / 8.0)
}
