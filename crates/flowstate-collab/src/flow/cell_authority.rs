//! `FlowCellAuthority` — a per-cell [`LocalWriteAuthority`] (flow spec Part B).
//!
//! Each open cell's `RichTextEditor` is driven by one of these through the
//! existing `RichTextEditor::set_write_authority` injection point. It
//! translates the editor's `LocalIntent`s into `FlowIntent::CellText` on the
//! shared flow handle, drains THAT cell's ordered stream, and provides exact
//! caret survival across concurrent same-cell edits via encoded Loro cursors
//! (`encode/resolve_selection_anchor`). Object/table/image intents are
//! rejected structurally — `flow_cell_surface` editors never emit them.

use std::sync::Arc;

use flowstate_flow::format::CellId;
use flowstate_flow::intents::FlowIntent;
use gpui_flowtext::local_intents::{
  IntentCounters, LocalCommit, LocalIntent, LocalWriteAuthority, LocalWriteOutcome, UndoOutcome, WriteRejected,
};
use gpui_flowtext::{DocumentProjection, EditorSelection, ProjectionPatchBatch, ProjectionStreamItem};
use loro::{ContainerTrait as _, cursor::Cursor};
use uuid::Uuid;

use super::cell_text::CellTextContext;
use super::handle::FlowDocHandle;
use super::runtime::{FlowUndoMeta, FlowWriteRejected};
use crate::crdt_runtime::cursor_for_boundary;
use crate::local_write::GateHolder;

pub struct FlowCellAuthority {
  handle: Arc<FlowDocHandle>,
  cell: CellId,
}

impl FlowCellAuthority {
  #[must_use]
  pub fn new(handle: Arc<FlowDocHandle>, cell: CellId) -> Self {
    Self { handle, cell }
  }

  #[must_use]
  pub fn cell_id(&self) -> CellId {
    self.cell
  }

  #[must_use]
  pub fn handle(&self) -> &Arc<FlowDocHandle> {
    &self.handle
  }

  fn map_rejection(rejected: FlowWriteRejected) -> WriteRejected {
    match rejected {
      FlowWriteRejected::UnresolvedParagraph(id) => WriteRejected::UnresolvedParagraph(id),
      FlowWriteRejected::UnresolvedCursor => WriteRejected::UnresolvedCursor,
      FlowWriteRejected::EmptyIntent => WriteRejected::EmptyIntent,
      FlowWriteRejected::GatePoisoned => WriteRejected::GatePoisoned,
      FlowWriteRejected::CompensatedFailure { class, diagnostic } => WriteRejected::CompensatedFailure { class, diagnostic },
      FlowWriteRejected::CompensationFailed { class, diagnostic } => WriteRejected::CompensationFailed { class, diagnostic },
      FlowWriteRejected::UnknownSheet(_) | FlowWriteRejected::UnknownCell(_) => {
        WriteRejected::StructureViolation("cell no longer exists in canonical flow state")
      },
      FlowWriteRejected::StructureViolation(reason) => {
        tracing::debug!(%reason, "cell write rejected as a structure violation");
        WriteRejected::StructureViolation("intent violates flow cell structure")
      },
    }
  }

  /// Resolve a restored undo/redo board context into THIS cell's selection
  /// (only when the restored focus IS this cell).
  fn selection_from_meta(&self, meta: &FlowUndoMeta) -> Option<EditorSelection> {
    if meta.focused_cell != Some(self.cell) {
      return None;
    }
    let (head_cursor, anchor_cursor) = (meta.head_cursor.as_deref()?, meta.anchor_cursor.as_deref()?);
    let (head, anchor) = self.resolve_selection_anchor(head_cursor, anchor_cursor)?;
    Some(EditorSelection {
      anchor,
      head,
      anchor_affinity: gpui_flowtext::SelectionAffinity::Neutral,
      head_affinity: gpui_flowtext::SelectionAffinity::Neutral,
      anchor_gravity: gpui_flowtext::VisualGravity::Neutral,
      head_gravity: gpui_flowtext::VisualGravity::Neutral,
    })
  }
}

