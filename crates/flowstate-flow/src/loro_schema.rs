//! The .fl0 v2 Loro container schema (spec Part A).
//!
//! ```text
//! LoroDoc (.fl0 v2)
//! ├─ "flow.meta" (Map): format (postcard, immutable), schema_version = 2,
//! │                     document_id (uuid string), created_at/modified_at
//! ├─ "flow.sheet_order" (MovableList<String>)  — board order of sheet uuids
//! ├─ "flow.sheets_by_id" (Map) → {sheet_uuid} (Map):
//! │     id, name (LWW), sheet_type_id, "cell_order" (MovableList<String>)
//! ├─ "flow.cells_by_id" (Map) → {cell_uuid} (Map):
//! │     id, sheet_id, column_id (LWW), parent_id (LWW, absent = root),
//! │     "flow" (Map) — the EXACT .db8 flow shape (`ensure_flow`): sentinel
//! │        text + MARK_* styles + a per-cell "paragraphs_by_id" registry
//! └─ "flow.annotations" (Map): stroke uuid → postcard blob
//!    (write-once/delete-only — already proven convergent)
//! ```
//!
//! Ordering is MovableList (`mov` writes only the order list, never a cell map
//! or text container, so a reorder can never clobber a concurrent text edit);
//! the parent tree is per-cell LWW `parent_id`/`column_id`. Everything here
//! writes WITHOUT committing — the caller (the gated flow runtime, or a test)
//! owns the commit and its origin.

use flowstate_document::loro_schema::{
  FLOW_TEXT_KEY, MARK_PARAGRAPH_STYLE, PARAGRAPHS_BY_ID, ROOT_FIRST_PARAGRAPH_ID, configure_text_styles, ensure_flow,
};
use gpui_flowtext::{InputParagraph, ParagraphStyle, RunSemanticStyle};
use loro::{LoroDoc, LoroMap, LoroMovableList, LoroResult, LoroText, LoroValue, ValueOrContainer, cursor::Side};
use uuid::Uuid;

use crate::format::{AnnotationStroke, CellId, FlowFormat, SheetId, StrokeId};

pub const FLOW_META: &str = "flow.meta";
pub const FLOW_SHEET_ORDER: &str = "flow.sheet_order";
pub const FLOW_SHEETS_BY_ID: &str = "flow.sheets_by_id";
pub const FLOW_CELLS_BY_ID: &str = "flow.cells_by_id";
pub const FLOW_ANNOTATIONS: &str = "flow.annotations";

pub const META_FORMAT: &str = "format";
pub const META_SCHEMA_VERSION: &str = "schema_version";
pub const META_DOCUMENT_ID: &str = "document_id";

pub const SHEET_CELL_ORDER: &str = "cell_order";
pub const CELL_FLOW_KEY: &str = "flow";
pub const CELL_FLOW_KIND: &str = "flow-cell";

/// `.fl0` v2 canonical schema version stamped into `flow.meta`.
pub const FLOW_SCHEMA_VERSION: i64 = 2;

/// The editable tag paragraph style every fresh cell seeds with
/// (`flowstate_document::PARAGRAPH_TAG`, kept literal here to avoid a gpui dep
/// for one constant — asserted equal in tests).
pub const CELL_SEED_PARAGRAPH_STYLE: ParagraphStyle = ParagraphStyle::Custom(3);

/// Initialize a fresh v2 flow document: meta (immutable format blob, schema
/// version, document id, timestamps) + the root containers + text-style
/// configuration. Does not commit.
pub fn init_flow_document(doc: &LoroDoc, format: &FlowFormat) -> anyhow::Result<Uuid> {
  configure_text_styles(doc);
  let meta = doc.get_map(FLOW_META);
  meta.insert(META_FORMAT, postcard::to_allocvec(format)?)?;
  meta.insert(META_SCHEMA_VERSION, FLOW_SCHEMA_VERSION)?;
  let document_id = Uuid::new_v4();
  meta.insert(META_DOCUMENT_ID, document_id.to_string())?;
  let now = unix_time_secs();
  meta.insert("created_at", now)?;
  meta.insert("modified_at", now)?;
  meta.insert("created_by_app_version", env!("CARGO_PKG_VERSION"))?;
  Ok(document_id)
}

/// A snapshot import always re-runs this: style configs are per-`LoroDoc`
/// runtime state, not persisted.
pub fn configure_flow_text_styles(doc: &LoroDoc) {
  configure_text_styles(doc);
}

