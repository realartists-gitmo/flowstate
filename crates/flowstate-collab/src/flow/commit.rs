//! Flow intent execution: resolve → mutate → commit → derive → publish (the
//! .db8 commit law at board scale). ALL frontier logic for the flow write path
//! lives in this file.
//!
//! Phases, verbatim from `local_write/commit.rs`:
//! 1. **Resolve** against the maintained board + live containers; any failure
//!    rejects the intent before the doc is touched (I-15).
//! 2. **Mutate** through the `flowstate_flow::loro_schema` writers / the
//!    cell-text executors. A mid-apply error triggers the I-10 compensation
//!    (`revert_to` + `"repair"`-origin commits), never a partial escape.
//! 3. **Commit** exactly once — origin `"local"`, message = intent class.
//! 4. **Derive** the projection change in place (structural intents mutate the
//!    maintained board directly; content intents rematerialize ONE cell) and
//!    push the ordered streams; keystrokes touch the board stream only when
//!    the cell's summary actually changed.
//! 5. **Publish**: export `updates(vv_before)` onto the publish queue.
//!
//! One intent = one Loro commit = one undo-group member.

use anyhow::{Context as _, Result};
use flowstate_flow::format::{CellId, SheetId};
use flowstate_flow::intents::{CellPlacement, CellSeed, FlowIntent, RelativePosition};
use flowstate_flow::projection::{Cell, Sheet};
use flowstate_flow::{board_ops, loro_schema};
use gpui_flowtext::{CursorEndpoint, SelectionAffinity, SelectionSnapshot, VisualGravity};
use loro::LoroDoc;
use uuid::Uuid;

use super::cell_text::{self, CellTextContext};
use super::runtime::{FlowRuntime, FlowWriteOutcome, FlowWriteRejected};
use crate::crdt_runtime::cursor_for_boundary;

/// Execute one flow intent against the gate-held runtime.
pub(crate) fn apply_flow_intent(core: &mut FlowRuntime, intent: &FlowIntent) -> Result<FlowWriteOutcome, FlowWriteRejected> {
  let class = intent.class();
  let doc = core.doc().clone();
  let frontier_before = doc.state_frontiers();
  let vv_before = doc.oplog_vv();

  // ---- Phases 1+2: resolve (reject before mutation), then mutate ------------
  let derived = match execute_intent(core, &doc, intent) {
    Ok(Some(derived)) => derived,
    // Resolved to a genuine no-op: nothing was mutated, nothing to commit.
    Ok(None) => {
      return Ok(FlowWriteOutcome {
        changed: false,
        frontier: core.frontier(),
        version_vector: doc.state_vv().encode(),
        selection_after: None,
      });
    },
    Err(ExecuteError::Rejected(rejected)) => return Err(rejected),
    Err(ExecuteError::MidApply(error)) => return Err(compensate_failed_intent(core, &doc, &frontier_before, &vv_before, class, &error)),
  };

  // ---- Phase 3: one commit, origin local, class message ---------------------
  doc.set_next_commit_origin("local");
  doc.set_next_commit_message(class);
  doc.commit();

  // ---- Phase 4: derive + streams --------------------------------------------
  let mut selection_after = None;
  match derived {
    Derived::Board => core.push_board_stream(),
    Derived::Content { cell, caret } => {
      let summary_changed = core
        .refresh_cell(cell)
        .map_err(|error| compensate_failed_intent(core, &doc, &frontier_before, &vv_before, class, &error))?;
      if summary_changed {
        core.push_board_stream();
      }
      selection_after = caret.and_then(|caret| cell_selection_snapshot(&doc, cell, caret));
    },
    Derived::BoardAndContent { cell } => {
      // Structural change already applied to the maintained board; fold in the
      // (possibly fresh) cell summary, then one board Replace.
      let _ = core
        .refresh_cell(cell)
        .map_err(|error| compensate_failed_intent(core, &doc, &frontier_before, &vv_before, class, &error))?;
      core.push_board_stream();
    },
  }

  // ---- Phase 5: publish ------------------------------------------------------
  core.queue_local_update_publish(&vv_before);

  #[cfg(debug_assertions)]
  core.audit_board_against_rebuild(class);

  Ok(FlowWriteOutcome {
    changed: true,
    frontier: core.frontier(),
    version_vector: doc.state_vv().encode(),
    selection_after,
  })
}

