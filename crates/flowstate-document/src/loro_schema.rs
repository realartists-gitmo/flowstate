use std::time::{SystemTime, UNIX_EPOCH};

use flowstate_fidelity::{self as fidelity, FidelityClass};
use loro::{
  ContainerTrait as _, ExpandType, LoroDoc, LoroMap, LoroResult, LoroText, LoroValue, StyleConfig, StyleConfigMap, ValueOrContainer,
  cursor::Side,
};
use uuid::Uuid;

use crate::LORO_SCHEMA_VERSION;

pub const ROOT: &str = "flowstate.root";
pub const META: &str = "meta";
pub const FLOWS_BY_ID: &str = "flows_by_id";
pub const BLOCKS_BY_ID: &str = "blocks_by_id";
pub const PARAGRAPHS_BY_ID: &str = "paragraphs_by_id";
pub const SECTIONS_BY_ID: &str = "sections_by_id";
pub const ASSETS_BY_ID: &str = "assets_by_id";
pub const REVISIONS: &str = "revisions";
pub const USERS_BY_ID: &str = "users_by_id";
pub const COMMENTS_BY_ID: &str = "comments_by_id";
pub const REPLICAS_BY_ID: &str = "replicas_by_id";

pub const FLOW_TEXT_KEY: &str = "text";
pub const FLOW_ATTRS_KEY: &str = "attrs";
pub const FLOW_KIND_KEY: &str = "kind";
pub const FLOW_ID_KEY: &str = "id";

pub const BODY_FLOW_ID: &str = "body";
pub const ROOT_BODY_FLOW_ID: &str = "body";
pub const ROOT_FIRST_PARAGRAPH_ID: &str = "paragraph.initial";
pub const MAIN_BODY_BLOCK_ID: &str = "block.body.initial";

pub const MARK_PARAGRAPH_STYLE: &str = "paragraph_style";
pub const MARK_RUN_SEMANTIC_STYLE: &str = "run_semantic_style_id";
pub const MARK_HIGHLIGHT_STYLE: &str = "highlight_style_id";
pub const MARK_DIRECT_UNDERLINE: &str = "direct_underline";
pub const MARK_STRIKETHROUGH: &str = "strikethrough";
/// Superscript/subscript run mark (OOXML `w:vertAlign`). Value: `1` superscript,
/// `2` subscript; absent ⇒ baseline. Currently written by the `.docx` import and
/// read by the projection (round-trips), but not yet emitted by the live editor —
/// see [`gpui_flowtext::VertAlign`].
pub const MARK_VERT_ALIGN: &str = "vert_align";

pub const OBJECT_REPLACEMENT: char = '\u{FFFC}';
pub const SENTINEL_NEWLINE: &str = "\n";

// §P2b durable-table containers (plan §12). Centralized so the writer, the
// projector, and the collab runtime address the identical child containers.
pub const TABLE_KEY: &str = "table";
pub const TABLE_ROW_ORDER: &str = "row_order";
pub const TABLE_COLUMN_ORDER: &str = "column_order";
pub const TABLE_ROWS_BY_ID: &str = "rows_by_id";
pub const TABLE_COLUMNS_BY_ID: &str = "columns_by_id";
pub const TABLE_CELLS_BY_ID: &str = "cells_by_id";

/// Loro string id for a durable table row (§P2b): `row.{u128}`. The dotted form
/// ends in the row's unique numeric id so the canonical `loro_id_u128` decoder
/// (which parses the trailing segment) round-trips it.
#[must_use]
pub fn row_loro_id(row: gpui_flowtext::RowId) -> String {
  format!("row.{}", row.0)
}

/// Loro string id for a durable table column (§P2b): `column.{u128}`.
#[must_use]
pub fn column_loro_id(column: gpui_flowtext::ColumnId) -> String {
  format!("column.{}", column.0)
}

/// Loro string id for a durable table cell (§P2b): `cell.{u128}` where the u128
/// is the deterministic [`gpui_flowtext::CellId`] mix of its coordinate.
#[must_use]
pub fn cell_loro_id(cell: gpui_flowtext::CellId) -> String {
  format!("cell.{}", cell.0)
}

/// Loro string id for the cell at coordinate `(row, column)` (§P2b).
#[must_use]
pub fn cell_loro_id_for(row: gpui_flowtext::RowId, column: gpui_flowtext::ColumnId) -> String {
  cell_loro_id(gpui_flowtext::CellId::from_coordinate(row, column))
}

/// Cell text flow id for a cell string id (§P2b): `{cell_id}.flow`.
#[must_use]
pub fn cell_flow_loro_id(cell_loro_id: &str) -> String {
  format!("{cell_loro_id}.flow")
}

/// Parse the trailing numeric segment of a dotted Loro id into a `u128`.
pub fn loro_id_trailing_u128(id: &str) -> Option<u128> {
  parse_trailing_u128(id)
}

fn parse_trailing_u128(id: &str) -> Option<u128> {
  id.rsplit('.')
    .next()
    .and_then(|segment| segment.parse().ok())
}