pub fn read_format(doc: &LoroDoc) -> anyhow::Result<FlowFormat> {
  let meta = doc.get_map(FLOW_META);
  let Some(ValueOrContainer::Value(LoroValue::Binary(bytes))) = meta.get(META_FORMAT) else {
    anyhow::bail!("flow document has no immutable format definition");
  };
  Ok(postcard::from_bytes(&bytes)?)
}

pub fn read_schema_version(doc: &LoroDoc) -> Option<i64> {
  match doc.get_map(FLOW_META).get(META_SCHEMA_VERSION)? {
    ValueOrContainer::Value(LoroValue::I64(version)) => Some(version),
    _ => None,
  }
}

pub fn read_document_id(doc: &LoroDoc) -> Option<Uuid> {
  match doc.get_map(FLOW_META).get(META_DOCUMENT_ID)? {
    ValueOrContainer::Value(LoroValue::String(value)) => Uuid::parse_str(&value).ok(),
    _ => None,
  }
}

pub fn touch_modified_at(doc: &LoroDoc) -> LoroResult<()> {
  doc
    .get_map(FLOW_META)
    .insert("modified_at", unix_time_secs())?;
  Ok(())
}

pub fn sheet_order(doc: &LoroDoc) -> LoroMovableList {
  doc.get_movable_list(FLOW_SHEET_ORDER)
}

pub fn sheets_by_id(doc: &LoroDoc) -> LoroMap {
  doc.get_map(FLOW_SHEETS_BY_ID)
}

pub fn cells_by_id(doc: &LoroDoc) -> LoroMap {
  doc.get_map(FLOW_CELLS_BY_ID)
}

pub fn annotations_map(doc: &LoroDoc) -> LoroMap {
  doc.get_map(FLOW_ANNOTATIONS)
}

pub fn sheet_map(doc: &LoroDoc, sheet_id: SheetId) -> Option<LoroMap> {
  child_map(&sheets_by_id(doc), &sheet_id.to_string())
}

pub fn cell_map(doc: &LoroDoc, cell_id: CellId) -> Option<LoroMap> {
  child_map(&cells_by_id(doc), &cell_id.to_string())
}

pub fn cell_flow_map(cell: &LoroMap) -> Option<LoroMap> {
  child_map(cell, CELL_FLOW_KEY)
}

pub fn cell_order_list(sheet: &LoroMap) -> LoroResult<LoroMovableList> {
  sheet.ensure_mergeable_movable_list(SHEET_CELL_ORDER)
}

/// Defect-label flow id for a cell's rich text (`materialize_single_flow`).
#[must_use]
pub fn cell_flow_label(cell_id: CellId) -> String {
  format!("cell.{cell_id}.flow")
}

// ---- Sheet records ---------------------------------------------------------

/// Create (or converge on) a sheet record and insert it into the board order
/// at `order_index` (clamped). Idempotent per field; two peers creating the
/// same sheet id converge by map LWW.
pub fn write_sheet(doc: &LoroDoc, sheet_id: SheetId, name: &str, sheet_type_id: Uuid, order_index: usize) -> LoroResult<LoroMap> {
  let sheet = sheets_by_id(doc).ensure_mergeable_map(&sheet_id.to_string())?;
  sheet.insert("id", sheet_id.to_string())?;
  sheet.insert("name", name)?;
  sheet.insert("sheet_type_id", sheet_type_id.to_string())?;
  cell_order_list(&sheet)?;
  let order = sheet_order(doc);
  if order_position(&order, &sheet_id.to_string()).is_none() {
    order.insert(order_index.min(order.len()), sheet_id.to_string())?;
  }
  Ok(sheet)
}

pub fn rename_sheet(doc: &LoroDoc, sheet_id: SheetId, name: &str) -> anyhow::Result<()> {
  let sheet = sheet_map(doc, sheet_id).ok_or_else(|| anyhow::anyhow!("unknown sheet"))?;
  sheet.insert("name", name)?;
  Ok(())
}

