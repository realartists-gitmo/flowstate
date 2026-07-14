//! The schema-level flow document: a Loro doc + its normalized board
//! projection. This is the SOLO/fixture/test entrance to the one intent
//! executor ([`crate::mutate`]); the collaborative runtime
//! (`flowstate-collab/src/flow`) wraps the same doc behind a write gate with
//! streams and recorded-inverse undo. Compat op methods mirror the legacy
//! surface as thin intent wrappers until the app's Living Grid rewiring
//! (build order S8) lands.

use loro::{ExportMode, LoroDoc, UndoManager, VersionVector};
use uuid::Uuid;

use crate::format::{FlowFormat, SheetId, SheetTypeId};
use crate::intents::{AnnotationScope, CellPlacement, CellSeed, FlowDropIntent, FlowIntent, RelativePosition};
use crate::loro_schema;
use crate::mutate::{self, MutationReport};
use crate::projection::{AnnotationOriginator, AnnotationStroke, FlowBoardProjection, FlowDefect, Sheet};
use crate::{CellId, board_ops};

pub type FlowFrontier = Vec<u8>;

pub struct FlowDocument {
  loro: LoroDoc,
  board: FlowBoardProjection,
  defects: Vec<FlowDefect>,
  undo_manager: UndoManager,
}

impl Clone for FlowDocument {
  fn clone(&self) -> Self {
    Self::from_snapshot(&self.snapshot().expect("validated flow snapshot")).expect("validated flow clone")
  }
}

impl Default for FlowDocument {
  fn default() -> Self {
    Self::new()
  }
}

impl FlowDocument {
  pub fn new() -> Self {
    Self::with_format(FlowFormat::policy_debate())
  }

  pub fn with_format(format: FlowFormat) -> Self {
    let loro = LoroDoc::new();
    loro_schema::init_flow_document(&loro, &format, Uuid::new_v4()).expect("fresh flow document seeds");
    let undo_manager = UndoManager::new(&loro);
    let mut document = Self {
      loro,
      board: FlowBoardProjection { format, sheets: Vec::new() },
      defects: Vec::new(),
      undo_manager,
    };
    document.reload().expect("fresh flow document materializes");
    document
  }

  pub fn from_snapshot(snapshot: &[u8]) -> anyhow::Result<Self> {
    let loro = LoroDoc::new();
    loro_schema::configure_flow_doc(&loro);
    loro.import(snapshot)?;
    match loro_schema::schema_version(&loro) {
      Some(crate::loro_schema::SCHEMA_VERSION) => {},
      Some(version) => anyhow::bail!("unsupported flow schema version {version}"),
      None => anyhow::bail!("Loro snapshot is missing immutable format definition"),
    }
    let undo_manager = UndoManager::new(&loro);
    let mut document = Self {
      loro,
      board: FlowBoardProjection::default(),
      defects: Vec::new(),
      undo_manager,
    };
    document.reload()?;
    Ok(document)
  }

  pub fn snapshot(&self) -> anyhow::Result<Vec<u8>> {
    Ok(self.loro.export(ExportMode::Snapshot)?)
  }

  pub fn doc(&self) -> &LoroDoc {
    &self.loro
  }

  pub fn projection(&self) -> &FlowBoardProjection {
    &self.board
  }

  pub fn defects(&self) -> &[FlowDefect] {
    &self.defects
  }

  pub fn document_id(&self) -> Option<Uuid> {
    loro_schema::document_id(&self.loro)
  }

  pub fn frontier(&self) -> FlowFrontier {
    self.loro.state_frontiers().encode()
  }

  pub fn version_vector(&self) -> VersionVector {
    self.loro.oplog_vv()
  }

  pub fn export_updates_for(&self, version: &VersionVector) -> anyhow::Result<Vec<u8>> {
    Ok(self.loro.export(ExportMode::updates(version))?)
  }

  pub fn import_updates(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
    self.loro.import(bytes)?;
    self.reload()
  }