/// Decode a `row.{u128}` Loro id into a [`gpui_flowtext::RowId`] (§P2b).
#[must_use]
pub fn parse_row_loro_id(id: &str) -> Option<gpui_flowtext::RowId> {
  parse_trailing_u128(id).map(gpui_flowtext::RowId)
}

/// Decode a `column.{u128}` Loro id into a [`gpui_flowtext::ColumnId`] (§P2b).
#[must_use]
pub fn parse_column_loro_id(id: &str) -> Option<gpui_flowtext::ColumnId> {
  parse_trailing_u128(id).map(gpui_flowtext::ColumnId)
}

/// Decode a `cell.{u128}` Loro id into a [`gpui_flowtext::CellId`] (§P2b).
#[must_use]
pub fn parse_cell_loro_id(id: &str) -> Option<gpui_flowtext::CellId> {
  parse_trailing_u128(id).map(gpui_flowtext::CellId)
}

/// §11 page-structure attribute keys stored on a `SectionMap`'s `attrs` map.
///
/// Encoding (documented and locked):
/// * page size: two `i64` keys in twips (1/1440 inch) — `page_width_twips`,
///   `page_height_twips`. US Letter defaults to `12240 x 15840`.
/// * margins: four `i64` keys in twips — `margin_{top,right,bottom,left}_twips`.
///   The default 1-inch margin is `1440` twips.
/// * columns: one `i64` key — `columns` (default `1`).
/// * orientation: one string key — `orientation` (`"portrait"` | `"landscape"`).
/// * page numbering: a small struct stored as `page_numbering_format` (string)
///   plus `page_numbering_start` (`i64`, default `1`).
/// * header/footer: optional `header_flow_id` / `footer_flow_id` string keys whose
///   values name independent header/footer text flows (kinds `"header"`/`"footer"`).
///
/// Twips are used because they are the DOCX-native, integer-exact unit, keeping the
/// first-class DOCX import/export pipeline lossless.
pub const SECTION_ATTR_PAGE_WIDTH: &str = "page_width_twips";
pub const SECTION_ATTR_PAGE_HEIGHT: &str = "page_height_twips";
pub const SECTION_ATTR_MARGIN_TOP: &str = "margin_top_twips";
pub const SECTION_ATTR_MARGIN_RIGHT: &str = "margin_right_twips";
pub const SECTION_ATTR_MARGIN_BOTTOM: &str = "margin_bottom_twips";
pub const SECTION_ATTR_MARGIN_LEFT: &str = "margin_left_twips";
pub const SECTION_ATTR_COLUMNS: &str = "columns";
pub const SECTION_ATTR_ORIENTATION: &str = "orientation";
pub const SECTION_ATTR_PAGE_NUMBERING_FORMAT: &str = "page_numbering_format";
pub const SECTION_ATTR_PAGE_NUMBERING_START: &str = "page_numbering_start";
pub const SECTION_ATTR_HEADER_FLOW_ID: &str = "header_flow_id";
pub const SECTION_ATTR_FOOTER_FLOW_ID: &str = "footer_flow_id";

/// US Letter width in twips (8.5in x 1440).
const DEFAULT_PAGE_WIDTH_TWIPS: i64 = 12_240;
/// US Letter height in twips (11in x 1440).
const DEFAULT_PAGE_HEIGHT_TWIPS: i64 = 15_840;
/// One-inch margin in twips.
const DEFAULT_MARGIN_TWIPS: i64 = 1_440;

/// Section page orientation (§11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SectionOrientation {
  Portrait,
  Landscape,
}

impl SectionOrientation {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::Portrait => "portrait",
      Self::Landscape => "landscape",
    }
  }

  #[must_use]
  pub fn from_attr(value: Option<&str>) -> Self {
    match value {
      Some("landscape") => Self::Landscape,
      _ => Self::Portrait,
    }
  }
}

/// Page-number rendering format for a section (§11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PageNumberFormat {
  None,
  Decimal,
  LowerRoman,
  UpperRoman,
  LowerAlpha,
  UpperAlpha,
}

impl PageNumberFormat {
  #[must_use]
  pub fn as_str(self) -> &'static str {
    match self {
      Self::None => "none",
      Self::Decimal => "decimal",
      Self::LowerRoman => "lower_roman",
      Self::UpperRoman => "upper_roman",
      Self::LowerAlpha => "lower_alpha",
      Self::UpperAlpha => "upper_alpha",
    }
  }

  #[must_use]
  pub fn from_attr(value: Option<&str>) -> Self {
    match value {
      Some("decimal") => Self::Decimal,
      Some("lower_roman") => Self::LowerRoman,
      Some("upper_roman") => Self::UpperRoman,
      Some("lower_alpha") => Self::LowerAlpha,
      Some("upper_alpha") => Self::UpperAlpha,
      _ => Self::None,
    }
  }
}

/// Page size for a section, in twips (§11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionPageSize {
  pub width_twips: i64,
  pub height_twips: i64,
}

