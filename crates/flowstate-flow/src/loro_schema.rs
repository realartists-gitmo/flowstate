//! The .fl0 v2 Loro container layout (flow architecture spec Part 2.1) and
//! its write-side ensure/seed helpers. Structure:
//!
//! ```text
//! "flow.meta"        Map    format (postcard, immutable) · schema_version=2 ·
//!                           document_id · created_at / modified_at
//! "flow.sheet_order" MovableList<String>   board order of sheet uuids
//! "flow.sheets_by_id" Map → {sheet}: id · name (LWW) · sheet_type_id ·
//!                           "cell_order" MovableList<String> (flat column order)
//! "flow.cells_by_id"  Map → {cell}: id · sheet_id · column_id (LWW) ·
//!                           parent_id (LWW, absent = root) ·
//!                           "flow" Map (exact .db8 flow shape + per-cell
//!                                      "paragraphs_by_id" registry)
//! "flow.annotations"  Map    stroke uuid → postcard blob (write-once/delete)
//! ```
//!
//! Ordering is `MovableList` so a reorder writes ONLY the order list — it can
//! never clobber a concurrent text edit. Parentage is a per-cell LWW register;
//! merged states that violate flow invariants are normalized (never rejected)
//! by [`crate::loro_projection`].

use loro::{ContainerTrait as _, LoroDoc, LoroMap, LoroMovableList, LoroResult, LoroValue, ValueOrContainer};
use uuid::Uuid;

use crate::format::{CellId, ColumnId, FlowFormat, SheetId, SheetTypeId};

pub const META_MAP: &str = "flow.meta";
pub const SHEET_ORDER: &str = "flow.sheet_order";
pub const SHEETS_BY_ID: &str = "flow.sheets_by_id";
pub const CELLS_BY_ID: &str = "flow.cells_by_id";
pub const ANNOTATIONS_MAP: &str = "flow.annotations";
/// C-S2: flow comment threads (the .db8 `comments_by_id` shape, cell-anchored).
/// `flow.annotations` was taken — it holds ink strokes.
pub const COMMENTS_BY_ID: &str = "flow.comments_by_id";
pub const CELL_ORDER_KEY: &str = "cell_order";
pub const CELL_FLOW_KEY: &str = "flow";
pub const CELL_PARAGRAPHS_KEY: &str = "paragraphs_by_id";
pub const SCHEMA_VERSION: i64 = 2;

pub const META_FORMAT_KEY: &str = "format";
pub const META_SCHEMA_VERSION_KEY: &str = "schema_version";
pub const META_DOCUMENT_ID_KEY: &str = "document_id";

/// One-time creation of a fresh .fl0 v2 document. The format is immutable by
/// law: written exactly once here, never rewritten by any executor.
pub fn init_flow_document(doc: &LoroDoc, format: &FlowFormat, document_id: Uuid) -> anyhow::Result<()> {
  flowstate_document::configure_text_styles(doc);
  let meta = doc.get_map(META_MAP);
  meta.insert(META_FORMAT_KEY, postcard::to_allocvec(format)?)?;
  meta.insert(META_SCHEMA_VERSION_KEY, SCHEMA_VERSION)?;
  meta.insert(META_DOCUMENT_ID_KEY, document_id.to_string())?;
  let now = unix_time_secs();
  meta.insert("created_at", now)?;
  meta.insert("modified_at", now)?;
  doc.commit();
  Ok(())
}

/// Style configuration is per-handle: every constructor (new, `from_snapshot`,
/// fork) must call this before touching text, mirroring the .db8 runtime.
pub fn configure_flow_doc(doc: &LoroDoc) {
  flowstate_document::configure_text_styles(doc);
}

pub fn read_format(doc: &LoroDoc) -> anyhow::Result<FlowFormat> {
  let meta = doc.get_map(META_MAP);
  let Some(ValueOrContainer::Value(LoroValue::Binary(bytes))) = meta.get(META_FORMAT_KEY) else {
    anyhow::bail!("Loro snapshot is missing immutable format definition");
  };
  Ok(postcard::from_bytes(&bytes)?)
}