/// What the committed intent changed, for stream derivation.
enum Derived {
  /// Structural board change (already applied to the maintained board).
  Board,
  /// One cell's rich text changed (cell rematerialization + summary check).
  Content { cell: CellId, caret: Option<usize> },
  /// Structural board change PLUS one cell's content (AddCell).
  BoardAndContent { cell: CellId },
}

enum ExecuteError {
  Rejected(FlowWriteRejected),
  MidApply(anyhow::Error),
}

impl From<FlowWriteRejected> for ExecuteError {
  fn from(rejected: FlowWriteRejected) -> Self {
    Self::Rejected(rejected)
  }
}

impl From<anyhow::Error> for ExecuteError {
  fn from(error: anyhow::Error) -> Self {
    Self::MidApply(error)
  }
}

fn sheet_of<'a>(core: &'a FlowRuntime, sheet_id: SheetId) -> Result<&'a Sheet, FlowWriteRejected> {
  core
    .board_ref()
    .sheet(sheet_id)
    .ok_or(FlowWriteRejected::UnknownSheet(sheet_id))
}

/// Resolve + mutate. `Ok(None)` = resolved no-op (zero mutation). The board is
/// updated in place for structural intents (the derive step's authority).
#[allow(clippy::too_many_lines, reason = "one arm per intent class; each arm is short and self-contained")]
fn execute_intent(core: &mut FlowRuntime, doc: &LoroDoc, intent: &FlowIntent) -> Result<Option<Derived>, ExecuteError> {
  match intent {
    FlowIntent::CreateSheet {
      sheet_id,
      name,
      sheet_type_id,
    } => {
      if core.board_ref().format.sheet_type(*sheet_type_id).is_none() {
        return Err(FlowWriteRejected::StructureViolation("unknown sheet type".into()).into());
      }
      if core.board_ref().sheet(*sheet_id).is_some() {
        return Err(FlowWriteRejected::StructureViolation("sheet id already exists".into()).into());
      }
      loro_schema::write_sheet(doc, *sheet_id, name, *sheet_type_id, usize::MAX).context("writing sheet record")?;
      core.board_mut().sheets.push(Sheet {
        id: *sheet_id,
        name: name.clone(),
        sheet_type_id: *sheet_type_id,
        cells: Vec::new(),
        annotations: Vec::new(),
      });
      Ok(Some(Derived::Board))
    },
    FlowIntent::RenameSheet { sheet_id, name } => {
      sheet_of(core, *sheet_id)?;
      loro_schema::rename_sheet(doc, *sheet_id, name).context("renaming sheet")?;
      if let Some(sheet) = core.board_mut().sheet_mut(*sheet_id) {
        sheet.name = name.clone();
      }
      Ok(Some(Derived::Board))
    },
    FlowIntent::DeleteSheet { sheet_id } => {
      sheet_of(core, *sheet_id)?;
      loro_schema::remove_sheet(doc, *sheet_id).context("removing sheet")?;
      let cells: Vec<CellId> = sheet_of(core, *sheet_id)
        .map(|sheet| sheet.cells.iter().map(|cell| cell.id).collect())
        .unwrap_or_default();
      for cell in cells {
        core.close_cell(cell);
      }
      core.board_mut().sheets.retain(|sheet| sheet.id != *sheet_id);
      Ok(Some(Derived::Board))
    },
    FlowIntent::MoveSheet { sheet_id, target_index } => {
      sheet_of(core, *sheet_id)?;
      loro_schema::move_sheet(doc, *sheet_id, *target_index).context("moving sheet")?;
      let board = core.board_mut();
      let from = board
        .sheets
        .iter()
        .position(|sheet| sheet.id == *sheet_id)
        .expect("sheet resolved above");
      let sheet = board.sheets.remove(from);
      let to = (*target_index).min(board.sheets.len());
      board.sheets.insert(to, sheet);
      Ok(Some(Derived::Board))
    },
    FlowIntent::AddCell {
      sheet_id,
      cell_id,
      placement,
      seed,
    } => {
      let sheet = sheet_of(core, *sheet_id)?;
      if core.board_ref().cell(*cell_id).is_some() {
        return Err(FlowWriteRejected::StructureViolation("cell id already exists".into()).into());
      }
      let columns = board_ops::sheet_column_ids(core.board_ref(), *sheet_id)
        .map_err(|_| FlowWriteRejected::StructureViolation("unknown sheet type".into()))?;
      let (column_index, insertion_index, parent) = resolve_placement(core, sheet.id, &columns, placement)?;
      let column_id = columns[column_index];
      let cell_map = loro_schema::write_cell(doc, *sheet_id, *cell_id, column_id, parent, insertion_index).context("writing cell record")?;
      match seed {
        CellSeed::Empty => {
          loro_schema::seed_cell_flow(&cell_map).context("seeding cell flow")?;
        },
        CellSeed::Paragraphs(paragraphs) => {
          loro_schema::cell_flow_from_paragraphs(&cell_map, paragraphs).context("writing seeded cell content")?;
        },
      }
      let summary = flowstate_flow::loro_projection::materialize_cell_rows(doc, *cell_id)
        .map(|rows| flowstate_flow::loro_projection::summary_from_rows(&rows.blocks))
        .unwrap_or_default();
      if let Some(sheet) = core.board_mut().sheet_mut(*sheet_id) {
        let index = insertion_index.min(sheet.cells.len());
        sheet.cells.insert(
          index,
          Cell {
            id: *cell_id,
            column_id,
            parent_id: parent,
            summary,
          },
        );
      }
      canonicalize_sheet_order(core, doc, *sheet_id)?;
      Ok(Some(Derived::Board))
    },
    FlowIntent::DeleteCell { sheet_id, cell_id } => {
      let sheet = sheet_of(core, *sheet_id)?;
      if sheet.cell(*cell_id).is_none() {
        return Err(FlowWriteRejected::UnknownCell(*cell_id).into());
      }
      // Canonical child orphaning IN the delete commit (the old projection
      // law), so the materializer never has to normalize a dangling parent.
      let children: Vec<CellId> = sheet
        .cells
        .iter()
        .filter(|cell| cell.parent_id == Some(*cell_id))
        .map(|cell| cell.id)
        .collect();
      for child in &children {
        let map = loro_schema::cell_map(doc, *child).context("resolving child cell for orphaning")?;
        loro_schema::set_cell_parent(&map, None).context("orphaning child cell")?;
      }
      loro_schema::remove_cell(doc, *sheet_id, *cell_id).context("removing cell record")?;
      core.close_cell(*cell_id);
      if let Some(sheet) = core.board_mut().sheet_mut(*sheet_id) {
        sheet.cells.retain(|cell| cell.id != *cell_id);
        for cell in &mut sheet.cells {
          if cell.parent_id == Some(*cell_id) {
            cell.parent_id = None;
          }
        }
      }
      // Orphaned children may now interleave another subtree's span; restore
      // the canonical linearization inside this same commit.
      canonicalize_sheet_order(core, doc, *sheet_id)?;
      Ok(Some(Derived::Board))
    },
    FlowIntent::MoveCellSubtree { sheet_id, cell_id, drop } => {
      let sheet = sheet_of(core, *sheet_id)?;
      if sheet.cell(*cell_id).is_none() {
        return Err(FlowWriteRejected::UnknownCell(*cell_id).into());
      }
      let columns = board_ops::sheet_column_ids(core.board_ref(), *sheet_id)
        .map_err(|_| FlowWriteRejected::StructureViolation("unknown sheet type".into()))?;
      // ONE law for preview and commit: the same pure move the drag preview
      // ran, applied to a clone, then translated into order-list moves +
      // column/parent rewrites.
      let mut target = sheet.clone();
      board_ops::apply_move_subtree(&mut target, &columns, *cell_id, *drop)
        .map_err(|error| FlowWriteRejected::StructureViolation(format!("{error:#}")))?;
      if !board_ops::sheet_topology_ok(&target, &columns) {
        return Err(FlowWriteRejected::StructureViolation("move would violate sheet topology".into()).into());
      }
      // The committed order is the canonical linearization of the moved sheet
      // (an arbitrary-index root drop can land inside another subtree's span);
      // the drag preview applies the identical step.
      board_ops::canonicalize_sheet(&mut target);
      let before = sheet.clone();
      let sheet_map = loro_schema::sheet_map(doc, *sheet_id).context("resolving sheet map")?;
      // Column/parent rewrites for changed subtree members (LWW fields).
      for cell in &target.cells {
        let old = before.cell(cell.id).context("moved cell missing from pre-state")?;
        if old.column_id != cell.column_id || old.parent_id != cell.parent_id {
          let map = loro_schema::cell_map(doc, cell.id).context("resolving moved cell map")?;
          if old.column_id != cell.column_id {
            loro_schema::set_cell_column(&map, cell.column_id).context("rewriting moved cell column")?;
          }
          if old.parent_id != cell.parent_id {
            loro_schema::set_cell_parent(&map, cell.parent_id).context("rewriting moved cell parent")?;
          }
        }
      }
      // Order-list moves ONLY (never a cell map/text write): minimal `mov`
      // sequence transforming the live order into the target order.
      let target_ids: Vec<String> = target.cells.iter().map(|cell| cell.id.to_string()).collect();
      apply_order_diff(&sheet_map, &target_ids).context("applying order-list moves")?;
      if let Some(sheet) = core.board_mut().sheet_mut(*sheet_id) {
        *sheet = target;
      }
      Ok(Some(Derived::Board))
    },
    FlowIntent::SetCellStruck { sheet_id, cell_id, struck } => {
      let sheet = sheet_of(core, *sheet_id)?;
      let cell = sheet.cell(*cell_id).ok_or(FlowWriteRejected::UnknownCell(*cell_id))?;
      if cell.summary.struck == *struck {
        return Ok(None);
      }
      let text = loro_schema::cell_text(doc, *cell_id).ok_or(FlowWriteRejected::UnknownCell(*cell_id))?;
      let len = text.len_unicode();
      if len <= 1 {
        return Ok(None);
      }
      // A whole-text mark (expand-`After`), so concurrent typing merges UNDER
      // the strike char-level instead of a record-blob LWW fight.
      if *struck {
        text
          .mark(1..len, flowstate_document::MARK_STRIKETHROUGH, true)
          .context("marking cell strikethrough")?;
      } else {
        text
          .unmark(1..len, flowstate_document::MARK_STRIKETHROUGH)
          .context("unmarking cell strikethrough")?;
      }
      Ok(Some(Derived::Content { cell: *cell_id, caret: None }))
    },
    FlowIntent::EnsureCellEditable { sheet_id, cell_id } => {
      let sheet = sheet_of(core, *sheet_id)?;
      let cell = sheet.cell(*cell_id).ok_or(FlowWriteRejected::UnknownCell(*cell_id))?;
      if cell.summary.uses_summary_projection {
        return Ok(None);
      }
      let text = loro_schema::cell_text(doc, *cell_id).ok_or(FlowWriteRejected::UnknownCell(*cell_id))?;
      if text.len_unicode() == 0 {
        return Err(FlowWriteRejected::StructureViolation("cell flow is unseeded".into()).into());
      }
      // Restyle the FIRST paragraph (boundary 0) to the editable tag style.
      text
        .mark(
          0..1,
          flowstate_document::MARK_PARAGRAPH_STYLE,
          cell_text::paragraph_style_value(loro_schema::CELL_SEED_PARAGRAPH_STYLE),
        )
        .context("restyling first paragraph to the tag style")?;
      Ok(Some(Derived::Content { cell: *cell_id, caret: None }))
    },
    FlowIntent::ReplaceCellContent {
      sheet_id,
      cell_id,
      paragraphs,
    } => {
      let sheet = sheet_of(core, *sheet_id)?;
      if sheet.cell(*cell_id).is_none() {
        return Err(FlowWriteRejected::UnknownCell(*cell_id).into());
      }
      let cell_map = loro_schema::cell_map(doc, *cell_id).ok_or(FlowWriteRejected::UnknownCell(*cell_id))?;
      loro_schema::cell_flow_from_paragraphs(&cell_map, paragraphs).context("replacing cell content")?;
      Ok(Some(Derived::Content { cell: *cell_id, caret: None }))
    },
    FlowIntent::AddAnnotation { sheet_id, stroke } => {
      sheet_of(core, *sheet_id)?;
      if stroke.sheet_id != *sheet_id {
        return Err(FlowWriteRejected::StructureViolation("annotation sheet id mismatch".into()).into());
      }
      loro_schema::put_annotation(doc, stroke).context("writing annotation stroke")?;
      if let Some(sheet) = core.board_mut().sheet_mut(*sheet_id) {
        sheet.annotations.retain(|existing| existing.id != stroke.id);
        sheet.annotations.push(stroke.clone());
        sheet.annotations.sort_by_key(|stroke| stroke.id);
      }
      Ok(Some(Derived::Board))
    },
    FlowIntent::DeleteAnnotation {
      sheet_id,
      stroke_id,
      originator,
    } => {
      let sheet = sheet_of(core, *sheet_id)?;
      let owned = sheet
        .annotations
        .iter()
        .any(|stroke| stroke.id == *stroke_id && &stroke.originator == originator);
      if !owned {
        return Ok(None);
      }
      loro_schema::delete_annotation(doc, *stroke_id).context("deleting annotation stroke")?;
      if let Some(sheet) = core.board_mut().sheet_mut(*sheet_id) {
        sheet.annotations.retain(|stroke| stroke.id != *stroke_id);
      }
      Ok(Some(Derived::Board))
    },
    FlowIntent::ClearAnnotations { sheet_id, originator } => {
      if let Some(sheet_id) = sheet_id {
        sheet_of(core, *sheet_id)?;
      }
      let doomed: Vec<Uuid> = core
        .board_ref()
        .sheets
        .iter()
        .filter(|sheet| sheet_id.is_none_or(|target| sheet.id == target))
        .flat_map(|sheet| &sheet.annotations)
        .filter(|stroke| &stroke.originator == originator)
        .map(|stroke| stroke.id)
        .collect();
      if doomed.is_empty() {
        return Ok(None);
      }
      for stroke in &doomed {
        loro_schema::delete_annotation(doc, *stroke).context("clearing annotation stroke")?;
      }
      for sheet in &mut core.board_mut().sheets {
        if sheet_id.is_none_or(|target| sheet.id == target) {
          sheet.annotations.retain(|stroke| &stroke.originator != originator);
        }
      }
      Ok(Some(Derived::Board))
    },
    FlowIntent::CellText { cell_id, intent } => {
      let (owning_sheet, _) = core
        .board_ref()
        .cell(*cell_id)
        .ok_or(FlowWriteRejected::UnknownCell(*cell_id))?;
      let _ = owning_sheet;
      let ctx = CellTextContext::resolve(doc, *cell_id)?;
      // Identity basis: the OPEN cell's cached paragraph ids (the projection
      // the intent's ids came from); fall back to a fresh materialization for
      // a not-yet-open cell.
      let paragraph_ids: Vec<gpui_flowtext::ParagraphId> = match core.cells.get(cell_id) {
        Some(entry) => entry.paragraph_ids.clone(),
        None => flowstate_flow::loro_projection::materialize_cell_rows(doc, *cell_id)
          .map(|rows| rows.paragraph_ids)
          .map_err(|_| FlowWriteRejected::UnknownCell(*cell_id))?,
      };
      let plan = cell_text::resolve_cell_plan(doc, &ctx, &paragraph_ids, intent)?;
      let caret = cell_text::execute_cell_plan(&ctx, &plan).context("executing cell text plan")?;
      Ok(Some(Derived::Content {
        cell: *cell_id,
        caret,
      }))
    },
  }
}

