//! `FlowRuntime` — the gate-protected flow document core (flow spec Part B).
//!
//! Owns the `LoroDoc`, the maintained [`FlowBoardProjection`], the per-open-cell
//! materialization cache, the ordered board/cell streams, the publish queue,
//! and the Loro `UndoManager` (origins `"remote"`/`"repair"` excluded, `"meta"`
//! inert — the .db8 configuration verbatim). Every method assumes the caller
//! holds the `WriteGate<FlowRuntime>`.

use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use flowstate_flow::format::{CellId, FlowFormat, SheetId};
use flowstate_flow::loro_projection::{materialize_board, materialize_cell_rows, summary_from_rows};
use flowstate_flow::projection::{CellSummary, FlowBoardProjection};
use flowstate_flow::{FlowDefect, loro_schema};
use gpui_flowtext::{DocumentProjection, InputBlock, ParagraphId, ProjectionStreamItem};
use loro::{ExportMode, InnerUndoManager as UndoManager, LoroDoc, LoroValue, UndoItemMeta, VersionVector, cursor::Cursor};
use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// One item of the ordered BOARD stream: Replace-per-change (board metadata is
/// small; summaries are shared `Arc<str>`s).
#[derive(Clone, Debug)]
pub enum FlowStreamItem {
  Board(Box<FlowBoardProjection>),
}

/// Publish-side events buffered under the gate for the flow I/O service (the
/// flow mirror of the .db8 `RuntimeEvent` publish subset).
#[derive(Clone, Debug)]
pub enum FlowPublishEvent {
  LocalUpdate {
    bytes: Vec<u8>,
    frontier: Vec<u8>,
    version_vector: Vec<u8>,
  },
  RemoteUpdateApplied {
    frontier: Vec<u8>,
    version_vector: Vec<u8>,
  },
}

/// Board-context metadata riding each undo item (`FlowUndoMeta`, spec Part B):
/// restores sheet focus, cell focus, and the cell-text selection through the
/// undo outcome. Cursors are Loro cursor bytes, actively transformed by the
/// `UndoManager` while the item sits on the stack.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct FlowUndoMeta {
  pub active_sheet: Option<Uuid>,
  pub focused_cell: Option<Uuid>,
  pub head_cursor: Option<Vec<u8>>,
  pub anchor_cursor: Option<Vec<u8>>,
}

/// Outcome of an undo/redo (the projection change rides the ordered streams).
#[derive(Clone, Debug, Default)]
pub struct FlowUndoOutcome {
  pub applied: bool,
  pub meta: Option<FlowUndoMeta>,
}

/// The synchronous result of a committed flow intent.
#[derive(Clone, Debug)]
pub struct FlowWriteOutcome {
  /// `false` when the intent resolved to a no-op (nothing committed).
  pub changed: bool,
  pub frontier: Vec<u8>,
  pub version_vector: Vec<u8>,
  /// Cursor-backed caret after a `CellText` intent.
  pub selection_after: Option<gpui_flowtext::SelectionSnapshot>,
}

/// Why a flow intent was rejected before mutation (I-15: rejection means the
/// doc was not touched; compensated failures mean it was restored).
#[derive(Clone, Debug)]
pub enum FlowWriteRejected {
  UnknownSheet(SheetId),
  UnknownCell(CellId),
  UnresolvedParagraph(ParagraphId),
  UnresolvedCursor,
  EmptyIntent,
  StructureViolation(String),
  GatePoisoned,
  CompensatedFailure { class: &'static str, diagnostic: String },
  CompensationFailed { class: &'static str, diagnostic: String },
}

impl std::fmt::Display for FlowWriteRejected {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::UnknownSheet(sheet) => write!(f, "sheet {sheet} did not resolve against canonical flow state"),
      Self::UnknownCell(cell) => write!(f, "cell {cell} did not resolve against canonical flow state"),
      Self::UnresolvedParagraph(id) => write!(f, "cell paragraph identity {} did not resolve", id.0),
      Self::UnresolvedCursor => f.write_str("supplied cursor could not be resolved against the cell's text"),
      Self::EmptyIntent => f.write_str("flow intent is a no-op"),
      Self::StructureViolation(reason) => write!(f, "flow intent violates board structure: {reason}"),
      Self::GatePoisoned => f.write_str("flow write gate poisoned; reload from persisted state"),
      Self::CompensatedFailure { class, diagnostic } => {
        write!(f, "flow intent '{class}' failed mid-apply and was compensated back: {diagnostic}")
      },
      Self::CompensationFailed { class, diagnostic } => {
        write!(f, "flow intent '{class}' failed mid-apply AND compensation failed: {diagnostic}")
      },
    }
  }
}

