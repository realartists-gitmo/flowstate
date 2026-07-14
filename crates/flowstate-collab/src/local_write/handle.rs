//! `LocalDocHandle` — the ONE local write API (spec §4, D1/D2, invariant 5).
//!
//! Owned by the UI thread. Every method acquires the write gate, executes the
//! whole intent (resolve → mutate → commit → patch) inside it, and returns the
//! committed outcome synchronously — the "optimistic" apply IS the committed
//! apply. Solo documents and collaborative documents use this identical
//! handle; collaboration only adds the I/O service draining the publish queue
//! on the other side of the same gate.

use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Instant;

use flowstate_document::DocumentProjection;

use super::commit::apply_local_intent;
use super::gate::{GateHolder, GateMetrics, WriteGate};
use super::intents::{
  DeleteBlocksIntent, DeleteRangeIntent, InsertObjectIntent, InsertRichFragmentIntent, InsertTextIntent, JoinParagraphsIntent, LocalIntent,
  LocalWriteAuthority, LocalWriteOutcome, MoveBlockIntent, ReplaceEquationSourceRangeIntent, ReplaceImageAltTextIntent,
  ReplaceImageCaptionIntent, ReplaceMatchesIntent, ReplaceObjectIntent, SetImageLayoutIntent, SetMarksIntent, SetParagraphStyleIntent,
  SetParagraphStylesIntent, SplitParagraphIntent, TableIntent, UndoOutcome, WriteRejected,
};
use crate::crdt_runtime::{CrdtRuntime, RuntimeEvent, SemanticCommand};

/// Write-path configuration (spec §7 audit knob).
#[derive(Clone, Copy, Debug, Default)]
pub struct LocalWriteConfig {
  /// Release-build projection-audit sampling: `Some(1)` audits every commit,
  /// `Some(n)` one-in-n, `None` disables release auditing entirely (the
  /// ratified one-line off switch). Debug/CI builds audit unconditionally
  /// regardless of this knob.
  ///
  /// Default is OFF (2026-07-07): fidelity bugs have dried up in the field
  /// while perf is under active measurement, and the audit's ~350ms sampled
  /// hiccup on large docs pollutes that read. Re-enable for fidelity-hunting
  /// builds.
  pub release_audit_sample: Option<NonZeroU32>,
}

/// The single local write authority over one shared document core.
pub struct LocalDocHandle {
  core: Arc<WriteGate<CrdtRuntime>>,
  config: LocalWriteConfig,
  intents_committed: AtomicU64,
}

impl LocalDocHandle {
  /// Wrap a document core. The returned gate handle is shared with the I/O
  /// service (`DocIoService`) so imports/exports serialize with local writes.
  #[must_use]
  pub fn new(core: CrdtRuntime, config: LocalWriteConfig) -> (Self, Arc<WriteGate<CrdtRuntime>>) {
    let gate = Arc::new(WriteGate::new(core));
    (
      Self {
        core: Arc::clone(&gate),
        config,
        intents_committed: AtomicU64::new(0),
      },
      gate,
    )
  }

  /// Attach to an existing shared core (e.g. handed over from document open).
  #[must_use]
  pub fn attach(core: Arc<WriteGate<CrdtRuntime>>, config: LocalWriteConfig) -> Self {
    Self {
      core,
      config,
      intents_committed: AtomicU64::new(0),
    }
  }

  #[must_use]
  pub fn gate_metrics(&self) -> Arc<GateMetrics> {
    self.core.metrics()
  }

  /// Clone the canonical projection (editor attach; afterwards the editor
  /// advances its copy exclusively by patch batches).
  pub fn projection(&self) -> Result<DocumentProjection, WriteRejected> {
    let guard = self
      .core
      .lock(GateHolder::LocalIntent)
      .map_err(|_| WriteRejected::GatePoisoned)?;
    let projection = guard.projection_ref().clone();
    drop(guard);
    Ok(projection)
  }

  // ---- The intent API (spec §4) --------------------------------------------