  /// The one write path: resolve → mutate → one commit → rematerialize.
  pub fn apply_intent(&mut self, intent: &FlowIntent) -> anyhow::Result<MutationReport> {
    let report = mutate::execute_intent(&self.loro, &self.board, intent)?;
    self.loro.set_next_commit_message(intent.class());
    self.loro.commit();
    loro_schema::touch_modified(&self.loro)?;
    self.loro.commit();
    self.reload()?;
    Ok(report)
  }

  pub fn reload(&mut self) -> anyhow::Result<()> {
    let materialized = crate::loro_projection::board_from_loro(&self.loro)?;
    self.board = materialized.board;
    self.defects = materialized.defects;
    Ok(())
  }

  /// Materialize one cell's rich text as a full editor projection (durable
  /// registry ids, current frontier).
  pub fn cell_document(&self, cell_id: CellId) -> anyhow::Result<flowstate_document::DocumentProjection> {
    crate::loro_projection::cell_document(&self.loro, cell_id)
  }

  pub fn can_undo(&self) -> bool {
    self.undo_manager.can_undo()
  }

  pub fn can_redo(&self) -> bool {
    self.undo_manager.can_redo()
  }

  pub fn undo(&mut self) -> anyhow::Result<bool> {
    let changed = self.undo_manager.undo()?;
    if changed {
      self.reload()?;
    }
    Ok(changed)
  }

  pub fn redo(&mut self) -> anyhow::Result<bool> {
    let changed = self.undo_manager.redo()?;
    if changed {
      self.reload()?;
    }
    Ok(changed)
  }
}

/// Legacy-surface compat: thin intent wrappers so pre-S8 app call sites keep
/// compiling. New code constructs [`FlowIntent`]s directly.
impl FlowDocument {
  pub fn create_sheet(&mut self, name: impl Into<String>, sheet_type_id: SheetTypeId) -> anyhow::Result<SheetId> {
    let sheet_id = Uuid::new_v4();
    self.apply_intent(&FlowIntent::CreateSheet {
      sheet_id,
      name: name.into(),
      sheet_type_id,
    })?;
    Ok(sheet_id)
  }

  pub fn rename_sheet(&mut self, sheet_id: SheetId, name: impl Into<String>) -> anyhow::Result<()> {
    self
      .apply_intent(&FlowIntent::RenameSheet { sheet_id, name: name.into() })
      .map(|_| ())
  }

  pub fn delete_sheet(&mut self, sheet_id: SheetId) -> anyhow::Result<()> {
    self
      .apply_intent(&FlowIntent::DeleteSheet { sheet_id })
      .map(|_| ())
  }

  pub fn move_sheet(&mut self, sheet_id: SheetId, target_index: usize) -> anyhow::Result<()> {
    let before = self
      .board
      .sheets
      .iter()
      .filter(|sheet| sheet.id != sheet_id)
      .nth(target_index)
      .map(|sheet| sheet.id);
    self
      .apply_intent(&FlowIntent::MoveSheet { sheet_id, before })
      .map(|_| ())
  }

  pub fn add_plain_cell(
    &mut self,
    sheet_id: SheetId,
    column_index: usize,
    parent_id: Option<CellId>,
    insertion_index: Option<usize>,
  ) -> anyhow::Result<CellId> {
    let cell_id = Uuid::new_v4();
    let placement = match (parent_id, insertion_index) {
      (Some(parent), _) => CellPlacement::LastChildOf(parent),
      (None, Some(0)) => CellPlacement::ColumnTop { column_index },
      (None, Some(index)) => {
        // Positional hint: anchor to the cell currently at `index`, else end.
        let sheet = self
          .board
          .sheet(sheet_id)
          .ok_or_else(|| anyhow::anyhow!("unknown sheet"))?;
        match sheet.cells.get(index) {
          Some(anchor) => CellPlacement::Before(anchor.id),
          None => CellPlacement::SheetEnd { column_index },
        }
      },
      (None, None) => CellPlacement::SheetEnd { column_index },
    };
    self.apply_intent(&FlowIntent::AddCell {
      sheet_id,
      cell_id,
      placement,
      seed: CellSeed::Empty,
    })?;
    Ok(cell_id)
  }