impl Default for SectionPageSize {
  fn default() -> Self {
    Self {
      width_twips: DEFAULT_PAGE_WIDTH_TWIPS,
      height_twips: DEFAULT_PAGE_HEIGHT_TWIPS,
    }
  }
}

/// Section margins, in twips (§11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionMargins {
  pub top_twips: i64,
  pub right_twips: i64,
  pub bottom_twips: i64,
  pub left_twips: i64,
}

impl Default for SectionMargins {
  fn default() -> Self {
    Self {
      top_twips: DEFAULT_MARGIN_TWIPS,
      right_twips: DEFAULT_MARGIN_TWIPS,
      bottom_twips: DEFAULT_MARGIN_TWIPS,
      left_twips: DEFAULT_MARGIN_TWIPS,
    }
  }
}

/// Section page-numbering descriptor (§11).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SectionPageNumbering {
  pub format: PageNumberFormat,
  pub start: i64,
}

impl Default for SectionPageNumbering {
  fn default() -> Self {
    Self {
      format: PageNumberFormat::None,
      start: 1,
    }
  }
}

/// §11 page-structure attributes carried by a `SectionMap`.
///
/// These are canonical in Loro regardless of whether a projection type can hold
/// them. They round-trip losslessly through [`write_section_page_attrs`] /
/// [`read_section_page_attrs`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SectionPageAttrs {
  pub page_size: SectionPageSize,
  pub margins: SectionMargins,
  pub columns: i64,
  pub orientation: SectionOrientation,
  pub page_numbering: SectionPageNumbering,
  pub header_flow_id: Option<String>,
  pub footer_flow_id: Option<String>,
}

impl Default for SectionPageAttrs {
  fn default() -> Self {
    Self {
      page_size: SectionPageSize::default(),
      margins: SectionMargins::default(),
      columns: 1,
      orientation: SectionOrientation::Portrait,
      page_numbering: SectionPageNumbering::default(),
      header_flow_id: None,
      footer_flow_id: None,
    }
  }
}

pub fn new_loro_document(title: &str) -> LoroResult<LoroDoc> {
  let doc = LoroDoc::new();
  init_loro_document(&doc, title)?;
  Ok(doc)
}

pub(crate) fn new_loro_import_document(title: &str) -> LoroResult<LoroDoc> {
  let doc = LoroDoc::new();
  init_loro_document_structure(&doc, title, false)?;
  Ok(doc)
}

pub fn init_loro_document(doc: &LoroDoc, title: &str) -> LoroResult<()> {
  init_loro_document_structure(doc, title, true)?;
  doc.commit();
  Ok(())
}

fn init_loro_document_structure(doc: &LoroDoc, title: &str, include_initial_paragraph: bool) -> LoroResult<()> {
  configure_text_styles(doc);

  let root = doc.get_map(ROOT);
  let meta = root.ensure_mergeable_map(META)?;
  let flows = root.ensure_mergeable_map(FLOWS_BY_ID)?;
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  let sections = root.ensure_mergeable_map(SECTIONS_BY_ID)?;
  let assets = root.ensure_mergeable_map(ASSETS_BY_ID)?;
  let revisions = root.ensure_mergeable_list(REVISIONS)?;
  let users = root.ensure_mergeable_map(USERS_BY_ID)?;
  let replicas = root.ensure_mergeable_map(REPLICAS_BY_ID)?;

  init_meta(&meta, title)?;
  meta.insert("root_container_id", root.id().to_string())?;
  meta.insert("flows_container_id", flows.id().to_string())?;
  meta.insert("blocks_container_id", blocks.id().to_string())?;
  meta.insert("paragraphs_container_id", paragraphs.id().to_string())?;
  meta.insert("sections_container_id", sections.id().to_string())?;
  meta.insert("assets_container_id", assets.id().to_string())?;
  meta.insert("revisions_container_id", revisions.id().to_string())?;
  meta.insert("users_container_id", users.id().to_string())?;
  meta.insert("replicas_container_id", replicas.id().to_string())?;
  // `ensure_flow` establishes the body flow's text + attrs containers for both
  // the seeded and the import (`include_initial_paragraph == false`) paths.
  ensure_flow(&flows, ROOT_BODY_FLOW_ID, "body")?;
  if include_initial_paragraph {
    // §P2a: route fresh-document creation through the unified seed so new
    // documents and the runtime empty-body repair path converge on identical
    // canonical state.
    seed_document_body(doc)?;
  }
  Ok(())
}