impl std::error::Error for FlowWriteRejected {}

/// Cached materialization for one OPEN cell (an attached editor): the change
/// detector for import-driven refreshes and the resolution basis for text
/// anchors.
pub(crate) struct CellEntry {
  pub(crate) blocks: Vec<InputBlock>,
  pub(crate) paragraph_ids: Vec<ParagraphId>,
}

#[derive(Default)]
pub(crate) struct FlowUndoMetaState {
  /// Set by the handle before each intent; captured by `on_push`.
  pub(crate) pending: Option<FlowUndoMeta>,
  /// Set by `on_pop` when an undo/redo restores an item's metadata.
  pub(crate) restored: Option<FlowUndoMeta>,
}

pub struct FlowRuntime {
  doc: LoroDoc,
  board: FlowBoardProjection,
  document_id: Uuid,
  pub(crate) cells: FxHashMap<CellId, CellEntry>,
  board_stream: Vec<FlowStreamItem>,
  cell_streams: FxHashMap<CellId, Vec<ProjectionStreamItem>>,
  pending_publish: Vec<FlowPublishEvent>,
  pub(crate) undo: UndoManager,
  undo_meta: Arc<Mutex<FlowUndoMetaState>>,
  /// Latest normalization defects (repair-pass input; capped by stable key).
  pub(crate) defects: Vec<FlowDefect>,
  /// Remaining import-side order-canonicalization repair passes (loop guard).
  order_repairs_remaining: u32,
  /// Batched-import calls served (one per gate hold) — the §6.4 coalescing
  /// counter the pump tests assert against.
  import_batches_served: u64,
}

impl FlowRuntime {
  /// Fresh flow document (solo new-tab path).
  pub fn new(format: &FlowFormat) -> Result<Self> {
    let doc = LoroDoc::new();
    let document_id = loro_schema::init_flow_document(&doc, format)?;
    doc.set_next_commit_origin("meta");
    doc.set_next_commit_message("flow-init");
    doc.commit();
    Self::from_doc(doc, document_id)
  }

  /// Attach to an imported snapshot (open / join). Validates the v2 schema —
  /// the wrong-kind-session and wrong-schema defense.
  pub fn from_snapshot(snapshot: &[u8]) -> Result<Self> {
    let doc = LoroDoc::new();
    loro_schema::configure_flow_text_styles(&doc);
    doc.import(snapshot).context("importing flow snapshot")?;
    let version = loro_schema::read_schema_version(&doc);
    anyhow::ensure!(
      version == Some(loro_schema::FLOW_SCHEMA_VERSION),
      "flow snapshot has schema version {version:?}, expected {} — not a Flowstate .fl0 v2 document",
      loro_schema::FLOW_SCHEMA_VERSION
    );
    let document_id = loro_schema::read_document_id(&doc).context("flow snapshot has no document id")?;
    Self::from_doc(doc, document_id)
  }

  fn from_doc(doc: LoroDoc, document_id: Uuid) -> Result<Self> {
    let (board, defects) = materialize_board(&doc)?;
    let undo = UndoManager::new(doc.inner());
    // Spec §10 (D6) verbatim: undo units are explicit group boundaries.
    undo.set_merge_interval(0);
    undo.set_max_undo_steps(300);
    undo.add_exclude_origin_prefix("remote");
    undo.add_exclude_origin_prefix("repair");
    undo.add_inert_origin_prefix("meta");
    let undo_meta = Arc::new(Mutex::new(FlowUndoMetaState::default()));
    install_undo_meta_callbacks(&undo, &undo_meta);
    Ok(Self {
      doc,
      board,
      document_id,
      cells: FxHashMap::default(),
      board_stream: Vec::new(),
      cell_streams: FxHashMap::default(),
      pending_publish: Vec::new(),
      undo,
      undo_meta,
      defects,
      order_repairs_remaining: 64,
      import_batches_served: 0,
    })
  }

  pub fn doc(&self) -> &LoroDoc {
    &self.doc
  }

  #[must_use]
  pub fn document_id(&self) -> Uuid {
    self.document_id
  }

  #[must_use]
  pub fn board_ref(&self) -> &FlowBoardProjection {
    &self.board
  }

  pub(crate) fn board_mut(&mut self) -> &mut FlowBoardProjection {
    &mut self.board
  }

  pub fn frontier(&self) -> Vec<u8> {
    self.doc.state_frontiers().encode()
  }

  pub fn oplog_version_vector(&self) -> VersionVector {
    self.doc.oplog_vv()
  }