/// Resolve a new cell's placement into `(column_index, flat_insertion_index,
/// parent)` against the maintained board — the former `add_*` quintet as one
/// law.
fn resolve_placement(
  core: &FlowRuntime,
  sheet_id: SheetId,
  columns: &[Uuid],
  placement: &CellPlacement,
) -> Result<(usize, usize, Option<CellId>), FlowWriteRejected> {
  let board = core.board_ref();
  let sheet = board.sheet(sheet_id).ok_or(FlowWriteRejected::UnknownSheet(sheet_id))?;
  match placement {
    CellPlacement::ColumnEnd { column_index } => {
      if *column_index >= columns.len() {
        return Err(FlowWriteRejected::StructureViolation("column index out of range".into()));
      }
      Ok((*column_index, sheet.cells.len(), None))
    },
    CellPlacement::ColumnTop { column_index } => {
      if *column_index >= columns.len() {
        return Err(FlowWriteRejected::StructureViolation("column index out of range".into()));
      }
      Ok((*column_index, 0, None))
    },
    CellPlacement::Sibling { of, position } => {
      let index = sheet
        .cells
        .iter()
        .position(|cell| cell.id == *of)
        .ok_or(FlowWriteRejected::UnknownCell(*of))?;
      let source = &sheet.cells[index];
      let column = columns
        .iter()
        .position(|column| *column == source.column_id)
        .ok_or_else(|| FlowWriteRejected::StructureViolation("sibling references unknown column".into()))?;
      let insertion = match position {
        RelativePosition::Before => index,
        // AFTER the sibling's WHOLE subtree: the canonical order keeps every
        // subtree contiguous, so "after the sibling" can never mean "between
        // the sibling and its responses".
        RelativePosition::After => {
          let subtree = board_ops::subtree_cell_ids(sheet, *of);
          sheet
            .cells
            .iter()
            .enumerate()
            .filter(|(_, cell)| subtree.contains(&cell.id))
            .map(|(index, _)| index)
            .max()
            .unwrap_or(index)
            + 1
        },
      };
      Ok((column, insertion, source.parent_id))
    },
    CellPlacement::FirstResponseTo { parent } | CellPlacement::ResponseTo { parent } => {
      let parent_cell = sheet.cell(*parent).ok_or(FlowWriteRejected::UnknownCell(*parent))?;
      let parent_column = columns
        .iter()
        .position(|column| *column == parent_cell.column_id)
        .ok_or_else(|| FlowWriteRejected::StructureViolation("parent references unknown column".into()))?;
      let child_column = parent_column + 1;
      if child_column >= columns.len() {
        return Err(FlowWriteRejected::StructureViolation("rightmost cells cannot receive responses".into()));
      }
      let insertion = match placement {
        CellPlacement::FirstResponseTo { .. } => board_ops::child_prepend_index(board, sheet_id, *parent),
        _ => board_ops::child_append_index(board, sheet_id, *parent),
      }
      .map_err(|error| FlowWriteRejected::StructureViolation(format!("{error:#}")))?;
      Ok((child_column, insertion, Some(*parent)))
    },
  }
}