/// §P2a unified canonical document seed. Establishes exactly the canonical body
/// state that both fresh-document creation and the runtime empty-body /
/// `MissingParagraph*` repair path must converge on:
/// * the boundary-0 [`SENTINEL_NEWLINE`] carrying a `Normal` paragraph-style mark,
/// * the first paragraph record ([`ROOT_FIRST_PARAGRAPH_ID`]) and its paired
///   paragraph block record ([`MAIN_BODY_BLOCK_ID`]).
///
/// Section page defaults are supplied lazily by [`read_section_page_attrs`]
/// (which substitutes documented US-Letter defaults for any missing section
/// `attrs`), so the seed does not need to materialize a section container to make
/// a new/repaired document render with correct page structure.
///
/// Idempotent: every step is an `ensure_*` / starts-with check, so seeding an
/// already-seeded document — or two peers seeding the same body concurrently —
/// converges via Loro map LWW semantics without duplicating structure. This
/// function does **not** commit; the caller controls the commit (and, for the
/// runtime repair path, its origin).
pub fn seed_document_body(doc: &LoroDoc) -> LoroResult<()> {
  let root = doc.get_map(ROOT);
  let flows = root.ensure_mergeable_map(FLOWS_BY_ID)?;
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  let body_flow = ensure_flow(&flows, ROOT_BODY_FLOW_ID, "body")?;
  let body_text = body_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  body_flow.ensure_mergeable_map(FLOW_ATTRS_KEY)?;
  ensure_sentinel(&body_text)?;
  ensure_initial_paragraph(&paragraphs, &blocks, &body_text)?;
  // §P2a fidelity: the canonical seed establishes exactly the sentinel +
  // first-paragraph state that the projection integrity and import invariants
  // assert. Trace each seeding so a convergence/repair trail is visible.
  fidelity::event(FidelityClass::Structure, "seed-body", || {
    format!(
      "seeded canonical body sentinel + first paragraph (body_unicode_len={})",
      body_text.len_unicode()
    )
  });
  Ok(())
}

/// Per-key mark expand table (Loro-first spec §9, D7).
///
/// Run-style marks expand `After`: typing at the end of a styled run inherits
/// the style natively (Peritext semantics — what a word processor does), so a
/// plain text insert emits ZERO style operations. `paragraph_style` stays
/// `None`: it anchors the boundary sentinel newline and must never bleed into
/// text. Two riders enforced by the write path:
/// * caret style *overrides* (style toggled at the caret) explicitly
///   mark/unmark exactly the inserted range;
/// * `split_paragraph` unmarks all run-style keys on its inserted `\n`
///   (sentinel hygiene) — an expand-`After` run ending at the split point
///   would otherwise absorb the sentinel and bleed into the next paragraph.
pub fn configure_text_styles(doc: &LoroDoc) {
  let mut styles = StyleConfigMap::new();
  let no_expand = StyleConfig::new().expand(ExpandType::None);
  let expand_after = StyleConfig::new().expand(ExpandType::After);
  styles.insert(MARK_PARAGRAPH_STYLE.into(), no_expand);
  styles.insert(MARK_RUN_SEMANTIC_STYLE.into(), expand_after);
  styles.insert(MARK_HIGHLIGHT_STYLE.into(), expand_after);
  styles.insert(MARK_DIRECT_UNDERLINE.into(), expand_after);
  styles.insert(MARK_STRIKETHROUGH.into(), expand_after);
  styles.insert(MARK_VERT_ALIGN.into(), expand_after);
  doc.config_text_style(styles);
}

pub fn root_map(doc: &LoroDoc) -> LoroMap {
  doc.get_map(ROOT)
}

pub fn body_text(doc: &LoroDoc) -> LoroText {
  let root = root_map(doc);
  let flows = root
    .ensure_mergeable_map(FLOWS_BY_ID)
    .expect("root flows map should be initialized");
  let body = flows
    .ensure_mergeable_map(ROOT_BODY_FLOW_ID)
    .expect("body flow should be initialized");
  body
    .ensure_mergeable_text(FLOW_TEXT_KEY)
    .expect("body text should be initialized")
}

pub fn record_revision(
  doc: &LoroDoc,
  revision_id: u128,
  frontier: Vec<u8>,
  title: &str,
  kind: crate::package::RevisionKind,
  author_user_id: Option<u128>,
) -> LoroResult<()> {
  let root = root_map(doc);
  let revisions = root.ensure_mergeable_list(REVISIONS)?;
  let revision = revisions.insert_container(revisions.len(), LoroMap::new())?;
  revision.insert("id", revision_id.to_string())?;
  revision.insert("timestamp", unix_time_secs())?;
  revision.insert("replica_id", doc.peer_id().to_string())?;
  revision.insert("frontier", frontier)?;
  revision.insert("title", title)?;
  // H-S1: the summary field carried filler ("Explicit save"); the tier now
  // says what the record is. Kept empty for old readers.
  revision.insert("summary", "")?;
  revision.insert("kind", kind.as_str())?;
  if let Some(author_user_id) = author_user_id {
    revision.insert("author_user_id", author_user_id.to_string())?;
  }
  // §act-ten A10.2: revision/metadata commits are INERT for undo — they touch
  // only the meta/revisions containers, never the body. The "meta" origin is
  // excluded from the UndoManager (like "remote"/"repair"), so an autosave
  // checkpoint no longer pushes a bogus undo step; the runtime re-arms the
  // recorded-inverse stacks across this commit for the same reason.
  doc.commit_with(loro::CommitOptions::new().origin("meta"));
  Ok(())
}