impl LocalWriteAuthority for FlowCellAuthority {
  fn apply(&self, intent: LocalIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    let outcome = self
      .handle
      .apply(FlowIntent::CellText { cell_id: self.cell, intent })
      .map_err(Self::map_rejection)?;
    if !outcome.changed {
      return Err(WriteRejected::EmptyIntent);
    }
    // The projection change rides the cell's ordered stream (whole-cell
    // Replace — cells are tiny); the returned batch carries only the frontier
    // pair + the caret. The editor never applies the batch directly
    // (`integrate_outcome` drains the stream), so an empty patch list is the
    // honest representation.
    Ok(LocalWriteOutcome::Committed(LocalCommit {
      patches: ProjectionPatchBatch {
        transaction_id: Uuid::new_v4().as_u128(),
        base_frontier: Vec::new(),
        new_frontier: outcome.frontier.clone(),
        patches: Arc::from(Vec::new()),
      },
      frontier: outcome.frontier,
      version_vector: outcome.version_vector,
      selection_after: outcome.selection_after,
      counters: IntentCounters::default(),
    }))
  }

  fn undo(&self) -> Result<UndoOutcome, WriteRejected> {
    let outcome = self.handle.undo().map_err(Self::map_rejection)?;
    let selection = outcome
      .meta
      .as_ref()
      .and_then(|meta| self.selection_from_meta(meta));
    Ok(UndoOutcome {
      applied: outcome.applied,
      selection,
    })
  }

  fn redo(&self) -> Result<UndoOutcome, WriteRejected> {
    let outcome = self.handle.redo().map_err(Self::map_rejection)?;
    let selection = outcome
      .meta
      .as_ref()
      .and_then(|meta| self.selection_from_meta(meta));
    Ok(UndoOutcome {
      applied: outcome.applied,
      selection,
    })
  }

  fn undo_group_start(&self) -> Result<bool, WriteRejected> {
    self.handle.undo_group_start().map_err(Self::map_rejection)
  }

  fn undo_group_end(&self) -> Result<(), WriteRejected> {
    self.handle.undo_group_end().map_err(Self::map_rejection)
  }

  fn drain_projection_stream(&self) -> Result<Vec<ProjectionStreamItem>, WriteRejected> {
    self
      .handle
      .with_runtime(GateHolder::LocalIntent, |runtime| runtime.take_cell_stream(self.cell))
      .map_err(Self::map_rejection)
  }

  fn canonical_projection(&self) -> Result<DocumentProjection, WriteRejected> {
    self
      .handle
      .open_cell(self.cell)
      .map_err(Self::map_rejection)
  }

  fn encode_selection_anchor(&self, selection: &EditorSelection, editor_frontier: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let cell = self.cell;
    let selection = selection.clone();
    let editor_frontier = editor_frontier.to_vec();
    let handle = Arc::clone(&self.handle);
    self
      .handle
      .with_runtime(GateHolder::Presence, move |runtime| {
        // Only encode while the editor is in sync with the core (an undrained
        // import would anchor the caret against a stale projection).
        if runtime.frontier() != editor_frontier {
          return None;
        }
        let ctx = CellTextContext::resolve(runtime.doc(), cell).ok()?;
        let head_unicode = ctx.unicode_for_offset(selection.head)?;
        let anchor_unicode = ctx.unicode_for_offset(selection.anchor)?;
        let head = cursor_for_boundary(&ctx.text, head_unicode, crate::presence::SelectionAffinity::Neutral)?;
        let anchor = cursor_for_boundary(&ctx.text, anchor_unicode, crate::presence::SelectionAffinity::Neutral)?;
        // Keep the flow undo context current: this is called at synced
        // moments, exactly when the selection is a trustworthy capture.
        let active_sheet = runtime.board_ref().cell(cell).map(|(sheet, _)| sheet.id);
        handle.set_undo_context(FlowUndoMeta {
          active_sheet,
          focused_cell: Some(cell),
          head_cursor: Some(head.encode()),
          anchor_cursor: Some(anchor.encode()),
        });
        Some((head.encode(), anchor.encode()))
      })
      .ok()?
  }

  fn resolve_selection_anchor(
    &self,
    head_cursor: &[u8],
    anchor_cursor: &[u8],
  ) -> Option<(gpui_flowtext::DocumentOffset, gpui_flowtext::DocumentOffset)> {
    let cell = self.cell;
    let head_cursor = head_cursor.to_vec();
    let anchor_cursor = anchor_cursor.to_vec();
    self
      .handle
      .with_runtime(GateHolder::Presence, move |runtime| {
        let ctx = CellTextContext::resolve(runtime.doc(), cell).ok()?;
        let resolve = |encoded: &[u8]| {
          let cursor = Cursor::decode(encoded).ok()?;
          if cursor.container != ctx.text.id() {
            return None;
          }
          let position = runtime.doc().get_cursor_pos(&cursor).ok()?;
          ctx.offset_for_unicode(position.current.pos)
        };
        Some((resolve(&head_cursor)?, resolve(&anchor_cursor)?))
      })
      .ok()
      .flatten()
  }
}