/// Re-linearize a sheet canonically after a structural mutation, in BOTH
/// representations inside the same commit: the maintained board's cell vec and
/// the raw Loro order list. Keeps "raw order == canonical order" a write-path
/// invariant instead of a view-time repair, so the materializer's DFS
/// normalization is a no-op on locally-produced states.
fn canonicalize_sheet_order(core: &mut FlowRuntime, doc: &LoroDoc, sheet_id: SheetId) -> Result<(), ExecuteError> {
  let Some(sheet) = core.board_mut().sheet_mut(sheet_id) else {
    return Ok(());
  };
  board_ops::canonicalize_sheet(sheet);
  let target: Vec<String> = sheet.cells.iter().map(|cell| cell.id.to_string()).collect();
  let sheet_map = loro_schema::sheet_map(doc, sheet_id).context("resolving sheet map for canonicalization")?;
  // ALWAYS diff against the live raw order: after a merge, raw may sit in a
  // non-canonical interleaving (normalized only in the materialized view), so
  // "maintained unchanged" does not imply "raw already matches".
  if loro_schema::cell_order_ids(&sheet_map) != target {
    apply_order_diff(&sheet_map, &target).context("re-linearizing sheet order")?;
  }
  Ok(())
}

/// Reconcile the live cell order onto `target`: drop dead/duplicate entries,
/// append missing ones, then a minimal `mov` sequence (only displaced elements
/// move; a subtree move emits one `mov` per member). Handles post-merge raw
/// orders that carry liveness drift the materialized view already normalized.
fn apply_order_diff(sheet_map: &loro::LoroMap, target: &[String]) -> Result<()> {
  let order = loro_schema::cell_order_list(sheet_map).context("resolving cell order list")?;
  reconcile_order_list(&order, &loro_schema::cell_order_ids(sheet_map), target)
}

