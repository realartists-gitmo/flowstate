use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::ops::Range;

use loro::cursor::{Cursor, PosType, Side};
use loro::event::Diff;
use loro::{
  Container, ContainerTrait, ExpandType, Index, LoroDoc, LoroMap, LoroMovableList, LoroText, LoroValue, StyleConfig, StyleConfigMap,
  ValueOrContainer, VersionVector,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::{
  AnchoredPosition, FlowChangeSummary, FlowId, FlowNodeId, FlowTextChange, MaterializedFlowWindow, ResolvedFlowPosition,
};
use crate::{ActorId, CollabError, CollabResult, DocumentId};
mod object_edits;
mod object_graph;
mod parsing;
mod text_edits;
mod validation;
pub(super) use object_edits::{anchor_at_node_index, delete_object_at, insert_seed_object};
pub(super) use object_graph::materialize_object_graph;
use parsing::{ParsedFlow, parse_flow};
#[cfg(test)]
pub(super) use parsing::parse_flow_call_count;
pub(super) use text_edits::{insert_text_with_exact_marks, replace_paragraph_text_at};
pub(super) use validation::validate;

pub const FLOW_SOURCE_SCHEMA_VERSION: u32 = 6;
pub const FLOW_HISTORY_EPOCH: u64 = 0;

const ROOT_MAP: &str = "flowstate_vnext";
const KEY_SCHEMA_VERSION: &str = "schema_version";
const KEY_HISTORY_EPOCH: &str = "history_epoch";
const KEY_HISTORY_MODE: &str = "history_mode";
const KEY_DOCUMENT_ID: &str = "document_id";
const KEY_CREATED_BY_ACTOR: &str = "created_by_actor";
const KEY_ROOT_FLOW_ID: &str = "root_flow_id";
const KEY_DOCUMENT_METADATA: &str = "document_metadata";
const KEY_FLOWS: &str = "flows";
const KEY_NODES: &str = "nodes";
const KEY_ASSETS: &str = "assets";
const KEY_FLOW_CONTENT: &str = "content";
const KEY_NODE_KIND: &str = "kind";
pub const KEY_NODE_METADATA: &str = "metadata";
const KEY_NODE_CHILD_FLOWS: &str = "child_flows";
const KEY_NODE_OWNER_FLOW: &str = "owner_flow";
const PARAGRAPH_MARK: &str = "flowstate_paragraph_token";
const OBJECT_MARK: &str = "flowstate_object_token";
const PARAGRAPH_TOKEN: char = '\u{FDD0}';
const OBJECT_TOKEN: char = '\u{FDD1}';
const HISTORY_MODE_FULL: i64 = 1;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FlowNodeKind {
  Paragraph,
  Object,
}

impl FlowNodeKind {
  const fn code(self) -> i64 {
    match self {
      Self::Paragraph => 1,
      Self::Object => 2,
    }
  }

  fn from_code(code: i64) -> CollabResult<Self> {
    match code {
      1 => Ok(Self::Paragraph),
      2 => Ok(Self::Object),
      _ => Err(CollabError::InvalidSchema(KEY_NODE_KIND)),
    }
  }

  const fn token(self) -> char {
    match self {
      Self::Paragraph => PARAGRAPH_TOKEN,
      Self::Object => OBJECT_TOKEN,
    }
  }

  const fn mark_key(self) -> &'static str {
    match self {
      Self::Paragraph => PARAGRAPH_MARK,
      Self::Object => OBJECT_MARK,
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FlowMarkValue {
  Bool(bool),
  I64(i64),
  String(String),
}

impl FlowMarkValue {
  fn from_loro(value: &LoroValue) -> Option<Self> {
    match value {
      LoroValue::Bool(value) => Some(Self::Bool(*value)),
      LoroValue::I64(value) => Some(Self::I64(*value)),
      LoroValue::String(value) => Some(Self::String(value.to_string())),
      _ => None,
    }
  }

  pub(super) fn into_loro(self) -> LoroValue {
    match self {
      Self::Bool(value) => value.into(),
      Self::I64(value) => value.into(),
      Self::String(value) => value.into(),
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlowInlineMark {
  pub range_utf8: Range<usize>,
  pub key: String,
  pub value: FlowMarkValue,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlowNodeRecord {
  pub id: FlowNodeId,
  pub kind: FlowNodeKind,
  pub metadata: Vec<u8>,
  pub child_flows: Vec<FlowId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FlowNode {
  Paragraph {
    record: FlowNodeRecord,
    text: String,
    marks: Vec<FlowInlineMark>,
  },
  Object {
    record: FlowNodeRecord,
  },
}

impl FlowNode {
  #[must_use]
  pub const fn record(&self) -> &FlowNodeRecord {
    match self {
      Self::Paragraph { record, .. } | Self::Object { record } => record,
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct MaterializedFlow {
  pub id: FlowId,
  pub nodes: Vec<FlowNode>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlowMaterialization {
  pub document_id: DocumentId,
  pub created_by_actor: ActorId,
  pub root_flow_id: FlowId,
  pub document_metadata: Vec<u8>,
  pub assets: BTreeMap<FlowAssetId, FlowAssetReference>,
  pub flows: BTreeMap<FlowId, MaterializedFlow>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializedObjectGraph {
  pub root: FlowNodeRecord,
  pub assets: BTreeMap<FlowAssetId, FlowAssetReference>,
  pub flows: BTreeMap<FlowId, MaterializedFlow>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct FlowAssetId(pub Uuid);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlowAssetReference {
  pub id: FlowAssetId,
  pub blake3_hash: [u8; 32],
  pub byte_len: u64,
  pub mime_type: String,
  pub original_name: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlowDocumentSeed {
  pub root_flow_id: FlowId,
  pub document_metadata: Vec<u8>,
  pub assets: Vec<FlowAssetReference>,
  pub flows: Vec<FlowSeedFlow>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlowSeedFlow {
  pub id: FlowId,
  pub nodes: Vec<FlowSeedNode>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlowSeedNode {
  pub record: FlowNodeRecord,
  pub text: String,
  pub marks: Vec<FlowInlineMark>,
}

#[derive(Clone, Debug)]
pub struct FlowSourceLimits {
  pub max_update_bytes: usize,
  pub max_update_ops: usize,
  pub max_total_ops: usize,
  pub max_total_changes: usize,
  pub max_flows: usize,
  pub max_nodes: usize,
  pub max_assets: usize,
  pub max_total_text_bytes: usize,
  pub max_flow_text_bytes: usize,
  pub max_total_metadata_bytes: usize,
  pub max_node_metadata_bytes: usize,
  pub max_asset_reference_bytes: usize,
  pub max_marks: usize,
  pub max_child_flows_per_node: usize,
  pub max_flow_depth: usize,
  pub max_validation_millis: u64,
}

impl Default for FlowSourceLimits {
  fn default() -> Self {
    Self {
      max_update_bytes: 16 * 1024 * 1024,
      max_update_ops: 1_000_000,
      max_total_ops: 20_000_000,
      max_total_changes: 5_000_000,
      max_flows: 100_000,
      max_nodes: 1_000_000,
      max_assets: 1_000_000,
      max_total_text_bytes: 512 * 1024 * 1024,
      max_flow_text_bytes: 128 * 1024 * 1024,
      max_total_metadata_bytes: 128 * 1024 * 1024,
      max_node_metadata_bytes: 4 * 1024 * 1024,
      max_asset_reference_bytes: 64 * 1024,
      max_marks: 4_000_000,
      max_child_flows_per_node: 100_000,
      max_flow_depth: 128,
      max_validation_millis: 2_000,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum FlowHistoryMode {
  Full,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct FlowHistoryPolicy {
  pub epoch: u64,
  pub mode: FlowHistoryMode,
}

impl FlowHistoryPolicy {
  #[must_use]
  pub const fn full_history() -> Self {
    Self {
      epoch: FLOW_HISTORY_EPOCH,
      mode: FlowHistoryMode::Full,
    }
  }

  #[must_use]
  pub const fn permits_compaction(self) -> bool {
    false
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct ProtectedState {
  schema_version: i64,
  history_epoch: i64,
  history_mode: i64,
  document_id: Uuid,
  created_by_actor: Uuid,
  root_flow_id: Uuid,
}

pub(super) struct ValidatedSchema {
  pub document_id: DocumentId,
  pub root_flow_id: FlowId,
}
pub(super) fn configure(doc: &LoroDoc) {
  doc.config_default_text_style(Some(StyleConfig { expand: ExpandType::After }));
  let mut styles = StyleConfigMap::new();
  styles.insert(PARAGRAPH_MARK.into(), StyleConfig { expand: ExpandType::None });
  styles.insert(OBJECT_MARK.into(), StyleConfig { expand: ExpandType::None });
  doc.config_text_style(styles);
}

pub(super) fn initialize(
  doc: &LoroDoc,
  document_id: DocumentId,
  created_by_actor: ActorId,
  root_flow_id: FlowId,
  first_paragraph_id: FlowNodeId,
  paragraph_metadata: &[u8],
) -> CollabResult<()> {
  initialize_from_seed(
    doc,
    document_id,
    created_by_actor,
    &FlowDocumentSeed {
      root_flow_id,
      document_metadata: Vec::new(),
      assets: Vec::new(),
      flows: vec![FlowSeedFlow {
        id: root_flow_id,
        nodes: vec![FlowSeedNode {
          record: FlowNodeRecord {
            id: first_paragraph_id,
            kind: FlowNodeKind::Paragraph,
            metadata: paragraph_metadata.to_vec(),
            child_flows: Vec::new(),
          },
          text: String::new(),
          marks: Vec::new(),
        }],
      }],
    },
  )
}

pub(super) fn initialize_from_seed(
  doc: &LoroDoc,
  document_id: DocumentId,
  created_by_actor: ActorId,
  seed: &FlowDocumentSeed,
) -> CollabResult<()> {
  let root = doc.get_map(ROOT_MAP);
  root.insert(KEY_SCHEMA_VERSION, i64::from(FLOW_SOURCE_SCHEMA_VERSION)).map_err(loro_error)?;
  root.insert(KEY_HISTORY_EPOCH, FLOW_HISTORY_EPOCH as i64).map_err(loro_error)?;
  root.insert(KEY_HISTORY_MODE, HISTORY_MODE_FULL).map_err(loro_error)?;
  root.insert(KEY_DOCUMENT_ID, document_id.0.as_bytes()).map_err(loro_error)?;
  root.insert(KEY_CREATED_BY_ACTOR, created_by_actor.0.as_bytes()).map_err(loro_error)?;
  root.insert(KEY_ROOT_FLOW_ID, seed.root_flow_id.0.as_bytes()).map_err(loro_error)?;
  root.insert(KEY_DOCUMENT_METADATA, seed.document_metadata.as_slice()).map_err(loro_error)?;
  root.insert_container(KEY_FLOWS, LoroMap::new()).map_err(loro_error)?;
  root.insert_container(KEY_NODES, LoroMap::new()).map_err(loro_error)?;
  root.insert_container(KEY_ASSETS, LoroMap::new()).map_err(loro_error)?;
  for asset in &seed.assets {
    put_asset_reference(doc, asset)?;
  }
  for flow in &seed.flows {
    create_seed_flow(doc, flow)?;
  }
  doc.commit();
  Ok(())
}

fn create_seed_flow(doc: &LoroDoc, seed: &FlowSeedFlow) -> CollabResult<()> {
  ensure_flow_absent(doc, seed.id)?;
  if seed.nodes.is_empty() {
    return Err(CollabError::InvalidSchema("vNext seed flow has no nodes"));
  }
  let flow = flows_map(doc)?
    .insert_container(&flow_key(seed.id), LoroMap::new())
    .map_err(loro_error)?;
  let text = flow.insert_container(KEY_FLOW_CONTENT, LoroText::new()).map_err(loro_error)?;
  let mut unicode_pos = 0;
  let mut utf8_pos = 0;
  for node in &seed.nodes {
    ensure_node_absent(doc, node.record.id)?;
    validate_user_text(&node.text)?;
    if node.record.kind == FlowNodeKind::Object && !node.text.is_empty() {
      return Err(CollabError::InvalidSchema("vNext seed object has text"));
    }
    create_node_record(
      doc,
      seed.id,
      node.record.kind,
      node.record.id,
      &node.record.metadata,
      &node.record.child_flows,
    )?;
    insert_token(&text, unicode_pos, node.record.kind, node.record.id)?;
    unicode_pos += 1;
    utf8_pos += node.record.kind.token().len_utf8();
    if !node.text.is_empty() {
      text.insert(unicode_pos, &node.text).map_err(loro_error)?;
      for mark in &node.marks {
        validate_inline_mark_key(&mark.key)?;
        if mark.range_utf8.start > mark.range_utf8.end
          || mark.range_utf8.end > node.text.len()
          || !node.text.is_char_boundary(mark.range_utf8.start)
          || !node.text.is_char_boundary(mark.range_utf8.end)
        {
          return Err(CollabError::InvalidSchema("vNext seed inline mark range"));
        }
        text
          .mark_utf8(
            utf8_pos + mark.range_utf8.start..utf8_pos + mark.range_utf8.end,
            &mark.key,
            mark.value.clone().into_loro(),
          )
          .map_err(loro_error)?;
      }
      unicode_pos += node.text.chars().count();
      utf8_pos += node.text.len();
    }
  }
  Ok(())
}

pub(super) fn create_flow(doc: &LoroDoc, flow_id: FlowId, first_paragraph_id: FlowNodeId, paragraph_metadata: &[u8]) -> CollabResult<()> {
  ensure_flow_absent(doc, flow_id)?;
  ensure_node_absent(doc, first_paragraph_id)?;
  let flow = flows_map(doc)?
    .insert_container(&flow_key(flow_id), LoroMap::new())
    .map_err(loro_error)?;
  let text = flow.insert_container(KEY_FLOW_CONTENT, LoroText::new()).map_err(loro_error)?;
  create_node_record(doc, flow_id, FlowNodeKind::Paragraph, first_paragraph_id, paragraph_metadata, &[])?;
  insert_token(&text, 0, FlowNodeKind::Paragraph, first_paragraph_id)
}

pub(super) fn insert_structural_node(
  doc: &LoroDoc,
  flow_id: FlowId,
  unicode_pos: usize,
  kind: FlowNodeKind,
  node_id: FlowNodeId,
  metadata: &[u8],
  child_flows: &[FlowId],
) -> CollabResult<()> {
  create_node_record(doc, flow_id, kind, node_id, metadata, child_flows)?;
  insert_token(&flow_text(doc, flow_id)?, unicode_pos, kind, node_id)
}

fn insert_token(text: &LoroText, unicode_pos: usize, kind: FlowNodeKind, node_id: FlowNodeId) -> CollabResult<()> {
  text.insert(unicode_pos, &kind.token().to_string()).map_err(loro_error)?;
  // User marks use Loro's default ExpandType::After behavior so typing at the
  // end of a styled span inherits that style. A structural token inserted at
  // the same boundary must never inherit those marks.
  let inherited_keys = text
    .slice_delta(unicode_pos, unicode_pos + 1, PosType::Unicode)
    .map_err(loro_error)?
    .into_iter()
    .flat_map(|delta| match delta {
      loro::TextDelta::Insert {
        attributes: Some(attributes),
        ..
      } => attributes.keys().cloned().collect::<Vec<_>>(),
      loro::TextDelta::Insert { attributes: None, .. }
      | loro::TextDelta::Retain { .. }
      | loro::TextDelta::Delete { .. } => Vec::new(),
    })
    .filter(|key| !is_structural_key(key))
    .collect::<BTreeSet<_>>();
  for key in inherited_keys {
    text.unmark(unicode_pos..unicode_pos + 1, &key).map_err(loro_error)?;
  }
  text
    .mark(unicode_pos..unicode_pos + 1, kind.mark_key(), node_key(node_id))
    .map_err(loro_error)
}

fn create_node_record(
  doc: &LoroDoc,
  owner_flow: FlowId,
  kind: FlowNodeKind,
  node_id: FlowNodeId,
  metadata: &[u8],
  child_flows: &[FlowId],
) -> CollabResult<()> {
  let record = nodes_map(doc)?
    .insert_container(&node_key(node_id), LoroMap::new())
    .map_err(loro_error)?;
  record.insert(KEY_NODE_KIND, kind.code()).map_err(loro_error)?;
  record.insert(KEY_NODE_OWNER_FLOW, owner_flow.0.as_bytes()).map_err(loro_error)?;
  record.insert(KEY_NODE_METADATA, metadata).map_err(loro_error)?;
  let children = record
    .insert_container(KEY_NODE_CHILD_FLOWS, LoroMovableList::new())
    .map_err(loro_error)?;
  for flow_id in child_flows {
    children.push(flow_key(*flow_id)).map_err(loro_error)?;
  }
  Ok(())
}

pub(super) fn materialize(doc: &LoroDoc, limits: &FlowSourceLimits) -> CollabResult<FlowMaterialization> {
  let validated = validate(doc, None, limits)?;
  let mut flows = BTreeMap::new();
  materialize_reachable(doc, validated.root_flow_id, limits, &mut flows)?;
  Ok(FlowMaterialization {
    document_id: validated.document_id,
    created_by_actor: ActorId(protected_state(doc)?.created_by_actor),
    root_flow_id: validated.root_flow_id,
    document_metadata: document_metadata(doc)?,
    assets: asset_references(doc)?,
    flows,
  })
}

pub(super) fn materialize_flow(doc: &LoroDoc, flow_id: FlowId, limits: &FlowSourceLimits) -> CollabResult<MaterializedFlow> {
  parse_flow(doc, flow_id, limits).map(|parsed| parsed.materialized)
}

fn materialize_reachable(
  doc: &LoroDoc,
  flow_id: FlowId,
  limits: &FlowSourceLimits,
  output: &mut BTreeMap<FlowId, MaterializedFlow>,
) -> CollabResult<()> {
  if output.contains_key(&flow_id) {
    return Ok(());
  }
  let flow = parse_flow(doc, flow_id, limits)?.materialized;
  let children = flow
    .nodes
    .iter()
    .flat_map(|node| node.record().child_flows.iter().copied())
    .collect::<Vec<_>>();
  output.insert(flow_id, flow);
  for child in children {
    materialize_reachable(doc, child, limits, output)?;
  }
  Ok(())
}

pub(super) fn protected_state(doc: &LoroDoc) -> CollabResult<ProtectedState> {
  Ok(ProtectedState {
    schema_version: root_i64(doc, KEY_SCHEMA_VERSION)?,
    history_epoch: root_i64(doc, KEY_HISTORY_EPOCH)?,
    history_mode: root_i64(doc, KEY_HISTORY_MODE)?,
    document_id: root_uuid(doc, KEY_DOCUMENT_ID)?,
    created_by_actor: root_uuid(doc, KEY_CREATED_BY_ACTOR)?,
    root_flow_id: root_uuid(doc, KEY_ROOT_FLOW_ID)?,
  })
}

pub(super) fn history_policy(doc: &LoroDoc) -> CollabResult<FlowHistoryPolicy> {
  let protected = protected_state(doc)?;
  let epoch = u64::try_from(protected.history_epoch).map_err(|_| CollabError::InvalidSchema(KEY_HISTORY_EPOCH))?;
  let mode = match protected.history_mode {
    HISTORY_MODE_FULL => FlowHistoryMode::Full,
    _ => return Err(CollabError::InvalidSchema(KEY_HISTORY_MODE)),
  };
  Ok(FlowHistoryPolicy { epoch, mode })
}

pub(super) fn document_metadata(doc: &LoroDoc) -> CollabResult<Vec<u8>> {
  match root_value(doc, KEY_DOCUMENT_METADATA)? {
    LoroValue::Binary(value) => Ok(value.unwrap()),
    _ => Err(CollabError::InvalidSchema(KEY_DOCUMENT_METADATA)),
  }
}

pub(super) fn created_by_actor(doc: &LoroDoc) -> CollabResult<ActorId> {
  Ok(ActorId(root_uuid(doc, KEY_CREATED_BY_ACTOR)?))
}

pub(super) fn set_document_metadata(doc: &LoroDoc, metadata: &[u8]) -> CollabResult<()> {
  doc
    .try_get_map(ROOT_MAP)
    .ok_or(CollabError::MissingRootValue(ROOT_MAP))?
    .insert(KEY_DOCUMENT_METADATA, metadata)
    .map_err(loro_error)
}

pub(super) fn flow_text(doc: &LoroDoc, flow_id: FlowId) -> CollabResult<LoroText> {
  let flow = match flows_map(doc)?.get(&flow_key(flow_id)) {
    Some(ValueOrContainer::Container(Container::Map(flow))) => flow,
    _ => return Err(CollabError::MissingRootValue("vNext flow")),
  };
  match flow.get(KEY_FLOW_CONTENT) {
    Some(ValueOrContainer::Container(Container::Text(text))) => Ok(text),
    _ => Err(CollabError::InvalidSchema(KEY_FLOW_CONTENT)),
  }
}

pub(super) fn node_record(doc: &LoroDoc, node_id: FlowNodeId) -> CollabResult<LoroMap> {
  match nodes_map(doc)?.get(&node_key(node_id)) {
    Some(ValueOrContainer::Container(Container::Map(record))) => Ok(record),
    _ => Err(CollabError::MissingRootValue("vNext node")),
  }
}

pub(super) fn node_owner_flow(doc: &LoroDoc, node_id: FlowNodeId) -> CollabResult<FlowId> {
  match node_record(doc, node_id)?.get(KEY_NODE_OWNER_FLOW) {
    Some(ValueOrContainer::Value(LoroValue::Binary(value))) => Ok(FlowId(
      Uuid::from_slice(&value.unwrap()).map_err(|_| CollabError::InvalidSchema(KEY_NODE_OWNER_FLOW))?,
    )),
    _ => Err(CollabError::InvalidSchema(KEY_NODE_OWNER_FLOW)),
  }
}

pub(super) fn node_token_position(doc: &LoroDoc, node_id: FlowNodeId) -> CollabResult<(FlowId, usize)> {
  let flow_id = node_owner_flow(doc, node_id)?;
  let parsed = parse_flow(doc, flow_id, &FlowSourceLimits::default())?;
  parsed
    .parsed_nodes
    .iter()
    .find(|node| node.node.record().id == node_id)
    .map(|node| (flow_id, node.token_unicode))
    .ok_or(CollabError::InvalidSchema("vNext node is not owned by its recorded flow"))
}

pub(super) fn node_token_cursors(doc: &LoroDoc) -> CollabResult<HashMap<FlowNodeId, (FlowId, Cursor)>> {
  let mut cursors = HashMap::new();
  for flow_id in flow_ids(doc)? {
    let text = flow_text(doc, flow_id)?;
    for node in parse_flow(doc, flow_id, &FlowSourceLimits::default())?.parsed_nodes {
      let cursor = cursor_at_unicode(&text, node.token_unicode, Side::Middle)?;
      cursors.insert(node.node.record().id, (flow_id, cursor));
    }
  }
  Ok(cursors)
}

pub(super) fn node_token_cursors_in_range(
  doc: &LoroDoc,
  flow_id: FlowId,
  changed_unicode: Range<usize>,
) -> CollabResult<Vec<(FlowNodeId, Cursor)>> {
  let text = flow_text(doc, flow_id)?;
  let start = changed_unicode.start.saturating_sub(1).min(text.len_unicode());
  let end = changed_unicode.end.saturating_add(1).min(text.len_unicode());
  let mut cursors = Vec::new();
  for unicode in start..end {
    if matches!(text.char_at(unicode).map_err(loro_error)?, PARAGRAPH_TOKEN | OBJECT_TOKEN) {
      cursors.push((token_node_id_at(&text, unicode)?, cursor_at_unicode(&text, unicode, Side::Middle)?));
    }
  }
  Ok(cursors)
}

pub(super) fn find_node_token_cursor(doc: &LoroDoc, node_id: FlowNodeId) -> CollabResult<(FlowId, Cursor, usize)> {
  let (flow_id, unicode) = node_token_position(doc, node_id)?;
  let cursor = cursor_at_unicode(&flow_text(doc, flow_id)?, unicode, Side::Middle)?;
  Ok((flow_id, cursor, unicode))
}

pub(super) fn resolve_node_token_cursor(
  doc: &LoroDoc,
  node_id: FlowNodeId,
  flow_id: FlowId,
  cursor: &Cursor,
) -> CollabResult<usize> {
  let text = flow_text(doc, flow_id)?;
  if cursor.container != text.id() {
    return Err(CollabError::InvalidSchema("vNext derived structural cursor flow mismatch"));
  }
  let (unicode, _) = resolve_cursor_unicode(doc, &text, cursor, "unresolvable vNext derived structural cursor")?;
  if token_node_id_at(&text, unicode)? != node_id {
    return Err(CollabError::InvalidSchema("vNext derived structural cursor target mismatch"));
  }
  Ok(unicode)
}

pub(super) fn token_node_id_at(text: &LoroText, unicode: usize) -> CollabResult<FlowNodeId> {
  let delta = text
    .slice_delta(unicode, unicode + 1, PosType::Unicode)
    .map_err(loro_error)?;
  for item in delta {
    let loro::TextDelta::Insert {
      insert,
      attributes: Some(attributes),
    } = item
    else {
      continue;
    };
    let Some(kind) = insert.chars().next().and_then(|ch| match ch {
      PARAGRAPH_TOKEN => Some(FlowNodeKind::Paragraph),
      OBJECT_TOKEN => Some(FlowNodeKind::Object),
      _ => None,
    }) else {
      continue;
    };
    let Some(value) = attributes.get(kind.mark_key()).and_then(FlowMarkValue::from_loro) else {
      return Err(CollabError::InvalidSchema("vNext structural cursor token mark"));
    };
    let FlowMarkValue::String(id) = value else {
      return Err(CollabError::InvalidSchema("vNext structural cursor token ID"));
    };
    return parse_node_id(&id);
  }
  Err(CollabError::InvalidSchema("vNext structural cursor does not target a token"))
}

pub(super) fn read_node_record(doc: &LoroDoc, node_id: FlowNodeId) -> CollabResult<FlowNodeRecord> {
  let record = node_record(doc, node_id)?;
  let kind = match record.get(KEY_NODE_KIND) {
    Some(ValueOrContainer::Value(LoroValue::I64(code))) => FlowNodeKind::from_code(code)?,
    _ => return Err(CollabError::InvalidSchema(KEY_NODE_KIND)),
  };
  let metadata = map_binary(&record, KEY_NODE_METADATA)?;
  let child_flows = match record.get(KEY_NODE_CHILD_FLOWS) {
    Some(ValueOrContainer::Container(Container::MovableList(children))) => children
      .to_vec()
      .into_iter()
      .map(|value| parse_flow_id(&loro_string(value)?))
      .collect::<CollabResult<Vec<_>>>()?,
    _ => return Err(CollabError::InvalidSchema(KEY_NODE_CHILD_FLOWS)),
  };
  let mut unique = HashSet::new();
  if !child_flows.iter().all(|id| unique.insert(*id)) {
    return Err(CollabError::InvalidSchema("duplicate vNext child flow"));
  }
  Ok(FlowNodeRecord {
    id: node_id,
    kind,
    metadata,
    child_flows,
  })
}

pub(super) fn anchor_at_utf8(doc: &LoroDoc, flow_id: FlowId, byte_offset: usize, side: Side) -> CollabResult<AnchoredPosition> {
  let text = flow_text(doc, flow_id)?;
  let unicode = text
    .convert_pos(byte_offset, PosType::Bytes, PosType::Unicode)
    .ok_or(CollabError::InvalidSchema("vNext UTF-8 position"))?;
  anchor_at_unicode(doc, flow_id, unicode, side)
}

pub(super) fn anchor_at_unicode(doc: &LoroDoc, flow_id: FlowId, unicode: usize, side: Side) -> CollabResult<AnchoredPosition> {
  let text = flow_text(doc, flow_id)?;
  if unicode > text.len_unicode() {
    return Err(CollabError::InvalidSchema("vNext Unicode position"));
  }
  let cursor = cursor_at_unicode(&text, unicode, side)?;
  let fallback_node_id = paragraph_at_unicode(doc, flow_id, unicode).ok();
  Ok(AnchoredPosition {
    flow_id,
    cursor,
    fallback_node_id,
  })
}

pub(super) fn anchor_in_paragraph_at_token_utf8(
  doc: &LoroDoc,
  paragraph_id: FlowNodeId,
  flow_id: FlowId,
  token: usize,
  byte_offset: usize,
  side: Side,
) -> CollabResult<AnchoredPosition> {
  let range = paragraph_content_range_at(doc, paragraph_id, flow_id, token)?;
  let text = flow_text(doc, flow_id)?;
  let paragraph = text.slice(range.start, range.end).map_err(loro_error)?;
  if byte_offset > paragraph.len() || !paragraph.is_char_boundary(byte_offset) {
    return Err(CollabError::InvalidSchema("vNext paragraph UTF-8 offset"));
  }
  let local_unicode = utf8_to_unicode(&paragraph, byte_offset)?;
  anchor_at_unicode(doc, flow_id, range.start + local_unicode, side)
}

pub(super) fn resolve_anchor_unicode(doc: &LoroDoc, position: &AnchoredPosition) -> CollabResult<usize> {
  let text = flow_text(doc, position.flow_id)?;
  if position.cursor.container != text.id() {
    return Err(CollabError::InvalidSchema("vNext cursor flow mismatch"));
  }
  resolve_cursor_unicode(doc, &text, &position.cursor, "unresolvable vNext cursor").map(|(unicode, _)| unicode)
}

pub(super) fn resolve_anchor_utf8(doc: &LoroDoc, position: &AnchoredPosition) -> CollabResult<usize> {
  let unicode = resolve_anchor_unicode(doc, position)?;
  flow_text(doc, position.flow_id)?
    .convert_pos(unicode, PosType::Unicode, PosType::Bytes)
    .ok_or(CollabError::InvalidSchema("vNext Unicode position"))
}

pub(super) fn resolve_anchor_in_paragraph_utf8(doc: &LoroDoc, position: &AnchoredPosition) -> CollabResult<ResolvedFlowPosition> {
  let text = flow_text(doc, position.flow_id)?;
  if position.cursor.container != text.id() {
    return Err(CollabError::InvalidSchema("vNext cursor flow mismatch"));
  }
  let (unicode, side) = resolve_cursor_unicode(doc, &text, &position.cursor, "unresolvable vNext cursor")?;
  let next_paragraph = (side == Side::Right
    && unicode < text.len_unicode()
    && token_node_id_at(&text, unicode)
      .ok()
      .is_some_and(|node_id| read_node_record(doc, node_id).is_ok_and(|record| record.kind == FlowNodeKind::Paragraph)))
  .then(|| token_node_id_at(&text, unicode))
  .transpose()?;
  let resolved_paragraph = if let Some(node_id) = next_paragraph {
    paragraph_content_range_at(doc, node_id, position.flow_id, unicode).map(|range| (node_id, range))
  } else {
    paragraph_at_unicode_with_range(doc, position.flow_id, unicode)
  };
  if let Ok((node_id, range)) = resolved_paragraph {
    let local_unicode = unicode.saturating_sub(range.start).min(range.len());
    let paragraph = text.slice(range.start, range.end).map_err(loro_error)?;
    return Ok(ResolvedFlowPosition {
      flow_id: position.flow_id,
      node_id,
      byte_offset: unicode_to_utf8(&paragraph, local_unicode)?,
    });
  }
  if let Some(fallback) = position.fallback_node_id {
    let (flow_id, range) = paragraph_content_range(doc, fallback)?;
    if flow_id == position.flow_id {
      return Ok(ResolvedFlowPosition {
        flow_id,
        node_id: fallback,
        byte_offset: text.slice(range.start, range.end).map_err(loro_error)?.len(),
      });
    }
  }
  Err(CollabError::InvalidSchema("vNext cursor no longer resolves into a paragraph"))
}

fn cursor_at_unicode(text: &LoroText, unicode: usize, side: Side) -> CollabResult<Cursor> {
  let event = text
    .convert_pos(unicode, PosType::Unicode, PosType::Event)
    .ok_or(CollabError::InvalidSchema("vNext Unicode cursor position"))?;
  text
    .get_cursor(event, side)
    .ok_or(CollabError::InvalidSchema("vNext cursor position"))
}

fn resolve_cursor_unicode(doc: &LoroDoc, text: &LoroText, cursor: &Cursor, error: &'static str) -> CollabResult<(usize, Side)> {
  let resolved = doc.get_cursor_pos(cursor).map_err(|_| CollabError::InvalidSchema(error))?;
  let unicode = text
    .convert_pos(resolved.current.pos, PosType::Event, PosType::Unicode)
    .ok_or(CollabError::InvalidSchema(error))?;
  Ok((unicode, resolved.current.side))
}

pub(super) fn validate_user_text(text: &str) -> CollabResult<()> {
  if text.contains(PARAGRAPH_TOKEN) || text.contains(OBJECT_TOKEN) {
    Err(CollabError::InvalidSchema("user text contains reserved vNext token"))
  } else {
    Ok(())
  }
}

pub(super) fn validate_inline_mark_key(key: &str) -> CollabResult<()> {
  if is_structural_key(key) || key.starts_with("flowstate_protected_") {
    Err(CollabError::Unauthorized("reserved vNext text mark"))
  } else {
    Ok(())
  }
}

pub(super) fn validate_plain_text_insert(doc: &LoroDoc, flow_id: FlowId, unicode_pos: usize) -> CollabResult<()> {
  paragraph_at_unicode(doc, flow_id, unicode_pos).map(|_| ())
}

pub(super) fn validate_plain_text_delete(doc: &LoroDoc, flow_id: FlowId, range: Range<usize>) -> CollabResult<()> {
  if range.start > range.end {
    return Err(CollabError::InvalidSchema("vNext text range"));
  }
  let (_, paragraph) = paragraph_at_unicode_with_range(doc, flow_id, range.start)?;
  if range.end <= paragraph.end {
    Ok(())
  } else {
    Err(CollabError::InvalidSchema("vNext text range crosses structural token"))
  }
}

pub(super) fn validate_document_delete(doc: &LoroDoc, flow_id: FlowId, range: Range<usize>) -> CollabResult<()> {
  if range.start > range.end {
    return Err(CollabError::InvalidSchema("vNext document range"));
  }
  paragraph_at_unicode(doc, flow_id, range.start)?;
  paragraph_at_unicode(doc, flow_id, range.end)?;
  Ok(())
}

pub(super) fn validate_block_insert(doc: &LoroDoc, flow_id: FlowId, unicode_pos: usize) -> CollabResult<()> {
  let text = flow_text(doc, flow_id)?;
  let at_token = unicode_pos < text.len_unicode()
    && matches!(text.char_at(unicode_pos).map_err(loro_error)?, PARAGRAPH_TOKEN | OBJECT_TOKEN);
  let at_previous_node_end = unicode_pos
    .checked_sub(1)
    .and_then(|probe| previous_structural_position(&text, probe).ok())
    .and_then(|token| token_node_id_at(&text, token).ok().map(|node_id| (token, node_id)))
    .and_then(|(token, node_id)| read_node_record(doc, node_id).ok().map(|record| (token, node_id, record)))
    .is_some_and(|(token, node_id, record)| match record.kind {
      FlowNodeKind::Object => token + 1 == unicode_pos,
      FlowNodeKind::Paragraph => {
        paragraph_content_range_at(doc, node_id, flow_id, token).is_ok_and(|range| range.end == unicode_pos)
      },
    });
  if at_token || at_previous_node_end {
    Ok(())
  } else {
    Err(CollabError::InvalidSchema("vNext object insertion is not at a block boundary"))
  }
}

fn paragraph_content_range(doc: &LoroDoc, paragraph_id: FlowNodeId) -> CollabResult<(FlowId, Range<usize>)> {
  let (flow_id, token) = node_token_position(doc, paragraph_id)?;
  Ok((flow_id, paragraph_content_range_at(doc, paragraph_id, flow_id, token)?))
}

fn paragraph_content_range_at(doc: &LoroDoc, paragraph_id: FlowNodeId, flow_id: FlowId, token: usize) -> CollabResult<Range<usize>> {
  let record = read_node_record(doc, paragraph_id)?;
  if record.kind != FlowNodeKind::Paragraph || node_owner_flow(doc, paragraph_id)? != flow_id {
    return Err(CollabError::InvalidSchema("vNext paragraph target is object or belongs to another flow"));
  }
  let text = flow_text(doc, flow_id)?;
  if token_node_id_at(&text, token)? != paragraph_id {
    return Err(CollabError::InvalidSchema("vNext paragraph token position mismatch"));
  }
  let start = token + 1;
  let end = next_structural_position(&text, start)?.unwrap_or_else(|| text.len_unicode());
  Ok(start..end)
}

fn previous_structural_position(text: &LoroText, mut unicode: usize) -> CollabResult<usize> {
  loop {
    if matches!(text.char_at(unicode).map_err(loro_error)?, PARAGRAPH_TOKEN | OBJECT_TOKEN) {
      return Ok(unicode);
    }
    if unicode == 0 {
      return Err(CollabError::InvalidSchema("vNext flow does not begin with structural token"));
    }
    unicode -= 1;
  }
}

fn next_structural_position(text: &LoroText, mut unicode: usize) -> CollabResult<Option<usize>> {
  while unicode < text.len_unicode() {
    if matches!(text.char_at(unicode).map_err(loro_error)?, PARAGRAPH_TOKEN | OBJECT_TOKEN) {
      return Ok(Some(unicode));
    }
    unicode += 1;
  }
  Ok(None)
}

pub(super) fn paragraph_at_unicode(doc: &LoroDoc, flow_id: FlowId, unicode_pos: usize) -> CollabResult<FlowNodeId> {
  paragraph_at_unicode_with_range(doc, flow_id, unicode_pos).map(|(node_id, _)| node_id)
}

fn paragraph_at_unicode_with_range(doc: &LoroDoc, flow_id: FlowId, unicode_pos: usize) -> CollabResult<(FlowNodeId, Range<usize>)> {
  let text = flow_text(doc, flow_id)?;
  let len = text.len_unicode();
  if len == 0 || unicode_pos > len {
    return Err(CollabError::InvalidSchema("vNext position is not inside paragraph text"));
  }
  let probe = if unicode_pos == len
    || (unicode_pos > 0
      && matches!(text.char_at(unicode_pos).map_err(loro_error)?, PARAGRAPH_TOKEN | OBJECT_TOKEN))
  {
    unicode_pos.saturating_sub(1)
  } else {
    unicode_pos
  };
  let token = previous_structural_position(&text, probe)?;
  let node_id = token_node_id_at(&text, token)?;
  let record = read_node_record(doc, node_id)?;
  if record.kind != FlowNodeKind::Paragraph {
    return Err(CollabError::InvalidSchema("vNext position is not inside paragraph text"));
  }
  let range = paragraph_content_range_at(doc, node_id, flow_id, token)?;
  (range.start <= unicode_pos && unicode_pos <= range.end)
    .then_some((node_id, range))
    .ok_or(CollabError::InvalidSchema("vNext position is not inside paragraph text"))
}

pub(super) fn join_target(doc: &LoroDoc, second_paragraph_id: FlowNodeId) -> CollabResult<(FlowId, usize, FlowNodeId)> {
  let (flow_id, token) = node_token_position(doc, second_paragraph_id)?;
  join_target_at(doc, second_paragraph_id, flow_id, token)
}

pub(super) fn join_target_at(
  doc: &LoroDoc,
  second_paragraph_id: FlowNodeId,
  flow_id: FlowId,
  token: usize,
) -> CollabResult<(FlowId, usize, FlowNodeId)> {
  let record = read_node_record(doc, second_paragraph_id)?;
  if record.kind != FlowNodeKind::Paragraph {
    return Err(CollabError::InvalidSchema("vNext join target is not paragraph"));
  }
  if node_owner_flow(doc, second_paragraph_id)? != flow_id || token_node_id_at(&flow_text(doc, flow_id)?, token)? != second_paragraph_id {
    return Err(CollabError::InvalidSchema("vNext join target position mismatch"));
  }
  let previous_probe = token
    .checked_sub(1)
    .ok_or(CollabError::InvalidSchema("vNext join target has no predecessor"))?;
  let text = flow_text(doc, flow_id)?;
  let previous_token = previous_structural_position(&text, previous_probe)?;
  let first_paragraph_id = token_node_id_at(&text, previous_token)?;
  if read_node_record(doc, first_paragraph_id)?.kind != FlowNodeKind::Paragraph {
    return Err(CollabError::InvalidSchema("vNext join target predecessor is not paragraph"));
  }
  Ok((flow_id, token, first_paragraph_id))
}

pub(super) fn ensure_flow_absent(doc: &LoroDoc, flow_id: FlowId) -> CollabResult<()> {
  if flows_map(doc)?.get(&flow_key(flow_id)).is_none() {
    Ok(())
  } else {
    Err(CollabError::InvalidSchema("duplicate vNext flow ID"))
  }
}

pub(super) fn ensure_node_absent(doc: &LoroDoc, node_id: FlowNodeId) -> CollabResult<()> {
  if nodes_map(doc)?.get(&node_key(node_id)).is_none() {
    Ok(())
  } else {
    Err(CollabError::InvalidSchema("duplicate vNext node ID"))
  }
}

pub(super) fn put_asset_reference(doc: &LoroDoc, asset: &FlowAssetReference) -> CollabResult<()> {
  if asset.mime_type.len() > FlowSourceLimits::default().max_asset_reference_bytes
    || asset.original_name.as_ref().is_some_and(|name| name.len() > FlowSourceLimits::default().max_asset_reference_bytes)
  {
    return Err(CollabError::InvalidSchema("vNext asset reference field limit"));
  }
  let bytes = postcard::to_stdvec(asset)?;
  assets_map(doc)?
    .insert(&asset_key(asset.id), bytes.as_slice())
    .map_err(loro_error)
}

pub(super) fn asset_references(doc: &LoroDoc) -> CollabResult<BTreeMap<FlowAssetId, FlowAssetReference>> {
  assets_map(doc)?
    .keys()
    .map(|key| {
      let id = parse_asset_id(&key)?;
      let bytes = match assets_map(doc)?.get(&key) {
        Some(ValueOrContainer::Value(LoroValue::Binary(value))) => value.unwrap(),
        _ => return Err(CollabError::InvalidSchema("vNext asset reference")),
      };
      let asset: FlowAssetReference = postcard::from_bytes(&bytes)?;
      if asset.id != id {
        return Err(CollabError::InvalidSchema("vNext asset reference ID"));
      }
      Ok((id, asset))
    })
    .collect()
}

pub(super) fn summarize_changes(doc: &LoroDoc, before: &VersionVector) -> CollabResult<FlowChangeSummary> {
  let old_frontiers = doc.vv_to_frontiers(before);
  let current_frontiers = doc.vv_to_frontiers(&doc.oplog_vv());
  let diff = doc.diff(&old_frontiers, &current_frontiers).map_err(loro_error)?;
  let mut summary = FlowChangeSummary::default();
  for (container_id, change) in diff.iter() {
    let Some(path) = doc.get_path_to_container(container_id) else {
      summary.touched_flows.insert(root_flow_id(doc)?);
      continue;
    };
    let keys = path
      .iter()
      .filter_map(|(_, index)| match index {
        Index::Key(key) => Some(key.to_string()),
        Index::Seq(_) | Index::Node(_) => None,
      })
      .collect::<Vec<_>>();
    collect_path_id(&keys, KEY_FLOWS, |value| parse_flow_id(value).ok(), &mut summary.touched_flows);
    collect_path_id(&keys, KEY_NODES, |value| parse_node_id(value).ok(), &mut summary.touched_nodes);
    if let Diff::Text(delta) = change
      && let Some(flow_id) = path_id(&keys, KEY_FLOWS, |value| parse_flow_id(value).ok())
      && let Some(text_change) = summarize_text_delta(delta)
    {
      summary
        .flow_text_changes
        .entry(flow_id)
        .and_modify(|current| merge_text_change(current, &text_change))
        .or_insert(text_change);
    }
    if let Diff::Map(map) = change {
      if keys.last().is_some_and(|key| key == KEY_FLOWS) {
        for key in map.updated.keys() {
          if let Ok(id) = parse_flow_id(key) {
            summary.touched_flows.insert(id);
          }
        }
      }
      if keys.last().is_some_and(|key| key == KEY_NODES) {
        for key in map.updated.keys() {
          if let Ok(id) = parse_node_id(key) {
            summary.touched_nodes.insert(id);
          }
        }
      }
    }
  }
  if summary.touched_flows.is_empty() && summary.touched_nodes.is_empty() {
    summary.touched_flows.insert(root_flow_id(doc)?);
  }
  Ok(summary)
}

pub(super) fn materialize_node_window_at(
  doc: &LoroDoc,
  node_id: FlowNodeId,
  flow_id: FlowId,
  unicode: usize,
) -> CollabResult<MaterializedFlowWindow> {
  if token_node_id_at(&flow_text(doc, flow_id)?, unicode)? != node_id {
    return Err(CollabError::InvalidSchema("vNext node window token position mismatch"));
  }
  materialize_flow_window(doc, flow_id, unicode..unicode + 1)
}

pub(super) fn materialize_flow_window(
  doc: &LoroDoc,
  flow_id: FlowId,
  changed_unicode: Range<usize>,
) -> CollabResult<MaterializedFlowWindow> {
  parsing::materialize_flow_window(doc, flow_id, changed_unicode)
}

fn summarize_text_delta(delta: &[loro::TextDelta]) -> Option<FlowTextChange> {
  let mut before = 0usize;
  let mut after = 0usize;
  let mut before_range: Option<Range<usize>> = None;
  let mut after_range: Option<Range<usize>> = None;
  for item in delta {
    match item {
      loro::TextDelta::Retain { retain, attributes } => {
        if attributes.is_some() {
          extend_range(&mut before_range, before..before + retain);
          extend_range(&mut after_range, after..after + retain);
        }
        before += retain;
        after += retain;
      },
      loro::TextDelta::Insert { insert, .. } => {
        let len = insert.chars().count();
        extend_range(&mut before_range, before..before);
        extend_range(&mut after_range, after..after + len);
        after += len;
      },
      loro::TextDelta::Delete { delete } => {
        extend_range(&mut before_range, before..before + delete);
        extend_range(&mut after_range, after..after);
        before += delete;
      },
    }
  }
  Some(FlowTextChange {
    before_unicode: before_range?,
    after_unicode: after_range?,
  })
}

fn extend_range(current: &mut Option<Range<usize>>, incoming: Range<usize>) {
  match current {
    Some(current) => {
      current.start = current.start.min(incoming.start);
      current.end = current.end.max(incoming.end);
    },
    None => *current = Some(incoming),
  }
}

fn merge_text_change(current: &mut FlowTextChange, incoming: &FlowTextChange) {
  current.before_unicode.start = current.before_unicode.start.min(incoming.before_unicode.start);
  current.before_unicode.end = current.before_unicode.end.max(incoming.before_unicode.end);
  current.after_unicode.start = current.after_unicode.start.min(incoming.after_unicode.start);
  current.after_unicode.end = current.after_unicode.end.max(incoming.after_unicode.end);
}

fn path_id<T>(keys: &[String], parent: &str, parse: impl Fn(&str) -> Option<T>) -> Option<T> {
  keys.windows(2).find_map(|pair| (pair[0] == parent).then(|| parse(&pair[1])).flatten())
}

fn collect_path_id<T: Ord + Copy>(
  keys: &[String],
  parent: &str,
  parse: impl Fn(&str) -> Option<T>,
  output: &mut BTreeSet<T>,
) {
  for pair in keys.windows(2) {
    if pair[0] == parent
      && let Some(id) = parse(&pair[1])
    {
      output.insert(id);
    }
  }
}

fn flows_map(doc: &LoroDoc) -> CollabResult<LoroMap> {
  root_container_map(doc, KEY_FLOWS)
}

fn nodes_map(doc: &LoroDoc) -> CollabResult<LoroMap> {
  root_container_map(doc, KEY_NODES)
}

fn assets_map(doc: &LoroDoc) -> CollabResult<LoroMap> {
  root_container_map(doc, KEY_ASSETS)
}

fn root_container_map(doc: &LoroDoc, key: &'static str) -> CollabResult<LoroMap> {
  let root = doc.try_get_map(ROOT_MAP).ok_or(CollabError::MissingRootValue(ROOT_MAP))?;
  match root.get(key) {
    Some(ValueOrContainer::Container(Container::Map(map))) => Ok(map),
    _ => Err(CollabError::MissingRootValue(key)),
  }
}

fn root_value(doc: &LoroDoc, key: &'static str) -> CollabResult<LoroValue> {
  let root = doc.try_get_map(ROOT_MAP).ok_or(CollabError::MissingRootValue(ROOT_MAP))?;
  match root.get(key) {
    Some(ValueOrContainer::Value(value)) => Ok(value),
    _ => Err(CollabError::MissingRootValue(key)),
  }
}

fn root_i64(doc: &LoroDoc, key: &'static str) -> CollabResult<i64> {
  match root_value(doc, key)? {
    LoroValue::I64(value) => Ok(value),
    _ => Err(CollabError::InvalidSchema(key)),
  }
}

fn root_uuid(doc: &LoroDoc, key: &'static str) -> CollabResult<Uuid> {
  let bytes = match root_value(doc, key)? {
    LoroValue::Binary(value) => value.unwrap(),
    _ => return Err(CollabError::InvalidSchema(key)),
  };
  Uuid::from_slice(&bytes).map_err(|_| CollabError::InvalidSchema(key))
}

fn root_flow_id(doc: &LoroDoc) -> CollabResult<FlowId> {
  root_uuid(doc, KEY_ROOT_FLOW_ID).map(FlowId)
}

fn map_binary(map: &LoroMap, key: &'static str) -> CollabResult<Vec<u8>> {
  match map.get(key) {
    Some(ValueOrContainer::Value(LoroValue::Binary(value))) => Ok(value.unwrap()),
    _ => Err(CollabError::InvalidSchema(key)),
  }
}

fn flow_ids(doc: &LoroDoc) -> CollabResult<BTreeSet<FlowId>> {
  flows_map(doc)?
    .keys()
    .map(|key| parse_flow_id(&key))
    .collect()
}

fn node_ids(doc: &LoroDoc) -> CollabResult<BTreeSet<FlowNodeId>> {
  nodes_map(doc)?
    .keys()
    .map(|key| parse_node_id(&key))
    .collect()
}

fn flow_key(id: FlowId) -> String {
  id.0.simple().to_string()
}

fn node_key(id: FlowNodeId) -> String {
  id.0.simple().to_string()
}

fn asset_key(id: FlowAssetId) -> String {
  id.0.simple().to_string()
}

fn parse_flow_id(value: &str) -> CollabResult<FlowId> {
  Uuid::parse_str(value)
    .map(FlowId)
    .map_err(|_| CollabError::InvalidSchema("vNext flow ID"))
}

fn parse_node_id(value: &str) -> CollabResult<FlowNodeId> {
  Uuid::parse_str(value)
    .map(FlowNodeId)
    .map_err(|_| CollabError::InvalidSchema("vNext node ID"))
}

fn parse_asset_id(value: &str) -> CollabResult<FlowAssetId> {
  Uuid::parse_str(value)
    .map(FlowAssetId)
    .map_err(|_| CollabError::InvalidSchema("vNext asset ID"))
}

fn loro_string(value: LoroValue) -> CollabResult<String> {
  match value {
    LoroValue::String(value) => Ok(value.to_string()),
    _ => Err(CollabError::InvalidSchema("vNext string")),
  }
}

fn is_structural_key(key: &str) -> bool {
  matches!(key, PARAGRAPH_MARK | OBJECT_MARK)
}

fn utf8_to_unicode(text: &str, byte_offset: usize) -> CollabResult<usize> {
  if byte_offset > text.len() || !text.is_char_boundary(byte_offset) {
    return Err(CollabError::InvalidSchema("vNext UTF-8 offset"));
  }
  Ok(text[..byte_offset].chars().count())
}

fn unicode_to_utf8(text: &str, unicode_offset: usize) -> CollabResult<usize> {
  if unicode_offset == text.chars().count() {
    return Ok(text.len());
  }
  text
    .char_indices()
    .nth(unicode_offset)
    .map(|(offset, _)| offset)
    .ok_or(CollabError::InvalidSchema("vNext Unicode offset"))
}

fn loro_error(error: impl std::fmt::Display) -> CollabError {
  CollabError::Loro(error.to_string())
}
