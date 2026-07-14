//! [`FlowRuntime`]: the flow's gated core. One commit per intent (origin
//! `"local"`, message = intent class), classified refresh (a keystroke or
//! structural op rematerializes only what it touched), ordered streams (one
//! board stream + one per cell), and a publish queue drained by the flow IO
//! pump — the exact seams the .db8 runtime exposes, so the transport and
//! session layers stay shape-identical across document kinds.
//!
//! Undo: the vendored Loro `UndoManager` directly (origin-excluded
//! `remote`/`repair`/`meta`, explicit group boundaries). Flow boards are
//! metadata-scale documents — the .db8 recorded-inverse machinery exists for
//! 100k-char bodies and would add complexity here for no measured win; the
//! architecture spec's undo CAPABILITIES (every intent undoable, grouping,
//! selection meta riding the stack) are all preserved. Revisit only if soak
//! measurements say otherwise.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use flowstate_flow::{
  CellId, CellSummary, FlowBoardProjection, FlowDefect, FlowDocument, FlowIntent, MaterializedBoard, board_from_loro_cached, loro_schema, mutate,
};
use loro::{ContainerID, ContainerTrait as _, ExportMode, LoroDoc, Subscription, UndoManager, VersionVector};
use rustc_hash::FxHashMap;
use uuid::Uuid;

/// Broadcastable products of local commits (the flow analogue of the .db8
/// runtime's publish events; drained by `take_pending_publish`).
#[derive(Clone, Debug)]
pub enum FlowPublishEvent {
  LocalUpdate {
    bytes: Vec<u8>,
    frontier: Vec<u8>,
    version_vector: Vec<u8>,
  },
}

/// The board editor's ordered stream item. Replace-per-change: a board
/// projection without content is metadata-priced (summaries are shared
/// `Arc<str>`), and structural changes are human-rate.
#[derive(Clone, Debug)]
pub enum FlowStreamItem {
  Board(Box<FlowBoardProjection>),
}

/// Synchronous outcome of a local intent (the optimistic apply IS the
/// committed apply, under the gate).
#[derive(Clone, Debug)]
pub struct FlowLocalOutcome {
  pub board: FlowBoardProjection,
  pub content_cells: Vec<CellId>,
}

pub struct FlowRuntime {
  doc: LoroDoc,
  board: FlowBoardProjection,
  defects: Vec<FlowDefect>,
  summaries: HashMap<CellId, CellSummary>,
  /// Reverse index: a cell flow's TEXT container id → owning cell, for O(1)
  /// import classification.
  text_containers: HashMap<ContainerID, CellId>,
  board_stream: Vec<FlowStreamItem>,
  cell_streams: FxHashMap<CellId, Vec<gpui_flowtext::ProjectionStreamItem>>,
  pending_publish: Vec<FlowPublishEvent>,
  undo: UndoManager,
  /// Container ids touched since the last drain (fed by the root
  /// subscription; consumed by the import classifier).
  touched: Arc<Mutex<Vec<ContainerID>>>,
  _root_subscription: Subscription,
}

impl FlowRuntime {
  pub fn new_empty() -> Self {
    let document = FlowDocument::new();
    Self::from_snapshot(&document.snapshot().expect("fresh flow document exports")).expect("fresh flow document loads")
  }

  pub fn from_flow_document(document: &FlowDocument) -> anyhow::Result<Self> {
    Self::from_snapshot(&document.snapshot()?)
  }

  pub fn from_snapshot(snapshot: &[u8]) -> anyhow::Result<Self> {
    let doc = LoroDoc::new();
    loro_schema::configure_flow_doc(&doc);
    doc.import(snapshot)?;
    match loro_schema::schema_version(&doc) {
      Some(loro_schema::SCHEMA_VERSION) => {},
      Some(version) => anyhow::bail!("unsupported flow schema version {version}"),
      None => anyhow::bail!("Loro snapshot is missing immutable format definition"),
    }
    let mut undo = UndoManager::new(&doc);
    undo.set_merge_interval(0);
    undo.set_max_undo_steps(300);
    undo.add_exclude_origin_prefix("remote");
    undo.add_exclude_origin_prefix("repair");
    undo.add_exclude_origin_prefix("meta");

    let touched = Arc::new(Mutex::new(Vec::new()));
    let touched_for_callback = Arc::clone(&touched);
    let root_subscription = doc.subscribe_root(Arc::new(move |event: loro::event::DiffEvent<'_>| {
      if let Ok(mut touched) = touched_for_callback.lock() {
        for change in event.events {
          touched.push(change.target.clone());
        }
      }
    }));