/// The generic movable-list reconcile behind [`apply_order_diff`], shared with
/// the import-side canonicalization repair (board sheet order + cell orders).
pub(crate) fn reconcile_order_list(order: &loro::LoroMovableList, current: &[String], target: &[String]) -> Result<()> {
  let mut working = current.to_vec();
  let target_set: std::collections::HashSet<&String> = target.iter().collect();
  // Dead or duplicate entries, removed back-to-front so indices stay live.
  let mut seen: std::collections::HashSet<&String> = std::collections::HashSet::new();
  let mut doomed: Vec<usize> = Vec::new();
  for (index, id) in working.iter().enumerate() {
    if !target_set.contains(id) || !seen.insert(id) {
      doomed.push(index);
    }
  }
  for index in doomed.into_iter().rev() {
    order.delete(index, 1).context("dropping dead order entry")?;
    working.remove(index);
  }
  // Missing target entries append (their canonical position lands via `mov`).
  for id in target {
    if !working.iter().any(|entry| entry == id) {
      order.insert(working.len(), id.as_str()).context("appending missing order entry")?;
      working.push(id.clone());
    }
  }
  anyhow::ensure!(
    working.len() == target.len(),
    "order reconcile left mismatched sets ({} live vs {} target)",
    working.len(),
    target.len()
  );
  for index in 0..target.len() {
    if working[index] == target[index] {
      continue;
    }
    let from = working
      .iter()
      .position(|id| *id == target[index])
      .context("target order entry missing from live order")?;
    order.mov(from, index).context("moving order entry")?;
    let id = working.remove(from);
    working.insert(index, id);
  }
  Ok(())
}