/// H-S1: rename a revision record in place. Naming a moment PINS it — the
/// record's kind becomes `named`, exempting it from auto-grain thinning.
/// Returns false when no record with the id exists.
pub fn rename_revision_record(doc: &LoroDoc, revision_id: u128, title: &str) -> LoroResult<bool> {
  let root = root_map(doc);
  let Some(loro::ValueOrContainer::Container(loro::Container::List(revisions))) = root.get(REVISIONS) else {
    return Ok(false);
  };
  let wanted = revision_id.to_string();
  for index in 0..revisions.len() {
    let Some(loro::ValueOrContainer::Container(loro::Container::Map(revision))) = revisions.get(index) else {
      continue;
    };
    let matches = matches!(
      revision.get("id"),
      Some(loro::ValueOrContainer::Value(loro::LoroValue::String(id))) if id.as_str() == wanted
    );
    if !matches {
      continue;
    }
    revision.insert("title", title)?;
    revision.insert("kind", crate::package::RevisionKind::Named.as_str())?;
    doc.commit_with(loro::CommitOptions::new().origin("meta"));
    return Ok(true);
  }
  Ok(false)
}

/// H-S1 thinning: delete the revision records whose ids are in `doomed`.
/// Positional deletes over the same elements converge (concurrent thinning on
/// two peers deletes the same records). Returns how many were removed.
pub fn remove_revision_records<S: std::hash::BuildHasher>(doc: &LoroDoc, doomed: &std::collections::HashSet<u128, S>) -> LoroResult<usize> {
  let root = root_map(doc);
  let Some(loro::ValueOrContainer::Container(loro::Container::List(revisions))) = root.get(REVISIONS) else {
    return Ok(0);
  };
  let mut removed = 0usize;
  // Walk backwards so earlier indexes stay valid across deletes.
  for index in (0..revisions.len()).rev() {
    let Some(loro::ValueOrContainer::Container(loro::Container::Map(revision))) = revisions.get(index) else {
      continue;
    };
    let id = match revision.get("id") {
      Some(loro::ValueOrContainer::Value(loro::LoroValue::String(id))) => id.as_str().parse::<u128>().ok(),
      _ => None,
    };
    if id.is_some_and(|id| doomed.contains(&id)) {
      revisions.delete(index, 1)?;
      removed += 1;
    }
  }
  if removed > 0 {
    doc.commit_with(loro::CommitOptions::new().origin("meta"));
  }
  Ok(removed)
}

pub fn set_document_id(doc: &LoroDoc, document_id: Uuid) -> LoroResult<()> {
  let meta = root_map(doc).ensure_mergeable_map(META)?;
  meta.insert("document_id", document_id.to_string())?;
  touch_document_metadata(doc)?;
  Ok(())
}

pub fn document_id(doc: &LoroDoc) -> Option<Uuid> {
  let ValueOrContainer::Container(container) = root_map(doc).get(META)? else {
    return None;
  };
  let meta = container.into_map().ok()?;
  let ValueOrContainer::Value(LoroValue::String(value)) = meta.get("document_id")? else {
    return None;
  };
  Uuid::parse_str(&value).ok()
}

pub fn document_schema_version(doc: &LoroDoc) -> Option<u32> {
  let ValueOrContainer::Container(container) = root_map(doc).get(META)? else {
    return None;
  };
  let meta = container.into_map().ok()?;
  let ValueOrContainer::Value(LoroValue::I64(version)) = meta.get("loro_schema_version")? else {
    return None;
  };
  u32::try_from(version).ok()
}

pub fn fork_document_lineage(doc: &LoroDoc) -> LoroResult<Uuid> {
  let root = root_map(doc);
  let meta = root.ensure_mergeable_map(META)?;
  if let Some(parent_id) = document_id(doc) {
    meta.insert("parent_document_id", parent_id.to_string())?;
  }
  let document_id = Uuid::new_v4();
  meta.insert("document_id", document_id.to_string())?;
  meta.insert("forked_at", unix_time_secs())?;
  meta.insert("modified_at", unix_time_secs())?;
  meta.insert("last_written_by_app_version", env!("CARGO_PKG_VERSION"))?;
  doc.commit();
  Ok(document_id)
}

pub fn touch_document_metadata(doc: &LoroDoc) -> LoroResult<()> {
  let meta = root_map(doc).ensure_mergeable_map(META)?;
  meta.insert("modified_at", unix_time_secs())?;
  meta.insert("last_written_by_app_version", env!("CARGO_PKG_VERSION"))?;
  Ok(())
}