pub fn schema_version(doc: &LoroDoc) -> Option<i64> {
  match doc.get_map(META_MAP).get(META_SCHEMA_VERSION_KEY) {
    Some(ValueOrContainer::Value(LoroValue::I64(version))) => Some(version),
    _ => None,
  }
}

/// The stable document identity (discovery fingerprints, standing access).
pub fn document_id(doc: &LoroDoc) -> Option<Uuid> {
  match doc.get_map(META_MAP).get(META_DOCUMENT_ID_KEY) {
    Some(ValueOrContainer::Value(LoroValue::String(id))) => Uuid::parse_str(&id).ok(),
    _ => None,
  }
}

pub fn touch_modified(doc: &LoroDoc) -> LoroResult<()> {
  doc
    .get_map(META_MAP)
    .insert("modified_at", unix_time_secs())?;
  Ok(())
}

pub fn sheet_order(doc: &LoroDoc) -> LoroMovableList {
  doc.get_movable_list(SHEET_ORDER)
}

pub fn sheets_map(doc: &LoroDoc) -> LoroMap {
  doc.get_map(SHEETS_BY_ID)
}

pub fn cells_map(doc: &LoroDoc) -> LoroMap {
  doc.get_map(CELLS_BY_ID)
}

pub fn annotations_map(doc: &LoroDoc) -> LoroMap {
  doc.get_map(ANNOTATIONS_MAP)
}

pub fn ensure_sheet_record(doc: &LoroDoc, sheet_id: SheetId, name: &str, sheet_type_id: SheetTypeId) -> LoroResult<LoroMap> {
  let sheet = sheets_map(doc).ensure_mergeable_map(&sheet_id.to_string())?;
  sheet.insert("id", sheet_id.to_string())?;
  sheet.insert("name", name)?;
  sheet.insert("sheet_type_id", sheet_type_id.to_string())?;
  sheet.ensure_mergeable_movable_list(CELL_ORDER_KEY)?;
  Ok(sheet)
}

pub fn sheet_record(doc: &LoroDoc, sheet_id: SheetId) -> Option<LoroMap> {
  child_map(&sheets_map(doc), &sheet_id.to_string())
}

pub fn sheet_cell_order(sheet: &LoroMap) -> LoroResult<LoroMovableList> {
  sheet.ensure_mergeable_movable_list(CELL_ORDER_KEY)
}

/// The durable flow id embedded in a cell's flow map — namespaced so cell
/// flows can never collide with .db8 flow ids if a doc is ever inspected by
/// shared tooling.
pub fn cell_flow_id(cell_id: CellId) -> String {
  format!("cell.{}.flow", cell_id.as_simple())
}

/// Create (or fetch) a cell's record. The cell's rich text arrives via
/// [`seed_cell_flow`] / [`crate::loro_import::write_cell_paragraphs`] —
/// creation and content are separate ops inside ONE intent commit.
pub fn ensure_cell_record(
  doc: &LoroDoc,
  cell_id: CellId,
  sheet_id: SheetId,
  column_id: ColumnId,
  parent: Option<CellId>,
) -> LoroResult<LoroMap> {
  let cell = cells_map(doc).ensure_mergeable_map(&cell_id.to_string())?;
  cell.insert("id", cell_id.to_string())?;
  cell.insert("sheet_id", sheet_id.to_string())?;
  cell.insert("column_id", column_id.to_string())?;
  match parent {
    Some(parent) => cell.insert("parent_id", parent.to_string())?,
    None => {
      if cell.get("parent_id").is_some() {
        cell.delete("parent_id")?;
      }
    },
  }
  let flow = cell.ensure_mergeable_map(CELL_FLOW_KEY)?;
  let flow_id = cell_flow_id(cell_id);
  flow.insert(flowstate_document::FLOW_ID_KEY, flow_id.as_str())?;
  flow.insert(flowstate_document::FLOW_KIND_KEY, "flow-cell")?;
  let text = flow.ensure_mergeable_text(flowstate_document::FLOW_TEXT_KEY)?;
  flow.ensure_mergeable_map(flowstate_document::FLOW_ATTRS_KEY)?;
  flow.insert("text_container_id", text.id().to_string())?;
  flow.ensure_mergeable_map(CELL_PARAGRAPHS_KEY)?;
  Ok(cell)
}