/// Import-side canonicalization repair (the flow mirror of
/// `schedule_projection_repairs`): rewrite the raw sheet/cell order lists to
/// the canonical linearization the materialized view already shows, under the
/// `"repair"` origin (undo-excluded), publishing the pass like any commit.
/// Returns whether anything was rewritten. The caller caps invocations.
pub(crate) fn repair_canonical_orders(core: &mut FlowRuntime) -> Result<bool> {
  let doc = core.doc().clone();
  let vv_before = doc.oplog_vv();
  let mut changed = false;

  let target_sheets: Vec<String> = core
    .board_ref()
    .sheets
    .iter()
    .map(|sheet| sheet.id.to_string())
    .collect();
  let sheet_order = loro_schema::sheet_order(&doc);
  let current_sheets = loro_schema::sheet_order_ids(&doc);
  if current_sheets != target_sheets {
    reconcile_order_list(&sheet_order, &current_sheets, &target_sheets).context("repairing sheet order")?;
    changed = true;
  }

  let sheets: Vec<(SheetId, Vec<String>)> = core
    .board_ref()
    .sheets
    .iter()
    .map(|sheet| (sheet.id, sheet.cells.iter().map(|cell| cell.id.to_string()).collect()))
    .collect();
  for (sheet_id, target) in sheets {
    let Some(sheet_map) = loro_schema::sheet_map(&doc, sheet_id) else {
      continue;
    };
    if loro_schema::cell_order_ids(&sheet_map) != target {
      apply_order_diff(&sheet_map, &target).context("repairing cell order")?;
      changed = true;
    }
  }

  if changed {
    doc.set_next_commit_origin("repair");
    doc.set_next_commit_message("flow-order-canonicalization");
    doc.commit();
    core.queue_local_update_publish(&vv_before);
  }
  Ok(changed)
}

