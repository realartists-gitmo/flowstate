use std::time::{SystemTime, UNIX_EPOCH};

use loro::{
  ContainerTrait as _, ExpandType, LoroDoc, LoroMap, LoroResult, LoroText, LoroValue, StyleConfig, StyleConfigMap,
  ValueOrContainer, cursor::Side,
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

pub const OBJECT_REPLACEMENT: char = '\u{FFFC}';
pub const SENTINEL_NEWLINE: &str = "\n";

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
  let body_flow = ensure_flow(&flows, ROOT_BODY_FLOW_ID, "body")?;
  let body_text = body_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  body_flow.ensure_mergeable_map(FLOW_ATTRS_KEY)?;
  if include_initial_paragraph {
    ensure_sentinel(&body_text)?;
    ensure_initial_paragraph(&paragraphs, &blocks, &body_text)?;
  }
  Ok(())
}

pub fn configure_text_styles(doc: &LoroDoc) {
  let mut styles = StyleConfigMap::new();
  let no_expand = StyleConfig::new().expand(ExpandType::None);
  styles.insert(MARK_PARAGRAPH_STYLE.into(), no_expand);
  styles.insert(MARK_RUN_SEMANTIC_STYLE.into(), no_expand);
  styles.insert(MARK_HIGHLIGHT_STYLE.into(), no_expand);
  styles.insert(MARK_DIRECT_UNDERLINE.into(), no_expand);
  styles.insert(MARK_STRIKETHROUGH.into(), no_expand);
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
  summary: &str,
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
  revision.insert("summary", summary)?;
  if let Some(author_user_id) = author_user_id {
    revision.insert("author_user_id", author_user_id.to_string())?;
  }
  doc.commit();
  Ok(())
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

pub fn register_replica(doc: &LoroDoc, user_id: Option<u128>) -> LoroResult<bool> {
  let root = root_map(doc);
  let replicas = root.ensure_mergeable_map(REPLICAS_BY_ID)?;
  let replica_id = doc.peer_id().to_string();
  let replica = replicas.ensure_mergeable_map(&replica_id)?;
  replica.insert("id", replica_id.as_str())?;
  replica.insert("container_id", replica.id().to_string())?;
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
  let attrs = flow.ensure_mergeable_map(FLOW_ATTRS_KEY)?;
  flow.insert("container_id", flow.id().to_string())?;
  flow.insert("text_container_id", text.id().to_string())?;
  flow.insert("attrs_container_id", attrs.id().to_string())?;
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
  paragraph.insert("container_id", paragraph.id().to_string())?;
  paragraph.insert("flow_id", ROOT_BODY_FLOW_ID)?;
  if let Some(cursor) = body.get_cursor(0, Side::Left) {
    paragraph.insert("start_cursor", cursor.encode())?;
  }
  if let Some(cursor) = body.get_cursor(0, Side::Right) {
    paragraph.insert("boundary_cursor", cursor.encode())?;
  }
  let paragraph_attrs = paragraph.ensure_mergeable_map("attrs")?;
  paragraph.insert("attrs_container_id", paragraph_attrs.id().to_string())?;

  let block = blocks.ensure_mergeable_map(MAIN_BODY_BLOCK_ID)?;
  block.insert("id", MAIN_BODY_BLOCK_ID)?;
  block.insert("container_id", block.id().to_string())?;
  block.insert("kind", "paragraph")?;
  block.insert("flow_id", ROOT_BODY_FLOW_ID)?;
  if let Some(cursor) = body.get_cursor(0, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  let block_attrs = block.ensure_mergeable_map("attrs")?;
  let nested_refs = block.ensure_mergeable_map("nested_refs")?;
  block.insert("attrs_container_id", block_attrs.id().to_string())?;
  block.insert("nested_refs_container_id", nested_refs.id().to_string())?;
  Ok(())
}

fn unix_time_secs() -> i64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |duration| i64::try_from(duration.as_secs()).unwrap_or(i64::MAX))
}
