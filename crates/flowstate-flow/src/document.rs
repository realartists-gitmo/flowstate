//! The schema-level flow document: a Loro doc + its normalized grid
//! projection. This is the SOLO/fixture/test entrance to the one intent
//! executor ([`crate::mutate`]); the collaborative runtime
//! (`flowstate-collab/src/flow`) wraps the same doc behind a write gate with
//! streams and recorded-inverse undo.

use loro::{ExportMode, LoroDoc, UndoManager, VersionVector};
use uuid::Uuid;

use crate::format::{ArgumentSide, ColumnId, FlowFormat, RowId, SheetId, SheetTypeId};
use crate::intents::{AnnotationScope, CellSeed, FlowIntent};
use crate::loro_schema;
use crate::mutate::{self, MutationReport};
use crate::projection::{AnnotationOriginator, AnnotationStroke, FlowBoardProjection, FlowDefect};
use crate::CellId;

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
      board: FlowBoardProjection {
        format,
        sheets: Vec::new(),
        round: crate::projection::RoundMetadata::default(),
      },
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

/// Grid conveniences: thin intent wrappers that mint fresh ids. New code that
/// already holds ids constructs [`FlowIntent`]s directly.
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

  pub fn move_sheet(&mut self, sheet_id: SheetId, before: Option<SheetId>) -> anyhow::Result<()> {
    self
      .apply_intent(&FlowIntent::MoveSheet { sheet_id, before })
      .map(|_| ())
  }

  /// Insert `count` fresh rows before the anchor (`None` = end); returns the
  /// minted row ids in order.
  pub fn insert_rows(&mut self, sheet_id: SheetId, before: Option<RowId>, count: usize) -> anyhow::Result<Vec<RowId>> {
    let row_ids: Vec<RowId> = (0..count).map(|_| Uuid::new_v4()).collect();
    self.apply_intent(&FlowIntent::InsertRows {
      sheet_id,
      before,
      row_ids: row_ids.clone(),
    })?;
    Ok(row_ids)
  }

  pub fn insert_row(&mut self, sheet_id: SheetId, before: Option<RowId>) -> anyhow::Result<RowId> {
    Ok(self.insert_rows(sheet_id, before, 1)?[0])
  }

  /// Append a fresh row at the bottom and return its id (the ghost-row
  /// materialization primitive).
  pub fn append_row(&mut self, sheet_id: SheetId) -> anyhow::Result<RowId> {
    self.insert_row(sheet_id, None)
  }

  pub fn move_rows(&mut self, sheet_id: SheetId, row_ids: Vec<RowId>, before: Option<RowId>) -> anyhow::Result<()> {
    self
      .apply_intent(&FlowIntent::MoveRows { sheet_id, row_ids, before })
      .map(|_| ())
  }

  pub fn delete_rows(&mut self, sheet_id: SheetId, row_ids: Vec<RowId>) -> anyhow::Result<()> {
    self
      .apply_intent(&FlowIntent::DeleteRows { sheet_id, row_ids })
      .map(|_| ())
  }

  pub fn set_row_height(&mut self, sheet_id: SheetId, row_id: RowId, height: Option<f32>) -> anyhow::Result<()> {
    self
      .apply_intent(&FlowIntent::SetRowHeight { sheet_id, row_id, height })
      .map(|_| ())
  }

  pub fn add_column(
    &mut self,
    sheet_id: SheetId,
    label: impl Into<String>,
    side: ArgumentSide,
    before: Option<ColumnId>,
  ) -> anyhow::Result<ColumnId> {
    let column_id = Uuid::new_v4();
    self.apply_intent(&FlowIntent::AddColumn {
      sheet_id,
      column_id,
      label: label.into(),
      side,
      before,
    })?;
    Ok(column_id)
  }

  /// A fresh empty cell at (row, column).
  pub fn add_cell_at(&mut self, sheet_id: SheetId, row_id: RowId, column_id: ColumnId) -> anyhow::Result<CellId> {
    let cell_id = Uuid::new_v4();
    self.apply_intent(&FlowIntent::AddCell {
      sheet_id,
      cell_id,
      row_id,
      column_id,
      seed: CellSeed::Empty,
    })?;
    Ok(cell_id)
  }

  pub fn set_cell_address(&mut self, sheet_id: SheetId, cell_id: CellId, row_id: RowId, column_id: ColumnId) -> anyhow::Result<()> {
    self
      .apply_intent(&FlowIntent::SetCellAddress {
        sheet_id,
        cell_id,
        row_id,
        column_id,
      })
      .map(|_| ())
  }

  pub fn delete_cell(&mut self, sheet_id: SheetId, cell_id: CellId) -> anyhow::Result<()> {
    self
      .apply_intent(&FlowIntent::DeleteCell { sheet_id, cell_id })
      .map(|_| ())
  }

  /// Toggle strike over the whole cell (legacy surface computed the toggle).
  pub fn strike_cell(&mut self, sheet_id: SheetId, cell_id: CellId) -> anyhow::Result<()> {
    let struck = self
      .board
      .sheet(sheet_id)
      .and_then(|sheet| sheet.find_cell(cell_id))
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
      .and_then(|sheet| sheet.find_cell(cell_id))
      .map(|cell| cell.summary.uses_summary_projection)
      .ok_or_else(|| anyhow::anyhow!("unknown cell"))?;
    if uses_summary {
      return Ok(false);
    }
    self.apply_intent(&FlowIntent::EnsureCellEditable { sheet_id, cell_id })?;
    Ok(true)
  }

  /// Transitional solo write-back: replace a cell's rich text from its
  /// editor's projection (surfaces on the per-cell authority don't need it).
  pub fn replace_cell_document(
    &mut self,
    sheet_id: SheetId,
    cell_id: CellId,
    document: &flowstate_document::DocumentProjection,
  ) -> anyhow::Result<()> {
    self
      .board
      .sheet(sheet_id)
      .and_then(|sheet| sheet.find_cell(cell_id))
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
}
