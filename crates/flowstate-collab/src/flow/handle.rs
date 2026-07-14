//! `FlowDocHandle` — the ONE local write API for a flow document (invariant 5:
//! solo and collaborative tabs hold this identical handle; collaboration only
//! adds the flow I/O service draining the publish queue behind the same gate).

use std::sync::{Arc, Mutex};

use flowstate_flow::format::{CellId, FlowFormat};
use flowstate_flow::intents::FlowIntent;
use flowstate_flow::projection::FlowBoardProjection;
use gpui_flowtext::DocumentProjection;
use uuid::Uuid;

use super::cell_authority::FlowCellAuthority;
use super::commit::apply_flow_intent;
use super::runtime::{FlowRuntime, FlowStreamItem, FlowUndoMeta, FlowUndoOutcome, FlowWriteOutcome, FlowWriteRejected};
use crate::local_write::{GateHolder, WriteGate};

pub struct FlowDocHandle {
  core: Arc<WriteGate<FlowRuntime>>,
  /// Board context stamped onto each committed intent's undo item — kept
  /// current by the editor (focus/sheet changes), read by `apply`.
  undo_context: Mutex<FlowUndoMeta>,
}

impl FlowDocHandle {
  /// Wrap a runtime. The returned gate is shared with the flow I/O service so
  /// imports/exports serialize with local writes.
  #[must_use]
  pub fn new(core: FlowRuntime) -> (Arc<Self>, Arc<WriteGate<FlowRuntime>>) {
    let gate = Arc::new(WriteGate::new(core));
    (Self::attach(Arc::clone(&gate)), gate)
  }

  /// Attach to an existing shared core (join handoff).
  #[must_use]
  pub fn attach(core: Arc<WriteGate<FlowRuntime>>) -> Arc<Self> {
    Arc::new(Self {
      core,
      undo_context: Mutex::new(FlowUndoMeta::default()),
    })
  }

  /// Fresh solo document.
  pub fn new_document(format: &FlowFormat) -> anyhow::Result<(Arc<Self>, Arc<WriteGate<FlowRuntime>>)> {
    Ok(Self::new(FlowRuntime::new(format)?))
  }

  #[must_use]
  pub fn gate(&self) -> Arc<WriteGate<FlowRuntime>> {
    Arc::clone(&self.core)
  }

  fn lock(&self, holder: GateHolder) -> Result<crate::local_write::gate::GateGuard<'_, FlowRuntime>, FlowWriteRejected> {
    self.core.lock(holder).map_err(|_| FlowWriteRejected::GatePoisoned)
  }

  /// Update the board context riding future undo items (focus restoration).
  pub fn set_undo_context(&self, meta: FlowUndoMeta) {
    if let Ok(mut context) = self.undo_context.lock() {
      *context = meta;
    }
  }

  pub(crate) fn undo_context(&self) -> FlowUndoMeta {
    self
      .undo_context
      .lock()
      .map(|context| context.clone())
      .unwrap_or_default()
  }

  /// Apply one flow intent: the whole resolve → mutate → commit → derive path
  /// under one gate hold; the returned outcome IS the committed state.
  pub fn apply(&self, intent: FlowIntent) -> Result<FlowWriteOutcome, FlowWriteRejected> {
    let meta = self.undo_context();
    let mut guard = self.lock(GateHolder::LocalIntent)?;
    guard.set_pending_undo_meta(Some(meta));
    let outcome = apply_flow_intent(&mut guard, &intent);
    guard.set_pending_undo_meta(None);
    outcome
  }

  pub fn document_id(&self) -> Result<Uuid, FlowWriteRejected> {
    Ok(self.lock(GateHolder::LocalIntent)?.document_id())
  }

  /// Clone the canonical board (editor attach; afterwards the editor advances
  /// its copy exclusively via the ordered board stream).
  pub fn board_projection(&self) -> Result<FlowBoardProjection, FlowWriteRejected> {
    Ok(self.lock(GateHolder::LocalIntent)?.board_ref().clone())
  }

  pub fn drain_board_stream(&self) -> Result<Vec<FlowStreamItem>, FlowWriteRejected> {
    Ok(self.lock(GateHolder::LocalIntent)?.take_board_stream())
  }

  /// Materialize + start streaming one cell (editor attach path).
  pub fn open_cell(&self, cell: CellId) -> Result<DocumentProjection, FlowWriteRejected> {
    self
      .lock(GateHolder::LocalIntent)?
      .open_cell(cell)
      .map_err(|error| FlowWriteRejected::StructureViolation(format!("{error:#}")))
  }

  pub fn close_cell(&self, cell: CellId) -> Result<(), FlowWriteRejected> {
    self.lock(GateHolder::LocalIntent)?.close_cell(cell);
    Ok(())
  }

  /// The per-cell write authority injected into that cell's `RichTextEditor`
  /// (`RichTextEditor::set_write_authority`).
  #[must_use]
  pub fn cell_authority(self: &Arc<Self>, cell: CellId) -> Arc<FlowCellAuthority> {
    Arc::new(FlowCellAuthority::new(Arc::clone(self), cell))
  }

  // ---- Undo (spec §10) -------------------------------------------------------

  pub fn undo(&self) -> Result<FlowUndoOutcome, FlowWriteRejected> {
    self
      .lock(GateHolder::UndoRedo)?
      .undo()
      .map_err(|error| FlowWriteRejected::CompensatedFailure {
        class: "flow-undo",
        diagnostic: format!("{error:#}"),
      })
  }

  pub fn redo(&self) -> Result<FlowUndoOutcome, FlowWriteRejected> {
    self
      .lock(GateHolder::UndoRedo)?
      .redo()
      .map_err(|error| FlowWriteRejected::CompensatedFailure {
        class: "flow-redo",
        diagnostic: format!("{error:#}"),
      })
  }

  pub fn undo_group_start(&self) -> Result<bool, FlowWriteRejected> {
    Ok(self.lock(GateHolder::UndoRedo)?.undo_group_start())
  }

  pub fn undo_group_end(&self) -> Result<(), FlowWriteRejected> {
    self.lock(GateHolder::UndoRedo)?.undo_group_end();
    Ok(())
  }

  pub fn can_undo(&self) -> Result<bool, FlowWriteRejected> {
    Ok(self.lock(GateHolder::UndoRedo)?.can_undo())
  }

  pub fn can_redo(&self) -> Result<bool, FlowWriteRejected> {
    Ok(self.lock(GateHolder::UndoRedo)?.can_redo())
  }

  /// Test/tooling support: run a closure against the gate-held runtime (the
  /// flow analog of holding a test gate guard on the .db8 core).
  pub fn with_test_runtime<T>(&self, call: impl FnOnce(&mut FlowRuntime) -> T) -> T {
    let mut guard = self
      .core
      .lock(GateHolder::DocumentService)
      .expect("flow write gate healthy");
    call(&mut guard)
  }

  // ---- Gate-held reads shared with the cell authority ------------------------

  pub(crate) fn with_runtime<T>(
    &self,
    holder: GateHolder,
    read: impl FnOnce(&mut FlowRuntime) -> T,
  ) -> Result<T, FlowWriteRejected> {
    let mut guard = self.lock(holder)?;
    Ok(read(&mut guard))
  }
}