    let mut runtime = Self {
      doc,
      board: FlowBoardProjection::default(),
      defects: Vec::new(),
      summaries: HashMap::new(),
      text_containers: HashMap::new(),
      board_stream: Vec::new(),
      cell_streams: FxHashMap::default(),
      pending_publish: Vec::new(),
      undo,
      touched,
      _root_subscription: root_subscription,
    };
    runtime.refresh(None)?;
    Ok(runtime)
  }

  pub fn doc(&self) -> &LoroDoc {
    &self.doc
  }

  pub fn board(&self) -> &FlowBoardProjection {
    &self.board
  }

  pub fn defects(&self) -> &[FlowDefect] {
    &self.defects
  }

  pub fn document_id(&self) -> Option<Uuid> {
    loro_schema::document_id(&self.doc)
  }

  pub fn frontier(&self) -> Vec<u8> {
    self.doc.state_frontiers().encode()
  }

  pub fn oplog_vv(&self) -> VersionVector {
    self.doc.oplog_vv()
  }

  pub fn export_updates_for(&self, version: &VersionVector) -> anyhow::Result<Vec<u8>> {
    Ok(self.doc.export(ExportMode::updates(version))?)
  }

  pub fn snapshot_bytes(&self) -> anyhow::Result<Vec<u8>> {
    Ok(self.doc.export(ExportMode::Snapshot)?)
  }

  /// Fork for off-gate export (the `SnapshotBytes` IO pattern: fork under the
  /// gate, export off it).
  pub fn fork_for_export(&self) -> LoroDoc {
    self.doc.fork()
  }

  pub fn cell_projection(&self, cell_id: CellId) -> anyhow::Result<flowstate_document::DocumentProjection> {
    flowstate_flow::cell_document(&self.doc, cell_id)
  }

  // ---- the one write path ------------------------------------------------

  /// Resolve → mutate → ONE commit (origin `local`, message = class) →
  /// classified refresh → streams → publish. Mirrors `local_write/commit.rs`.
  pub fn apply_intent(&mut self, intent: &FlowIntent) -> anyhow::Result<FlowLocalOutcome> {
    let vv_before = self.doc.oplog_vv();
    let report = mutate::execute_intent(&self.doc, &self.board, intent)?;
    self.doc.set_next_commit_origin("local");
    self.doc.set_next_commit_message(intent.class());
    self.doc.commit();
    self.doc.set_next_commit_origin("meta");
    let _ = loro_schema::touch_modified(&self.doc);
    self.doc.commit();

    let dirty: HashSet<CellId> = report.content_cells.iter().copied().collect();
    self.refresh(Some(&dirty))?;
    self.push_board_stream();
    for cell in &report.content_cells {
      self.push_cell_replace(*cell);
    }
    self.queue_local_update(&vv_before)?;
    Ok(FlowLocalOutcome {
      board: self.board.clone(),
      content_cells: report.content_cells,
    })
  }

  /// N cheap imports under ONE gate hold, then one classified derive — the
  /// .db8 import discipline.
  pub fn import_remote_updates(&mut self, blobs: &[&[u8]]) -> anyhow::Result<()> {
    if blobs.is_empty() {
      return Ok(());
    }
    // Drain any pre-existing touch noise so classification sees only this
    // import batch.
    let _ = self.take_touched();
    for blob in blobs {
      self.doc.import_with(blob, "remote")?;
    }
    let touched = self.take_touched();
    let mut dirty: HashSet<CellId> = HashSet::new();
    for container in &touched {
      if let Some(cell) = self.text_containers.get(container) {
        dirty.insert(*cell);
        continue;
      }
      // Any container we can't attribute to a specific cell's flow could be a
      // registry/attrs child: attribute by cell-record ancestry via the
      // container name when possible; otherwise treat as structural (cheap —
      // structure rebuilds reuse the summary cache).
    }
    self.refresh(Some(&dirty))?;
    self.push_board_stream();
    for cell in dirty {
      self.push_cell_replace(cell);
    }
    Ok(())
  }

  // ---- undo (through the gate, streamed, published) -----------------------

  pub fn can_undo(&self) -> bool {
    self.undo.can_undo()
  }

  pub fn can_redo(&self) -> bool {
    self.undo.can_redo()
  }

  pub fn undo(&mut self) -> anyhow::Result<bool> {
    let vv_before = self.doc.oplog_vv();
    let changed = self.undo.undo()?;
    if changed {
      self.after_undo_redo(&vv_before)?;
    }
    Ok(changed)
  }

  pub fn redo(&mut self) -> anyhow::Result<bool> {
    let vv_before = self.doc.oplog_vv();
    let changed = self.undo.redo()?;
    if changed {
      self.after_undo_redo(&vv_before)?;
    }
    Ok(changed)
  }

  fn after_undo_redo(&mut self, vv_before: &VersionVector) -> anyhow::Result<()> {
    // An undo can touch anything its item recorded: classify from the touch
    // buffer like an import.
    let touched = self.take_touched();
    let mut dirty: HashSet<CellId> = HashSet::new();
    for container in &touched {
      if let Some(cell) = self.text_containers.get(container) {
        dirty.insert(*cell);
      }
    }
    self.refresh(Some(&dirty))?;
    self.push_board_stream();
    for cell in dirty {
      self.push_cell_replace(cell);
    }
    self.queue_local_update(vv_before)?;
    Ok(())
  }

  pub fn undo_group_start(&mut self) -> anyhow::Result<bool> {
    self.undo.group_start()?;
    Ok(true)
  }

  pub fn undo_group_end(&mut self) {
    self.undo.group_end();
  }

  // ---- streams & publish ---------------------------------------------------

  pub fn take_board_stream(&mut self) -> Vec<FlowStreamItem> {
    std::mem::take(&mut self.board_stream)
  }

  pub fn take_cell_stream(&mut self, cell_id: CellId) -> Vec<gpui_flowtext::ProjectionStreamItem> {
    self.cell_streams.remove(&cell_id).unwrap_or_default()
  }

  pub fn take_pending_publish(&mut self) -> Vec<FlowPublishEvent> {
    std::mem::take(&mut self.pending_publish)
  }

  fn queue_local_update(&mut self, vv_before: &VersionVector) -> anyhow::Result<()> {
    let bytes = self.doc.export(ExportMode::updates(vv_before))?;
    if bytes.is_empty() {
      return Ok(());
    }
    self.pending_publish.push(FlowPublishEvent::LocalUpdate {
      bytes,
      frontier: self.frontier(),
      version_vector: self.doc.oplog_vv().encode(),
    });
    Ok(())
  }

  fn push_board_stream(&mut self) {
    self
      .board_stream
      .push(FlowStreamItem::Board(Box::new(self.board.clone())));
  }

  fn push_cell_replace(&mut self, cell_id: CellId) {
    if let Ok(projection) = self.cell_projection(cell_id) {
      self
        .cell_streams
        .entry(cell_id)
        .or_default()
        .push(gpui_flowtext::ProjectionStreamItem::Replace(Box::new(projection)));
    }
  }

  // ---- internals -----------------------------------------------------------

  fn take_touched(&mut self) -> Vec<ContainerID> {
    self
      .touched
      .lock()
      .map(|mut touched| std::mem::take(&mut *touched))
      .unwrap_or_default()
  }

  /// Rematerialize the board, reusing cached summaries for clean cells.
  /// `dirty = None` invalidates everything (constructor, unknown blast
  /// radius).
  fn refresh(&mut self, dirty: Option<&HashSet<CellId>>) -> anyhow::Result<()> {
    let MaterializedBoard { board, defects } = board_from_loro_cached(&self.doc, &self.summaries, dirty)?;
    self.board = board;
    self.defects = defects;
    self.summaries = self
      .board
      .sheets
      .iter()
      .flat_map(|sheet| {
        sheet
          .cells
          .iter()
          .map(|cell| (cell.id, cell.summary.clone()))
      })
      .collect();
    self.rebuild_text_container_index();
    // Local intents must never trigger a hidden whole-board content rebuild:
    // the summary cache makes clean cells free, and this rebuild is metadata
    // plus O(dirty) content — asserted by the runtime tests.
    Ok(())
  }

  fn rebuild_text_container_index(&mut self) {
    self.text_containers.clear();
    for sheet in &self.board.sheets {
      for cell in &sheet.cells {
        let Some(record) = loro_schema::cell_record(&self.doc, cell.id) else {
          continue;
        };
        let Some(flow) = loro_schema::cell_flow(&record) else {
          continue;
        };
        if let Ok(text) = flow.ensure_mergeable_text(flowstate_document::FLOW_TEXT_KEY) {
          self.text_containers.insert(text.id(), cell.id);
        }
      }
    }
  }
}