  pub fn add_orphan_at_column_top(&mut self, sheet_id: SheetId, column_index: usize) -> anyhow::Result<CellId> {
    let cell_id = Uuid::new_v4();
    self.apply_intent(&FlowIntent::AddCell {
      sheet_id,
      cell_id,
      placement: CellPlacement::ColumnTop { column_index },
      seed: CellSeed::Empty,
    })?;
    Ok(cell_id)
  }

  pub fn add_sibling(&mut self, sheet_id: SheetId, cell_id: CellId, position: RelativePosition) -> anyhow::Result<CellId> {
    let new_id = Uuid::new_v4();
    let placement = match position {
      RelativePosition::Before => CellPlacement::Before(cell_id),
      RelativePosition::After => CellPlacement::After(cell_id),
    };
    self.apply_intent(&FlowIntent::AddCell {
      sheet_id,
      cell_id: new_id,
      placement,
      seed: CellSeed::Empty,
    })?;
    Ok(new_id)
  }

  pub fn add_response(&mut self, sheet_id: SheetId, parent_id: CellId) -> anyhow::Result<CellId> {
    let new_id = Uuid::new_v4();
    self.apply_intent(&FlowIntent::AddCell {
      sheet_id,
      cell_id: new_id,
      placement: CellPlacement::LastChildOf(parent_id),
      seed: CellSeed::Empty,
    })?;
    Ok(new_id)
  }

  pub fn add_first_response(&mut self, sheet_id: SheetId, parent_id: CellId) -> anyhow::Result<CellId> {
    let new_id = Uuid::new_v4();
    self.apply_intent(&FlowIntent::AddCell {
      sheet_id,
      cell_id: new_id,
      placement: CellPlacement::FirstChildOf(parent_id),
      seed: CellSeed::Empty,
    })?;
    Ok(new_id)
  }

  pub fn delete_cell(&mut self, sheet_id: SheetId, cell_id: CellId) -> anyhow::Result<()> {
    self
      .apply_intent(&FlowIntent::DeleteCell { sheet_id, cell_id })
      .map(|_| ())
  }

  pub fn move_cell_subtree(&mut self, sheet_id: SheetId, cell_id: CellId, intent: FlowDropIntent) -> anyhow::Result<()> {
    self
      .apply_intent(&FlowIntent::MoveCellSubtree {
        sheet_id,
        cell_id,
        drop: intent,
      })
      .map(|_| ())
  }

  pub fn move_cell(
    &mut self,
    sheet_id: SheetId,
    cell_id: CellId,
    target_column: usize,
    target_index: usize,
    new_parent: Option<CellId>,
  ) -> anyhow::Result<()> {
    let intent = new_parent.map_or(
      FlowDropIntent::RootInColumn {
        column_index: target_column,
        insertion_index: target_index,
      },
      FlowDropIntent::LastChildOf,
    );
    self.move_cell_subtree(sheet_id, cell_id, intent)
  }

  /// Toggle strike over the whole cell (legacy surface computed the toggle).
  pub fn strike_cell(&mut self, sheet_id: SheetId, cell_id: CellId) -> anyhow::Result<()> {
    let struck = self
      .board
      .sheet(sheet_id)
      .and_then(|sheet| sheet.cells.iter().find(|cell| cell.id == cell_id))
      .map(|cell| cell.summary.struck)
      .ok_or_else(|| anyhow::anyhow!("unknown cell"))?;
    self
      .apply_intent(&FlowIntent::SetCellStruck {
        sheet_id,
        cell_id,
        struck: !struck,
      })
      .map(|_| ())
  }

  pub fn ensure_cell_editable_projection(&mut self, sheet_id: SheetId, cell_id: CellId) -> anyhow::Result<bool> {
    let uses_summary = self
      .board
      .sheet(sheet_id)
      .and_then(|sheet| sheet.cells.iter().find(|cell| cell.id == cell_id))
      .map(|cell| cell.summary.uses_summary_projection)
      .ok_or_else(|| anyhow::anyhow!("unknown cell"))?;
    if uses_summary {
      return Ok(false);
    }
    self.apply_intent(&FlowIntent::EnsureCellEditable { sheet_id, cell_id })?;
    Ok(true)
  }