  pub fn insert_text(&self, intent: InsertTextIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::InsertText(intent))
  }

  pub fn delete_range(&self, intent: DeleteRangeIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::DeleteRange(intent))
  }

  pub fn split_paragraph(&self, intent: SplitParagraphIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::SplitParagraph(intent))
  }

  pub fn join_paragraphs(&self, intent: JoinParagraphsIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::JoinParagraphs(intent))
  }

  pub fn set_marks(&self, intent: SetMarksIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::SetMarks(intent))
  }

  pub fn set_paragraph_style(&self, intent: SetParagraphStyleIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::SetParagraphStyle(intent))
  }

  pub fn set_paragraph_styles(&self, intent: SetParagraphStylesIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::SetParagraphStyles(intent))
  }

  pub fn insert_object(&self, intent: InsertObjectIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::InsertObject(intent))
  }

  pub fn replace_object(&self, intent: ReplaceObjectIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::ReplaceObject(intent))
  }

  pub fn delete_blocks(&self, intent: DeleteBlocksIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::DeleteBlocks(intent))
  }

  pub fn move_block(&self, intent: MoveBlockIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::MoveBlock(intent))
  }

  pub fn insert_rich_fragment(&self, intent: InsertRichFragmentIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::InsertRichFragment(intent))
  }

  pub fn replace_matches(&self, intent: ReplaceMatchesIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::ReplaceMatches(intent))
  }

  pub fn replace_equation_source_range(&self, intent: ReplaceEquationSourceRangeIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::ReplaceEquationSourceRange(intent))
  }

  pub fn replace_image_alt_text(&self, intent: ReplaceImageAltTextIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::ReplaceImageAltText(intent))
  }

  pub fn replace_image_caption(&self, intent: ReplaceImageCaptionIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::ReplaceImageCaption(intent))
  }

  pub fn set_image_layout(&self, intent: SetImageLayoutIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::SetImageLayout(intent))
  }

  pub fn table_op(&self, intent: TableIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(LocalIntent::Table(intent))
  }

  /// Generic dispatch — the fuzz/property harness entry point.
  #[hotpath::measure]
  pub fn apply_intent(&self, intent: LocalIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    let started = Instant::now();
    let mut guard = self
      .core
      .lock(GateHolder::LocalIntent)
      .map_err(|_| WriteRejected::GatePoisoned)?;
    let mut outcome = apply_local_intent(&mut guard, &intent)?;
    self.maybe_release_audit(&mut guard, &intent);
    drop(guard);
    let hold_micros = u64::try_from(started.elapsed().as_micros()).unwrap_or(u64::MAX);
    match &mut outcome {
      LocalWriteOutcome::Committed(commit) | LocalWriteOutcome::CommittedWithRebuild { commit, .. } => {
        commit.counters.gate_hold_micros = hold_micros;
      },
    }
    self.intents_committed.fetch_add(1, Ordering::Relaxed);
    Ok(outcome)
  }

  /// Release-build sampled audit (spec §7). Debug builds audit inside
  /// `apply_local_intent` unconditionally.
  fn maybe_release_audit(&self, core: &mut CrdtRuntime, intent: &LocalIntent) {
    #[cfg(not(debug_assertions))]
    {
      if let Some(sample) = self.config.release_audit_sample {
        let n = self.intents_committed.load(Ordering::Relaxed);
        // n starts at 0: auditing on `n % sample == 0` would put the O(doc)
        // rebuild-and-compare on the FIRST keystroke of every session (field
        // symptom: one high-latency op per fresh peer). Sample the Nth op.
        if n % std::num::NonZeroU64::from(sample) == u64::from(sample.get()) - 1
          && let Err(error) = core.audit_projection_against_full_rebuild(intent.class())
        {
          tracing::error!(%error, class = intent.class(), "sampled release audit mismatch");
        }
      }
    }
    #[cfg(debug_assertions)]
    {
      let _ = (core, intent, &self.config);
    }
  }

  // ---- Undo (spec §10) ------------------------------------------------------

  /// Execute undo through the Loro `UndoManager`. The manager commits
  /// internally (origin `"undo"`); patches are a full projection replace (undo
  /// can touch arbitrary spans).
  pub fn apply_undo(&self) -> Result<UndoOutcome, WriteRejected> {
    self.undo_redo(SemanticCommand::Undo)
  }

  pub fn apply_redo(&self) -> Result<UndoOutcome, WriteRejected> {
    self.undo_redo(SemanticCommand::Redo)
  }

  fn undo_redo(&self, command: SemanticCommand) -> Result<UndoOutcome, WriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::UndoRedo)
      .map_err(|_| WriteRejected::GatePoisoned)?;
    let events = guard
      .command(command)
      .map_err(|error| WriteRejected::CompensatedFailure {
        class: "undo-redo",
        diagnostic: format!("{error:#}"),
      })?;
    let mut applied = false;
    let mut selection = None;
    let mut publish = Vec::new();
    for event in events {
      match event {
        // Whether expressed as a replace or incrementally: the projection
        // change reaches the editor through the ORDERED stream (already
        // queued by the runtime); the outcome only carries the applied
        // signal. The former shape cloned the ENTIRE projection here per
        // undo — pure waste, the editor never read it.
        RuntimeEvent::ProjectionUpdated { .. } | RuntimeEvent::ProjectionPatched { .. } => applied = true,
        RuntimeEvent::SelectionRestored { selection: restored } => selection = Some(restored),
        RuntimeEvent::RevisionOpened { .. } | RuntimeEvent::RevisionForked { .. } | RuntimeEvent::HistoryRebaseRequired { .. } => {},
        publishable @ (RuntimeEvent::LocalUpdate { .. } | RuntimeEvent::RemoteUpdateApplied { .. }) => publish.push(publishable),
      }
    }
    guard.queue_publish(publish);
    drop(guard);
    Ok(UndoOutcome { applied, selection })
  }

  /// Begin an undo group (word/burst boundary — spec §10). Fallible by design:
  /// a non-disjoint remote import closes the active group underneath us
  /// (semantics-audit F3), and a subsequent `group_start` while one is
  /// notionally open reports an error. Returns whether a fresh group opened;
  /// the editor re-arms at the next boundary either way.
  pub fn begin_undo_group(&self) -> Result<bool, WriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::UndoRedo)
      .map_err(|_| WriteRejected::GatePoisoned)?;
    match guard.undo_manager_mut().group_start() {
      Ok(()) => Ok(true),
      Err(error) => {
        tracing::debug!(%error, "undo group_start declined (group already open or manager not ready); re-arming at next boundary");
        Ok(false)
      },
    }
  }

  pub fn finish_undo_group(&self) -> Result<(), WriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::UndoRedo)
      .map_err(|_| WriteRejected::GatePoisoned)?;
    guard.undo_manager_mut().group_end();
    drop(guard);
    Ok(())
  }
}

