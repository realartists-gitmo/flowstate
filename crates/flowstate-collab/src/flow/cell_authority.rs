//! [`FlowCellAuthority`] — a per-cell [`LocalWriteAuthority`] over the flow's
//! write gate, injected into each cell's `RichTextEditor` via the existing
//! `set_write_authority` seam. The cell editor becomes a REAL loro-first
//! editor: its intents execute as minimal ops on the cell's flow (char-level
//! remote merging), its projections arrive on the cell's ordered stream, and
//! its carets ride Loro cursors that survive concurrent edits exactly.
//!
//! Outcome shape (spec Part 2.2, scale-justified): every committed cell
//! intent returns `CommittedWithRebuild` with the freshly materialized cell
//! projection. Cells are card-scale documents — a whole-cell projection is
//! cheaper than the body's patch synthesis machinery by orders of magnitude,
//! and `full_rebuild` is expected (not a defect) for this authority.

use std::sync::Arc;

use flowstate_flow::CellId;
use gpui_flowtext::{
  CursorEndpoint, DocumentOffset, EditorSelection, LocalCommit, LocalIntent, LocalWriteAuthority, LocalWriteOutcome, ProjectionReplace,
  ProjectionStreamItem, SelectionAffinity, SelectionSnapshot, UndoOutcome, VisualGravity, WriteRejected,
};
use loro::cursor::{Cursor, Side};
use loro::{ContainerTrait as _, LoroDoc};

use super::runtime::FlowRuntime;
use crate::local_write::{GateHolder, WriteGate};

pub struct FlowCellAuthority {
  core: Arc<WriteGate<FlowRuntime>>,
  cell_id: CellId,
}

impl FlowCellAuthority {
  pub fn new(core: Arc<WriteGate<FlowRuntime>>, cell_id: CellId) -> Self {
    Self { core, cell_id }
  }

  pub fn cell_id(&self) -> CellId {
    self.cell_id
  }

  pub(crate) fn cell_text(doc: &LoroDoc, cell_id: CellId) -> Option<loro::LoroText> {
    let record = flowstate_flow::loro_schema::cell_record(doc, cell_id)?;
    let flow = flowstate_flow::loro_schema::cell_flow(&record)?;
    flow
      .ensure_mergeable_text(flowstate_document::FLOW_TEXT_KEY)
      .ok()
  }

  /// Projection offset → absolute flow unicode position, via the cell's
  /// current projection (paragraph byte offsets are projection-space).
  fn flow_pos_for_offset(projection: &flowstate_document::DocumentProjection, offset: DocumentOffset) -> Option<usize> {
    let mut pos = 0_usize; // running flow position: each paragraph is `\n` + text
    for (index, _) in projection.paragraphs.iter().enumerate() {
      let range = flowstate_document::paragraph_byte_range(projection, index);
      let text = projection.text.byte_slice(range).to_string();
      if index == offset.paragraph {
        let byte = offset.byte.min(text.len());
        let chars = text
          .get(..byte)
          .map_or_else(|| text.chars().count(), |prefix| prefix.chars().count());
        return Some(pos + 1 + chars);
      }
      pos += 1 + text.chars().count();
    }
    None
  }

  /// Absolute flow unicode position → projection offset.
  fn offset_for_flow_pos(projection: &flowstate_document::DocumentProjection, flow_pos: usize) -> DocumentOffset {
    let mut pos = 0_usize;
    for (index, _) in projection.paragraphs.iter().enumerate() {
      let range = flowstate_document::paragraph_byte_range(projection, index);
      let text = projection.text.byte_slice(range).to_string();
      let chars = text.chars().count();
      let start = pos + 1;
      let end = start + chars;
      if flow_pos < end || index + 1 == projection.paragraphs.len() {
        let within = flow_pos.saturating_sub(start).min(chars);
        let byte = text
          .char_indices()
          .nth(within)
          .map_or(text.len(), |(byte, _)| byte);
        return DocumentOffset { paragraph: index, byte };
      }
      pos = end;
    }
    DocumentOffset { paragraph: 0, byte: 0 }
  }
}