/// Cursor-backed caret snapshot after a cell-text intent (spec §8 shape).
fn cell_selection_snapshot(doc: &LoroDoc, cell: CellId, caret_unicode: usize) -> Option<SelectionSnapshot> {
  let text = loro_schema::cell_text(doc, cell)?;
  let cursor = cursor_for_boundary(&text, caret_unicode, crate::presence::SelectionAffinity::Neutral)?;
  let ctx = CellTextContext::resolve(doc, cell).ok()?;
  let offset = ctx
    .offset_for_unicode(caret_unicode)
    .unwrap_or(gpui_flowtext::DocumentOffset { paragraph: 0, byte: 0 });
  let endpoint = CursorEndpoint {
    cursor: cursor.encode(),
    delta: 0,
    affinity: SelectionAffinity::Neutral,
    gravity: VisualGravity::Neutral,
    offset,
  };
  Some(SelectionSnapshot {
    anchor: endpoint.clone(),
    head: endpoint,
  })
}

/// I-10 recovery, verbatim law from the .db8 side: repair origin BEFORE
/// `revert_to` (its internal diff commits the partial), explicit repair commit
/// for the inverse ops, ONE publish payload covering partial + inverse, then a
/// defensive full refresh.
fn compensate_failed_intent(
  core: &mut FlowRuntime,
  doc: &LoroDoc,
  frontier_before: &loro::Frontiers,
  vv_before: &loro::VersionVector,
  class: &'static str,
  error: &anyhow::Error,
) -> FlowWriteRejected {
  tracing::error!(class, %error, "flow intent failed mid-apply; compensating via revert_to (I-10)");
  doc.set_next_commit_origin("repair");
  doc.set_next_commit_message("flow-intent-compensation");
  if let Err(revert_error) = doc.revert_to(frontier_before) {
    tracing::error!(class, %revert_error, "revert_to during flow intent compensation FAILED; core must be reloaded");
    return FlowWriteRejected::CompensationFailed {
      class,
      diagnostic: format!("mid-apply error: {error:#}; revert_to error: {revert_error}"),
    };
  }
  doc.set_next_commit_origin("repair");
  doc.set_next_commit_message("flow-intent-compensation-inverse");
  doc.commit();
  // One atomic publish payload covering partial + inverse (spec I-10c).
  core.queue_local_update_publish(vv_before);
  if let Err(refresh_error) = core.refresh_all() {
    tracing::error!(class, %refresh_error, "flow refresh after compensation failed; core must be reloaded");
    return FlowWriteRejected::CompensationFailed {
      class,
      diagnostic: format!("mid-apply error: {error:#}; post-compensation refresh error: {refresh_error:#}"),
    };
  }
  FlowWriteRejected::CompensatedFailure {
    class,
    diagnostic: format!("{error:#}"),
  }
}
