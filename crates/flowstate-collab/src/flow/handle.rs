//! [`FlowDocHandle`] — the app-facing flow write authority, mirroring
//! `LocalDocHandle`: every method takes ONE gate hold; the optimistic apply
//! IS the committed apply, synchronously. Solo and collaborative flows
//! receive this same authority object (spec invariant 5).

use std::sync::Arc;

use flowstate_flow::{CellId, FlowBoardProjection, FlowDefect, FlowIntent};
use uuid::Uuid;

use super::runtime::{FlowLocalOutcome, FlowRuntime, FlowStreamItem};
use crate::local_write::{GateHolder, WriteGate};

#[derive(Clone, Debug)]
pub enum FlowWriteRejected {
  GatePoisoned,
  Invalid(String),
}

impl std::fmt::Display for FlowWriteRejected {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::GatePoisoned => f.write_str("flow write gate is poisoned"),
      Self::Invalid(reason) => f.write_str(reason),
    }
  }
}

impl std::error::Error for FlowWriteRejected {}

pub struct FlowDocHandle {
  core: Arc<WriteGate<FlowRuntime>>,
}

impl FlowDocHandle {
  pub fn new(runtime: FlowRuntime) -> (Self, Arc<WriteGate<FlowRuntime>>) {
    let core = Arc::new(WriteGate::new(runtime));
    (Self { core: Arc::clone(&core) }, core)
  }

  pub fn from_gate(core: Arc<WriteGate<FlowRuntime>>) -> Self {
    Self { core }
  }

  pub fn gate(&self) -> &Arc<WriteGate<FlowRuntime>> {
    &self.core
  }

  pub fn apply(&self, intent: &FlowIntent) -> Result<FlowLocalOutcome, FlowWriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::LocalIntent)
      .map_err(|_| FlowWriteRejected::GatePoisoned)?;
    guard
      .apply_intent(intent)
      .map_err(|error| FlowWriteRejected::Invalid(error.to_string()))
  }

  pub fn undo(&self) -> Result<bool, FlowWriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::UndoRedo)
      .map_err(|_| FlowWriteRejected::GatePoisoned)?;
    guard
      .undo()
      .map_err(|error| FlowWriteRejected::Invalid(error.to_string()))
  }

  pub fn redo(&self) -> Result<bool, FlowWriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::UndoRedo)
      .map_err(|_| FlowWriteRejected::GatePoisoned)?;
    guard
      .redo()
      .map_err(|error| FlowWriteRejected::Invalid(error.to_string()))
  }

  pub fn can_undo(&self) -> Result<(bool, bool), FlowWriteRejected> {
    let guard = self
      .core
      .lock(GateHolder::UndoRedo)
      .map_err(|_| FlowWriteRejected::GatePoisoned)?;
    Ok((guard.can_undo(), guard.can_redo()))
  }

  pub fn undo_group_start(&self) -> Result<bool, FlowWriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::UndoRedo)
      .map_err(|_| FlowWriteRejected::GatePoisoned)?;
    guard
      .undo_group_start()
      .map_err(|error| FlowWriteRejected::Invalid(error.to_string()))
  }

  pub fn undo_group_end(&self) -> Result<(), FlowWriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::UndoRedo)
      .map_err(|_| FlowWriteRejected::GatePoisoned)?;
    guard.undo_group_end();
    drop(guard);
    Ok(())
  }

  pub fn import_remote_updates(&self, blobs: &[&[u8]]) -> Result<(), FlowWriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::ImportChunk)
      .map_err(|_| FlowWriteRejected::GatePoisoned)?;
    guard
      .import_remote_updates(blobs)
      .map_err(|error| FlowWriteRejected::Invalid(error.to_string()))
  }

  pub fn drain_board_stream(&self) -> Result<Vec<FlowStreamItem>, FlowWriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::LocalIntent)
      .map_err(|_| FlowWriteRejected::GatePoisoned)?;
    Ok(guard.take_board_stream())
  }

  pub fn drain_cell_stream(&self, cell_id: CellId) -> Result<Vec<gpui_flowtext::ProjectionStreamItem>, FlowWriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::LocalIntent)
      .map_err(|_| FlowWriteRejected::GatePoisoned)?;
    Ok(guard.take_cell_stream(cell_id))
  }

  pub fn board_projection(&self) -> Result<FlowBoardProjection, FlowWriteRejected> {
    let guard = self
      .core
      .lock(GateHolder::LocalIntent)
      .map_err(|_| FlowWriteRejected::GatePoisoned)?;
    Ok(guard.board().clone())
  }

  pub fn defects(&self) -> Result<Vec<FlowDefect>, FlowWriteRejected> {
    let guard = self
      .core
      .lock(GateHolder::LocalIntent)
      .map_err(|_| FlowWriteRejected::GatePoisoned)?;
    Ok(guard.defects().to_vec())
  }

  pub fn cell_projection(&self, cell_id: CellId) -> Result<flowstate_document::DocumentProjection, FlowWriteRejected> {
    let guard = self
      .core
      .lock(GateHolder::LocalIntent)
      .map_err(|_| FlowWriteRejected::GatePoisoned)?;
    guard
      .cell_projection(cell_id)
      .map_err(|error| FlowWriteRejected::Invalid(error.to_string()))
  }

  /// A per-cell [`gpui_flowtext::LocalWriteAuthority`] over this same gate,
  /// for injection into the cell's `RichTextEditor`.
  pub fn cell_authority(&self, cell_id: CellId) -> std::sync::Arc<super::FlowCellAuthority> {
    std::sync::Arc::new(super::FlowCellAuthority::new(std::sync::Arc::clone(&self.core), cell_id))
  }

  /// R6 scrubber: a read-only historical board at `fraction` of the change
  /// timeline (fork under the gate; checkout + materialize on the fork).
  pub fn history_board_at(&self, fraction: f32) -> Result<(FlowBoardProjection, usize, usize), FlowWriteRejected> {
    let guard = self
      .core
      .lock(GateHolder::LocalIntent)
      .map_err(|_| FlowWriteRejected::GatePoisoned)?;
    guard
      .history_board_at(fraction)
      .map_err(|error| FlowWriteRejected::Invalid(error.to_string()))
  }

  pub fn document_id(&self) -> Result<Option<Uuid>, FlowWriteRejected> {
    let guard = self
      .core
      .lock(GateHolder::LocalIntent)
      .map_err(|_| FlowWriteRejected::GatePoisoned)?;
    Ok(guard.document_id())
  }
}