/// Remove a sheet: its order entry, its record, its cells (records + order are
/// both dropped with the sheet), and its annotations.
pub fn remove_sheet(doc: &LoroDoc, sheet_id: SheetId) -> anyhow::Result<()> {
  let order = sheet_order(doc);
  if let Some(position) = order_position(&order, &sheet_id.to_string()) {
    order.delete(position, 1)?;
  }
  let cells = cells_by_id(doc);
  let sheet_key = sheet_id.to_string();
  let mut doomed = Vec::new();
  cells.for_each(|key, value| {
    if let ValueOrContainer::Container(container) = value
      && let Ok(cell) = container.into_map()
      && map_string(&cell, "sheet_id").as_deref() == Some(sheet_key.as_str())
    {
      doomed.push(key.to_string());
    }
  });
  for key in doomed {
    cells.delete(&key)?;
  }
  sheets_by_id(doc).delete(&sheet_key)?;
  // Annotations of the sheet: delete-only map, same routing rule as the
  // materializer (skip-on-unknown-sheet would also hide them, but deleting
  // keeps the map from accumulating garbage).
  let annotations = annotations_map(doc);
  let mut doomed_strokes = Vec::new();
  annotations.for_each(|key, value| {
    if let ValueOrContainer::Value(LoroValue::Binary(bytes)) = value
      && let Ok(stroke) = postcard::from_bytes::<AnnotationStroke>(&bytes)
      && stroke.sheet_id == sheet_id
    {
      doomed_strokes.push(key.to_string());
    }
  });
  for key in doomed_strokes {
    annotations.delete(&key)?;
  }
  Ok(())
}

pub fn move_sheet(doc: &LoroDoc, sheet_id: SheetId, target_index: usize) -> anyhow::Result<()> {
  let order = sheet_order(doc);
  let from = order_position(&order, &sheet_id.to_string()).ok_or_else(|| anyhow::anyhow!("unknown sheet"))?;
  let to = target_index.min(order.len().saturating_sub(1));
  if from != to {
    order.mov(from, to)?;
  }
  Ok(())
}

// ---- Cell records ----------------------------------------------------------

/// Create a cell record (map fields + seeded flow) and insert its id into the
/// sheet's `cell_order` at `order_index` (clamped). The rich text is seeded
/// separately by the caller (`seed_cell_flow` / `cell_flow_from_paragraphs`).
pub fn write_cell(
  doc: &LoroDoc,
  sheet_id: SheetId,
  cell_id: CellId,
  column_id: Uuid,
  parent_id: Option<CellId>,
  order_index: usize,
) -> anyhow::Result<LoroMap> {
  let sheet = sheet_map(doc, sheet_id).ok_or_else(|| anyhow::anyhow!("unknown sheet"))?;
  let cell = cells_by_id(doc).ensure_mergeable_map(&cell_id.to_string())?;
  cell.insert("id", cell_id.to_string())?;
  cell.insert("sheet_id", sheet_id.to_string())?;
  cell.insert("column_id", column_id.to_string())?;
  match parent_id {
    Some(parent) => cell.insert("parent_id", parent.to_string())?,
    None => {
      if cell.get("parent_id").is_some() {
        cell.delete("parent_id")?;
      }
    },
  }
  let order = cell_order_list(&sheet)?;
  if order_position(&order, &cell_id.to_string()).is_none() {
    order.insert(order_index.min(order.len()), cell_id.to_string())?;
  }
  Ok(cell)
}

pub fn set_cell_column(cell: &LoroMap, column_id: Uuid) -> LoroResult<()> {
  cell.insert("column_id", column_id.to_string())?;
  Ok(())
}

pub fn set_cell_parent(cell: &LoroMap, parent_id: Option<CellId>) -> LoroResult<()> {
  match parent_id {
    Some(parent) => cell.insert("parent_id", parent.to_string())?,
    None => {
      if cell.get("parent_id").is_some() {
        cell.delete("parent_id")?;
      }
    },
  }
  Ok(())
}

/// Delete a cell record and its order entry (liveness = in map ∧ in order).
pub fn remove_cell(doc: &LoroDoc, sheet_id: SheetId, cell_id: CellId) -> anyhow::Result<()> {
  if let Some(sheet) = sheet_map(doc, sheet_id) {
    let order = cell_order_list(&sheet)?;
    if let Some(position) = order_position(&order, &cell_id.to_string()) {
      order.delete(position, 1)?;
    }
  }
  cells_by_id(doc).delete(&cell_id.to_string())?;
  Ok(())
}

/// `MovableListHandler::mov` over the sheet's cell order: `from`/`to` are
/// current-list indices (Loro's move semantics — the element lands at `to`
/// as observed AFTER the removal of `from`).
pub fn move_cell_order(sheet: &LoroMap, from: usize, to: usize) -> anyhow::Result<()> {
  let order = cell_order_list(sheet)?;
  if from != to {
    order.mov(from, to)?;
  }
  Ok(())
}