/// §act-twelve A12.1.1: the OPEN-time replica invariant without the commit.
/// `register_replica` unconditionally rewrites `last_seen_at`/`app_version`
/// and commits — which advanced the frontier on EVERY open, permanently
/// invalidating the package's frontier-stamped projection/search caches for
/// all later opens. This variant writes (and commits) only when the record is
/// genuinely absent or from another app version; the `last_seen_at` touch
/// rides the first real local edit instead (see the collab layer's deferred
/// author-identity hook). Returns whether it committed.
pub fn register_replica_if_absent(doc: &LoroDoc) -> LoroResult<bool> {
  let root = root_map(doc);
  let replicas = root.ensure_mergeable_map(REPLICAS_BY_ID)?;
  let replica_id = doc.peer_id().to_string();
  let existing = replicas.get(&replica_id).and_then(|value| match value {
    loro::ValueOrContainer::Container(loro::Container::Map(map)) => Some(map),
    _ => None,
  });
  if let Some(replica) = existing {
    let same_version = matches!(
      replica.get("app_version"),
      Some(loro::ValueOrContainer::Value(loro::LoroValue::String(version))) if version.as_str() == env!("CARGO_PKG_VERSION")
    );
    if same_version {
      return Ok(false);
    }
  }
  register_replica(doc, None)
}

pub fn register_replica(doc: &LoroDoc, user_id: Option<u128>) -> LoroResult<bool> {
  let root = root_map(doc);
  let replicas = root.ensure_mergeable_map(REPLICAS_BY_ID)?;
  let replica_id = doc.peer_id().to_string();
  let replica = replicas.ensure_mergeable_map(&replica_id)?;
  replica.insert("id", replica_id.as_str())?;
  replica.insert("app_version", env!("CARGO_PKG_VERSION"))?;
  if replica.get("created_at").is_none() {
    replica.insert("created_at", unix_time_secs())?;
  }
  replica.insert("last_seen_at", unix_time_secs())?;
  if let Some(user_id) = user_id {
    replica.insert("user_id", user_id.to_string())?;
  }
  doc.commit();
  Ok(true)
}

/// Register (or refresh) a durable user identity in `users_by_id` (§15/§31).
///
/// Mirrors [`register_replica`]: it writes a `UserMap` record (`id`,
/// `container_id`, optional `display_name`, `created_at`, `last_seen_at`) and
/// links the *current* editing replica to this user so authorship/blame can be
/// resolved from replica to user. Returns `true` once the record is present.
///
/// The collaboration layer calls this with a real identity; the schema only
/// guarantees the record shape and the replica→user link.
pub fn register_user(doc: &LoroDoc, user_id: u128, display_name: Option<&str>) -> LoroResult<bool> {
  let root = root_map(doc);
  let users = root.ensure_mergeable_map(USERS_BY_ID)?;
  let user_key = user_id.to_string();
  let user = users.ensure_mergeable_map(&user_key)?;
  user.insert("id", user_key.as_str())?;
  if let Some(display_name) = display_name {
    user.insert("display_name", display_name)?;
  }
  if user.get("created_at").is_none() {
    user.insert("created_at", unix_time_secs())?;
  }
  user.insert("last_seen_at", unix_time_secs())?;

  // Link the current editing replica to this durable user identity (§15).
  let replicas = root.ensure_mergeable_map(REPLICAS_BY_ID)?;
  let replica_id = doc.peer_id().to_string();
  let replica = replicas.ensure_mergeable_map(&replica_id)?;
  replica.insert("id", replica_id.as_str())?;
  replica.insert("user_id", user_key.as_str())?;
  if replica.get("created_at").is_none() {
    replica.insert("created_at", unix_time_secs())?;
  }
  replica.insert("last_seen_at", unix_time_secs())?;
  doc.commit();
  Ok(true)
}

/// Ensure a `SectionMap` exists in `sections_by_id` with its identity and an
/// `attrs` child map (§11). Returns the section map.
pub fn ensure_section(doc: &LoroDoc, section_id: &str) -> LoroResult<LoroMap> {
  let root = root_map(doc);
  let sections = root.ensure_mergeable_map(SECTIONS_BY_ID)?;
  let section = sections.ensure_mergeable_map(section_id)?;
  section.insert("id", section_id)?;
  let _attrs = section.ensure_mergeable_map("attrs")?;
  Ok(section)
}

/// Create/update a section's §11 page-structure attributes and, when referenced,
/// its independent header/footer text flows. Commits the change.
pub fn set_section_page_attrs(doc: &LoroDoc, section_id: &str, attrs: &SectionPageAttrs) -> LoroResult<()> {
  let section = ensure_section(doc, section_id)?;
  let attrs_map = section.ensure_mergeable_map("attrs")?;
  let flows = root_map(doc).ensure_mergeable_map(FLOWS_BY_ID)?;
  write_section_page_attrs(&attrs_map, &flows, attrs)?;
  doc.commit();
  Ok(())
}