  pub fn export_updates_for(&self, version: &VersionVector) -> Result<Vec<u8>> {
    Ok(self.doc.export(ExportMode::updates(version))?)
  }

  pub fn snapshot(&self) -> Result<Vec<u8>> {
    Ok(self.doc.export(ExportMode::Snapshot)?)
  }

  // ---- Streams + publish queue ---------------------------------------------

  pub(crate) fn push_board_stream(&mut self) {
    self
      .board_stream
      .push(FlowStreamItem::Board(Box::new(self.board.clone())));
  }

  pub fn take_board_stream(&mut self) -> Vec<FlowStreamItem> {
    std::mem::take(&mut self.board_stream)
  }

  pub fn take_cell_stream(&mut self, cell: CellId) -> Vec<ProjectionStreamItem> {
    self
      .cell_streams
      .get_mut(&cell)
      .map(std::mem::take)
      .unwrap_or_default()
  }

  pub fn queue_publish(&mut self, events: Vec<FlowPublishEvent>) {
    self.pending_publish.extend(events);
  }

  pub fn take_pending_publish(&mut self) -> Vec<FlowPublishEvent> {
    std::mem::take(&mut self.pending_publish)
  }

  pub fn has_pending_publish(&self) -> bool {
    !self.pending_publish.is_empty()
  }

  /// Export the covering update since `vv_before` and queue it for publish.
  pub(crate) fn queue_local_update_publish(&mut self, vv_before: &VersionVector) {
    match self.doc.export(ExportMode::updates(vv_before)) {
      Ok(bytes) if !bytes.is_empty() => {
        let event = FlowPublishEvent::LocalUpdate {
          bytes,
          frontier: self.frontier(),
          version_vector: self.doc.state_vv().encode(),
        };
        self.pending_publish.push(event);
      },
      Ok(_) => {},
      Err(error) => {
        // Anti-entropy recovers the update; the committed intent stands.
        tracing::error!(%error, "exporting committed flow update failed; anti-entropy must recover it");
      },
    }
  }

  // ---- Open-cell lifecycle ---------------------------------------------------

  /// Materialize (and start streaming) one cell's rich-text projection — the
  /// editor-attach path. Idempotent per cell.
  pub fn open_cell(&mut self, cell: CellId) -> Result<DocumentProjection> {
    let rows = materialize_cell_rows(&self.doc, cell)?;
    let projection = flowstate_flow::loro_projection::document_from_rows(
      &self.doc,
      flowstate_document::RegionRows {
        blocks: rows.blocks.clone(),
        paragraph_ids: rows.paragraph_ids.clone(),
        block_ids: rows.block_ids.clone(),
        defects: Vec::new(),
      },
      flowstate_document::DocumentTheme::clone(&flowstate_document::flowstate_document_theme()),
    );
    self.cells.insert(
      cell,
      CellEntry {
        blocks: rows.blocks,
        paragraph_ids: rows.paragraph_ids,
      },
    );
    self.cell_streams.entry(cell).or_default();
    Ok(projection)
  }

  /// Drop the cell's stream + cache (editor detached).
  pub fn close_cell(&mut self, cell: CellId) {
    self.cells.remove(&cell);
    self.cell_streams.remove(&cell);
  }

  #[must_use]
  pub fn is_cell_open(&self, cell: CellId) -> bool {
    self.cells.contains_key(&cell)
  }

  /// Rematerialize one OPEN cell; on change, update the cache, push a whole-
  /// cell `Replace` onto its stream, and refresh its board summary in place.
  /// Returns whether the cell's SUMMARY changed (the board-stream trigger).
  pub(crate) fn refresh_cell(&mut self, cell: CellId) -> Result<bool> {
    let Some(entry) = self.cells.get(&cell) else {
      // Not open: only the summary can be stale.
      return self.refresh_board_summary(cell);
    };
    let rows = materialize_cell_rows(&self.doc, cell)?;
    if rows.blocks == entry.blocks && rows.paragraph_ids == entry.paragraph_ids {
      return Ok(false);
    }
    let summary_changed = self.set_board_summary(cell, summary_from_rows(&rows.blocks));
    let projection = flowstate_flow::loro_projection::document_from_rows(
      &self.doc,
      flowstate_document::RegionRows {
        blocks: rows.blocks.clone(),
        paragraph_ids: rows.paragraph_ids.clone(),
        block_ids: rows.block_ids.clone(),
        defects: Vec::new(),
      },
      flowstate_document::DocumentTheme::clone(&flowstate_document::flowstate_document_theme()),
    );
    self.cells.insert(
      cell,
      CellEntry {
        blocks: rows.blocks,
        paragraph_ids: rows.paragraph_ids,
      },
    );
    self
      .cell_streams
      .entry(cell)
      .or_default()
      .push(ProjectionStreamItem::Replace(Box::new(projection)));
    Ok(summary_changed)
  }