  /// Transitional solo write-back: replace a cell's rich text from its
  /// editor's projection (S4's per-cell authority supersedes this).
  pub fn replace_cell_document(
    &mut self,
    sheet_id: SheetId,
    cell_id: CellId,
    document: &flowstate_document::DocumentProjection,
  ) -> anyhow::Result<()> {
    self
      .board
      .sheet(sheet_id)
      .and_then(|sheet| sheet.cells.iter().find(|cell| cell.id == cell_id))
      .ok_or_else(|| anyhow::anyhow!("unknown cell"))?;
    mutate::replace_cell_document(&self.loro, cell_id, document)?;
    self.loro.set_next_commit_message("cell.replace-document");
    self.loro.commit();
    self.reload()
  }

  pub fn add_annotation(&mut self, sheet_id: SheetId, stroke: AnnotationStroke) -> anyhow::Result<()> {
    if stroke.sheet_id != sheet_id {
      anyhow::bail!("annotation sheet id mismatch");
    }
    self
      .apply_intent(&FlowIntent::AddAnnotation { stroke })
      .map(|_| ())
  }

  pub fn clear_annotations(&mut self, sheet_id: SheetId, originator: &AnnotationOriginator) -> anyhow::Result<()> {
    self
      .apply_intent(&FlowIntent::ClearAnnotations {
        scope: AnnotationScope::Sheet(sheet_id),
        originator: originator.clone(),
      })
      .map(|_| ())
  }

  pub fn clear_all_annotations(&mut self, originator: &AnnotationOriginator) -> anyhow::Result<()> {
    self
      .apply_intent(&FlowIntent::ClearAnnotations {
        scope: AnnotationScope::AllSheets,
        originator: originator.clone(),
      })
      .map(|_| ())
  }

  pub fn delete_annotation(&mut self, sheet_id: SheetId, stroke_id: Uuid, originator: &AnnotationOriginator) -> anyhow::Result<bool> {
    match self.apply_intent(&FlowIntent::DeleteAnnotation {
      sheet_id,
      stroke_id,
      originator: originator.clone(),
    }) {
      Ok(_) => Ok(true),
      Err(error) if error.to_string().contains("unknown annotation") => Ok(false),
      Err(error) => Err(error),
    }
  }

  // ---- pure preview pass-throughs (kept for pre-S8 call sites) -----------

  pub fn child_append_index(&self, sheet_id: SheetId, parent_id: CellId) -> anyhow::Result<usize> {
    let sheet = self
      .board
      .sheet(sheet_id)
      .ok_or_else(|| anyhow::anyhow!("unknown sheet"))?;
    board_ops::child_append_index(sheet, parent_id)
  }

  pub fn child_prepend_index(&self, sheet_id: SheetId, parent_id: CellId) -> anyhow::Result<usize> {
    let sheet = self
      .board
      .sheet(sheet_id)
      .ok_or_else(|| anyhow::anyhow!("unknown sheet"))?;
    board_ops::child_prepend_index(sheet, parent_id)
  }

  pub fn deletion_fallback(&self, sheet_id: SheetId, cell_id: CellId) -> Option<CellId> {
    let sheet = self.board.sheet(sheet_id)?;
    let column_ids = self.board.sheet_column_ids(sheet_id).ok()?;
    board_ops::deletion_fallback(sheet, &column_ids, cell_id)
  }

  pub fn preview_move_cell_subtree(&self, sheet_id: SheetId, cell_id: CellId, intent: FlowDropIntent) -> Option<Sheet> {
    let sheet = self.board.sheet(sheet_id)?;
    let column_ids = self.board.sheet_column_ids(sheet_id).ok()?;
    board_ops::preview_move_cell_subtree(sheet, &column_ids, cell_id, intent)
  }

  pub fn preview_without_subtree(&self, sheet_id: SheetId, cell_id: CellId) -> Option<Sheet> {
    let sheet = self.board.sheet(sheet_id)?;
    board_ops::preview_without_subtree(sheet, cell_id)
  }

  pub fn subtree_cell_ids_for(&self, sheet_id: SheetId, cell_id: CellId) -> Vec<CellId> {
    self
      .board
      .sheet(sheet_id)
      .map(|sheet| board_ops::subtree_cell_ids(sheet, cell_id))
      .unwrap_or_default()
  }
}