/// Write §11 page-structure attributes onto a section's `attrs` map using the
/// documented twip/string encoding. When `header_flow_id`/`footer_flow_id` are
/// set, the corresponding header/footer flows are created in `flows`.
pub(crate) fn write_section_page_attrs(attrs_map: &LoroMap, flows: &LoroMap, attrs: &SectionPageAttrs) -> LoroResult<()> {
  attrs_map.insert(SECTION_ATTR_PAGE_WIDTH, attrs.page_size.width_twips)?;
  attrs_map.insert(SECTION_ATTR_PAGE_HEIGHT, attrs.page_size.height_twips)?;
  attrs_map.insert(SECTION_ATTR_MARGIN_TOP, attrs.margins.top_twips)?;
  attrs_map.insert(SECTION_ATTR_MARGIN_RIGHT, attrs.margins.right_twips)?;
  attrs_map.insert(SECTION_ATTR_MARGIN_BOTTOM, attrs.margins.bottom_twips)?;
  attrs_map.insert(SECTION_ATTR_MARGIN_LEFT, attrs.margins.left_twips)?;
  attrs_map.insert(SECTION_ATTR_COLUMNS, attrs.columns)?;
  attrs_map.insert(SECTION_ATTR_ORIENTATION, attrs.orientation.as_str())?;
  attrs_map.insert(SECTION_ATTR_PAGE_NUMBERING_FORMAT, attrs.page_numbering.format.as_str())?;
  attrs_map.insert(SECTION_ATTR_PAGE_NUMBERING_START, attrs.page_numbering.start)?;
  if let Some(header_flow_id) = &attrs.header_flow_id {
    attrs_map.insert(SECTION_ATTR_HEADER_FLOW_ID, header_flow_id.as_str())?;
    ensure_flow(flows, header_flow_id, "header")?;
  }
  if let Some(footer_flow_id) = &attrs.footer_flow_id {
    attrs_map.insert(SECTION_ATTR_FOOTER_FLOW_ID, footer_flow_id.as_str())?;
    ensure_flow(flows, footer_flow_id, "footer")?;
  }
  Ok(())
}

/// Read §11 page-structure attributes back from a section's `attrs` map,
/// substituting the documented defaults (US Letter, 1-inch margins, 1 column,
/// portrait, no numbering, no header/footer) for any missing keys.
#[must_use]
pub fn read_section_page_attrs(attrs_map: &LoroMap) -> SectionPageAttrs {
  let defaults = SectionPageAttrs::default();
  SectionPageAttrs {
    page_size: SectionPageSize {
      width_twips: map_i64_value(attrs_map, SECTION_ATTR_PAGE_WIDTH).unwrap_or(defaults.page_size.width_twips),
      height_twips: map_i64_value(attrs_map, SECTION_ATTR_PAGE_HEIGHT).unwrap_or(defaults.page_size.height_twips),
    },
    margins: SectionMargins {
      top_twips: map_i64_value(attrs_map, SECTION_ATTR_MARGIN_TOP).unwrap_or(defaults.margins.top_twips),
      right_twips: map_i64_value(attrs_map, SECTION_ATTR_MARGIN_RIGHT).unwrap_or(defaults.margins.right_twips),
      bottom_twips: map_i64_value(attrs_map, SECTION_ATTR_MARGIN_BOTTOM).unwrap_or(defaults.margins.bottom_twips),
      left_twips: map_i64_value(attrs_map, SECTION_ATTR_MARGIN_LEFT).unwrap_or(defaults.margins.left_twips),
    },
    columns: map_i64_value(attrs_map, SECTION_ATTR_COLUMNS).unwrap_or(defaults.columns),
    orientation: SectionOrientation::from_attr(map_string_value(attrs_map, SECTION_ATTR_ORIENTATION).as_deref()),
    page_numbering: SectionPageNumbering {
      format: PageNumberFormat::from_attr(map_string_value(attrs_map, SECTION_ATTR_PAGE_NUMBERING_FORMAT).as_deref()),
      start: map_i64_value(attrs_map, SECTION_ATTR_PAGE_NUMBERING_START).unwrap_or(defaults.page_numbering.start),
    },
    header_flow_id: map_string_value(attrs_map, SECTION_ATTR_HEADER_FLOW_ID),
    footer_flow_id: map_string_value(attrs_map, SECTION_ATTR_FOOTER_FLOW_ID),
  }
}

fn map_i64_value(map: &LoroMap, key: &str) -> Option<i64> {
  match map.get(key)? {
    ValueOrContainer::Value(LoroValue::I64(value)) => Some(value),
    _ => None,
  }
}

fn map_string_value(map: &LoroMap, key: &str) -> Option<String> {
  match map.get(key)? {
    ValueOrContainer::Value(LoroValue::String(value)) => Some(value.to_string()),
    _ => None,
  }
}

fn init_meta(meta: &LoroMap, title: &str) -> LoroResult<()> {
  let now = unix_time_secs();
  if meta.get("document_id").is_none() {
    meta.insert("document_id", Uuid::new_v4().to_string())?;
  }
  meta.insert("loro_schema_version", i64::from(LORO_SCHEMA_VERSION))?;
  meta.insert("schema_features", "flow-v1")?;
  meta.insert("title", title)?;
  meta.insert("created_by_app_version", env!("CARGO_PKG_VERSION"))?;
  meta.insert("last_written_by_app_version", env!("CARGO_PKG_VERSION"))?;
  if meta.get("created_at").is_none() {
    meta.insert("created_at", now)?;
  }
  meta.insert("modified_at", now)?;
  Ok(())
}

