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
use loro::{ContainerID, ContainerTrait as _, ExportMode, Frontiers, LoroDoc, Subscription, UndoManager, VersionVector};
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
  /// Attempt counts for the cell-registry repair pass, keyed by repaired
  /// record key — a defect the repair cannot clear is quarantined after the
  /// cap instead of looping (the body's per-`stable_key` discipline).
  repair_attempts: HashMap<String, u8>,
  /// Batched-import calls served (a coalesced chunk of N blobs counts once) —
  /// the §6.4 coalescing observability counter, mirroring `CrdtRuntime`'s.
  import_batches_served: u64,
  /// True between `undo_group_start`/`undo_group_end`: the excluded "meta"
  /// modified-stamp commit is deferred to group end so grouped members stay
  /// counter-contiguous (the group-merge law).
  undo_group_active: bool,
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
      repair_attempts: HashMap::new(),
      import_batches_served: 0,
      undo_group_active: false,
      _root_subscription: root_subscription,
    };
    runtime.refresh(None)?;
    Ok(runtime)
  }

  pub fn doc(&self) -> &LoroDoc {
    &self.doc
  }

  pub fn import_batches_served(&self) -> u64 {
    self.import_batches_served
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

  /// R6 scrubber: a READ-ONLY historical board at `fraction` (0..=1) of the
  /// lamport-ordered change timeline. Works on a fork — the live doc, its
  /// undo stacks, and its subscriptions are untouched. Returns the board plus
  /// (ops shown, total ops) for the slider annotation.
  /// The lamport-ordered change list the replay timeline scrubs over — one
  /// canonical ordering shared by fraction checkout and mark positioning.
  fn history_timeline_changes(fork: &LoroDoc) -> anyhow::Result<Vec<(u32, loro::ID, usize)>> {
    let heads = fork.oplog_frontiers();
    let mut changes: Vec<(u32, loro::ID, usize)> = Vec::new();
    let head_ids: Vec<loro::ID> = heads.iter().collect();
    fork
      .travel_change_ancestors(&head_ids, &mut |meta| {
        changes.push((meta.lamport, meta.id, meta.len));
        std::ops::ControlFlow::Continue(())
      })
      .map_err(|error| anyhow::anyhow!("traversing flow history failed: {error}"))?;
    // Lamport order respects causality: any prefix is causally closed.
    changes.sort_by_key(|(lamport, id, _)| (*lamport, id.peer));
    Ok(changes)
  }

  /// H-S6 tape marks: each frontier's position (0.0..=1.0) along the SAME
  /// lamport-ordered op timeline the replay scrubs — where a checkpoint's
  /// mark sits on the tape. `None` for frontiers that fail to decode.
  pub fn history_timeline_positions(&self, frontiers: &[Vec<u8>]) -> anyhow::Result<Vec<Option<f32>>> {
    let fork = self.doc.fork();
    let changes = Self::history_timeline_changes(&fork)?;
    let total_ops: usize = changes.iter().map(|(_, _, len)| *len).sum();
    if total_ops == 0 {
      return Ok(frontiers.iter().map(|_| Some(0.0)).collect());
    }
    Ok(
      frontiers
        .iter()
        .map(|encoded| {
          let decoded = Frontiers::decode(encoded).ok()?;
          let vv = fork.frontiers_to_vv(&decoded)?;
          let included: usize = changes
            .iter()
            .map(|(_, id, len)| {
              let end = vv.get(&id.peer).copied().unwrap_or(0);
              usize::try_from((end - id.counter).max(0)).unwrap_or(0).min(*len)
            })
            .sum();
          Some((included as f64 / total_ops as f64) as f32)
        })
        .collect(),
    )
  }

  /// Returns (board, shown ops, total ops, encoded frontier of the checked-out
  /// position) — the frontier is what Restore consumes, so a restore works
  /// from ANY thumb position, not just at a checkpoint mark.
  pub fn history_board_at(&self, fraction: f32) -> anyhow::Result<(FlowBoardProjection, usize, usize, Vec<u8>)> {
    let fork = self.doc.fork();
    let changes = Self::history_timeline_changes(&fork)?;
    let total_ops: usize = changes.iter().map(|(_, _, len)| *len).sum();
    let target_ops = (f64::from(fraction.clamp(0.0, 1.0)) * total_ops as f64).round() as usize;
    let mut vv = VersionVector::default();
    let mut included = 0_usize;
    for (_, id, len) in &changes {
      if included >= target_ops {
        break;
      }
      // Loro merges consecutive same-peer commits into one storage Change, so
      // commit-level scrubbing must cut INSIDE a change: ops within a change
      // are sequential, so a per-op prefix stays causally sound (checkout
      // closes over the frontier heads' causal past anyway).
      let take = (*len).min(target_ops - included);
      included += take;
      let end = id.counter + i32::try_from(take).unwrap_or(i32::MAX);
      if vv.get(&id.peer).copied().unwrap_or(0) < end {
        vv.insert(id.peer, end);
      }
    }
    let frontiers = fork.vv_to_frontiers(&vv);
    fork
      .checkout(&frontiers)
      .map_err(|error| anyhow::anyhow!("checking out flow history failed: {error}"))?;
    let board = flowstate_flow::board_from_loro(&fork)?;
    Ok((board.board, included.min(total_ops), total_ops, frontiers.encode()))
  }

  pub fn cell_projection(&self, cell_id: CellId) -> anyhow::Result<flowstate_document::DocumentProjection> {
    flowstate_flow::cell_document(&self.doc, cell_id)
  }

  /// H-K0 keystone (flow mirror): a READ-ONLY board at an arbitrary encoded
  /// frontier. Same fork-don't-touch-the-live-doc law as `history_board_at`,
  /// but keyed by frontier instead of a timeline fraction — flow history pins,
  /// restore preview, and flow comment orphan-jump all consume this.
  pub fn board_at_frontier(&self, frontier: &[u8]) -> anyhow::Result<FlowBoardProjection> {
    let frontiers =
      loro::Frontiers::decode(frontier).map_err(|error| anyhow::anyhow!("decoding flow frontier blob failed: {error}"))?;
    let fork = self.doc.fork();
    fork
      .checkout(&frontiers)
      .map_err(|error| anyhow::anyhow!("checking out flow frontier failed: {error}"))?;
    Ok(flowstate_flow::board_from_loro(&fork)?.board)
  }

  // ---- the one write path ------------------------------------------------

  /// Resolve → mutate → ONE commit (origin `local`, message = class) →
  /// classified refresh → streams → publish. Mirrors `local_write/commit.rs`.
  pub fn apply_intent(&mut self, intent: &FlowIntent) -> anyhow::Result<FlowLocalOutcome> {
    if let FlowIntent::CellText { cell_id, intent } = intent {
      self
        .apply_cell_text(*cell_id, intent)
        .map_err(|rejection| anyhow::anyhow!(rejection.to_string()))?;
      return Ok(FlowLocalOutcome {
        board: self.board.clone(),
        content_cells: vec![*cell_id],
      });
    }
    let vv_before = self.doc.oplog_vv();
    let frontier_before = self.doc.state_frontiers();
    let report = match mutate::execute_intent(&self.doc, &self.board, intent) {
      Ok(report) => report,
      Err(rejection) => {
        // I-10 compensation: a compound structural intent may have partially
        // applied before the failure — never let uncommitted partials leak
        // into the NEXT commit. Converge back to the pre-intent frontier.
        self.doc.set_next_commit_origin("repair");
        self.doc.commit();
        if self.doc.state_frontiers() != frontier_before {
          let _ = self.doc.revert_to(&frontier_before);
          self.doc.set_next_commit_origin("repair");
          self.doc.commit();
          let _ = self.refresh(None);
          // The partial + revert ops entered the oplog; later exports start
          // AFTER them, so an unpublished compensation is a permanent causal
          // gap on every peer. Publish it like any local commit.
          let _ = self.queue_local_update(&vv_before);
        }
        let _ = self.take_touched();
        return Err(rejection);
      },
    };
    self.doc.set_next_commit_origin("local");
    self.doc.set_next_commit_message(intent.class());
    self.doc.commit();
    // Undo-group law: the group merges by COUNTER CONTIGUITY, so the excluded
    // "meta" modified-stamp commit must never land between grouped members —
    // it would open a counter gap exactly when the wall clock ticks a new
    // timestamp value (a once-per-millisecond heisenbug caught by the S8
    // grouped-strike probe). Inside a group the touch defers to `group_end`.
    if !self.undo_group_active {
      self.touch_modified_meta();
    }

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

  /// A cell-text intent: minimal ops on the cell's flow (char-level remote
  /// merging), one commit, compensation via revert on mid-apply failure.
  pub fn apply_cell_text(
    &mut self,
    cell_id: CellId,
    intent: &gpui_flowtext::LocalIntent,
  ) -> Result<super::cell_authority::CellTextCommit, gpui_flowtext::WriteRejected> {
    let vv_before = self.doc.oplog_vv();
    let frontier_before = self.doc.state_frontiers();
    let caret_pos = match super::cell_text::execute_cell_text(&self.doc, cell_id, intent) {
      Ok(caret_pos) => caret_pos,
      Err(rejection) => {
        // Compound intents may have partially applied: converge back.
        self.doc.set_next_commit_origin("repair");
        self.doc.commit();
        if self.doc.state_frontiers() != frontier_before {
          let _ = self.doc.revert_to(&frontier_before);
          self.doc.set_next_commit_origin("repair");
          self.doc.commit();
          let _ = self.refresh(None);
          // Publish the compensation: later exports start after these ops, so
          // leaving them unpublished is a permanent causal gap on every peer.
          let _ = self.queue_local_update(&vv_before);
        }
        return Err(rejection);
      },
    };
    self.doc.set_next_commit_origin("local");
    self
      .doc
      .set_next_commit_message(&format!("cell.{}", intent.class()));
    self.doc.commit();

    let mut dirty = HashSet::new();
    dirty.insert(cell_id);
    self
      .refresh(Some(&dirty))
      .map_err(|_| gpui_flowtext::WriteRejected::StructureViolation("flow refresh failed"))?;
    self.push_board_stream();
    // The authority receives the replace synchronously; OTHER consumers (a
    // second editor on the same cell) still get the stream item.
    self.push_cell_replace(cell_id);
    self
      .queue_local_update(&vv_before)
      .map_err(|_| gpui_flowtext::WriteRejected::StructureViolation("flow publish failed"))?;
    let projection = self
      .cell_projection(cell_id)
      .map_err(|_| gpui_flowtext::WriteRejected::StructureViolation("flow cell projection unavailable"))?;
    // Anchor the post-edit caret on a Loro cursor so it survives concurrent
    // remote edits exactly (spec §8), mirroring the body's `selection_after`.
    let caret = caret_pos.and_then(|flow_pos| {
      let text = super::cell_authority::FlowCellAuthority::cell_text(&self.doc, cell_id)?;
      let cursor = text.get_cursor(flow_pos, loro::cursor::Side::Left)?;
      Some((flow_pos, cursor.encode()))
    });
    Ok(super::cell_authority::CellTextCommit {
      replace: gpui_flowtext::ProjectionReplace {
        document: projection,
        frontier: self.frontier(),
        version_vector: self.doc.oplog_vv().encode(),
      },
      caret,
    })
  }

  /// N cheap imports under ONE gate hold, then one classified derive — the
  /// .db8 import discipline.
  pub fn import_remote_updates(&mut self, blobs: &[&[u8]]) -> anyhow::Result<()> {
    if blobs.is_empty() {
      return Ok(());
    }
    self.import_batches_served += 1;
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
    // The import itself never writes repairs; a follow-up LOCAL canonicalization
    // commit re-mints registry records the merge left unresolvable. Imports
    // publish nothing themselves, so the repair commit exports its own delta.
    let vv_pre_repair = self.doc.oplog_vv();
    if self.repair_cell_registries(&dirty) {
      self.queue_local_update(&vv_pre_repair)?;
    }
    self.refresh(Some(&dirty))?;
    // Merge artifacts the normalizer repaired in projection space (phantom
    // rows, D2 bumps) converge canonically here; the repair delta publishes
    // its own window like any local commit.
    let vv_pre_grid_repair = self.doc.oplog_vv();
    if self.repair_grid_structure() {
      self.queue_local_update(&vv_pre_grid_repair)?;
      self.refresh(Some(&HashSet::new()))?;
    }
    self.push_board_stream();
    for cell in dirty {
      self.push_cell_replace(cell);
    }
    Ok(())
  }

  /// The spec's capped, `repair`-origin cell-registry canonicalization pass:
  /// after an import or undo/redo, a cell text can contain boundaries whose
  /// registry records no longer resolve (an undo restores a record whose
  /// cursors point at the tombstoned `\n`, not the re-inserted one). Re-mint
  /// durable records under the projection's own fabricated-key law so every
  /// peer writes the identical keys and Loro map LWW converges the
  /// concurrent repairs. Commits (never queues — the caller decides whether
  /// the repair delta needs its own publish or rides an enclosing export
  /// window) and returns whether anything was written.
  fn repair_cell_registries(&mut self, cells: &HashSet<CellId>) -> bool {
    if cells.is_empty() {
      return false;
    }
    const MAX_REPAIR_ATTEMPTS: u8 = 3;
    let mut wrote = false;
    for &cell in cells {
      let attempts = &mut self.repair_attempts;
      let written = super::cell_text::repair_missing_paragraph_records(&self.doc, cell, |key| {
        let count = attempts.entry(format!("{cell}/{key}")).or_insert(0);
        if *count >= MAX_REPAIR_ATTEMPTS {
          return false; // quarantined: the fabricated id keeps the cell editable
        }
        *count += 1;
        true
      });
      wrote |= !written.is_empty();
    }
    if wrote {
      self.doc.set_next_commit_origin("repair");
      self.doc.set_next_commit_message("cell.registry-repair");
      self.doc.commit();
    }
    // Repair touches are registry children, never text containers — drain
    // them so the next classification pass sees only real work.
    let _ = self.take_touched();
    wrote
  }

  /// The grid STRUCTURAL repair pass (excel flow spec §3): converge canonical
  /// state to the normalized projection the shared materializer already
  /// decided — phantom/bump rows written into `row_order`, re-homed cell
  /// addresses written to their LWW registers, type-fallback columns made
  /// real. Deterministic on every peer (it writes projection facts, and the
  /// projection is a pure function of canonical state); capped per target so
  /// an unclearable defect quarantines instead of looping. Commits with
  /// `repair` origin (undo-inert); the caller decides the publish window.
  fn repair_grid_structure(&mut self) -> bool {
    const MAX_REPAIR_ATTEMPTS: u8 = 3;
    if self.defects.is_empty() {
      return false;
    }
    let mut wrote = false;
    let sheets = self.board.sheets.clone();
    for sheet in &sheets {
      let Some(record) = loro_schema::sheet_record(&self.doc, sheet.id) else {
        continue;
      };
      // Type-fallback columns become canonical records.
      if let Ok(column_order) = loro_schema::sheet_column_order(&record)
        && column_order.is_empty()
        && !sheet.columns.is_empty()
        && self.repair_allowed(&format!("grid/{}/columns", sheet.id), MAX_REPAIR_ATTEMPTS)
      {
        for column in &sheet.columns {
          if loro_schema::ensure_column_record(&record, column.id, &column.label, column.side).is_ok() {
            let _ = column_order.insert(column_order.len(), column.id.to_string());
          }
        }
        wrote = true;
      }
      // Phantom / bump rows enter the canonical order at their projected
      // position (anchor: the first LATER projected row that is already
      // canonical — identical on every peer because projections are).
      if let Ok(row_order) = loro_schema::sheet_row_order(&record) {
        for (index, row) in sheet.rows.iter().enumerate() {
          let entry = row.id.to_string();
          let canonical = loro_schema::list_strings(&row_order);
          if canonical.iter().any(|candidate| candidate == &entry) {
            continue;
          }
          if !self.repair_allowed(&format!("grid/{}/row/{}", sheet.id, row.id), MAX_REPAIR_ATTEMPTS) {
            continue;
          }
          let position = sheet.rows[index + 1..]
            .iter()
            .find_map(|later| {
              let later_entry = later.id.to_string();
              canonical.iter().position(|candidate| candidate == &later_entry)
            })
            .unwrap_or(canonical.len());
          if row_order.insert(position.min(row_order.len()), entry).is_ok() {
            wrote = true;
          }
        }
      }
      // Bumped / re-homed cell addresses converge to the projection.
      for cell in sheet.cells() {
        let Some(cell_record) = loro_schema::cell_record(&self.doc, cell.id) else {
          continue;
        };
        let canonical_row = loro_schema::map_uuid(&cell_record, "row_id");
        let canonical_column = loro_schema::map_uuid(&cell_record, "column_id");
        if canonical_row == Some(cell.row_id) && canonical_column == Some(cell.column_id) {
          continue;
        }
        if !self.repair_allowed(&format!("grid/{}/cell/{}", sheet.id, cell.id), MAX_REPAIR_ATTEMPTS) {
          continue;
        }
        if canonical_row != Some(cell.row_id) {
          let _ = loro_schema::set_cell_row(&cell_record, cell.row_id);
        }
        if canonical_column != Some(cell.column_id) {
          let _ = loro_schema::set_cell_column(&cell_record, cell.column_id);
        }
        wrote = true;
      }
    }
    if wrote {
      self.doc.set_next_commit_origin("repair");
      self.doc.set_next_commit_message("flow.grid-repair");
      self.doc.commit();
    }
    let _ = self.take_touched();
    wrote
  }

  fn repair_allowed(&mut self, key: &str, cap: u8) -> bool {
    let count = self.repair_attempts.entry(key.to_string()).or_insert(0);
    if *count >= cap {
      return false;
    }
    *count += 1;
    true
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
    // An undo re-inserts text under NEW op ids; restored registry records
    // still point at the tombstones. Re-mint before materializing; the
    // repair delta rides the `vv_before` export below (no separate publish).
    self.repair_cell_registries(&dirty);
    self.refresh(Some(&dirty))?;
    // Undo can resurrect states the normalizer had to repair; converge them
    // canonically inside the same publish window (`vv_before` export below).
    if self.repair_grid_structure() {
      self.refresh(Some(&HashSet::new()))?;
    }
    self.push_board_stream();
    for cell in dirty {
      self.push_cell_replace(cell);
    }
    self.queue_local_update(vv_before)?;
    Ok(())
  }

  pub fn undo_group_start(&mut self) -> anyhow::Result<bool> {
    self.undo.group_start()?;
    self.undo_group_active = true;
    Ok(true)
  }

  pub fn undo_group_end(&mut self) {
    self.undo.group_end();
    if self.undo_group_active {
      self.undo_group_active = false;
      // The deferred modified-stamp for the whole group (see `apply_intent`).
      // It lands AFTER the group members' publishes, so it must publish its
      // own delta — an unpublished local commit is a permanent causal gap.
      let vv_before = self.doc.oplog_vv();
      self.touch_modified_meta();
      let _ = self.queue_local_update(&vv_before);
    }
  }

  fn touch_modified_meta(&mut self) {
    self.doc.set_next_commit_origin("meta");
    let _ = loro_schema::touch_modified(&self.doc);
    self.doc.commit();
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
      .flat_map(|sheet| sheet.cells().map(|cell| (cell.id, cell.summary.clone())))
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
      for cell in sheet.cells() {
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

// ---- C-S2: flow comments (the .db8 shape, cell-anchored) -------------------

/// A flow comment thread: general (no anchor) or anchored to a durable
/// [`CellId`]. Cell moves/re-parenting never orphan (the id survives); only
/// cell deletion does, and `cell_alive` reports it while the quoted text and
/// birth frontier keep the thread readable + history-jumpable (C-S6).
/// H-S6: one flow checkpoint record (the .fl0 mirror of a .db8 revision).
#[derive(Clone, Debug, PartialEq)]
pub struct FlowCheckpoint {
  pub checkpoint_id: u128,
  pub title: String,
  pub kind: flowstate_document::RevisionKind,
  pub frontier: Vec<u8>,
  pub created_at_unix_secs: i64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct FlowCommentThread {
  pub comment_id: u128,
  pub author_user_id: Option<u128>,
  pub cell_id: Option<CellId>,
  pub cell_alive: bool,
  pub general: bool,
  pub quoted_text: String,
  pub resolved: bool,
  pub created_at_unix_secs: i64,
  pub updated_at_unix_secs: i64,
  pub created_frontier: Option<Vec<u8>>,
  pub messages: Vec<crate::crdt_runtime::RuntimeCommentMessage>,
}

impl FlowRuntime {
  fn comments_map(&self) -> loro::LoroMap {
    self.doc.get_map(flowstate_flow::loro_schema::COMMENTS_BY_ID)
  }

  pub fn flow_comments(&self) -> Vec<FlowCommentThread> {
    use loro::{Container, ValueOrContainer};
    let comments = self.doc.get_map(flowstate_flow::loro_schema::COMMENTS_BY_ID);
    let live_cells: std::collections::HashSet<CellId> = self
      .board
      .sheets
      .iter()
      .flat_map(|sheet| sheet.cells().map(|cell| cell.id))
      .collect();
    let mut threads = Vec::new();
    comments.for_each(|key, value| {
      let ValueOrContainer::Container(Container::Map(thread)) = value else {
        return;
      };
      let Ok(comment_id) = key.parse::<u128>() else {
        return;
      };
      let cell_id = crate::crdt_runtime::map_string_opt(&thread, "cell_id").and_then(|id| id.parse::<CellId>().ok());
      let mut messages = Vec::new();
      if let Some(ValueOrContainer::Container(Container::Map(message_map))) = thread.get("messages_by_id") {
        message_map.for_each(|message_key, value| {
          let ValueOrContainer::Container(Container::Map(message)) = value else {
            return;
          };
          let Ok(message_id) = message_key.parse() else {
            return;
          };
          let Some(author_user_id) = crate::crdt_runtime::map_string_opt(&message, "author_user_id").and_then(|id| id.parse().ok())
          else {
            return;
          };
          messages.push(crate::crdt_runtime::RuntimeCommentMessage {
            message_id,
            author_user_id,
            author_display_name: crate::crdt_runtime::map_string_opt(&message, "author_display_name")
              .unwrap_or_else(|| "Unknown author".into()),
            body: crate::crdt_runtime::map_string_opt(&message, "body").unwrap_or_default(),
            created_at_unix_secs: crate::crdt_runtime::map_i64_opt(&message, "created_at").unwrap_or_default(),
            updated_at_unix_secs: crate::crdt_runtime::map_i64_opt(&message, "updated_at").unwrap_or_default(),
            deleted: crate::crdt_runtime::map_bool_opt(&message, "deleted").unwrap_or(false),
          });
        });
      }
      messages.sort_by_key(|message| (message.created_at_unix_secs, message.message_id));
      threads.push(FlowCommentThread {
        comment_id,
        author_user_id: crate::crdt_runtime::comment_thread_author(&thread),
        cell_id,
        cell_alive: cell_id.is_some_and(|cell| live_cells.contains(&cell)),
        general: crate::crdt_runtime::map_bool_opt(&thread, "general").unwrap_or(false),
        quoted_text: crate::crdt_runtime::map_string_opt(&thread, "quoted_text").unwrap_or_default(),
        resolved: crate::crdt_runtime::map_bool_opt(&thread, "resolved").unwrap_or(false),
        created_at_unix_secs: crate::crdt_runtime::map_i64_opt(&thread, "created_at").unwrap_or_default(),
        updated_at_unix_secs: crate::crdt_runtime::map_i64_opt(&thread, "updated_at").unwrap_or_default(),
        created_frontier: crate::crdt_runtime::map_binary_opt(&thread, "created_frontier"),
        messages,
      });
    });
    threads.sort_by_key(|thread| (thread.resolved, thread.created_at_unix_secs, thread.comment_id));
    threads
  }

  /// `cell: None` = a general note (F3). Anchored comments quote the cell's
  /// summary text at creation.
  pub fn create_flow_comment(
    &mut self,
    cell: Option<CellId>,
    body: &str,
    author_user_id: u128,
    author_display_name: &str,
  ) -> anyhow::Result<u128> {
    let body = crate::crdt_runtime::validated_comment_body(body)?;
    let quoted_text = match cell {
      Some(cell_id) => {
        let cell = self
          .board
          .sheets
          .iter()
          .flat_map(|sheet| sheet.cells())
          .find(|candidate| candidate.id == cell_id)
          .ok_or_else(|| anyhow::anyhow!("cell {cell_id:?} does not exist"))?;
        cell.summary.summary_text.to_string()
      },
      None => String::new(),
    };
    let comment_id = Uuid::new_v4().as_u128();
    let message_id = Uuid::new_v4().as_u128();
    let now = crate::crdt_runtime::unix_time_secs();
    let vv_before = self.doc.oplog_vv();
    let created_frontier = self.doc.state_frontiers().encode();
    let comments = self.comments_map();
    let thread = comments
      .ensure_mergeable_map(&comment_id.to_string())
      .map_err(|error| anyhow::anyhow!("creating flow comment thread failed: {error}"))?;
    thread.insert("id", comment_id.to_string())?;
    thread.insert("author_user_id", author_user_id.to_string())?;
    match cell {
      Some(cell_id) => {
        thread.insert("cell_id", cell_id.to_string())?;
      },
      None => {
        thread.insert("general", true)?;
      },
    }
    thread.insert("quoted_text", quoted_text)?;
    thread.insert("created_frontier", loro::LoroValue::Binary(created_frontier.into()))?;
    thread.insert("resolved", false)?;
    thread.insert("created_at", now)?;
    thread.insert("updated_at", now)?;
    let messages = thread
      .ensure_mergeable_map("messages_by_id")?;
    crate::crdt_runtime::write_comment_message(&messages, message_id, &body, author_user_id, author_display_name, now)?;
    self.finish_flow_comment_mutation(&vv_before, "comment-create")?;
    Ok(comment_id)
  }

  pub fn reply_to_flow_comment(&mut self, comment_id: u128, body: &str, author_user_id: u128, author_display_name: &str) -> anyhow::Result<u128> {
    let body = crate::crdt_runtime::validated_comment_body(body)?;
    let message_id = Uuid::new_v4().as_u128();
    let now = crate::crdt_runtime::unix_time_secs();
    let vv_before = self.doc.oplog_vv();
    let thread = self.existing_flow_thread(comment_id)?;
    let messages = thread.ensure_mergeable_map("messages_by_id")?;
    crate::crdt_runtime::write_comment_message(&messages, message_id, &body, author_user_id, author_display_name, now)?;
    thread.insert("updated_at", now)?;
    self.finish_flow_comment_mutation(&vv_before, "comment-reply")?;
    Ok(message_id)
  }

  pub fn set_flow_comment_resolved(&mut self, comment_id: u128, resolved: bool) -> anyhow::Result<()> {
    let vv_before = self.doc.oplog_vv();
    let thread = self.existing_flow_thread(comment_id)?;
    thread.insert("resolved", resolved)?;
    thread.insert("updated_at", crate::crdt_runtime::unix_time_secs())?;
    self.finish_flow_comment_mutation(&vv_before, if resolved { "comment-resolve" } else { "comment-reopen" })
  }

  pub fn edit_flow_comment_message(&mut self, comment_id: u128, message_id: u128, body: &str, actor_user_id: u128) -> anyhow::Result<()> {
    let body = crate::crdt_runtime::validated_comment_body(body)?;
    let vv_before = self.doc.oplog_vv();
    let thread = self.existing_flow_thread(comment_id)?;
    let messages = crate::crdt_runtime::existing_child_map(&thread, "messages_by_id")?;
    let message = crate::crdt_runtime::existing_child_map(&messages, &message_id.to_string())?;
    anyhow::ensure!(
      crate::crdt_runtime::map_string_opt(&message, "author_user_id").and_then(|id| id.parse().ok()) == Some(actor_user_id),
      "Only the message author can edit it"
    );
    message.insert("body", body.as_str())?;
    message.insert("updated_at", crate::crdt_runtime::unix_time_secs())?;
    thread.insert("updated_at", crate::crdt_runtime::unix_time_secs())?;
    self.finish_flow_comment_mutation(&vv_before, "comment-edit")
  }

  pub fn delete_flow_comment_message(&mut self, comment_id: u128, message_id: u128, actor_user_id: u128) -> anyhow::Result<()> {
    let vv_before = self.doc.oplog_vv();
    let thread = self.existing_flow_thread(comment_id)?;
    let messages = crate::crdt_runtime::existing_child_map(&thread, "messages_by_id")?;
    let message = crate::crdt_runtime::existing_child_map(&messages, &message_id.to_string())?;
    anyhow::ensure!(
      crate::crdt_runtime::map_string_opt(&message, "author_user_id").and_then(|id| id.parse().ok()) == Some(actor_user_id),
      "Only the message author can delete it"
    );
    message.insert("deleted", true)?;
    message.insert("body", "")?;
    message.insert("updated_at", crate::crdt_runtime::unix_time_secs())?;
    thread.insert("updated_at", crate::crdt_runtime::unix_time_secs())?;
    self.finish_flow_comment_mutation(&vv_before, "comment-message-delete")
  }

  pub fn delete_flow_comment(&mut self, comment_id: u128, actor_user_id: u128) -> anyhow::Result<()> {
    let vv_before = self.doc.oplog_vv();
    let comments = self.comments_map();
    let thread = self.existing_flow_thread(comment_id)?;
    anyhow::ensure!(
      crate::crdt_runtime::comment_thread_author(&thread) == Some(actor_user_id),
      "Only the thread author can delete it"
    );
    comments.delete(&comment_id.to_string())?;
    self.finish_flow_comment_mutation(&vv_before, "comment-delete")
  }

  fn existing_flow_thread(&self, comment_id: u128) -> anyhow::Result<loro::LoroMap> {
    let comments = self.comments_map();
    crate::crdt_runtime::existing_child_map(&comments, &comment_id.to_string())
  }

  // ---- H-S6: flow history parity — the checkpoint subtree ------------------

  /// The `flow.checkpoints` records, oldest-first. Same tiering as .db8
  /// revisions (named pins / session saves / autosave grain).
  pub fn flow_checkpoints(&self) -> Vec<FlowCheckpoint> {
    let list = self.doc.get_list(flowstate_flow::loro_schema::CHECKPOINTS_LIST);
    let mut checkpoints = Vec::with_capacity(list.len());
    for index in 0..list.len() {
      let Some(loro::ValueOrContainer::Container(loro::Container::Map(record))) = list.get(index) else {
        continue;
      };
      let get_string = |key: &str| match record.get(key) {
        Some(loro::ValueOrContainer::Value(loro::LoroValue::String(value))) => Some(value.to_string()),
        _ => None,
      };
      let Some(checkpoint_id) = get_string("id").and_then(|id| id.parse::<u128>().ok()) else {
        continue;
      };
      let frontier = match record.get("frontier") {
        Some(loro::ValueOrContainer::Value(loro::LoroValue::Binary(bytes))) => bytes.to_vec(),
        _ => continue,
      };
      let created_at_unix_secs = match record.get("timestamp") {
        Some(loro::ValueOrContainer::Value(loro::LoroValue::I64(value))) => value,
        _ => 0,
      };
      checkpoints.push(FlowCheckpoint {
        checkpoint_id,
        title: get_string("title").unwrap_or_else(|| "Checkpoint".to_string()),
        kind: flowstate_document::RevisionKind::from_str_or_session(&get_string("kind").unwrap_or_default()),
        frontier,
        created_at_unix_secs,
      });
    }
    checkpoints
  }

  /// Mint a checkpoint record at the CURRENT content frontier.
  pub fn create_flow_checkpoint(
    &mut self,
    title: Option<&str>,
    kind: flowstate_document::RevisionKind,
  ) -> anyhow::Result<u128> {
    let frontier = self.doc.state_frontiers().encode();
    self.create_flow_checkpoint_at(frontier, title, kind)
  }

  /// Restore support: re-mint a checkpoint record the revert erased,
  /// preserving its identity and stamp.
  fn remint_flow_checkpoint(&mut self, record: &FlowCheckpoint) -> anyhow::Result<()> {
    let vv_before = self.doc.oplog_vv();
    let list = self.doc.get_list(flowstate_flow::loro_schema::CHECKPOINTS_LIST);
    let entry = list.insert_container(list.len(), loro::LoroMap::new())?;
    entry.insert("id", record.checkpoint_id.to_string())?;
    entry.insert("title", record.title.as_str())?;
    entry.insert("kind", record.kind.as_str())?;
    entry.insert("frontier", loro::LoroValue::Binary(record.frontier.clone().into()))?;
    entry.insert("timestamp", record.created_at_unix_secs)?;
    self.doc.set_next_commit_origin("meta");
    self.doc.commit();
    self.queue_local_update(&vv_before)?;
    Ok(())
  }

  /// The record commits at the current head but points at an explicit
  /// frontier — restore mints its safety pin AFTER the revert.
  fn create_flow_checkpoint_at(
    &mut self,
    frontier: Vec<u8>,
    title: Option<&str>,
    kind: flowstate_document::RevisionKind,
  ) -> anyhow::Result<u128> {
    let checkpoint_id = uuid::Uuid::new_v4().as_u128();
    let vv_before = self.doc.oplog_vv();
    let list = self.doc.get_list(flowstate_flow::loro_schema::CHECKPOINTS_LIST);
    let record = list.insert_container(list.len(), loro::LoroMap::new())?;
    record.insert("id", checkpoint_id.to_string())?;
    record.insert(
      "title",
      title
        .map(str::trim)
        .filter(|title| !title.is_empty())
        .unwrap_or(kind.default_title()),
    )?;
    record.insert("kind", kind.as_str())?;
    record.insert("frontier", loro::LoroValue::Binary(frontier.into()))?;
    record.insert(
      "timestamp",
      std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_secs() as i64),
    )?;
    // Meta origin: checkpoint records are undo-inert, like .db8 revisions.
    self.doc.set_next_commit_origin("meta");
    self.doc.commit();
    self.queue_local_update(&vv_before)?;
    Ok(checkpoint_id)
  }

  /// Rename PINS (kind → named), mirroring the .db8 law.
  pub fn rename_flow_checkpoint(&mut self, checkpoint_id: u128, title: &str) -> anyhow::Result<()> {
    let title = title.trim();
    anyhow::ensure!(!title.is_empty(), "A checkpoint name cannot be empty");
    let vv_before = self.doc.oplog_vv();
    let list = self.doc.get_list(flowstate_flow::loro_schema::CHECKPOINTS_LIST);
    let wanted = checkpoint_id.to_string();
    for index in 0..list.len() {
      let Some(loro::ValueOrContainer::Container(loro::Container::Map(record))) = list.get(index) else {
        continue;
      };
      let matches = matches!(
        record.get("id"),
        Some(loro::ValueOrContainer::Value(loro::LoroValue::String(id))) if id.as_str() == wanted
      );
      if !matches {
        continue;
      }
      record.insert("title", title)?;
      record.insert("kind", flowstate_document::RevisionKind::Named.as_str())?;
      self.doc.set_next_commit_origin("meta");
      self.doc.commit();
      self.queue_local_update(&vv_before)?;
      return Ok(());
    }
    anyhow::bail!("That checkpoint no longer exists")
  }

  /// H-S6 restore, under the same LAW as .db8: a named safety pin of the
  /// present first, then `revert_to` as ordinary local ops — one-step
  /// undoable, and peers converge on a normal edit. The whole board and every
  /// open cell refresh.
  pub fn restore_flow_frontier(&mut self, frontier: &[u8]) -> anyhow::Result<()> {
    let target = Frontiers::decode(frontier).map_err(|error| anyhow::anyhow!("decoding flow frontier for restore: {error}"))?;
    if self.doc.state_frontiers() == target {
      return Ok(());
    }
    let pre_restore_frontier = self.doc.state_frontiers().encode();
    // Flow checkpoint records live IN the doc, so the revert erases every
    // record minted after the target — snapshot them for re-minting.
    let records_before = self.flow_checkpoints();
    let vv_before = self.doc.oplog_vv();
    self
      .doc
      .revert_to(&target)
      .map_err(|error| anyhow::anyhow!("reverting flow document to historical frontier: {error}"))?;
    // revert_to leaves its ops in the pending txn: commit them with the
    // default (undoable) origin before any meta re-minting below.
    self.doc.commit();
    // Re-mint the records the revert erased (only this peer writes them, so
    // there is no concurrent-duplicate hazard), then the safety pin of the
    // present — minted AFTER the revert, pointing at the pre-restore
    // frontier, riding the same publish window.
    let surviving: std::collections::HashSet<u128> = self
      .flow_checkpoints()
      .iter()
      .map(|checkpoint| checkpoint.checkpoint_id)
      .collect();
    for record in records_before {
      if !surviving.contains(&record.checkpoint_id) {
        self.remint_flow_checkpoint(&record)?;
      }
    }
    self.create_flow_checkpoint_at(pre_restore_frontier, Some("Before restore"), flowstate_document::RevisionKind::Named)?;
    self.touch_modified_meta();
    self.queue_local_update(&vv_before)?;
    self.refresh(None)?;
    let vv_pre_grid_repair = self.doc.oplog_vv();
    if self.repair_grid_structure() {
      self.queue_local_update(&vv_pre_grid_repair)?;
      self.refresh(Some(&HashSet::new()))?;
    }
    self.push_board_stream();
    let cells: Vec<CellId> = self
      .board
      .sheets
      .iter()
      .flat_map(|sheet| sheet.cells().map(|cell| cell.id))
      .collect();
    for cell in cells {
      self.push_cell_replace(cell);
    }
    Ok(())
  }

  /// One commit (origin `comment`), modified-meta touch, publish — the same
  /// tail the intent path uses, minus board refresh (comments never change
  /// the board projection).
  fn finish_flow_comment_mutation(&mut self, vv_before: &VersionVector, message: &str) -> anyhow::Result<()> {
    self.doc.set_next_commit_origin("comment");
    self.doc.set_next_commit_message(message);
    self.doc.commit();
    if !self.undo_group_active {
      self.touch_modified_meta();
    }
    self.queue_local_update(vv_before)?;
    Ok(())
  }
}
