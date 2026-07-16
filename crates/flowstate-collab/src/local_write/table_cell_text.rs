//! B-S4: table-cell TEXT execution — translate the editor's cell edits into
//! MINIMAL Loro ops on the cell's flow text, so concurrent same-cell edits
//! merge char-level at the CRDT. This retires the whole-cell `ReplaceCell`
//! rewrite (LWW: one peer's typing vanished) for the typing path.
//!
//! Addressing law (v1): positions are cell-local POSITIONAL (paragraph index
//! within the cell + byte offset), pinned by a hash of the cell's canonical
//! flow string ([`gpui_flowtext::table_cell_flow_string`]). A mismatch means
//! the cell changed since the editor snapshot — the op REJECTS
//! ([`WriteRejected::StaleCellText`]), never mis-splices, and the editor
//! re-resolves against the fresh projection and retries. Durable per-cell
//! paragraph identity + CRDT cursors (the flow crate's `cell_text.rs` law)
//! are the B-S5 follow-up. Cells containing NESTED TABLES refuse — positional
//! addressing cannot see object anchors — and stay on the whole-cell path.

use flowstate_document::{BlockId, ParagraphStyle, RunStyles, WriteRejected, table_cell_text_hash};
use gpui_flowtext::{TableCellTextIntent, TableCellTextOp};
use loro::{LoroDoc, LoroText};

use crate::crdt_runtime::{CrdtRuntime, mark_run_styles, paragraph_style_value};

/// A cell-text op with POSITIONS RESOLVED to absolute unicode offsets in the
/// cell's flow text. Valid for exactly the text state whose hash matched.
#[derive(Debug)]
pub(crate) enum ResolvedCellTextOp {
  Insert {
    pos: usize,
    text: String,
    style_override: Option<RunStyles>,
  },
  Delete {
    pos: usize,
    len: usize,
  },
  Split {
    pos: usize,
    style: ParagraphStyle,
  },
  Join {
    boundary: usize,
  },
  Marks {
    pos: usize,
    len: usize,
    styles: RunStyles,
  },
  ParagraphStyle {
    boundary: usize,
    style: ParagraphStyle,
  },
}

#[derive(Debug)]
pub(crate) struct ResolvedCellText {
  pub table: BlockId,
  pub table_ix: usize,
  /// The cell's flow id in `FLOWS_BY_ID` — re-fetched at execute time.
  pub flow_id: String,
  pub op: ResolvedCellTextOp,
}

/// A flow's text container by flow id (undo capture/replay reuse).
pub(crate) fn flow_text_by_id(doc: &LoroDoc, flow_id: &str) -> Option<LoroText> {
  let root = flowstate_document::loro_schema::root_map(doc);
  let flows = crate::crdt_runtime::child_map(&root, flowstate_document::FLOWS_BY_ID)?;
  let flow = crate::crdt_runtime::child_map(&flows, flow_id)?;
  flow.ensure_mergeable_text(flowstate_document::FLOW_TEXT_KEY).ok()
}

/// The cell's flow text container, via the owning table's cell registry.
fn cell_flow_text(doc: &LoroDoc, table: BlockId, cell: gpui_flowtext::CellId) -> Result<(String, LoroText), WriteRejected> {
  let table_map = crate::crdt_runtime::projection_table_map_by_block_id(doc, table).ok_or(WriteRejected::UnresolvedBlock(table))?;
  let cells_by_id = table_map
    .ensure_mergeable_map(flowstate_document::TABLE_CELLS_BY_ID)
    .map_err(|_| WriteRejected::StructureViolation("table cell registry unavailable"))?;
  let cell_key = flowstate_document::cell_loro_id(cell);
  let cell_map = crate::crdt_runtime::child_map(&cells_by_id, &cell_key).ok_or(WriteRejected::UnresolvedTableEntity {
    table,
    detail: format!("cell {cell_key} not found"),
  })?;
  let flow_id = crate::crdt_runtime::map_string_opt(&cell_map, "flow_id").ok_or(WriteRejected::StructureViolation("cell has no flow id"))?;
  let text = flow_text_by_id(doc, &flow_id).ok_or(WriteRejected::StructureViolation("cell flow missing"))?;
  Ok((flow_id, text))
}

/// Boundary (`\n`) unicode positions of a cell flow string. Position 0 is the
/// leading sentinel (the first paragraph's boundary) by construction.
fn boundaries(flow: &str) -> Vec<usize> {
  flow
    .chars()
    .enumerate()
    .filter_map(|(pos, ch)| (ch == '\n').then_some(pos))
    .collect()
}

/// Absolute unicode position for `(paragraph index, byte offset)` against the
/// live flow string.
fn resolve_position(flow: &str, bounds: &[usize], paragraph: usize, byte: usize) -> Result<usize, WriteRejected> {
  let start = *bounds
    .get(paragraph)
    .ok_or(WriteRejected::StructureViolation("cell paragraph index out of range"))?
    + 1;
  let end = bounds.get(paragraph + 1).copied().unwrap_or_else(|| flow.chars().count());
  let paragraph_text: String = flow.chars().skip(start).take(end - start).collect();
  if byte > paragraph_text.len() || !paragraph_text.is_char_boundary(byte) {
    return Err(WriteRejected::StructureViolation("cell byte offset is not a char boundary"));
  }
  let chars = paragraph_text[..byte].chars().count();
  Ok(start + chars)
}