fn ensure_flow(flows: &LoroMap, flow_id: &str, kind: &str) -> LoroResult<LoroMap> {
  let flow = flows.ensure_mergeable_map(flow_id)?;
  flow.insert(FLOW_ID_KEY, flow_id)?;
  flow.insert(FLOW_KIND_KEY, kind)?;
  let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  let _attrs = flow.ensure_mergeable_map(FLOW_ATTRS_KEY)?;
  flow.insert("text_container_id", text.id().to_string())?;
  Ok(flow)
}

fn ensure_sentinel(text: &LoroText) -> LoroResult<()> {
  if text.len_unicode() == 0 || !text.to_string().starts_with(SENTINEL_NEWLINE) {
    text.insert(0, SENTINEL_NEWLINE)?;
    text.mark(0..1, MARK_PARAGRAPH_STYLE, 0_i64)?;
  }
  Ok(())
}

fn ensure_initial_paragraph(paragraphs: &LoroMap, blocks: &LoroMap, body: &LoroText) -> LoroResult<()> {
  let paragraph = paragraphs.ensure_mergeable_map(ROOT_FIRST_PARAGRAPH_ID)?;
  paragraph.insert("id", ROOT_FIRST_PARAGRAPH_ID)?;
  paragraph.insert("flow_id", ROOT_BODY_FLOW_ID)?;
  if let Some(cursor) = body.get_cursor(0, Side::Left) {
    paragraph.insert("start_cursor", cursor.encode())?;
  }
  if let Some(cursor) = body.get_cursor(0, Side::Left) {
    paragraph.insert("boundary_cursor", cursor.encode())?;
  }
  let _paragraph_attrs = paragraph.ensure_mergeable_map("attrs")?;

  let block = blocks.ensure_mergeable_map(MAIN_BODY_BLOCK_ID)?;
  block.insert("id", MAIN_BODY_BLOCK_ID)?;
  block.insert("kind", "paragraph")?;
  block.insert("flow_id", ROOT_BODY_FLOW_ID)?;
  if let Some(cursor) = body.get_cursor(0, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  let _block_attrs = block.ensure_mergeable_map("attrs")?;
  let _nested_refs = block.ensure_mergeable_map("nested_refs")?;
  Ok(())
}

fn unix_time_secs() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn section_page_attrs_round_trip_through_loro() -> LoroResult<()> {
    let doc = new_loro_document("Sections")?;
    let attrs = SectionPageAttrs {
      page_size: SectionPageSize {
        width_twips: 15_840,
        height_twips: 12_240,
      },
      margins: SectionMargins {
        top_twips: 720,
        right_twips: 720,
        bottom_twips: 1_440,
        left_twips: 1_440,
      },
      columns: 2,
      orientation: SectionOrientation::Landscape,
      page_numbering: SectionPageNumbering {
        format: PageNumberFormat::LowerRoman,
        start: 3,
      },
      header_flow_id: Some("section.s1.header".to_string()),
      footer_flow_id: Some("section.s1.footer".to_string()),
    };
    set_section_page_attrs(&doc, "section.s1", &attrs)?;

    let root = root_map(&doc);
    let sections = root.ensure_mergeable_map(SECTIONS_BY_ID)?;
    let section = sections.ensure_mergeable_map("section.s1")?;
    let attrs_map = section.ensure_mergeable_map("attrs")?;
    assert_eq!(read_section_page_attrs(&attrs_map), attrs);

    // §11: header/footer are independent flows of their respective kinds.
    let flows = root.ensure_mergeable_map(FLOWS_BY_ID)?;
    let header = flows.ensure_mergeable_map("section.s1.header")?;
    assert_eq!(map_string_value(&header, FLOW_KIND_KEY).as_deref(), Some("header"));
    let footer = flows.ensure_mergeable_map("section.s1.footer")?;
    assert_eq!(map_string_value(&footer, FLOW_KIND_KEY).as_deref(), Some("footer"));
    Ok(())
  }

  #[test]
  fn section_page_attrs_default_to_letter_when_unset() -> LoroResult<()> {
    let doc = new_loro_document("Sections")?;
    let section = ensure_section(&doc, "section.bare")?;
    let attrs_map = section.ensure_mergeable_map("attrs")?;
    assert_eq!(read_section_page_attrs(&attrs_map), SectionPageAttrs::default());
    Ok(())
  }

  #[test]
  fn register_user_writes_record_and_links_replica() -> LoroResult<()> {
    let doc = new_loro_document("Users")?;
    register_user(&doc, 0x42, Some("Ada"))?;

    let root = root_map(&doc);
    let users = root.ensure_mergeable_map(USERS_BY_ID)?;
    let user = users.ensure_mergeable_map("66")?;
    assert_eq!(map_string_value(&user, "display_name").as_deref(), Some("Ada"));
    assert_eq!(map_string_value(&user, "id").as_deref(), Some("66"));

    let replicas = root.ensure_mergeable_map(REPLICAS_BY_ID)?;
    let replica = replicas.ensure_mergeable_map(&doc.peer_id().to_string())?;
    assert_eq!(map_string_value(&replica, "user_id").as_deref(), Some("66"));
    Ok(())
  }
}
