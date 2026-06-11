use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::ops::Range;
use std::sync::RwLock;
use std::time::{Duration, Instant};

use loro::cursor::{Cursor, Side};
use loro::{CommitOptions, ExportMode, LoroDoc, PeerID, VersionVector};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ActorId, CollabError, CollabResult, DocumentId, ReplicaId, Role};

mod schema;
mod undo;
#[cfg(test)]
mod performance_tests;
#[cfg(test)]
mod transaction_tests;

pub use schema::{
  FLOW_HISTORY_EPOCH, FLOW_SOURCE_SCHEMA_VERSION, FlowAssetId, FlowAssetReference, FlowDocumentSeed, FlowHistoryMode, FlowHistoryPolicy,
  FlowInlineMark, FlowMarkValue, FlowMaterialization, FlowNode, FlowNodeKind, FlowNodeRecord, FlowSeedFlow, FlowSeedNode, FlowSourceLimits,
  MaterializedFlow, MaterializedObjectGraph,
};
pub use undo::FlowUndoManager;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct FlowId(pub Uuid);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Ord, PartialOrd, Serialize, Deserialize)]
pub struct FlowNodeId(pub Uuid);

impl FlowId {
  #[must_use]
  pub fn new() -> Self {
    Self(Uuid::new_v4())
  }
}

impl Default for FlowId {
  fn default() -> Self {
    Self::new()
  }
}

impl FlowNodeId {
  #[must_use]
  pub fn new() -> Self {
    Self(Uuid::new_v4())
  }
}