pub fn cell_record(doc: &LoroDoc, cell_id: CellId) -> Option<LoroMap> {
  child_map(&cells_map(doc), &cell_id.to_string())
}

pub fn cell_flow(cell: &LoroMap) -> Option<LoroMap> {
  child_map(cell, CELL_FLOW_KEY)
}

pub fn cell_paragraph_registry(flow: &LoroMap) -> Option<LoroMap> {
  child_map(flow, CELL_PARAGRAPHS_KEY)
}

pub fn set_cell_parent(cell: &LoroMap, parent: Option<CellId>) -> LoroResult<()> {
  match parent {
    Some(parent) => cell.insert("parent_id", parent.to_string()),
    None => {
      if cell.get("parent_id").is_some() {
        cell.delete("parent_id")?;
      }
      Ok(())
    },
  }
}

pub fn set_cell_column(cell: &LoroMap, column: ColumnId) -> LoroResult<()> {
  cell.insert("column_id", column.to_string())
}

/// Seed a fresh cell's flow: sentinel + empty TAG paragraph + the initial
/// paragraph record (`paragraph.initial`, matching the body's boundary-0
/// preference law in the shared boundary indexer).
pub fn seed_cell_flow(doc: &LoroDoc, cell_id: CellId) -> anyhow::Result<()> {
  let cell = cell_record(doc, cell_id).ok_or_else(|| anyhow::anyhow!("unknown cell {cell_id}"))?;
  let flow = cell_flow(&cell).ok_or_else(|| anyhow::anyhow!("cell {cell_id} has no flow"))?;
  let registry = cell_paragraph_registry(&flow).ok_or_else(|| anyhow::anyhow!("cell {cell_id} has no registry"))?;
  let seed = flowstate_document::document_from_input(
    flowstate_document::DocumentTheme::clone(&flowstate_document::flowstate_document_theme()),
    vec![flowstate_document::InputParagraph {
      style: flowstate_document::PARAGRAPH_TAG,
      runs: vec![flowstate_document::InputRun {
        text: String::new(),
        styles: flowstate_document::RunStyles::default(),
      }],
    }],
  );
  flowstate_document::replace_single_flow_from_document(doc, &flow, &registry, &cell_flow_id(cell_id), &seed)?;
  Ok(())
}

pub fn map_keys(map: &LoroMap) -> Vec<String> {
  let mut keys: Vec<String> = Vec::with_capacity(map.len());
  map.for_each(|key, _| keys.push(key.to_string()));
  keys.sort();
  keys
}

pub fn child_map(parent: &LoroMap, key: &str) -> Option<LoroMap> {
  match parent.get(key) {
    Some(ValueOrContainer::Container(container)) => container.into_map().ok(),
    _ => None,
  }
}

pub fn map_string(map: &LoroMap, key: &str) -> Option<String> {
  match map.get(key) {
    Some(ValueOrContainer::Value(LoroValue::String(value))) => Some(value.to_string()),
    _ => None,
  }
}

pub fn map_uuid(map: &LoroMap, key: &str) -> Option<Uuid> {
  map_string(map, key).and_then(|value| Uuid::parse_str(&value).ok())
}

pub fn map_binary(map: &LoroMap, key: &str) -> Option<Vec<u8>> {
  match map.get(key) {
    Some(ValueOrContainer::Value(LoroValue::Binary(value))) => Some(value.to_vec()),
    _ => None,
  }
}

/// Enumerate a movable list's string entries in order.
pub fn list_strings(list: &LoroMovableList) -> Vec<String> {
  let mut out = Vec::with_capacity(list.len());
  for index in 0..list.len() {
    if let Some(ValueOrContainer::Value(LoroValue::String(value))) = list.get(index) {
      out.push(value.to_string());
    }
  }
  out
}

fn unix_time_secs() -> i64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map(|elapsed| elapsed.as_secs() as i64)
    .unwrap_or_default()
}