  /// Refresh only the board-side summary of a (possibly closed) cell.
  pub(crate) fn refresh_board_summary(&mut self, cell: CellId) -> Result<bool> {
    let rows = materialize_cell_rows(&self.doc, cell)?;
    Ok(self.set_board_summary(cell, summary_from_rows(&rows.blocks)))
  }

  fn set_board_summary(&mut self, cell: CellId, summary: CellSummary) -> bool {
    for sheet in &mut self.board.sheets {
      if let Some(found) = sheet.cells.iter_mut().find(|candidate| candidate.id == cell) {
        if found.summary == summary {
          return false;
        }
        found.summary = summary;
        return true;
      }
    }
    false
  }

  // ---- Whole-board refresh (imports, undo, compensation) --------------------

  /// Rebuild the board from canonical state, refresh every open cell, and push
  /// Replace items on every affected stream. The import/undo derivation path.
  pub(crate) fn refresh_all(&mut self) -> Result<()> {
    let (board, defects) = materialize_board(&self.doc)?;
    self.defects = defects;
    self.board = board;
    self.push_board_stream();
    let open: Vec<CellId> = self.cells.keys().copied().collect();
    for cell in open {
      let live = self.board.cell(cell).is_some();
      if live {
        // `refresh_cell` re-derives the summary; the fresh board already has
        // it, so only the cell stream side effect matters here.
        let _ = self.refresh_cell(cell)?;
      } else {
        // Cell deleted remotely: the board Replace already tells the editor;
        // drop the dead stream + cache.
        self.close_cell(cell);
      }
    }
    Ok(())
  }

  /// Debug audit (spec §7 shape): the in-place board maintained by the intent
  /// executors must equal an independent fresh materialization.
  #[cfg(debug_assertions)]
  pub(crate) fn audit_board_against_rebuild(&self, class: &str) {
    match materialize_board(&self.doc) {
      Ok((fresh, _)) => {
        if fresh != self.board {
          panic!(
            "flow intent '{class}' left the maintained board out of sync with a fresh materialization:\n{}",
            board_divergence(&self.board, &fresh)
          );
        }
      },
      Err(error) => panic!("flow board audit rematerialization failed after '{class}': {error:#}"),
    }
  }

  // ---- Remote import ---------------------------------------------------------

  /// Import remote update chunks under ONE gate hold with ONE derivation.
  pub fn import_remote_updates(&mut self, chunks: &[&[u8]]) -> Result<()> {
    if chunks.is_empty() {
      return Ok(());
    }
    self.import_batches_served += 1;
    let frontier_before = self.doc.state_frontiers();
    for chunk in chunks {
      self.doc.import(chunk).context("importing remote flow update")?;
    }
    if self.doc.state_frontiers() == frontier_before {
      return Ok(());
    }
    self.refresh_all()?;
    // Capped canonicalization repair (spec Part A): when the merge left the
    // raw order lists in a non-canonical interleaving, rewrite them (origin
    // "repair") to the canonical linearization every peer already renders.
    // Deterministic-target rewrites converge; the cap stops any pathological
    // repair ping-pong from spinning.
    if self.order_repairs_remaining > 0 {
      match super::commit::repair_canonical_orders(self) {
        Ok(true) => self.order_repairs_remaining -= 1,
        Ok(false) => {},
        Err(error) => tracing::error!(%error, "flow order canonicalization repair failed"),
      }
    }
    let event = FlowPublishEvent::RemoteUpdateApplied {
      frontier: self.frontier(),
      version_vector: self.doc.state_vv().encode(),
    };
    self.pending_publish.push(event);
    Ok(())
  }

  // ---- Undo / redo -----------------------------------------------------------

  pub(crate) fn set_pending_undo_meta(&mut self, meta: Option<FlowUndoMeta>) {
    if let Ok(mut state) = self.undo_meta.lock() {
      state.pending = meta;
    }
  }

  pub fn undo(&mut self) -> Result<FlowUndoOutcome> {
    self.undo_redo_step(true)
  }

  pub fn redo(&mut self) -> Result<FlowUndoOutcome> {
    self.undo_redo_step(false)
  }

  fn undo_redo_step(&mut self, undo: bool) -> Result<FlowUndoOutcome> {
    let vv_before = self.doc.oplog_vv();
    let applied = if undo {
      self.undo.undo().context("applying flow undo")?
    } else {
      self.undo.redo().context("applying flow redo")?
    };
    if !applied {
      return Ok(FlowUndoOutcome::default());
    }
    self.refresh_all()?;
    self.queue_local_update_publish(&vv_before);
    let meta = self
      .undo_meta
      .lock()
      .ok()
      .and_then(|mut state| state.restored.take());
    Ok(FlowUndoOutcome { applied: true, meta })
  }