/// The one write path, as injected into the editor (spec invariant 5): solo
/// and collaborative documents receive this same authority object.
impl LocalWriteAuthority for LocalDocHandle {
  fn apply(&self, intent: LocalIntent) -> Result<LocalWriteOutcome, WriteRejected> {
    self.apply_intent(intent)
  }

  fn undo(&self) -> Result<UndoOutcome, WriteRejected> {
    self.apply_undo()
  }

  fn redo(&self) -> Result<UndoOutcome, WriteRejected> {
    self.apply_redo()
  }

  fn undo_group_start(&self) -> Result<bool, WriteRejected> {
    self.begin_undo_group()
  }

  fn undo_group_end(&self) -> Result<(), WriteRejected> {
    self.finish_undo_group()
  }

  fn drain_projection_stream(&self) -> Result<Vec<gpui_flowtext::ProjectionStreamItem>, WriteRejected> {
    let mut guard = self
      .core
      .lock(GateHolder::LocalIntent)
      .map_err(|_| WriteRejected::GatePoisoned)?;
    let items = guard.take_editor_stream();
    drop(guard);
    Ok(items)
  }

  fn canonical_projection(&self) -> Result<DocumentProjection, WriteRejected> {
    self.projection()
  }

  fn rebase_selection(
    &self,
    selection: &gpui_flowtext::EditorSelection,
    before: &DocumentProjection,
    editor_frontier: &[u8],
  ) -> Option<gpui_flowtext::EditorSelection> {
    let guard = self.core.lock(GateHolder::LocalIntent).ok()?;
    let rebased = guard.rebase_selection_across_import(selection, before, editor_frontier);
    drop(guard);
    rebased
  }

  fn encode_selection_anchor(&self, selection: &gpui_flowtext::EditorSelection, editor_frontier: &[u8]) -> Option<(Vec<u8>, Vec<u8>)> {
    let guard = self.core.lock(GateHolder::LocalIntent).ok()?;
    // Only encode while the editor is in sync with the core. If a remote import
    // has advanced the core but the editor has not yet drained it, the editor's
    // offsets are in a stale projection; encoding them here would anchor the
    // caret to the wrong body position. Skip → the next drain re-arms (or the
    // rebase fork covers this rare window correctly).
    if guard.doc().state_frontiers().encode() != editor_frontier {
      return None;
    }
    let encoded = guard.encode_selection_anchor(selection);
    drop(guard);
    encoded
  }

  fn resolve_selection_anchor(
    &self,
    head_cursor: &[u8],
    anchor_cursor: &[u8],
  ) -> Option<(flowstate_document::DocumentOffset, flowstate_document::DocumentOffset)> {
    let guard = self.core.lock(GateHolder::LocalIntent).ok()?;
    let resolved = guard.resolve_selection_anchor(head_cursor, anchor_cursor);
    drop(guard);
    resolved
  }
}