pub(crate) fn resolve_cell_text(core: &CrdtRuntime, table_ix: usize, intent: &TableCellTextIntent) -> Result<ResolvedCellText, WriteRejected> {
  let doc = core.doc();
  let (flow_id, text) = cell_flow_text(doc, intent.table, intent.cell)?;
  let live = text.to_string();
  if live.contains(flowstate_document::OBJECT_REPLACEMENT) {
    return Err(WriteRejected::StructureViolation(
      "cell contains nested tables; positional cell text ops refuse (whole-cell path handles it)",
    ));
  }
  // THE PIN: positional addressing is only sound against the exact text the
  // editor resolved its positions in.
  if table_cell_text_hash(&live) != intent.expected_text_hash {
    return Err(WriteRejected::StaleCellText);
  }
  let bounds = boundaries(&live);
  if bounds.is_empty() {
    return Err(WriteRejected::StructureViolation("cell flow has no boundary sentinel"));
  }
  let op = match &intent.op {
    TableCellTextOp::Insert {
      paragraph,
      byte,
      text: inserted,
      style_override,
    } => {
      if inserted.is_empty() {
        return Err(WriteRejected::EmptyIntent);
      }
      if inserted.contains('\n') {
        return Err(WriteRejected::StructureViolation("cell inserts must not contain structural newlines"));
      }
      ResolvedCellTextOp::Insert {
        pos: resolve_position(&live, &bounds, *paragraph, *byte)?,
        text: inserted.clone(),
        style_override: *style_override,
      }
    },
    TableCellTextOp::Delete { start, end } => {
      let start_pos = resolve_position(&live, &bounds, start.0, start.1)?;
      let end_pos = resolve_position(&live, &bounds, end.0, end.1)?;
      if end_pos <= start_pos {
        return Err(WriteRejected::EmptyIntent);
      }
      if start_pos == 0 {
        return Err(WriteRejected::StructureViolation("cell delete cannot take the boundary sentinel"));
      }
      ResolvedCellTextOp::Delete {
        pos: start_pos,
        len: end_pos - start_pos,
      }
    },
    TableCellTextOp::Split { at, inherited_style } => ResolvedCellTextOp::Split {
      pos: resolve_position(&live, &bounds, at.0, at.1)?,
      style: *inherited_style,
    },
    TableCellTextOp::Join { second } => {
      if *second == 0 || *second >= bounds.len() {
        return Err(WriteRejected::StructureViolation("cell join target out of range"));
      }
      ResolvedCellTextOp::Join { boundary: bounds[*second] }
    },
    TableCellTextOp::SetMarks { start, end, styles } => {
      let start_pos = resolve_position(&live, &bounds, start.0, start.1)?;
      let end_pos = resolve_position(&live, &bounds, end.0, end.1)?;
      if end_pos <= start_pos {
        return Err(WriteRejected::EmptyIntent);
      }
      ResolvedCellTextOp::Marks {
        pos: start_pos,
        len: end_pos - start_pos,
        styles: *styles,
      }
    },
    TableCellTextOp::SetParagraphStyle { paragraph, style } => ResolvedCellTextOp::ParagraphStyle {
      boundary: *bounds
        .get(*paragraph)
        .ok_or(WriteRejected::StructureViolation("cell paragraph index out of range"))?,
      style: *style,
    },
  };
  Ok(ResolvedCellText {
    table: intent.table,
    table_ix,
    flow_id,
    op,
  })
}

/// Apply the resolved op as minimal Loro ops. The caller owns commit/patches.
pub(crate) fn execute_cell_text(doc: &LoroDoc, plan: &ResolvedCellText) -> anyhow::Result<()> {
  let text = flow_text_by_id(doc, &plan.flow_id).ok_or_else(|| anyhow::anyhow!("cell flow vanished mid-commit"))?;
  match &plan.op {
    ResolvedCellTextOp::Insert { pos, text: inserted, style_override } => {
      text.insert(*pos, inserted)?;
      if let Some(styles) = style_override {
        mark_run_styles(&text, *pos..*pos + inserted.chars().count(), *styles)?;
      }
    },
    ResolvedCellTextOp::Delete { pos, len } => {
      text.delete(*pos, *len)?;
    },
    ResolvedCellTextOp::Split { pos, style } => {
      // The exact boundary law: insert the `\n`, mark its paragraph style,
      // strip absorbed run keys so styling never bleeds across the boundary.
      text.insert(*pos, "\n")?;
      text.mark(*pos..*pos + 1, flowstate_document::MARK_PARAGRAPH_STYLE, paragraph_style_value(*style))?;
      mark_run_styles(&text, *pos..*pos + 1, RunStyles::default())?;
    },
    ResolvedCellTextOp::Join { boundary } => {
      text.delete(*boundary, 1)?;
    },
    ResolvedCellTextOp::Marks { pos, len, styles } => {
      mark_run_styles(&text, *pos..*pos + *len, *styles)?;
    },
    ResolvedCellTextOp::ParagraphStyle { boundary, style } => {
      text.mark(*boundary..*boundary + 1, flowstate_document::MARK_PARAGRAPH_STYLE, paragraph_style_value(*style))?;
    },
  }
  Ok(())
}