pub fn cell_order_ids(sheet: &LoroMap) -> Vec<String> {
  let Ok(order) = cell_order_list(sheet) else {
    return Vec::new();
  };
  movable_list_strings(&order)
}

pub fn sheet_order_ids(doc: &LoroDoc) -> Vec<String> {
  movable_list_strings(&sheet_order(doc))
}

// ---- Cell rich text --------------------------------------------------------

/// Ensure the cell's flow container exists in the EXACT .db8 flow shape
/// (sentinel text container + attrs + text_container_id + a per-cell
/// paragraph registry child).
pub fn ensure_cell_flow(cell: &LoroMap) -> LoroResult<LoroMap> {
  let flow = ensure_flow(cell, CELL_FLOW_KEY, CELL_FLOW_KIND)?;
  flow.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  Ok(flow)
}

/// Canonical fresh-cell seed, mirroring `seed_document_body`: the boundary-0
/// sentinel carrying the TAG paragraph-style mark plus the first paragraph
/// record (`ROOT_FIRST_PARAGRAPH_ID`) in the cell's own registry. Idempotent.
pub fn seed_cell_flow(cell: &LoroMap) -> LoroResult<LoroMap> {
  let flow = ensure_cell_flow(cell)?;
  let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  if text.len_unicode() == 0 || !text.to_string().starts_with('\n') {
    text.insert(0, "\n")?;
    text.mark(0..1, MARK_PARAGRAPH_STYLE, paragraph_style_value(CELL_SEED_PARAGRAPH_STYLE))?;
  }
  let paragraphs = flow.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  write_paragraph_record(&paragraphs, &text, ROOT_FIRST_PARAGRAPH_ID, 0)?;
  Ok(flow)
}

/// Replace the cell's whole rich text with `paragraphs` (paste / import /
/// `ReplaceCellContent`): clear text + registry, one contiguous insert, merged
/// paragraph-style + run marks, one paragraph record per row (mirrors the
/// .db8 import plan's write shape at cell scale).
pub fn cell_flow_from_paragraphs(cell: &LoroMap, paragraphs: &[InputParagraph]) -> LoroResult<LoroMap> {
  let flow = ensure_cell_flow(cell)?;
  let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  let registry = flow.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  let len = text.len_unicode();
  if len > 0 {
    text.delete(0, len)?;
  }
  registry.clear()?;

  if paragraphs.is_empty() {
    // Converge on the canonical empty seed rather than a sentinel-less flow.
    text.insert(0, "\n")?;
    text.mark(0..1, MARK_PARAGRAPH_STYLE, paragraph_style_value(CELL_SEED_PARAGRAPH_STYLE))?;
    write_paragraph_record(&registry, &text, ROOT_FIRST_PARAGRAPH_ID, 0)?;
    return Ok(flow);
  }

  // ONE contiguous insert for the whole flow text, then explicit mark ranges
  // (the .db8 import-plan lesson: per-run inserts fragment text ids).
  let mut full = String::new();
  let mut boundaries = Vec::with_capacity(paragraphs.len());
  for paragraph in paragraphs {
    boundaries.push(full.chars().count());
    full.push('\n');
    for run in &paragraph.runs {
      full.push_str(&run.text);
    }
  }
  text.insert(0, &full)?;

  for (ix, paragraph) in paragraphs.iter().enumerate() {
    let boundary = boundaries[ix];
    text.mark(
      boundary..boundary + 1,
      MARK_PARAGRAPH_STYLE,
      paragraph_style_value(paragraph.style),
    )?;
    let mut unicode_pos = boundary + 1;
    for run in &paragraph.runs {
      let run_len = run.text.chars().count();
      if run_len > 0 {
        mark_run_styles(&text, unicode_pos..unicode_pos + run_len, &run.styles)?;
      }
      unicode_pos += run_len;
    }
    let record_key = if ix == 0 {
      ROOT_FIRST_PARAGRAPH_ID.to_string()
    } else {
      format!("paragraph.{}", Uuid::new_v4().as_u128())
    };
    write_paragraph_record(&registry, &text, &record_key, boundary)?;
  }
  Ok(flow)
}

/// One durable paragraph record in a cell's registry: identity + boundary/start
/// cursors (start `Side::Left`, boundary `Side::Right` — the .db8 import shape).
pub fn write_paragraph_record(registry: &LoroMap, text: &LoroText, key: &str, boundary_pos: usize) -> LoroResult<LoroMap> {
  let record = registry.ensure_mergeable_map(key)?;
  record.insert("id", key)?;
  record.insert("flow_id", CELL_FLOW_KEY)?;
  if let Some(cursor) = text.get_cursor(boundary_pos, Side::Left) {
    record.insert("start_cursor", cursor.encode())?;
  }
  if let Some(cursor) = text.get_cursor(boundary_pos, Side::Right) {
    record.insert("boundary_cursor", cursor.encode())?;
  }
  record.ensure_mergeable_map("attrs")?;
  Ok(record)
}