impl LocalWriteAuthority for FlowCellAuthority {
  fn apply(&self, intent: LocalIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::LocalIntent)
      .map_err(|_| WriteRejected::GatePoisoned)?;
    let outcome = guard.apply_cell_text(self.cell_id, &intent)?;
    drop(guard);
    // The post-edit caret (spec §8): map the runtime's flow-position back to a
    // projection offset and carry its Loro cursor so the editor advances the
    // caret instead of stranding it at the pre-edit position.
    let selection_after = outcome.caret.as_ref().map(|(flow_pos, cursor_bytes)| {
      let offset = Self::offset_for_flow_pos(&outcome.replace.document, *flow_pos);
      let endpoint = CursorEndpoint {
        cursor: cursor_bytes.clone(),
        delta: 0,
        affinity: SelectionAffinity::Neutral,
        gravity: VisualGravity::Neutral,
        offset,
      };
      SelectionSnapshot {
        anchor: endpoint.clone(),
        head: endpoint,
      }
    });
    let commit = LocalCommit {
      patches: gpui_flowtext::ProjectionPatchBatch {
        transaction_id: 0,
        base_frontier: outcome.replace.frontier.clone(),
        new_frontier: outcome.replace.frontier.clone(),
        patches: Arc::from([]),
      },
      frontier: outcome.replace.frontier.clone(),
      version_vector: outcome.replace.version_vector.clone(),
      selection_after,
      counters: gpui_flowtext::IntentCounters {
        full_rebuild: true,
        ..Default::default()
      },
    };
    Ok(LocalWriteOutcome::CommittedWithRebuild {
      commit,
      replace: Box::new(outcome.replace),
    })
  }

  fn undo(&self) -> Result<UndoOutcome, WriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::UndoRedo)
      .map_err(|_| WriteRejected::GatePoisoned)?;
    let applied = guard
      .undo()
      .map_err(|_| WriteRejected::StructureViolation("flow undo failed"))?;
    drop(guard);
    Ok(UndoOutcome { applied, selection: None })
  }

  fn redo(&self) -> Result<UndoOutcome, WriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::UndoRedo)
      .map_err(|_| WriteRejected::GatePoisoned)?;
    let applied = guard
      .redo()
      .map_err(|_| WriteRejected::StructureViolation("flow redo failed"))?;
    drop(guard);
    Ok(UndoOutcome { applied, selection: None })
  }

  fn undo_group_start(&self) -> Result<bool, WriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::UndoRedo)
      .map_err(|_| WriteRejected::GatePoisoned)?;
    guard
      .undo_group_start()
      .map_err(|_| WriteRejected::StructureViolation("flow undo grouping failed"))
  }

  fn undo_group_end(&self) -> Result<(), WriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::UndoRedo)
      .map_err(|_| WriteRejected::GatePoisoned)?;
    guard.undo_group_end();
    drop(guard);
    Ok(())
  }

  fn drain_projection_stream(&self) -> Result<Vec<ProjectionStreamItem>, WriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::LocalIntent)
      .map_err(|_| WriteRejected::GatePoisoned)?;
    let items = guard.take_cell_stream(self.cell_id);
    drop(guard);
    Ok(items)
  }

  fn canonical_projection(&self) -> Result<flowstate_document::DocumentProjection, WriteRejected> {
    let guard = self
      .core
      .lock(GateHolder::LocalIntent)
      .map_err(|_| WriteRejected::GatePoisoned)?;
    guard
      .cell_projection(self.cell_id)
      .map_err(|_| WriteRejected::StructureViolation("flow cell projection unavailable"))
  }

  fn encode_selection_anchor(&self, selection: &EditorSelection, editor_frontier: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let guard = self.core.lock(GateHolder::LocalIntent).ok()?;
    if guard.frontier() != editor_frontier {
      return None; // editor is behind the core: anchors would mis-place
    }
    let projection = guard.cell_projection(self.cell_id).ok()?;
    let text = Self::cell_text(guard.doc(), self.cell_id)?;
    let head_pos = Self::flow_pos_for_offset(&projection, selection.head)?;
    let anchor_pos = Self::flow_pos_for_offset(&projection, selection.anchor)?;
    let head = text.get_cursor(head_pos, Side::Left)?;
    let anchor = text.get_cursor(anchor_pos, Side::Left)?;
    drop(guard);
    Some((head.encode(), anchor.encode()))
  }

  fn resolve_selection_anchor(&self, head_cursor: &[u8], anchor_cursor: &[u8]) -> Option<(DocumentOffset, DocumentOffset)> {
    let guard = self.core.lock(GateHolder::LocalIntent).ok()?;
    let projection = guard.cell_projection(self.cell_id).ok()?;
    let text = Self::cell_text(guard.doc(), self.cell_id)?;
    let resolve = |bytes: &[u8]| -> Option<usize> {
      let cursor = Cursor::decode(bytes).ok()?;
      if cursor.container != text.id() {
        return None;
      }
      Some(guard.doc().get_cursor_pos(&cursor).ok()?.current.pos)
    };
    let head_pos = resolve(head_cursor)?;
    let anchor_pos = resolve(anchor_cursor)?;
    drop(guard);
    let head = Self::offset_for_flow_pos(&projection, head_pos);
    let anchor = Self::offset_for_flow_pos(&projection, anchor_pos);
    Some((head, anchor))
  }
}

/// Runtime-side product of a committed cell-text intent, consumed by the
/// authority to build its outcome.
pub struct CellTextCommit {
  pub replace: ProjectionReplace,
  /// Post-edit caret: `(flow-unicode position, encoded Loro cursor)`. `None`
  /// when the intent doesn't move the caret (pure styling).
  pub caret: Option<(usize, Vec<u8>)>,
}
