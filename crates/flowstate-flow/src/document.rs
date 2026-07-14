use std::collections::{HashMap, HashSet};

use anyhow::{Context as _, bail};
use loro::{ExportMode, LoroDoc, LoroValue, UndoManager, VersionVector, ValueOrContainer};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{
  FlowCommitResult, FlowFrontier, FlowProjectionSnapshot, FlowRuntimeEvent, FlowTransactionId, FlowUpdateBytes, StaleFlowProjectionError,
};

pub type FormatId = Uuid;
pub type SheetTypeId = Uuid;
pub type SheetId = Uuid;
pub type ColumnId = Uuid;
pub type CellId = Uuid;
pub type StrokeId = Uuid;

#[derive(Clone, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnnotationOriginator(pub String);

const FORMAT_KEY: &str = "immutable-format";
const ANNOTATIONS_MAP: &str = "flow-annotations";
const SHEETS_MAP: &str = "flow-sheets";
const CELLS_MAP: &str = "flow-cells";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ArgumentSide {
  One,
  Two,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ColumnDefinition {
  pub id: ColumnId,
  pub label: String,
  pub side: ArgumentSide,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SheetTypeDefinition {
  pub id: SheetTypeId,
  pub name: String,
  pub columns: Vec<ColumnDefinition>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowFormat {
  pub id: FormatId,
  pub name: String,
  pub sheet_types: Vec<SheetTypeDefinition>,
}

impl FlowFormat {
  pub fn policy_debate() -> Self {
    let affirmative = sheet_type(
      "Affirmative",
      &[
        ("1AC", ArgumentSide::One),
        ("1NC", ArgumentSide::Two),
        ("2AC", ArgumentSide::One),
        ("Block", ArgumentSide::Two),
        ("1AR", ArgumentSide::One),
        ("2NR", ArgumentSide::Two),
        ("2AR", ArgumentSide::One),
      ],
    );
    let negative = sheet_type(
      "Negative",
      &[
        ("1NC", ArgumentSide::Two),
        ("2AC", ArgumentSide::One),
        ("Block", ArgumentSide::Two),
        ("1AR", ArgumentSide::One),
        ("2NR", ArgumentSide::Two),
        ("2AR", ArgumentSide::One),
      ],
    );
    Self {
      id: Uuid::new_v4(),
      name: "Policy Debate".into(),
      sheet_types: vec![affirmative, negative],
    }
  }

  pub fn sheet_type(&self, id: SheetTypeId) -> Option<&SheetTypeDefinition> {
    self.sheet_types.iter().find(|definition| definition.id == id)
  }
}

fn sheet_type(name: &str, columns: &[(&str, ArgumentSide)]) -> SheetTypeDefinition {
  SheetTypeDefinition {
    id: Uuid::new_v4(),
    name: name.into(),
    columns: columns
      .iter()
      .map(|(label, side)| ColumnDefinition {
        id: Uuid::new_v4(),
        label: (*label).into(),
        side: *side,
      })
      .collect(),
  }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cell {
  pub id: CellId,
  pub column_id: ColumnId,
  pub parent_id: Option<CellId>,
  pub document_bytes: Vec<u8>,
}

impl Cell {
  pub fn plain(column_id: ColumnId) -> anyhow::Result<Self> {
    let document = flowstate_document::document_from_input(
      flowstate_document::flowstate_document_theme(),
      vec![flowstate_document::InputParagraph {
        style: flowstate_document::PARAGRAPH_TAG,
        runs: vec![flowstate_document::InputRun {
          text: String::new(),
          styles: flowstate_document::RunStyles::default(),
        }],
      }],
    );
    Ok(Self {
      id: Uuid::new_v4(),
      column_id,
      parent_id: None,
      document_bytes: flowstate_document::db8_bytes(&document)?,
    })
  }

  pub fn document(&self) -> std::io::Result<flowstate_document::Document> {
    flowstate_document::read_db8_bytes(&self.document_bytes)
  }

  pub fn is_empty(&self) -> std::io::Result<bool> {
    let document = self.document()?;
    Ok(document.text.to_string().trim().is_empty() && document.blocks.len() == document.paragraphs.len())
  }

  pub fn summary_text(&self) -> std::io::Result<String> {
    let document = self.document()?;
    let mut projection = Vec::new();
    for (index, paragraph) in document.paragraphs.iter().enumerate() {
      if matches!(
        paragraph.style,
        flowstate_document::PARAGRAPH_TAG
          | flowstate_document::PARAGRAPH_UNDERTAG
          | flowstate_document::PARAGRAPH_ANALYTIC
      ) {
        projection.push(paragraph_text(&document, index));
        continue;
      }

      let text = paragraph_text(&document, index);
      let mut cite_text = String::new();
      let mut offset = 0;
      for run in &paragraph.runs {
        let end = offset + run.len;
        if run.styles.semantic == flowstate_document::SEMANTIC_CITE {
          cite_text.push_str(&text[offset..end]);
        }
        offset = end;
      }
      if !cite_text.is_empty() {
        projection.push(cite_text);
      }
    }
    if !projection.is_empty() {
      return Ok(projection.join("\n"));
    }
    Ok(document.text.to_string())
  }

  pub fn uses_summary_projection(&self) -> std::io::Result<bool> {
    let document = self.document()?;
    Ok(document.paragraphs.iter().any(|paragraph| {
      matches!(
        paragraph.style,
        flowstate_document::PARAGRAPH_TAG
          | flowstate_document::PARAGRAPH_UNDERTAG
          | flowstate_document::PARAGRAPH_ANALYTIC
      ) || paragraph
        .runs
        .iter()
        .any(|run| run.styles.semantic == flowstate_document::SEMANTIC_CITE)
    }))
  }
}

fn paragraph_text(document: &flowstate_document::Document, index: usize) -> String {
  document
    .text
    .byte_slice(flowstate_document::paragraph_byte_range(document, index))
    .to_string()
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BoardPoint {
  pub x: f32,
  pub y: f32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Serialize, Deserialize)]
pub struct BoardRect {
  pub min: BoardPoint,
  pub max: BoardPoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
pub struct StrokeStyle {
  pub color_rgba: u32,
  pub width: f32,
  pub opacity: f32,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct AnnotationStroke {
  pub id: StrokeId,
  pub sheet_id: SheetId,
  pub originator: AnnotationOriginator,
  pub points: Vec<BoardPoint>,
  pub style: StrokeStyle,
  pub bbox: BoardRect,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct Sheet {
  pub id: SheetId,
  pub name: String,
  pub sheet_type_id: SheetTypeId,
  pub cells: Vec<Cell>,
  pub annotations: Vec<AnnotationStroke>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct SheetRecord {
  id: SheetId,
  name: String,
  sheet_type_id: SheetTypeId,
  order: u64,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
struct CellRecord {
  sheet_id: SheetId,
  cell: Cell,
  order: u64,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct FlowProjection {
  pub format: FlowFormat,
  pub sheets: Vec<Sheet>,
}

impl Default for FlowProjection {
  fn default() -> Self {
    Self {
      format: FlowFormat::policy_debate(),
      sheets: Vec::new(),
    }
  }
}

impl FlowProjection {
  pub fn validate(&self) -> anyhow::Result<()> {
    let mut ids = HashSet::new();
    let mut column_levels = HashMap::new();
    if !ids.insert(self.format.id) {
      bail!("duplicate format id");
    }
    for sheet_type in &self.format.sheet_types {
      if !ids.insert(sheet_type.id) || sheet_type.columns.is_empty() {
        bail!("invalid sheet type {}", sheet_type.name);
      }
      for (level, column) in sheet_type.columns.iter().enumerate() {
        if !ids.insert(column.id) {
          bail!("duplicate column id");
        }
        column_levels.insert(column.id, level);
      }
    }
    for sheet in &self.sheets {
      let definition = self
        .format
        .sheet_type(sheet.sheet_type_id)
        .with_context(|| format!("sheet {} references unknown type", sheet.name))?;
      if !ids.insert(sheet.id) {
        bail!("duplicate sheet id");
      }
      let valid_columns: HashSet<_> = definition.columns.iter().map(|column| column.id).collect();
      let cells: HashMap<_, _> = sheet.cells.iter().map(|cell| (cell.id, cell)).collect();
      if cells.len() != sheet.cells.len() {
        bail!("sheet {} contains duplicate cell ids", sheet.name);
      }
      for cell in &sheet.cells {
        if !valid_columns.contains(&cell.column_id) {
          bail!("cell references a column outside its sheet type");
        }
        let _ = cell.document().context("cell contains invalid rich-text document")?;
        if let Some(parent_id) = cell.parent_id {
          let parent = cells.get(&parent_id).context("cell references missing parent")?;
          let child_level = column_levels[&cell.column_id];
          let parent_level = column_levels[&parent.column_id];
          if child_level != parent_level + 1 {
            bail!("parent-child link must connect adjacent columns");
          }
        }
      }
      for column in &definition.columns {
        let column_cells: Vec<_> = sheet.cells.iter().filter(|cell| cell.column_id == column.id).collect();
        let mut completed_parents = HashSet::new();
        let mut current_parent = None;
        for cell in column_cells {
          if cell.parent_id != current_parent {
            if let Some(parent) = current_parent {
              completed_parents.insert(parent);
            }
            if cell.parent_id.is_some_and(|parent| completed_parents.contains(&parent)) {
              bail!("orphan or unrelated cell breaks a sibling run");
            }
            current_parent = cell.parent_id;
          }
        }
      }
      if sheet.annotations.iter().any(|stroke| stroke.sheet_id != sheet.id) {
        bail!("annotation references the wrong sheet");
      }
    }
    Ok(())
  }
}

pub struct FlowDocument {
  loro: LoroDoc,
  projection: FlowProjection,
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
    Self::from_projection(FlowProjection::default()).expect("default flow projection is valid")
  }

  pub fn projection(&self) -> &FlowProjection {
    &self.projection
  }

  pub fn from_projection(projection: FlowProjection) -> anyhow::Result<Self> {
    projection.validate()?;
    let loro = LoroDoc::new();
    loro.get_map("flow").insert(FORMAT_KEY, postcard::to_allocvec(&projection.format)?)?;
    let annotations = loro.get_map(ANNOTATIONS_MAP);
    for stroke in projection.sheets.iter().flat_map(|sheet| &sheet.annotations) {
      annotations.insert(&stroke.id.to_string(), postcard::to_allocvec(stroke)?)?;
    }
    write_all_entity_records(&loro, &projection)?;
    loro.commit();
    let undo_manager = UndoManager::new(&loro);
    Ok(Self {
      loro,
      projection,
      undo_manager,
    })
  }

  pub fn from_snapshot(snapshot: &[u8]) -> anyhow::Result<Self> {
    let loro = LoroDoc::new();
    loro.import(snapshot)?;
    let projection = load_projection(&loro)?;
    let undo_manager = UndoManager::new(&loro);
    Ok(Self {
      loro,
      projection,
      undo_manager,
    })
  }

  pub fn snapshot(&self) -> anyhow::Result<Vec<u8>> {
    Ok(self.loro.export(ExportMode::Snapshot)?)
  }

  pub fn projection_snapshot(&self) -> FlowProjectionSnapshot {
    FlowProjectionSnapshot {
      projection: self.projection.clone(),
      frontier: self.frontier(),
      version_vector: self.version_vector(),
    }
  }

  pub fn frontier(&self) -> FlowFrontier {
    self.loro.state_frontiers().encode()
  }

  pub fn export_updates_for(&self, version: &VersionVector) -> anyhow::Result<FlowUpdateBytes> {
    Ok(self.loro.export(ExportMode::updates(version))?)
  }

  pub fn updates_since(&self, version: &VersionVector) -> anyhow::Result<FlowUpdateBytes> {
    self.export_updates_for(version)
  }

  pub fn version_vector(&self) -> VersionVector {
    self.loro.oplog_vv()
  }

  pub fn import_remote_update(&mut self, bytes: &[u8]) -> anyhow::Result<Vec<FlowRuntimeEvent>> {
    self.loro.import(bytes)?;
    self.reload_projection()?;
    Ok(vec![
      FlowRuntimeEvent::RemoteUpdateApplied {
        bytes_len: bytes.len(),
        frontier: self.frontier(),
        version_vector: self.version_vector(),
      },
      FlowRuntimeEvent::ProjectionUpdated {
        snapshot: Box::new(self.projection_snapshot()),
      },
    ])
  }

  pub fn import_updates(&mut self, bytes: &[u8]) -> anyhow::Result<()> {
    self.import_remote_update(bytes).map(|_| ())
  }

  pub fn apply_projection_transaction(
    &mut self,
    transaction_id: FlowTransactionId,
    base_frontier: &[u8],
    change: impl FnOnce(&mut FlowProjection) -> anyhow::Result<()>,
  ) -> anyhow::Result<FlowCommitResult> {
    let actual_frontier = self.frontier();
    if !base_frontier.is_empty() && base_frontier != actual_frontier.as_slice() {
      return Err(StaleFlowProjectionError {
        expected: base_frontier.to_vec(),
        actual: actual_frontier,
      }
      .into());
    }

    let base_frontier = actual_frontier;
    let before_vv = self.version_vector();
    self.update(change)?;
    let update = self.export_updates_for(&before_vv)?;
    let new_frontier = self.frontier();
    let version_vector = self.version_vector();
    Ok(FlowCommitResult {
      transaction_id,
      base_frontier,
      new_frontier: new_frontier.clone(),
      events: vec![
        FlowRuntimeEvent::LocalUpdate {
          bytes: update,
          frontier: new_frontier,
          version_vector: version_vector.clone(),
        },
        FlowRuntimeEvent::ProjectionUpdated {
          snapshot: Box::new(FlowProjectionSnapshot {
            projection: self.projection.clone(),
            frontier: self.frontier(),
            version_vector,
          }),
        },
      ],
    })
  }

  pub fn update(&mut self, change: impl FnOnce(&mut FlowProjection) -> anyhow::Result<()>) -> anyhow::Result<()> {
    // Mutate a draft clone so a failing `change` or a rejected validation leaves `self` untouched.
    let mut draft = self.projection.clone();
    change(&mut draft)?;
    draft.validate()?;
    if draft.format != read_immutable_format(&self.loro)? {
      bail!("flow format definitions are immutable");
    }
    // Serialize the whole delta up front. Only once every fallible step has succeeded do we touch the
    // Loro doc, so a rejected update can never leave half-written, uncommitted state behind.
    let delta = compute_entity_delta(&self.projection, &draft)?;
    apply_entity_delta(&self.loro, delta)?;
    self.loro.commit();
    self.projection = draft;
    Ok(())
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
      self.reload_projection()?;
    }
    Ok(changed)
  }

  pub fn redo(&mut self) -> anyhow::Result<bool> {
    let changed = self.undo_manager.redo()?;
    if changed {
      self.reload_projection()?;
    }
    Ok(changed)
  }

  fn reload_projection(&mut self) -> anyhow::Result<()> {
    self.projection = load_projection(&self.loro)?;
    Ok(())
  }
}

/// Rebuild the projection from the immutable format plus the granular per-entity records. These
/// records are the source of truth for sheets, cells, and annotations — there is no whole-projection
/// blob to read back.
fn load_projection(loro: &LoroDoc) -> anyhow::Result<FlowProjection> {
  let format = read_immutable_format(loro)?;
  let mut projection = FlowProjection {
    format,
    sheets: Vec::new(),
  };
  merge_entity_records_from_loro(&mut projection, loro)?;
  projection.validate()?;
  Ok(projection)
}

fn read_immutable_format(loro: &LoroDoc) -> anyhow::Result<FlowFormat> {
  let value = loro.get_map("flow").get(FORMAT_KEY).context("Loro snapshot is missing immutable format definition")?;
  let ValueOrContainer::Value(LoroValue::Binary(bytes)) = value else {
    bail!("Loro immutable format definition has invalid type");
  };
  Ok(postcard::from_bytes(&bytes)?)
}

/// A single pending change to a Loro entity map: `Some(bytes)` inserts/updates a key, `None` deletes it.
type EntityOp = (&'static str, String, Option<Vec<u8>>);

/// Serialize the full sheet/cell/annotation delta between two projections *without* touching the Loro
/// doc, so serialization failures can be surfaced before any state is mutated (see [`FlowDocument::update`]).
fn compute_entity_delta(before: &FlowProjection, after: &FlowProjection) -> anyhow::Result<Vec<EntityOp>> {
  let mut ops = Vec::new();
  push_record_ops(&mut ops, SHEETS_MAP, &sheet_records(before), &sheet_records(after))?;
  push_record_ops(&mut ops, CELLS_MAP, &cell_records(before), &cell_records(after))?;
  push_annotation_ops(&mut ops, before, after)?;
  Ok(ops)
}

fn push_record_ops<T: Serialize + PartialEq>(
  ops: &mut Vec<EntityOp>,
  map_name: &'static str,
  before: &HashMap<Uuid, T>,
  after: &HashMap<Uuid, T>,
) -> anyhow::Result<()> {
  for id in before.keys().filter(|id| !after.contains_key(id)) {
    ops.push((map_name, id.to_string(), None));
  }
  for (id, record) in after {
    if before.get(id) != Some(record) {
      ops.push((map_name, id.to_string(), Some(postcard::to_allocvec(record)?)));
    }
  }
  Ok(())
}

fn push_annotation_ops(ops: &mut Vec<EntityOp>, before: &FlowProjection, after: &FlowProjection) -> anyhow::Result<()> {
  let before: HashMap<_, _> = before
    .sheets
    .iter()
    .flat_map(|sheet| sheet.annotations.iter().map(|stroke| (stroke.id, stroke)))
    .collect();
  let after: HashMap<_, _> = after
    .sheets
    .iter()
    .flat_map(|sheet| sheet.annotations.iter().map(|stroke| (stroke.id, stroke)))
    .collect();
  for id in before.keys().filter(|id| !after.contains_key(id)) {
    ops.push((ANNOTATIONS_MAP, id.to_string(), None));
  }
  for (id, stroke) in &after {
    if before.get(id).is_none_or(|existing| *existing != *stroke) {
      ops.push((ANNOTATIONS_MAP, id.to_string(), Some(postcard::to_allocvec(*stroke)?)));
    }
  }
  Ok(())
}

/// Apply a pre-serialized delta to the Loro doc. This is the only step in [`FlowDocument::update`] that
/// mutates Loro state; it runs after every fallible step has already succeeded.
fn apply_entity_delta(loro: &LoroDoc, ops: Vec<EntityOp>) -> anyhow::Result<()> {
  for (map_name, key, value) in ops {
    let map = loro.get_map(map_name);
    match value {
      Some(bytes) => map.insert(key.as_str(), bytes)?,
      None => map.delete(key.as_str())?,
    }
  }
  Ok(())
}

fn write_all_entity_records(loro: &LoroDoc, projection: &FlowProjection) -> anyhow::Result<()> {
  for (id, record) in sheet_records(projection) {
    loro.get_map(SHEETS_MAP).insert(&id.to_string(), postcard::to_allocvec(&record)?)?;
  }
  for (id, record) in cell_records(projection) {
    loro.get_map(CELLS_MAP).insert(&id.to_string(), postcard::to_allocvec(&record)?)?;
  }
  Ok(())
}

fn sheet_records(projection: &FlowProjection) -> HashMap<Uuid, SheetRecord> {
  projection
    .sheets
    .iter()
    .enumerate()
    .map(|(order, sheet)| {
      (
        sheet.id,
        SheetRecord {
          id: sheet.id,
          name: sheet.name.clone(),
          sheet_type_id: sheet.sheet_type_id,
          order: order as u64,
        },
      )
    })
    .collect()
}

fn cell_records(projection: &FlowProjection) -> HashMap<Uuid, CellRecord> {
  projection
    .sheets
    .iter()
    .flat_map(|sheet| {
      sheet.cells.iter().enumerate().map(|(order, cell)| {
        (
          cell.id,
          CellRecord {
            sheet_id: sheet.id,
            cell: cell.clone(),
            order: order as u64,
          },
        )
      })
    })
    .collect()
}

fn merge_entity_records_from_loro(projection: &mut FlowProjection, loro: &LoroDoc) -> anyhow::Result<()> {
  let mut sheets: Vec<SheetRecord> = read_record_map(loro, SHEETS_MAP)?;
  sheets.sort_by_key(|record| (record.order, record.id));
  let existing_annotations: HashMap<_, _> = projection
    .sheets
    .iter_mut()
    .map(|sheet| (sheet.id, std::mem::take(&mut sheet.annotations)))
    .collect();
  projection.sheets = sheets
    .into_iter()
    .map(|record| Sheet {
      id: record.id,
      name: record.name,
      sheet_type_id: record.sheet_type_id,
      cells: Vec::new(),
      annotations: existing_annotations.get(&record.id).cloned().unwrap_or_default(),
    })
    .collect();
  let mut cells: Vec<CellRecord> = read_record_map(loro, CELLS_MAP)?;
  cells.sort_by_key(|record| (record.sheet_id, record.order, record.cell.id));
  for record in cells {
    if let Some(sheet) = projection.sheets.iter_mut().find(|sheet| sheet.id == record.sheet_id) {
      sheet.cells.push(record.cell);
    }
  }
  merge_annotations_from_loro(projection, loro)
}

fn read_record_map<T: for<'de> Deserialize<'de>>(loro: &LoroDoc, map_name: &str) -> anyhow::Result<Vec<T>> {
  let mut records = Vec::new();
  let mut error = None;
  loro.get_map(map_name).for_each(|key, value| {
    if error.is_some() {
      return;
    }
    let ValueOrContainer::Value(LoroValue::Binary(bytes)) = value else {
      error = Some(anyhow::anyhow!("{map_name} record {key} has invalid Loro value type"));
      return;
    };
    match postcard::from_bytes(&bytes) {
      Ok(record) => records.push(record),
      Err(source) => error = Some(source.into()),
    }
  });
  error.map_or(Ok(records), Err)
}

fn merge_annotations_from_loro(projection: &mut FlowProjection, loro: &LoroDoc) -> anyhow::Result<()> {
  for sheet in &mut projection.sheets {
    sheet.annotations.clear();
  }
  let mut error = None;
  loro.get_map(ANNOTATIONS_MAP).for_each(|key, value| {
    if error.is_some() {
      return;
    }
    let ValueOrContainer::Value(LoroValue::Binary(bytes)) = value else {
      error = Some(anyhow::anyhow!("annotation {key} has invalid Loro value type"));
      return;
    };
    let stroke: AnnotationStroke = match postcard::from_bytes(&bytes) {
      Ok(stroke) => stroke,
      Err(source) => {
        error = Some(source.into());
        return;
      },
    };
    if key != stroke.id.to_string() {
      error = Some(anyhow::anyhow!("annotation key does not match stable stroke id"));
      return;
    }
    if let Some(sheet) = projection.sheets.iter_mut().find(|sheet| sheet.id == stroke.sheet_id) {
      sheet.annotations.push(stroke);
    }
  });
  error.map_or(Ok(()), Err)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn cell_with_paragraphs(paragraphs: Vec<flowstate_document::InputParagraph>) -> Cell {
    let document = flowstate_document::document_from_input(flowstate_document::flowstate_document_theme(), paragraphs);
    Cell {
      id: Uuid::new_v4(),
      column_id: Uuid::new_v4(),
      parent_id: None,
      document_bytes: flowstate_document::db8_bytes(&document).unwrap(),
    }
  }

  fn run(text: &str) -> flowstate_document::InputRun {
    flowstate_document::InputRun {
      text: text.into(),
      styles: flowstate_document::RunStyles::default(),
    }
  }

  fn cite(text: &str) -> flowstate_document::InputRun {
    let mut run = run(text);
    run.styles.semantic = flowstate_document::SEMANTIC_CITE;
    run
  }

  #[test]
  fn summary_projects_mixed_card_and_analytic_content_in_document_order() {
    let cell = cell_with_paragraphs(vec![
      flowstate_document::InputParagraph {
        style: flowstate_document::PARAGRAPH_TAG,
        runs: vec![run("Tag")],
      },
      flowstate_document::InputParagraph {
        style: flowstate_document::ParagraphStyle::Normal,
        runs: vec![run("hidden before "), cite("Cite"), run(" hidden after")],
      },
      flowstate_document::InputParagraph {
        style: flowstate_document::PARAGRAPH_UNDERTAG,
        runs: vec![run("Undertag")],
      },
      flowstate_document::InputParagraph {
        style: flowstate_document::PARAGRAPH_ANALYTIC,
        runs: vec![run("Analytic")],
      },
    ]);

    assert_eq!(cell.summary_text().unwrap(), "Tag\nCite\nUndertag\nAnalytic");
    assert!(cell.uses_summary_projection().unwrap());
  }

  #[test]
  fn round_trips_loro_snapshot() {
    let mut document = FlowDocument::new();
    let sheet_type = document.projection().format.sheet_types[0].id;
    document
      .update(|projection| {
        projection.sheets.push(Sheet {
          id: Uuid::new_v4(),
          name: "Case".into(),
          sheet_type_id: sheet_type,
          cells: Vec::new(),
          annotations: Vec::new(),
        });
        Ok(())
      })
      .unwrap();
    let restored = FlowDocument::from_snapshot(&document.snapshot().unwrap()).unwrap();
    assert_eq!(document.projection(), restored.projection());
  }

  #[test]
  fn rejects_format_mutation() {
    let mut document = FlowDocument::new();
    assert!(
      document
        .update(|projection| {
          projection.format.name = "Changed".into();
          Ok(())
        })
        .is_err()
    );
  }

  #[test]
  fn collaboration_transaction_returns_update_and_projection_events() {
    let mut document = FlowDocument::new();
    let base_frontier = document.frontier();
    let sheet_type = document.projection().format.sheet_types[0].id;
    let sheet_id = Uuid::new_v4();

    let commit = document
      .apply_projection_transaction(7, &base_frontier, |projection| {
        projection.sheets.push(Sheet {
          id: sheet_id,
          name: "Shared".into(),
          sheet_type_id: sheet_type,
          cells: Vec::new(),
          annotations: Vec::new(),
        });
        Ok(())
      })
      .unwrap();

    assert_eq!(commit.transaction_id, 7);
    assert_eq!(commit.base_frontier, base_frontier);
    assert_eq!(commit.new_frontier, document.frontier());
    assert_eq!(commit.events.len(), 2);
    assert!(matches!(commit.events[0], FlowRuntimeEvent::LocalUpdate { .. }));
    assert!(matches!(commit.events[1], FlowRuntimeEvent::ProjectionUpdated { .. }));
  }

  #[test]
  fn collaboration_transaction_rejects_stale_frontier() {
    let mut document = FlowDocument::new();
    let sheet_type = document.projection().format.sheet_types[0].id;
    document.create_sheet("Case", sheet_type).unwrap();
    let stale_frontier = document.frontier();
    document
      .update(|projection| {
        projection.sheets[0].name = "Changed".into();
        Ok(())
      })
      .unwrap();

    let error = document
      .apply_projection_transaction(8, &stale_frontier, |projection| {
        projection.sheets[0].name = "Rejected".into();
        Ok(())
      })
      .unwrap_err();

    assert!(error.downcast_ref::<StaleFlowProjectionError>().is_some());
    assert_eq!(document.projection().sheets[0].name, "Changed");
  }

  #[test]
  fn collaboration_remote_update_reloads_projection() {
    let mut source = FlowDocument::new();
    let sheet_type = source.projection().format.sheet_types[0].id;
    source.create_sheet("Case", sheet_type).unwrap();
    let mut target = FlowDocument::from_snapshot(&source.snapshot().unwrap()).unwrap();
    let target_vv = target.version_vector();
    source
      .update(|projection| {
        projection.sheets[0].name = "Remote".into();
        Ok(())
      })
      .unwrap();
    let update = source.export_updates_for(&target_vv).unwrap();

    let events = target.import_remote_update(&update).unwrap();

    assert_eq!(target.projection().sheets[0].name, "Remote");
    assert_eq!(events.len(), 2);
    assert!(matches!(events[0], FlowRuntimeEvent::RemoteUpdateApplied { .. }));
    assert!(matches!(events[1], FlowRuntimeEvent::ProjectionUpdated { .. }));
  }
}