impl Default for FlowNodeId {
  fn default() -> Self {
    Self::new()
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum FlowUpdateOrigin {
  LocalInput,
  Undo,
  Redo,
  Migration,
  Recovery,
  Host,
}

impl FlowUpdateOrigin {
  const fn as_str(self) -> &'static str {
    match self {
      Self::LocalInput => "flowstate:local-input",
      Self::Undo => "flowstate:undo",
      Self::Redo => "flowstate:redo",
      Self::Migration => "flowstate:migration",
      Self::Recovery => "flowstate:recovery",
      Self::Host => "flowstate:host",
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AnchoredPosition {
  pub flow_id: FlowId,
  pub cursor: Cursor,
  pub fallback_node_id: Option<FlowNodeId>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AnchoredSelection {
  pub anchor: AnchoredPosition,
  pub head: AnchoredPosition,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ResolvedFlowPosition {
  pub flow_id: FlowId,
  pub node_id: FlowNodeId,
  pub byte_offset: usize,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct FlowChangeSummary {
  pub touched_flows: BTreeSet<FlowId>,
  pub touched_nodes: BTreeSet<FlowNodeId>,
  pub flow_text_changes: BTreeMap<FlowId, FlowTextChange>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlowTextChange {
  pub before_unicode: Range<usize>,
  pub after_unicode: Range<usize>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct MaterializedFlowWindow {
  pub id: FlowId,
  pub unicode_range: Range<usize>,
  pub nodes: Vec<FlowNode>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlowTextInsert {
  pub text: String,
  pub marks: Vec<(String, FlowMarkValue)>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlowParagraphInsert {
  pub paragraph_id: FlowNodeId,
  pub metadata: Vec<u8>,
  pub runs: Vec<FlowTextInsert>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlowCommit {
  pub update: Vec<u8>,
  pub base_frontier: Vec<u8>,
  pub resulting_frontier: Vec<u8>,
  pub changes: FlowChangeSummary,
}

#[derive(Clone, Debug)]
pub enum FlowEdit {
  InsertText {
    at: AnchoredPosition,
    text: String,
    marks: Vec<(String, FlowMarkValue)>,
  },
  InsertParagraphFragment {
    at: AnchoredPosition,
    first_runs: Vec<FlowTextInsert>,
    additional_paragraphs: Vec<FlowParagraphInsert>,
  },
  DeleteDocumentRange {
    start: AnchoredPosition,
    end: AnchoredPosition,
  },
  SplitParagraph {
    at: AnchoredPosition,
    new_paragraph_id: FlowNodeId,
    metadata: Vec<u8>,
  },
  JoinParagraph {
    second_paragraph_id: FlowNodeId,
  },
  SetNodeMetadata {
    node_id: FlowNodeId,
    metadata: Vec<u8>,
  },
  SetTextMarks {
    start: AnchoredPosition,
    end: AnchoredPosition,
    clear_keys: Vec<String>,
    marks: Vec<(String, FlowMarkValue)>,
  },
  ReplaceParagraphText {
    paragraph_id: FlowNodeId,
    text: String,
    marks: Vec<(String, FlowMarkValue)>,
  },
  PutAssetReference {
    asset: FlowAssetReference,
  },
  InsertObject {
    at: AnchoredPosition,
    object: FlowSeedNode,
    child_flows: Vec<FlowSeedFlow>,
  },
  DeleteObject {
    object_id: FlowNodeId,
  },
}

#[derive(Clone, Debug)]
pub struct FlowImportPolicy {
  pub role: Role,
  pub allowed_peer_ids: BTreeSet<PeerID>,
  pub allow_protected_mutation: bool,
  pub limits: FlowSourceLimits,
}

impl FlowImportPolicy {
  #[must_use]
  pub fn from_peer(role: Role, peer_id: PeerID) -> Self {
    Self {
      role,
      allowed_peer_ids: BTreeSet::from([peer_id]),
      allow_protected_mutation: false,
      limits: FlowSourceLimits::default(),
    }
  }

  #[must_use]
  pub fn editor_from_peer(peer_id: PeerID) -> Self {
    Self::from_peer(Role::Editor, peer_id)
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct FlowImportOutcome {
  pub frontier: Vec<u8>,
  pub changes: FlowChangeSummary,
}

#[derive(Debug)]
pub struct FlowDocument {
  doc: LoroDoc,
  document_id: DocumentId,
  root_flow_id: FlowId,
  replica_id: ReplicaId,
  node_tokens: RwLock<HashMap<FlowNodeId, (FlowId, Cursor)>>,
}

impl FlowDocument {
  pub fn new(document_id: DocumentId, created_by_actor: ActorId, replica_id: ReplicaId, paragraph_metadata: &[u8]) -> CollabResult<Self> {
    let doc = LoroDoc::new();
    schema::configure(&doc);
    set_replica_peer_id(&doc, replica_id)?;
    let root_flow_id = FlowId::new();
    let first_paragraph_id = FlowNodeId::new();
    schema::initialize(&doc, document_id, created_by_actor, root_flow_id, first_paragraph_id, paragraph_metadata)?;
    schema::validate(&doc, Some(document_id), &FlowSourceLimits::default())?;
    let node_tokens = RwLock::new(schema::node_token_cursors(&doc)?);
    Ok(Self {
      doc,
      document_id,
      root_flow_id,
      replica_id,
      node_tokens,
    })
  }

  pub fn from_seed(document_id: DocumentId, created_by_actor: ActorId, replica_id: ReplicaId, seed: &FlowDocumentSeed) -> CollabResult<Self> {
    let doc = LoroDoc::new();
    schema::configure(&doc);
    set_replica_peer_id(&doc, replica_id)?;
    schema::initialize_from_seed(&doc, document_id, created_by_actor, seed)?;
    let validated = schema::validate(&doc, Some(document_id), &FlowSourceLimits::default())?;
    let node_tokens = RwLock::new(schema::node_token_cursors(&doc)?);
    Ok(Self {
      doc,
      document_id,
      root_flow_id: validated.root_flow_id,
      replica_id,
      node_tokens,
    })
  }

  pub fn from_snapshot(snapshot: &[u8], expected_document_id: Option<DocumentId>, replica_id: ReplicaId) -> CollabResult<Self> {
    let doc = LoroDoc::from_snapshot(snapshot).map_err(loro_error)?;
    schema::configure(&doc);
    set_replica_peer_id(&doc, replica_id)?;
    let validated = schema::validate(&doc, expected_document_id, &FlowSourceLimits::default())?;
    let node_tokens = RwLock::new(schema::node_token_cursors(&doc)?);
    Ok(Self {
      doc,
      document_id: validated.document_id,
      root_flow_id: validated.root_flow_id,
      replica_id,
      node_tokens,
    })
  }

  #[must_use]
  pub const fn document_id(&self) -> DocumentId {
    self.document_id
  }

  #[must_use]
  pub const fn root_flow_id(&self) -> FlowId {
    self.root_flow_id
  }

  #[must_use]
  pub const fn replica_id(&self) -> ReplicaId {
    self.replica_id
  }

  #[must_use]
  pub fn peer_id(&self) -> PeerID {
    self.doc.peer_id()
  }

  #[cfg(test)]
  fn parse_flow_call_count(&self) -> usize {
    schema::parse_flow_call_count()
  }

  pub fn frontier(&self) -> CollabResult<Vec<u8>> {
    encode_frontier(&self.doc.oplog_vv())
  }

  pub fn history_policy(&self) -> CollabResult<FlowHistoryPolicy> {
    schema::history_policy(&self.doc)
  }

  pub fn export_snapshot(&self) -> CollabResult<Vec<u8>> {
    self.doc.export(ExportMode::Snapshot).map_err(loro_error)
  }

  pub fn export_update_since(&self, encoded_frontier: &[u8]) -> CollabResult<Vec<u8>> {
    let frontier = decode_frontier(encoded_frontier)?;
    self
      .doc
      .export(ExportMode::updates(&frontier))
      .map_err(loro_error)
  }

  pub fn materialize(&self) -> CollabResult<FlowMaterialization> {
    schema::materialize(&self.doc, &FlowSourceLimits::default())
  }

  pub fn materialize_flow(&self, flow_id: FlowId) -> CollabResult<MaterializedFlow> {
    schema::materialize_flow(&self.doc, flow_id, &FlowSourceLimits::default())
  }

  pub fn materialize_flow_window(&self, flow_id: FlowId, changed_unicode: Range<usize>) -> CollabResult<MaterializedFlowWindow> {
    schema::materialize_flow_window(&self.doc, flow_id, changed_unicode)
  }

  pub fn materialize_node_window(&self, node_id: FlowNodeId) -> CollabResult<MaterializedFlowWindow> {
    let (flow_id, unicode) = self.cached_node_token_position(node_id)?;
    schema::materialize_node_window_at(&self.doc, node_id, flow_id, unicode)
  }

  pub fn materialize_object_graph(&self, node_id: FlowNodeId) -> CollabResult<MaterializedObjectGraph> {
    schema::materialize_object_graph(&self.doc, node_id, &FlowSourceLimits::default())
  }

  pub fn node_record(&self, node_id: FlowNodeId) -> CollabResult<FlowNodeRecord> {
    schema::read_node_record(&self.doc, node_id)
  }

  pub fn node_owner_flow(&self, node_id: FlowNodeId) -> CollabResult<FlowId> {
    schema::node_owner_flow(&self.doc, node_id)
  }

  pub fn source_hash(&self) -> CollabResult<[u8; 32]> {
    postcard::to_stdvec(&self.materialize()?)
      .map(|bytes| crate::blake3_hash(&bytes))
      .map_err(Into::into)
  }

  pub fn document_metadata(&self) -> CollabResult<Vec<u8>> {
    schema::document_metadata(&self.doc)
  }

  pub fn asset_references(&self) -> CollabResult<BTreeMap<FlowAssetId, FlowAssetReference>> {
    schema::asset_references(&self.doc)
  }

  pub fn created_by_actor(&self) -> CollabResult<ActorId> {
    schema::created_by_actor(&self.doc)
  }

  pub fn set_document_metadata(&mut self, role: Role, metadata: &[u8]) -> CollabResult<FlowCommit> {
    let metadata = metadata.to_vec();
    self.transact(role, FlowUpdateOrigin::LocalInput, move |doc| schema::set_document_metadata(doc, &metadata))
  }

  pub fn apply_edits(&mut self, role: Role, edits: &[FlowEdit]) -> CollabResult<FlowCommit> {
    if edits.is_empty() {
      return Err(CollabError::InvalidSchema("empty vNext edit transaction"));
    }
    validate_flow_edits(edits)?;
    let edits = edits.to_vec();
    let structural_cursors = self.structural_cursors_for_edits(&edits)?;
    let bounded_preflight = edits.len() == 1 && preflight_flow_edits(&self.doc, &edits, &structural_cursors).is_ok();
    let mutation = move |doc: &LoroDoc| {
      for edit in &edits {
        apply_flow_edit(doc, edit, &structural_cursors)?;
      }
      Ok(())
    };
    if bounded_preflight {
      self.transact_prevalidated(role, FlowUpdateOrigin::LocalInput, mutation)
    } else {
      self.transact(role, FlowUpdateOrigin::LocalInput, mutation)
    }
  }

  pub fn validate(&self, limits: &FlowSourceLimits) -> CollabResult<()> {
    schema::validate(&self.doc, Some(self.document_id), limits).map(|_| ())
  }

  pub fn anchor_at_utf8(&self, flow_id: FlowId, byte_offset: usize, side: Side) -> CollabResult<AnchoredPosition> {
    schema::anchor_at_utf8(&self.doc, flow_id, byte_offset, side)
  }

  pub fn anchor_at_unicode(&self, flow_id: FlowId, unicode_offset: usize, side: Side) -> CollabResult<AnchoredPosition> {
    schema::anchor_at_unicode(&self.doc, flow_id, unicode_offset, side)
  }

  pub fn anchor_in_paragraph_utf8(&self, paragraph_id: FlowNodeId, byte_offset: usize, side: Side) -> CollabResult<AnchoredPosition> {
    let (flow_id, token) = self.cached_node_token_position(paragraph_id)?;
    schema::anchor_in_paragraph_at_token_utf8(&self.doc, paragraph_id, flow_id, token, byte_offset, side)
  }

  pub fn anchor_at_node_index(&self, flow_id: FlowId, node_index: usize, side: Side) -> CollabResult<AnchoredPosition> {
    schema::anchor_at_node_index(&self.doc, flow_id, node_index, side)
  }

  pub fn resolve_anchor_utf8(&self, position: &AnchoredPosition) -> CollabResult<usize> {
    schema::resolve_anchor_utf8(&self.doc, position)
  }

  pub fn resolve_anchor_in_paragraph_utf8(&self, position: &AnchoredPosition) -> CollabResult<ResolvedFlowPosition> {
    schema::resolve_anchor_in_paragraph_utf8(&self.doc, position)
  }

  pub fn insert_text(&mut self, role: Role, at: &AnchoredPosition, text: &str) -> CollabResult<FlowCommit> {
    schema::validate_user_text(text)?;
    let unicode_pos = schema::resolve_anchor_unicode(&self.doc, at)?;
    schema::validate_plain_text_insert(&self.doc, at.flow_id, unicode_pos)?;
    let flow_id = at.flow_id;
    let owned = text.to_string();
    self.transact(role, FlowUpdateOrigin::LocalInput, move |doc| {
      schema::insert_text_with_exact_marks(doc, flow_id, unicode_pos, &owned, &[])
    })
  }

  pub fn insert_text_with_marks(
    &mut self,
    role: Role,
    at: &AnchoredPosition,
    text: &str,
    marks: &[(String, FlowMarkValue)],
  ) -> CollabResult<FlowCommit> {
    schema::validate_user_text(text)?;
    let unicode_pos = schema::resolve_anchor_unicode(&self.doc, at)?;
    schema::validate_plain_text_insert(&self.doc, at.flow_id, unicode_pos)?;
    for (key, _) in marks {
      schema::validate_inline_mark_key(key)?;
    }
    let flow_id = at.flow_id;
    let owned = text.to_string();
    let marks = marks.to_vec();
    self.transact(role, FlowUpdateOrigin::LocalInput, move |doc| {
      schema::insert_text_with_exact_marks(doc, flow_id, unicode_pos, &owned, &marks)
    })
  }

  pub fn delete_text(&mut self, role: Role, start: &AnchoredPosition, end: &AnchoredPosition) -> CollabResult<FlowCommit> {
    if start.flow_id != end.flow_id {
      return Err(CollabError::InvalidSchema("cross-flow text delete"));
    }
    let start_pos = schema::resolve_anchor_unicode(&self.doc, start)?;
    let end_pos = schema::resolve_anchor_unicode(&self.doc, end)?;
    let range = ordered_range(start_pos, end_pos);
    schema::validate_plain_text_delete(&self.doc, start.flow_id, range.clone())?;
    let flow_id = start.flow_id;
    self.transact(role, FlowUpdateOrigin::LocalInput, move |doc| {
      schema::flow_text(doc, flow_id)?.delete(range.start, range.len()).map_err(loro_error)
    })
  }

  pub fn delete_document_range(
    &mut self,
    role: Role,
    start: &AnchoredPosition,
    end: &AnchoredPosition,
  ) -> CollabResult<FlowCommit> {
    if start.flow_id != end.flow_id {
      return Err(CollabError::InvalidSchema("cross-flow document delete"));
    }
    let start_pos = schema::resolve_anchor_unicode(&self.doc, start)?;
    let end_pos = schema::resolve_anchor_unicode(&self.doc, end)?;
    let range = ordered_range(start_pos, end_pos);
    schema::validate_document_delete(&self.doc, start.flow_id, range.clone())?;
    let flow_id = start.flow_id;
    self.transact(role, FlowUpdateOrigin::LocalInput, move |doc| {
      schema::flow_text(doc, flow_id)?.delete(range.start, range.len()).map_err(loro_error)
    })
  }

  pub fn split_paragraph(
    &mut self,
    role: Role,
    at: &AnchoredPosition,
    new_paragraph_id: FlowNodeId,
    paragraph_metadata: &[u8],
  ) -> CollabResult<FlowCommit> {
    let unicode_pos = schema::resolve_anchor_unicode(&self.doc, at)?;
    let source_node = schema::paragraph_at_unicode(&self.doc, at.flow_id, unicode_pos)?;
    schema::ensure_node_absent(&self.doc, new_paragraph_id)?;
    let flow_id = at.flow_id;
    let metadata = paragraph_metadata.to_vec();
    let _ = source_node;
    self.transact(role, FlowUpdateOrigin::LocalInput, move |doc| {
      schema::insert_structural_node(doc, flow_id, unicode_pos, FlowNodeKind::Paragraph, new_paragraph_id, &metadata, &[])
    })
  }

  pub fn join_paragraph(&mut self, role: Role, second_paragraph_id: FlowNodeId) -> CollabResult<FlowCommit> {
    let (flow_id, unicode_pos, first_paragraph_id) = schema::join_target(&self.doc, second_paragraph_id)?;
    let _ = first_paragraph_id;
    self.transact(role, FlowUpdateOrigin::LocalInput, move |doc| {
      schema::flow_text(doc, flow_id)?.delete(unicode_pos, 1).map_err(loro_error)
    })
  }

  pub fn set_node_metadata(&mut self, role: Role, node_id: FlowNodeId, metadata: &[u8]) -> CollabResult<FlowCommit> {
    schema::node_record(&self.doc, node_id)?;
    let metadata = metadata.to_vec();
    self.transact(role, FlowUpdateOrigin::LocalInput, move |doc| {
      schema::node_record(doc, node_id)?
        .insert(schema::KEY_NODE_METADATA, metadata.as_slice())
        .map_err(loro_error)
    })
  }

  pub fn mark_text(
    &mut self,
    role: Role,
    start: &AnchoredPosition,
    end: &AnchoredPosition,
    key: &str,
    value: FlowMarkValue,
  ) -> CollabResult<FlowCommit> {
    schema::validate_inline_mark_key(key)?;
    if start.flow_id != end.flow_id {
      return Err(CollabError::InvalidSchema("cross-flow text mark"));
    }
    let start_pos = schema::resolve_anchor_unicode(&self.doc, start)?;
    let end_pos = schema::resolve_anchor_unicode(&self.doc, end)?;
    let range = ordered_range(start_pos, end_pos);
    schema::validate_plain_text_delete(&self.doc, start.flow_id, range.clone())?;
    let flow_id = start.flow_id;
    let key = key.to_string();
    self.transact(role, FlowUpdateOrigin::LocalInput, move |doc| {
      schema::flow_text(doc, flow_id)?
        .mark(range.clone(), &key, value.clone().into_loro())
        .map_err(loro_error)
    })
  }

  pub fn set_text_marks(
    &mut self,
    role: Role,
    start: &AnchoredPosition,
    end: &AnchoredPosition,
    clear_keys: &[String],
    marks: &[(String, FlowMarkValue)],
  ) -> CollabResult<FlowCommit> {
    if start.flow_id != end.flow_id {
      return Err(CollabError::InvalidSchema("cross-flow text mark"));
    }
    for key in clear_keys {
      schema::validate_inline_mark_key(key)?;
    }
    for (key, _) in marks {
      schema::validate_inline_mark_key(key)?;
    }
    let start_pos = schema::resolve_anchor_unicode(&self.doc, start)?;
    let end_pos = schema::resolve_anchor_unicode(&self.doc, end)?;
    let range = ordered_range(start_pos, end_pos);
    schema::validate_plain_text_delete(&self.doc, start.flow_id, range.clone())?;
    let flow_id = start.flow_id;
    let clear_keys = clear_keys.to_vec();
    let marks = marks.to_vec();
    self.transact(role, FlowUpdateOrigin::LocalInput, move |doc| {
      let flow = schema::flow_text(doc, flow_id)?;
      for key in &clear_keys {
        flow.unmark(range.clone(), key).map_err(loro_error)?;
      }
      for (key, value) in &marks {
        flow
          .mark(range.clone(), key, value.clone().into_loro())
          .map_err(loro_error)?;
      }
      Ok(())
    })
  }

  pub fn create_child_flow_object(
    &mut self,
    role: Role,
    at: &AnchoredPosition,
    object_id: FlowNodeId,
    object_metadata: &[u8],
    child_flow_id: FlowId,
    child_paragraph_id: FlowNodeId,
    child_paragraph_metadata: &[u8],
  ) -> CollabResult<FlowCommit> {
    let unicode_pos = schema::resolve_anchor_unicode(&self.doc, at)?;
    schema::validate_block_insert(&self.doc, at.flow_id, unicode_pos)?;
    schema::ensure_node_absent(&self.doc, object_id)?;
    schema::ensure_node_absent(&self.doc, child_paragraph_id)?;
    schema::ensure_flow_absent(&self.doc, child_flow_id)?;
    let parent_flow_id = at.flow_id;
    let object_metadata = object_metadata.to_vec();
    let child_metadata = child_paragraph_metadata.to_vec();
    self.transact(role, FlowUpdateOrigin::LocalInput, move |doc| {
      schema::create_flow(doc, child_flow_id, child_paragraph_id, &child_metadata)?;
      schema::insert_structural_node(
        doc,
        parent_flow_id,
        unicode_pos,
        FlowNodeKind::Object,
        object_id,
        &object_metadata,
        &[child_flow_id],
      )
    })
  }

  pub fn new_undo_manager(&self) -> FlowUndoManager {
    FlowUndoManager::new(&self.doc)
  }

  pub fn undo(&mut self, role: Role, undo: &mut FlowUndoManager) -> CollabResult<Option<FlowCommit>> {
    self.apply_undo_redo(role, undo, FlowUpdateOrigin::Undo, true)
  }

  pub fn redo(&mut self, role: Role, undo: &mut FlowUndoManager) -> CollabResult<Option<FlowCommit>> {
    self.apply_undo_redo(role, undo, FlowUpdateOrigin::Redo, false)
  }

  pub fn import_update_checked(&mut self, update: &[u8], policy: &FlowImportPolicy) -> CollabResult<FlowImportOutcome> {
    require_writer(policy.role)?;
    if update.len() > policy.limits.max_update_bytes {
      return Err(CollabError::InvalidSchema("vNext update exceeds byte limit"));
    }
    let validation_started = Instant::now();
    self.doc.commit();
    let before = self.doc.oplog_vv();
    let before_ops = self.doc.len_ops();
    let protected_before = schema::protected_state(&self.doc)?;
    let working = self.doc.fork();
    schema::configure(&working);
    working.import_with(update, "flowstate:remote").map_err(loro_error)?;
    ensure_validation_budget(validation_started, &policy.limits)?;
    if working.len_ops().saturating_sub(before_ops) > policy.limits.max_update_ops {
      return Err(CollabError::InvalidSchema("vNext update operation limit"));
    }
    schema::validate(&working, Some(self.document_id), &policy.limits)?;
    ensure_validation_budget(validation_started, &policy.limits)?;
    if !policy.allow_protected_mutation && schema::protected_state(&working)? != protected_before {
      return Err(CollabError::Unauthorized("attempted to mutate protected vNext source state"));
    }
    validate_update_peers(&before, &working.oplog_vv(), &policy.allowed_peer_ids)?;
    ensure_validation_budget(validation_started, &policy.limits)?;

    self.doc.import_with(update, "flowstate:remote").map_err(loro_error)?;
    let changes = schema::summarize_changes(&self.doc, &before)?;
    self.refresh_node_token_cache(&changes);
    Ok(FlowImportOutcome {
      frontier: self.frontier()?,
      changes,
    })
  }

  fn transact(
    &mut self,
    role: Role,
    origin: FlowUpdateOrigin,
    mutation: impl Fn(&LoroDoc) -> CollabResult<()>,
  ) -> CollabResult<FlowCommit> {
    require_writer(role)?;
    self.doc.commit();
    let limits = FlowSourceLimits::default();
    let before = self.doc.oplog_vv();
    let before_ops = self.doc.len_ops();
    let working = self.doc.fork();
    schema::configure(&working);
    mutation(&working)?;
    working.commit_with(CommitOptions::new().origin(origin.as_str()));
    schema::validate(&working, Some(self.document_id), &limits)?;
    if working.len_ops().saturating_sub(before_ops) > limits.max_update_ops {
      return Err(CollabError::InvalidSchema("vNext local update operation limit"));
    }
    if working
      .export(ExportMode::updates(&before))
      .map_err(loro_error)?
      .len()
      > limits.max_update_bytes
    {
      return Err(CollabError::InvalidSchema("vNext local update exceeds byte limit"));
    }

    let base_frontier = encode_frontier(&before)?;
    mutation(&self.doc)?;
    self.doc.commit_with(CommitOptions::new().origin(origin.as_str()));
    let changes = schema::summarize_changes(&self.doc, &before)?;
    self.refresh_node_token_cache(&changes);
    let update = self
      .doc
      .export(ExportMode::updates(&before))
      .map_err(loro_error)?;
    Ok(FlowCommit {
      update,
      base_frontier,
      resulting_frontier: self.frontier()?,
      changes,
    })
  }

  fn transact_prevalidated(
    &mut self,
    role: Role,
    origin: FlowUpdateOrigin,
    mutation: impl Fn(&LoroDoc) -> CollabResult<()>,
  ) -> CollabResult<FlowCommit> {
    require_writer(role)?;
    self.doc.commit();
    let before = self.doc.oplog_vv();
    let base_frontier = encode_frontier(&before)?;
    mutation(&self.doc)?;
    self.doc.commit_with(CommitOptions::new().origin(origin.as_str()));
    let changes = schema::summarize_changes(&self.doc, &before)?;
    self.refresh_node_token_cache(&changes);
    let update = self
      .doc
      .export(ExportMode::updates(&before))
      .map_err(loro_error)?;
    debug_assert!(update.len() <= FlowSourceLimits::default().max_update_bytes);
    Ok(FlowCommit {
      update,
      base_frontier,
      resulting_frontier: self.frontier()?,
      changes,
    })
  }

  fn apply_undo_redo(
    &mut self,
    role: Role,
    undo: &mut FlowUndoManager,
    origin: FlowUpdateOrigin,
    is_undo: bool,
  ) -> CollabResult<Option<FlowCommit>> {
    require_writer(role)?;
    self.doc.commit();
    let before = self.doc.oplog_vv();
    self
      .doc
      .set_next_commit_options(CommitOptions::new().origin(origin.as_str()));
    let changed = if is_undo { undo.undo() } else { undo.redo() }.map_err(loro_error)?;
    if !changed {
      return Ok(None);
    }
    let changes = schema::summarize_changes(&self.doc, &before)?;
    self.refresh_node_token_cache(&changes);
    Ok(Some(FlowCommit {
      update: self
        .doc
        .export(ExportMode::updates(&before))
        .map_err(loro_error)?,
      base_frontier: encode_frontier(&before)?,
      resulting_frontier: self.frontier()?,
      changes,
    }))
  }

  fn cached_node_token_position(&self, node_id: FlowNodeId) -> CollabResult<(FlowId, usize)> {
    let (flow_id, cursor) = self.cached_node_token_cursor(node_id)?;
    let unicode = schema::resolve_node_token_cursor(&self.doc, node_id, flow_id, &cursor)?;
    Ok((flow_id, unicode))
  }

  fn cached_node_token_cursor(&self, node_id: FlowNodeId) -> CollabResult<(FlowId, Cursor)> {
    let cached = self
      .node_tokens
      .read()
      .map_err(|_| CollabError::InvalidSchema("vNext node token cache lock poisoned"))?
      .get(&node_id)
      .cloned();
    if let Some((flow_id, cursor)) = cached
      && schema::resolve_node_token_cursor(&self.doc, node_id, flow_id, &cursor).is_ok()
    {
      return Ok((flow_id, cursor));
    }
    let (flow_id, cursor, _) = schema::find_node_token_cursor(&self.doc, node_id)?;
    self
      .node_tokens
      .write()
      .map_err(|_| CollabError::InvalidSchema("vNext node token cache lock poisoned"))?
      .insert(node_id, (flow_id, cursor.clone()));
    Ok((flow_id, cursor))
  }

  fn structural_cursors_for_edits(&self, edits: &[FlowEdit]) -> CollabResult<HashMap<FlowNodeId, (FlowId, Cursor)>> {
    edits
      .iter()
      .filter_map(|edit| match edit {
        FlowEdit::JoinParagraph { second_paragraph_id } => Some(*second_paragraph_id),
        FlowEdit::ReplaceParagraphText { paragraph_id, .. } => Some(*paragraph_id),
        FlowEdit::DeleteObject { object_id } => Some(*object_id),
        _ => None,
      })
      .map(|node_id| self.cached_node_token_cursor(node_id).map(|cursor| (node_id, cursor)))
      .collect()
  }

  fn refresh_node_token_cache(&self, changes: &FlowChangeSummary) {
    let mut refreshed = Vec::new();
    for (flow_id, change) in &changes.flow_text_changes {
      if let Ok(cursors) = schema::node_token_cursors_in_range(&self.doc, *flow_id, change.after_unicode.clone()) {
        refreshed.extend(cursors.into_iter().map(|(node_id, cursor)| (node_id, *flow_id, cursor)));
      }
    }
    let Ok(mut cache) = self.node_tokens.write() else {
      return;
    };
    for (node_id, flow_id, cursor) in refreshed {
      cache.insert(node_id, (flow_id, cursor));
    }
  }
}

fn ensure_validation_budget(started: Instant, limits: &FlowSourceLimits) -> CollabResult<()> {
  if started.elapsed() > Duration::from_millis(limits.max_validation_millis) {
    Err(CollabError::InvalidSchema("vNext update validation exceeded time budget"))
  } else {
    Ok(())
  }
}

fn preflight_flow_edits(
  doc: &LoroDoc,
  edits: &[FlowEdit],
  structural_cursors: &HashMap<FlowNodeId, (FlowId, Cursor)>,
) -> CollabResult<()> {
  let limits = FlowSourceLimits::default();
  let mut estimated_bytes = edits.len().saturating_mul(4_096);
  let mut estimated_ops = edits.len().saturating_mul(32);
  for edit in edits {
    match edit {
      FlowEdit::InsertText { at, text, marks } => {
        let unicode = schema::resolve_anchor_unicode(doc, at)?;
        schema::validate_plain_text_insert(doc, at.flow_id, unicode)?;
        estimated_bytes = estimated_bytes.saturating_add(text.len().saturating_mul(4));
        estimated_ops = estimated_ops.saturating_add(text.chars().count().saturating_mul(8) + marks.len().saturating_mul(4));
      },
      FlowEdit::InsertParagraphFragment {
        at,
        first_runs,
        additional_paragraphs,
      } => {
        let unicode = schema::resolve_anchor_unicode(doc, at)?;
        schema::validate_plain_text_insert(doc, at.flow_id, unicode)?;
        for paragraph in additional_paragraphs {
          schema::ensure_node_absent(doc, paragraph.paragraph_id)?;
          if paragraph.metadata.len() > limits.max_node_metadata_bytes {
            return Err(CollabError::InvalidSchema("vNext local paragraph metadata limit"));
          }
        }
        let text_bytes = first_runs
          .iter()
          .chain(additional_paragraphs.iter().flat_map(|paragraph| &paragraph.runs))
          .map(|run| run.text.len())
          .sum::<usize>();
        estimated_bytes = estimated_bytes.saturating_add(text_bytes.saturating_mul(4));
        estimated_ops = estimated_ops.saturating_add(text_bytes.saturating_mul(8));
      },
      FlowEdit::DeleteDocumentRange { start, end } => {
        if start.flow_id != end.flow_id {
          return Err(CollabError::InvalidSchema("cross-flow document delete"));
        }
        let range = ordered_range(
          schema::resolve_anchor_unicode(doc, start)?,
          schema::resolve_anchor_unicode(doc, end)?,
        );
        schema::validate_document_delete(doc, start.flow_id, range)?;
      },
      FlowEdit::SplitParagraph {
        at,
        new_paragraph_id,
        metadata,
      } => {
        let unicode = schema::resolve_anchor_unicode(doc, at)?;
        schema::paragraph_at_unicode(doc, at.flow_id, unicode)?;
        schema::ensure_node_absent(doc, *new_paragraph_id)?;
        if metadata.len() > limits.max_node_metadata_bytes {
          return Err(CollabError::InvalidSchema("vNext local paragraph metadata limit"));
        }
        estimated_bytes = estimated_bytes.saturating_add(metadata.len().saturating_mul(4));
        estimated_ops = estimated_ops.saturating_add(64);
      },
      FlowEdit::JoinParagraph { second_paragraph_id } => {
        resolve_join_target(doc, *second_paragraph_id, structural_cursors)?;
      },
      FlowEdit::SetNodeMetadata { node_id, metadata } => {
        schema::node_record(doc, *node_id)?;
        if metadata.len() > limits.max_node_metadata_bytes {
          return Err(CollabError::InvalidSchema("vNext local node metadata limit"));
        }
        estimated_bytes = estimated_bytes.saturating_add(metadata.len().saturating_mul(4));
        estimated_ops = estimated_ops.saturating_add(metadata.len());
      },
      FlowEdit::SetTextMarks {
        start,
        end,
        clear_keys,
        marks,
      } => {
        if start.flow_id != end.flow_id {
          return Err(CollabError::InvalidSchema("cross-flow text mark"));
        }
        let range = ordered_range(
          schema::resolve_anchor_unicode(doc, start)?,
          schema::resolve_anchor_unicode(doc, end)?,
        );
        schema::validate_plain_text_delete(doc, start.flow_id, range.clone())?;
        estimated_ops = estimated_ops.saturating_add(range.len().saturating_mul(clear_keys.len() + marks.len() + 1));
      },
      FlowEdit::ReplaceParagraphText {
        paragraph_id,
        text,
        marks,
      } => {
        let record = schema::read_node_record(doc, *paragraph_id)?;
        if record.kind != FlowNodeKind::Paragraph {
          return Err(CollabError::InvalidSchema("vNext paragraph text replacement target is not a paragraph"));
        }
        resolve_structural_token(doc, *paragraph_id, structural_cursors)?;
        estimated_bytes = estimated_bytes.saturating_add(text.len().saturating_mul(4));
        estimated_ops = estimated_ops.saturating_add(text.chars().count().saturating_mul(8) + marks.len().saturating_mul(4));
      },
      FlowEdit::PutAssetReference { asset } => {
        let bytes = postcard::to_stdvec(asset)?;
        if bytes.len() > limits.max_asset_reference_bytes {
          return Err(CollabError::InvalidSchema("vNext local asset reference size limit"));
        }
        estimated_bytes = estimated_bytes.saturating_add(bytes.len().saturating_mul(4));
        estimated_ops = estimated_ops.saturating_add(bytes.len());
      },
      FlowEdit::DeleteObject { object_id } => {
        if schema::read_node_record(doc, *object_id)?.kind != FlowNodeKind::Object {
          return Err(CollabError::InvalidSchema("vNext object delete targets a paragraph"));
        }
        resolve_structural_token(doc, *object_id, structural_cursors)?;
      },
      // Rich graph insertion can create several dependent flows and records.
      // Validate it on an isolated fork until its prepared-edit representation
      // makes every intermediate failure impossible.
      FlowEdit::InsertObject { .. } => return Err(CollabError::InvalidSchema("vNext rich insert requires isolated validation")),
    }
  }
  if estimated_bytes > limits.max_update_bytes / 4
    || estimated_ops > limits.max_update_ops / 4
    || doc.len_ops().saturating_add(estimated_ops) > limits.max_total_ops
    || doc.len_changes().saturating_add(1) > limits.max_total_changes
  {
    return Err(CollabError::InvalidSchema("vNext local edit requires isolated validation"));
  }
  Ok(())
}

fn validate_flow_edits(edits: &[FlowEdit]) -> CollabResult<()> {
  for edit in edits {
    match edit {
      FlowEdit::InsertText { text, marks, .. } => {
        schema::validate_user_text(text)?;
        for (key, _) in marks {
          schema::validate_inline_mark_key(key)?;
        }
      },
      FlowEdit::InsertParagraphFragment {
        first_runs,
        additional_paragraphs,
        ..
      } => {
        validate_flow_text_inserts(first_runs)?;
        let mut paragraph_ids = BTreeSet::new();
        for paragraph in additional_paragraphs {
          if !paragraph_ids.insert(paragraph.paragraph_id) {
            return Err(CollabError::InvalidSchema("duplicate vNext inserted paragraph ID"));
          }
          validate_flow_text_inserts(&paragraph.runs)?;
        }
      },
      FlowEdit::SetTextMarks { clear_keys, marks, .. } => {
        for key in clear_keys {
          schema::validate_inline_mark_key(key)?;
        }
        for (key, _) in marks {
          schema::validate_inline_mark_key(key)?;
        }
      },
      FlowEdit::ReplaceParagraphText { text, marks, .. } => {
        schema::validate_user_text(text)?;
        for (key, _) in marks {
          schema::validate_inline_mark_key(key)?;
        }
      },
      FlowEdit::PutAssetReference { asset } => {
        if asset.mime_type.is_empty() {
          return Err(CollabError::InvalidSchema("vNext asset MIME type is empty"));
        }
      },
      FlowEdit::DeleteDocumentRange { .. }
      | FlowEdit::SplitParagraph { .. }
      | FlowEdit::JoinParagraph { .. }
      | FlowEdit::SetNodeMetadata { .. }
      | FlowEdit::DeleteObject { .. } => {},
      FlowEdit::InsertObject {
        object,
        child_flows,
        ..
      } => {
        if object.record.kind != FlowNodeKind::Object || !object.text.is_empty() || !object.marks.is_empty() {
          return Err(CollabError::InvalidSchema("vNext inserted object seed is not an object"));
        }
        if child_flows.iter().any(|flow| flow.nodes.is_empty()) {
          return Err(CollabError::InvalidSchema("vNext inserted child flow has no nodes"));
        }
      },
    }
  }
  Ok(())
}

fn apply_flow_edit(
  doc: &LoroDoc,
  edit: &FlowEdit,
  structural_cursors: &HashMap<FlowNodeId, (FlowId, Cursor)>,
) -> CollabResult<()> {
  match edit {
    FlowEdit::InsertText { at, text, marks } => {
      let unicode_pos = schema::resolve_anchor_unicode(doc, at)?;
      schema::validate_plain_text_insert(doc, at.flow_id, unicode_pos)?;
      schema::insert_text_with_exact_marks(doc, at.flow_id, unicode_pos, text, marks)
    },
    FlowEdit::InsertParagraphFragment {
      at,
      first_runs,
      additional_paragraphs,
    } => {
      let mut unicode_pos = schema::resolve_anchor_unicode(doc, at)?;
      schema::validate_plain_text_insert(doc, at.flow_id, unicode_pos)?;
      for run in first_runs {
        schema::insert_text_with_exact_marks(doc, at.flow_id, unicode_pos, &run.text, &run.marks)?;
        unicode_pos += run.text.chars().count();
      }
      for paragraph in additional_paragraphs {
        schema::ensure_node_absent(doc, paragraph.paragraph_id)?;
        schema::insert_structural_node(
          doc,
          at.flow_id,
          unicode_pos,
          FlowNodeKind::Paragraph,
          paragraph.paragraph_id,
          &paragraph.metadata,
          &[],
        )?;
        unicode_pos += 1;
        for run in &paragraph.runs {
          schema::insert_text_with_exact_marks(doc, at.flow_id, unicode_pos, &run.text, &run.marks)?;
          unicode_pos += run.text.chars().count();
        }
      }
      Ok(())
    },
    FlowEdit::DeleteDocumentRange { start, end } => {
      if start.flow_id != end.flow_id {
        return Err(CollabError::InvalidSchema("cross-flow document delete"));
      }
      let range = ordered_range(
        schema::resolve_anchor_unicode(doc, start)?,
        schema::resolve_anchor_unicode(doc, end)?,
      );
      schema::validate_document_delete(doc, start.flow_id, range.clone())?;
      schema::flow_text(doc, start.flow_id)?
        .delete(range.start, range.len())
        .map_err(loro_error)
    },
    FlowEdit::SplitParagraph {
      at,
      new_paragraph_id,
      metadata,
    } => {
      let unicode_pos = schema::resolve_anchor_unicode(doc, at)?;
      schema::paragraph_at_unicode(doc, at.flow_id, unicode_pos)?;
      schema::ensure_node_absent(doc, *new_paragraph_id)?;
      schema::insert_structural_node(
        doc,
        at.flow_id,
        unicode_pos,
        FlowNodeKind::Paragraph,
        *new_paragraph_id,
        metadata,
        &[],
      )
    },
    FlowEdit::JoinParagraph { second_paragraph_id } => {
      let (flow_id, unicode_pos, _) = resolve_join_target(doc, *second_paragraph_id, structural_cursors)?;
      schema::flow_text(doc, flow_id)?.delete(unicode_pos, 1).map_err(loro_error)
    },
    FlowEdit::SetNodeMetadata { node_id, metadata } => schema::node_record(doc, *node_id)?
      .insert(schema::KEY_NODE_METADATA, metadata.as_slice())
      .map_err(loro_error),
    FlowEdit::SetTextMarks {
      start,
      end,
      clear_keys,
      marks,
    } => {
      if start.flow_id != end.flow_id {
        return Err(CollabError::InvalidSchema("cross-flow text mark"));
      }
      let range = ordered_range(
        schema::resolve_anchor_unicode(doc, start)?,
        schema::resolve_anchor_unicode(doc, end)?,
      );
      schema::validate_plain_text_delete(doc, start.flow_id, range.clone())?;
      let flow = schema::flow_text(doc, start.flow_id)?;
      for key in clear_keys {
        flow.unmark(range.clone(), key).map_err(loro_error)?;
      }
      for (key, value) in marks {
        flow
          .mark(range.clone(), key, value.clone().into_loro())
          .map_err(loro_error)?;
      }
      Ok(())
    },
    FlowEdit::ReplaceParagraphText {
      paragraph_id,
      text,
      marks,
    } => {
      let (flow_id, token) = resolve_structural_token(doc, *paragraph_id, structural_cursors)?;
      schema::replace_paragraph_text_at(doc, *paragraph_id, flow_id, token, text, marks)
    },
    FlowEdit::PutAssetReference { asset } => schema::put_asset_reference(doc, asset),
    FlowEdit::InsertObject {
      at,
      object,
      child_flows,
    } => schema::insert_seed_object(doc, at, object, child_flows),
    FlowEdit::DeleteObject { object_id } => {
      let (flow_id, token) = resolve_structural_token(doc, *object_id, structural_cursors)?;
      schema::delete_object_at(doc, *object_id, flow_id, token)
    },
  }
}

fn resolve_join_target(
  doc: &LoroDoc,
  second_paragraph_id: FlowNodeId,
  structural_cursors: &HashMap<FlowNodeId, (FlowId, Cursor)>,
) -> CollabResult<(FlowId, usize, FlowNodeId)> {
  let (flow_id, token) = resolve_structural_token(doc, second_paragraph_id, structural_cursors)?;
  schema::join_target_at(doc, second_paragraph_id, flow_id, token)
}

fn resolve_structural_token(
  doc: &LoroDoc,
  node_id: FlowNodeId,
  structural_cursors: &HashMap<FlowNodeId, (FlowId, Cursor)>,
) -> CollabResult<(FlowId, usize)> {
  let (flow_id, cursor) = structural_cursors
    .get(&node_id)
    .ok_or(CollabError::InvalidSchema("missing prepared vNext structural cursor"))?;
  schema::resolve_node_token_cursor(doc, node_id, *flow_id, cursor).map(|token| (*flow_id, token))
}

fn validate_flow_text_inserts(runs: &[FlowTextInsert]) -> CollabResult<()> {
  for run in runs {
    schema::validate_user_text(&run.text)?;
    for (key, _) in &run.marks {
      schema::validate_inline_mark_key(key)?;
    }
  }
  Ok(())
}

fn validate_update_peers(before: &VersionVector, after: &VersionVector, allowed: &BTreeSet<PeerID>) -> CollabResult<()> {
  for span in after.sub_iter(before) {
    if !allowed.contains(&span.peer) {
      return Err(CollabError::Unauthorized("update contains an unregistered Loro peer"));
    }
  }
  Ok(())
}

fn set_replica_peer_id(doc: &LoroDoc, replica_id: ReplicaId) -> CollabResult<()> {
  doc.set_peer_id(loro_peer_id_for_replica(replica_id)).map_err(loro_error)
}

#[must_use]
pub fn loro_peer_id_for_replica(replica_id: ReplicaId) -> PeerID {
  let hash = crate::blake3_hash(replica_id.0.as_bytes());
  let bytes: [u8; 8] = hash[..8].try_into().unwrap();
  let mut peer_id = PeerID::from_le_bytes(bytes);
  if peer_id == 0 {
    peer_id = 1;
  }
  peer_id
}

fn ordered_range(left: usize, right: usize) -> Range<usize> {
  left.min(right)..left.max(right)
}

fn encode_frontier(frontier: &VersionVector) -> CollabResult<Vec<u8>> {
  postcard::to_stdvec(frontier).map_err(Into::into)
}

fn decode_frontier(encoded: &[u8]) -> CollabResult<VersionVector> {
  if encoded.is_empty() {
    Ok(VersionVector::default())
  } else {
    postcard::from_bytes(encoded).map_err(Into::into)
  }
}

fn require_writer(role: Role) -> CollabResult<()> {
  if role.can_write() {
    Ok(())
  } else {
    Err(CollabError::Unauthorized("viewer cannot mutate vNext source"))
  }
}

fn loro_error(error: impl std::fmt::Display) -> CollabError {
  CollabError::Loro(error.to_string())
}

#[cfg(test)]
mod tests;