/// Apply `styles` as explicit marks over `range` (unicode). The inverse of
/// `run_styles_from_attrs`; only non-default fields emit marks.
pub fn mark_run_styles(text: &LoroText, range: std::ops::Range<usize>, styles: &gpui_flowtext::RunStyles) -> LoroResult<()> {
  use flowstate_document::loro_schema::{MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE, MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH, MARK_VERT_ALIGN};
  if let RunSemanticStyle::Custom(slot) = styles.semantic {
    text.mark(range.clone(), MARK_RUN_SEMANTIC_STYLE, i64::from(slot))?;
  }
  if let Some(gpui_flowtext::HighlightStyle::Custom(slot)) = styles.highlight {
    text.mark(range.clone(), MARK_HIGHLIGHT_STYLE, i64::from(slot))?;
  }
  if styles.direct_underline {
    text.mark(range.clone(), MARK_DIRECT_UNDERLINE, true)?;
  }
  if styles.strikethrough {
    text.mark(range.clone(), MARK_STRIKETHROUGH, true)?;
  }
  if let Some(value) = styles.vert_align.mark_value() {
    text.mark(range, MARK_VERT_ALIGN, value)?;
  }
  Ok(())
}

// ---- Annotations ----------------------------------------------------------

pub fn put_annotation(doc: &LoroDoc, stroke: &AnnotationStroke) -> anyhow::Result<()> {
  annotations_map(doc).insert(&stroke.id.to_string(), postcard::to_allocvec(stroke)?)?;
  Ok(())
}

pub fn delete_annotation(doc: &LoroDoc, stroke_id: StrokeId) -> LoroResult<()> {
  let annotations = annotations_map(doc);
  if annotations.get(&stroke_id.to_string()).is_some() {
    annotations.delete(&stroke_id.to_string())?;
  }
  Ok(())
}

pub fn read_annotations(doc: &LoroDoc) -> Vec<AnnotationStroke> {
  let mut strokes = Vec::new();
  annotations_map(doc).for_each(|key, value| {
    if let ValueOrContainer::Value(LoroValue::Binary(bytes)) = value
      && let Ok(stroke) = postcard::from_bytes::<AnnotationStroke>(&bytes)
      && key == stroke.id.to_string()
    {
      strokes.push(stroke);
    }
  });
  strokes.sort_by_key(|stroke| stroke.id);
  strokes
}

// ---- Shared readers --------------------------------------------------------

pub(crate) fn child_map(parent: &LoroMap, key: &str) -> Option<LoroMap> {
  match parent.get(key)? {
    ValueOrContainer::Container(container) => container.into_map().ok(),
    ValueOrContainer::Value(_) => None,
  }
}

pub(crate) fn map_string(map: &LoroMap, key: &str) -> Option<String> {
  match map.get(key)? {
    ValueOrContainer::Value(LoroValue::String(value)) => Some(value.to_string()),
    _ => None,
  }
}

pub(crate) fn map_uuid(map: &LoroMap, key: &str) -> Option<Uuid> {
  map_string(map, key).and_then(|value| Uuid::parse_str(&value).ok())
}

fn movable_list_strings(list: &LoroMovableList) -> Vec<String> {
  let mut ids = Vec::with_capacity(list.len());
  list.for_each(|value| {
    if let ValueOrContainer::Value(LoroValue::String(id)) = value {
      ids.push(id.to_string());
    }
  });
  ids
}

pub(crate) fn order_position(list: &LoroMovableList, id: &str) -> Option<usize> {
  let mut position = None;
  let mut ix = 0usize;
  list.for_each(|value| {
    if position.is_none() {
      if let ValueOrContainer::Value(LoroValue::String(entry)) = value
        && entry.as_str() == id
      {
        position = Some(ix);
      }
      ix += 1;
    }
  });
  position
}

pub(crate) fn paragraph_style_value(style: ParagraphStyle) -> i64 {
  match style {
    ParagraphStyle::Normal => 0,
    ParagraphStyle::Custom(slot) => i64::from(slot) + 1,
  }
}

fn unix_time_secs() -> i64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}