  pub fn undo_group_start(&mut self) -> bool {
    match self.undo.group_start() {
      Ok(()) => true,
      Err(error) => {
        tracing::debug!(%error, "flow undo group_start declined; re-arming at next boundary");
        false
      },
    }
  }

  pub fn undo_group_end(&mut self) {
    self.undo.group_end();
  }

  #[must_use]
  pub fn can_undo(&self) -> bool {
    self.undo.can_undo()
  }

  #[must_use]
  pub fn can_redo(&self) -> bool {
    self.undo.can_redo()
  }

  #[must_use]
  pub fn import_batches_served(&self) -> u64 {
    self.import_batches_served
  }
}

/// Debug-audit diagnostics: the first field-level difference between the
/// maintained and freshly-materialized boards.
#[cfg(debug_assertions)]
fn board_divergence(maintained: &FlowBoardProjection, fresh: &FlowBoardProjection) -> String {
  if maintained.format != fresh.format {
    return "format differs".to_string();
  }
  if maintained.sheets.len() != fresh.sheets.len() {
    return format!("sheet count {} vs fresh {}", maintained.sheets.len(), fresh.sheets.len());
  }
  for (ours, theirs) in maintained.sheets.iter().zip(&fresh.sheets) {
    if ours.id != theirs.id || ours.name != theirs.name || ours.sheet_type_id != theirs.sheet_type_id {
      return format!("sheet header {:?} vs fresh {:?}", (ours.id, &ours.name), (theirs.id, &theirs.name));
    }
    if ours.annotations != theirs.annotations {
      return format!("sheet {} annotations differ", ours.id);
    }
    let our_ids: Vec<_> = ours.cells.iter().map(|cell| cell.id).collect();
    let their_ids: Vec<_> = theirs.cells.iter().map(|cell| cell.id).collect();
    if our_ids != their_ids {
      return format!("sheet {} order {our_ids:?} vs fresh {their_ids:?}", ours.id);
    }
    for (our_cell, their_cell) in ours.cells.iter().zip(&theirs.cells) {
      if our_cell != their_cell {
        return format!("cell {} maintained {our_cell:?} vs fresh {their_cell:?}", our_cell.id);
      }
    }
  }
  "boards differ in an uncompared field".to_string()
}

fn install_undo_meta_callbacks(undo: &UndoManager, state: &Arc<Mutex<FlowUndoMetaState>>) {
  let push_state = Arc::clone(state);
  undo.set_on_push(Some(Box::new(move |_, _, _| {
    let mut meta = UndoItemMeta::new();
    if let Ok(state) = push_state.lock()
      && let Some(pending) = &state.pending
    {
      // Cursors ride the first-class `cursors` field so Loro transforms them
      // through remote diffs while the item sits on the stack (spec §10).
      for encoded in [pending.head_cursor.as_deref(), pending.anchor_cursor.as_deref()]
        .into_iter()
        .flatten()
      {
        if let Ok(cursor) = Cursor::decode(encoded) {
          meta.add_cursor(&cursor);
        }
      }
      if let Ok(bytes) = postcard::to_allocvec(pending) {
        meta.set_value(LoroValue::Binary(bytes.into()));
      }
    }
    meta
  })));

  let pop_state = Arc::clone(state);
  undo.set_on_pop(Some(Box::new(move |_, _, meta| {
    let transformed: Vec<Vec<u8>> = meta
      .cursors
      .iter()
      .map(|entry| entry.cursor.encode())
      .collect();
    let LoroValue::Binary(bytes) = meta.value else {
      return;
    };
    match postcard::from_bytes::<FlowUndoMeta>(bytes.as_ref()) {
      Ok(mut restored) => {
        // Prefer the stack-transformed cursors over the capture-time bytes.
        let had_head = restored.head_cursor.is_some();
        let had_anchor = restored.anchor_cursor.is_some();
        let mut transformed = transformed.into_iter();
        if had_head && let Some(head) = transformed.next() {
          restored.head_cursor = Some(head);
        }
        if had_anchor && let Some(anchor) = transformed.next() {
          restored.anchor_cursor = Some(anchor);
        }
        if let Ok(mut state) = pop_state.lock() {
          state.restored = Some(restored);
        }
      },
      Err(error) => {
        tracing::warn!(%error, "decoding flow undo metadata failed");
      },
    }
  })));
}
