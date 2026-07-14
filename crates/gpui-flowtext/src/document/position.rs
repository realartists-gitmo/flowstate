
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

/// §16 selection affinity: which side of a position a selection endpoint
/// "sticks" to.
///
/// This is the genuine, stored intent behind a caret/endpoint — it is **not**
/// re-derived from selection direction. It records *why* a side was chosen so
/// the caret behaves correctly across concurrent inserts at the same position,
/// undo/redo restoration, object/paragraph boundaries, and selection extension:
///
/// * [`SelectionAffinity::Before`] — the endpoint belongs to the content
///   *before* the offset (e.g. the caret arrived by moving Left, or sits at the
///   trailing edge of the preceding glyph). Stable cursors anchor to that
///   preceding glyph when one exists.
/// * [`SelectionAffinity::After`] — the endpoint belongs to the content *after*
///   the offset (e.g. the caret arrived by moving Right, or sits at the leading
///   edge of the following glyph). Stable cursors anchor to that following glyph
///   when one exists.
/// * [`SelectionAffinity::Neutral`] — no strong side was expressed (fresh edit
///   result, mouse placement, programmatic selection). Maps to a middle cursor
///   side.
///
/// The collaboration runtime maps these onto Loro stable cursor boundaries when
/// encoding presence/undo cursors instead of guessing from selection direction.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum SelectionAffinity {
  Before,
  After,
  #[default]
  Neutral,
}

/// §16 visual gravity: which visual line a soft-wrap-seam offset renders on.
///
/// At a soft wrap, one byte offset is shared by the end of the upper visual line
/// and the start of the lower visual line. Affinity alone cannot disambiguate
/// this purely visual choice, so gravity is tracked separately and **consumed**
/// by caret layout (see `caret_bounds`/`locate_line`):
///
/// * [`VisualGravity::Upstream`] — render at the trailing edge of the visual
///   line that *ends* at the offset (end of the upper line). Produced by
///   "move/extend to line end".
/// * [`VisualGravity::Downstream`] — render at the leading edge of the visual
///   line that *starts* at the offset (start of the lower line). Produced by
///   "move/extend to line start".
/// * [`VisualGravity::Neutral`] — editor default. Preserves the historical
///   "caret-at-start-of-next-line" wrap-seam bias (i.e. behaves like
///   `Downstream` at a seam), used for arrow/word/vertical motion, fresh edits,
///   mouse placement, and remote carets.
///
/// Bidi/RTL reordering is **not** modeled here; gravity currently disambiguates
/// only soft-wrap seams in left-to-right text. RTL caret placement remains a
/// defined-but-simplified behavior (treated as `Neutral`).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum VisualGravity {
  Upstream,
  Downstream,
  #[default]
  Neutral,
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
