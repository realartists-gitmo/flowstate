// Durable source lives here: projection caches and editor hints are inputs, but Loro state in this module is the source of truth.
use std::{
  collections::{BTreeMap, HashMap},
  ops::Range,
};

use loro::{
  Container, ExpandType, ExportMode, LoroDoc, LoroMap, LoroMovableList, LoroText, LoroValue, PeerID, StyleConfig, ValueOrContainer,
  VersionVector,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ActorId, COLLAB_SCHEMA_VERSION, CollabError, CollabResult, DocumentId, FormatKind, Role, blake3_hash};

const ROOT_MAP: &str = "flowstate";
const KEY_SCHEMA_VERSION: &str = "schema_version";
const KEY_FORMAT_KIND: &str = "format_kind";
const KEY_DOCUMENT_ID: &str = "document_id";
const KEY_CREATED_BY_ACTOR: &str = "created_by_actor";
const KEY_ROLE_POLICY: &str = "role_policy";
const KEY_SOURCE_MODEL: &str = "source_model";
const KEY_SOURCE_PAYLOAD: &str = "source_payload";
const KEY_SOURCE_PAYLOAD_HASH: &str = "source_payload_hash";
const KEY_PROJECTION_HASH: &str = "projection_hash";
const KEY_ASSET_MANIFEST: &str = "asset_manifest";
const KEY_ASSET_MANIFEST_HASH: &str = "asset_manifest_hash";
const KEY_GRANULAR_METADATA: &str = "granular_metadata";
const KEY_GRANULAR_ORDERS: &str = "granular_orders";
const KEY_GRANULAR_TEXTS: &str = "granular_texts";
const KEY_GRANULAR_BINARIES: &str = "granular_binaries";
const KEY_RECORD_METADATA: &str = "metadata";
const KEY_RECORD_TEXT: &str = "text";

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct CollabRolePolicy {
  pub owner: ActorId,
  pub editors: Vec<ActorId>,
  pub viewers: Vec<ActorId>,
}

impl CollabRolePolicy {
  #[must_use]
  pub fn owner_only(owner: ActorId) -> Self {
    Self {
      owner,
      editors: Vec::new(),
      viewers: Vec::new(),
    }
  }

  #[must_use]
  pub fn role_for_actor(&self, actor_id: ActorId) -> Option<Role> {
    if actor_id == self.owner {
      Some(Role::Owner)
    } else if self.editors.contains(&actor_id) {
      Some(Role::Editor)
    } else if self.viewers.contains(&actor_id) {
      Some(Role::Viewer)
    } else {
      None
    }
  }

  #[must_use]
  pub fn grants(&self, actor_id: ActorId, requested: Role) -> bool {
    self
      .role_for_actor(actor_id)
      .is_some_and(|granted| role_includes(granted, requested))
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum SourceModel {
  ProjectionPayload,
  GranularRecords,
}

impl SourceModel {
  const fn code(self) -> i64 {
    match self {
      Self::ProjectionPayload => 1,
      Self::GranularRecords => 2,
    }
  }

  fn from_code(code: i64) -> CollabResult<Self> {
    match code {
      1 => Ok(Self::ProjectionPayload),
      2 => Ok(Self::GranularRecords),
      _ => Err(CollabError::InvalidSchema(KEY_SOURCE_MODEL)),
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum GranularValue {
  Bool(bool),
  I64(i64),
  String(String),
}

impl GranularValue {
  fn from_loro(value: &LoroValue) -> Option<Self> {
    match value {
      LoroValue::Bool(value) => Some(Self::Bool(*value)),
      LoroValue::I64(value) => Some(Self::I64(*value)),
      LoroValue::String(value) => Some(Self::String(value.to_string())),
      _ => None,
    }
  }

  fn into_loro(self) -> LoroValue {
    match self {
      Self::Bool(value) => value.into(),
      Self::I64(value) => value.into(),
      Self::String(value) => value.into(),
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GranularTextMark {
  pub start_utf8: usize,
  pub end_utf8: usize,
  pub key: String,
  pub value: GranularValue,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GranularTextRecord {
  pub id: String,
  pub text: String,
  pub metadata: Vec<u8>,
  pub marks: Vec<GranularTextMark>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GranularBinaryRecord {
  pub id: String,
  pub metadata: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GranularOrderRecord {
  pub name: String,
  pub ids: Vec<String>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct GranularSource {
  pub metadata: Vec<u8>,
  pub orders: Vec<GranularOrderRecord>,
  pub texts: Vec<GranularTextRecord>,
  pub binaries: Vec<GranularBinaryRecord>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum GranularSourceMutation {
  InsertText {
    text_id: String,
    byte_offset: usize,
    text: String,
  },
  DeleteText {
    text_id: String,
    byte_offset: usize,
    byte_len: usize,
  },
  MarkText {
    text_id: String,
    range: Range<usize>,
    key: String,
    value: GranularValue,
  },
  UnmarkText {
    text_id: String,
    range: Range<usize>,
    key: String,
  },
  SetTextMetadata {
    text_id: String,
    metadata: Vec<u8>,
  },
  ClearTextMetadata {
    text_id: String,
  },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct GranularSourceMerkleHash {
  pub metadata_hash: [u8; 32],
  pub order_hashes: Vec<(String, [u8; 32])>,
  pub text_hashes: Vec<(String, [u8; 32])>,
  pub binary_hashes: Vec<(String, [u8; 32])>,
  pub root_hash: [u8; 32],
}

impl GranularSource {
  #[must_use]
  pub fn canonicalized(mut self) -> Self {
    self
      .orders
      .sort_by(|left, right| left.name.cmp(&right.name));
    self.texts.sort_by(|left, right| left.id.cmp(&right.id));
    self.binaries.sort_by(|left, right| left.id.cmp(&right.id));
    for text in &mut self.texts {
      text
        .marks
        .sort_by(|left, right| (left.start_utf8, left.end_utf8, &left.key).cmp(&(right.start_utf8, right.end_utf8, &right.key)));
    }
    self
  }

  pub fn merkle_hash(&self) -> CollabResult<GranularSourceMerkleHash> {
    let source = self.clone().canonicalized();
    let metadata_hash = blake3_hash(&source.metadata);
    let mut order_hashes = Vec::with_capacity(source.orders.len());
    let mut text_hashes = Vec::with_capacity(source.texts.len());
    let mut binary_hashes = Vec::with_capacity(source.binaries.len());
    let mut root_bytes = Vec::new();
    root_bytes.extend_from_slice(b"flowstate-db8-merkle-v1");
    root_bytes.extend_from_slice(&metadata_hash);

    for order in &source.orders {
      let bytes = postcard::to_stdvec(order)?;
      let hash = blake3_hash(&bytes);
      root_bytes.extend_from_slice(order.name.as_bytes());
      root_bytes.extend_from_slice(&hash);
      order_hashes.push((order.name.clone(), hash));
    }
    for text in &source.texts {
      let bytes = postcard::to_stdvec(text)?;
      let hash = blake3_hash(&bytes);
      root_bytes.extend_from_slice(text.id.as_bytes());
      root_bytes.extend_from_slice(&hash);
      text_hashes.push((text.id.clone(), hash));
    }
    for binary in &source.binaries {
      let bytes = postcard::to_stdvec(binary)?;
      let hash = blake3_hash(&bytes);
      root_bytes.extend_from_slice(binary.id.as_bytes());
      root_bytes.extend_from_slice(&hash);
      binary_hashes.push((binary.id.clone(), hash));
    }

    Ok(GranularSourceMerkleHash {
      metadata_hash,
      order_hashes,
      text_hashes,
      binary_hashes,
      root_hash: blake3_hash(&root_bytes),
    })
  }

  fn source_hash(&self) -> CollabResult<[u8; 32]> {
    self.merkle_hash().map(|hash| hash.root_hash)
  }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SourceProvenance {
  pub source_hash: [u8; 32],
  pub frontier: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct ProjectionCacheProvenance {
  pub source_hash: [u8; 32],
  pub projection_cache_hash: [u8; 32],
  pub frontier: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollabMaterializationSizes {
  pub projection_cache: usize,
  pub asset_manifest: usize,
  pub frontier: usize,
}

impl ProjectionCacheProvenance {
  #[must_use]
  pub fn can_reuse_for(&self, source_hash: [u8; 32], frontier: &[u8]) -> bool {
    self.source_hash == source_hash && self.frontier.as_slice() == frontier
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ProjectionCacheRecovery {
  Reused,
  Rebuilt,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct GranularMaterialization {
  pub source: GranularSource,
  pub recovery: ProjectionCacheRecovery,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollabProjectionPatch {
  pub old_projection_hash: [u8; 32],
  pub new_projection_hash: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CollabImportOutcome {
  pub patch: Option<CollabProjectionPatch>,
  pub frontier: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct CollabDocument {
  doc: LoroDoc,
  format_kind: FormatKind,
  document_id: DocumentId,
  source_model: SourceModel,
}

impl CollabDocument {
  pub fn from_projection_source(
    format_kind: FormatKind,
    document_id: DocumentId,
    created_by_actor: ActorId,
    projection_cache: &[u8],
    asset_manifest: &[u8],
  ) -> CollabResult<Self> {
    let doc = LoroDoc::new();
    configure_granular_text_styles(&doc);
    configure_peer_id(&doc, created_by_actor)?;
    let role_policy = CollabRolePolicy::owner_only(created_by_actor);
    initialize_projection_root(
      &doc,
      format_kind,
      document_id,
      created_by_actor,
      &role_policy,
      projection_cache,
      asset_manifest,
    )?;
    Self::from_doc(doc, Some(format_kind), Some(document_id))
  }

  pub fn from_granular_source(
    format_kind: FormatKind,
    document_id: DocumentId,
    created_by_actor: ActorId,
    source: &GranularSource,
    projection_cache: &[u8],
    asset_manifest: &[u8],
  ) -> CollabResult<Self> {
    let doc = LoroDoc::new();
    configure_granular_text_styles(&doc);
    configure_peer_id(&doc, created_by_actor)?;
    let role_policy = CollabRolePolicy::owner_only(created_by_actor);
    initialize_granular_root(
      &doc,
      format_kind,
      document_id,
      created_by_actor,
      &role_policy,
      source,
      projection_cache,
      asset_manifest,
    )?;
    Self::from_doc(doc, Some(format_kind), Some(document_id))
  }

  pub fn from_snapshot(snapshot: &[u8], expected_format: Option<FormatKind>, expected_document_id: Option<DocumentId>) -> CollabResult<Self> {
    let doc = LoroDoc::from_snapshot(snapshot).map_err(|error| CollabError::Loro(error.to_string()))?;
    configure_granular_text_styles(&doc);
    Self::from_doc(doc, expected_format, expected_document_id)
  }

  fn from_doc(doc: LoroDoc, expected_format: Option<FormatKind>, expected_document_id: Option<DocumentId>) -> CollabResult<Self> {
    let schema = validate_schema(&doc, expected_format, expected_document_id)?;
    Ok(Self {
      doc,
      format_kind: schema.format_kind,
      document_id: schema.document_id,
      source_model: schema.source_model,
    })
  }

  #[must_use]
  pub const fn format_kind(&self) -> FormatKind {
    self.format_kind
  }

  #[must_use]
  pub const fn document_id(&self) -> DocumentId {
    self.document_id
  }

  #[must_use]
  pub const fn is_granular(&self) -> bool {
    matches!(self.source_model, SourceModel::GranularRecords)
  }

  pub fn set_local_actor(&self, actor_id: ActorId) -> CollabResult<()> {
    configure_peer_id(&self.doc, actor_id)
  }

  pub fn peer_id(&self) -> PeerID {
    self.doc.peer_id()
  }
  pub fn source_hash(&self) -> CollabResult<[u8; 32]> {
    if self.is_granular() {
      read_granular_source(&self.doc)?.source_hash()
    } else {
      root_hash(&self.doc, KEY_SOURCE_PAYLOAD_HASH).or_else(|_| root_hash(&self.doc, KEY_PROJECTION_HASH))
    }
  }

  pub fn granular_merkle_hash(&self) -> CollabResult<Option<GranularSourceMerkleHash>> {
    if self.is_granular() {
      read_granular_source(&self.doc)?.merkle_hash().map(Some)
    } else {
      Ok(None)
    }
  }

  pub fn projection_cache_hash(&self) -> CollabResult<[u8; 32]> {
    root_hash(&self.doc, KEY_SOURCE_PAYLOAD_HASH).or_else(|_| root_hash(&self.doc, KEY_PROJECTION_HASH))
  }

  pub fn source_provenance(&self) -> CollabResult<SourceProvenance> {
    Ok(SourceProvenance {
      source_hash: self.source_hash()?,
      frontier: self.frontier()?,
    })
  }

  pub fn projection_cache_provenance(&self) -> CollabResult<ProjectionCacheProvenance> {
    Ok(ProjectionCacheProvenance {
      source_hash: self.source_hash()?,
      projection_cache_hash: self.projection_cache_hash()?,
      frontier: self.frontier()?,
    })
  }

  pub fn can_reuse_projection_cache(&self, provenance: &ProjectionCacheProvenance) -> CollabResult<bool> {
    let source_hash = self.source_hash()?;
    if provenance.source_hash != source_hash {
      return Ok(false);
    }
    Ok(provenance.frontier.as_slice() == self.frontier()?.as_slice())
  }

  pub fn projection_hash(&self) -> CollabResult<[u8; 32]> {
    self.source_hash()
  }

  pub fn frontier(&self) -> CollabResult<Vec<u8>> {
    postcard::to_stdvec(&self.doc.oplog_vv()).map_err(Into::into)
  }

  pub fn asset_manifest_bytes(&self) -> CollabResult<Vec<u8>> {
    root_binary(&self.doc, KEY_ASSET_MANIFEST)
  }

  pub fn materialize_projection_cache(&self) -> CollabResult<Vec<u8>> {
    root_binary(&self.doc, KEY_SOURCE_PAYLOAD)
  }

  pub fn materialization_sizes(&self) -> CollabResult<CollabMaterializationSizes> {
    Ok(CollabMaterializationSizes {
      projection_cache: self.materialize_projection_cache()?.len(),
      asset_manifest: self.asset_manifest_bytes()?.len(),
      frontier: self.frontier()?.len(),
    })
  }

  pub fn materialize_granular_source(&self) -> CollabResult<Option<GranularSource>> {
    self
      .materialize_granular_source_with_recovery()
      .map(|result| result.map(|materialized| materialized.source))
  }

  pub fn materialize_granular_source_with_recovery(&self) -> CollabResult<Option<GranularMaterialization>> {
    if !self.is_granular() {
      return Ok(None);
    }
    Ok(Some(GranularMaterialization {
      source: read_granular_source(&self.doc)?,
      recovery: ProjectionCacheRecovery::Reused,
    }))
  }

  pub fn export_snapshot(&self) -> CollabResult<Vec<u8>> {
    self
      .doc
      .export(ExportMode::Snapshot)
      .map_err(|error| CollabError::Loro(error.to_string()))
  }

  pub fn export_update_since_frontier(&self, encoded_frontier: &[u8]) -> CollabResult<Vec<u8>> {
    let frontier = if encoded_frontier.is_empty() {
      VersionVector::default()
    } else {
      postcard::from_bytes(encoded_frontier)?
    };
    self
      .doc
      .export(ExportMode::updates(&frontier))
      .map_err(|error| CollabError::Loro(error.to_string()))
  }

  pub fn replace_projection_source(&self, role: Role, projection_cache: &[u8], asset_manifest: &[u8]) -> CollabResult<Vec<u8>> {
    require_writer(role)?;
    let before = self.doc.oplog_vv();
    write_projection_payload(&self.doc, projection_cache, asset_manifest)?;
    self.doc.commit();
    validate_schema(&self.doc, Some(self.format_kind), Some(self.document_id))?;
    self
      .doc
      .export(ExportMode::updates(&before))
      .map_err(|error| CollabError::Loro(error.to_string()))
  }

  pub fn replace_granular_source(
    &self,
    role: Role,
    source: &GranularSource,
    projection_cache: &[u8],
    asset_manifest: &[u8],
  ) -> CollabResult<Vec<u8>> {
    require_writer(role)?;
    let before = self.doc.oplog_vv();
    let current_source = self.materialize_granular_source()?;
    write_source_model(&self.doc, SourceModel::GranularRecords)?;
    write_projection_payload(&self.doc, projection_cache, asset_manifest)?;
    if let Some(current_source) = current_source {
      merge_granular_source(&self.doc, &current_source, source)?;
    } else {
      write_granular_source(&self.doc, source)?;
    }
    self.doc.commit();
    validate_schema(&self.doc, Some(self.format_kind), Some(self.document_id))?;
    self
      .doc
      .export(ExportMode::updates(&before))
      .map_err(|error| CollabError::Loro(error.to_string()))
  }

  pub fn apply_granular_source_mutations(&self, role: Role, mutations: &[GranularSourceMutation]) -> CollabResult<Vec<u8>> {
    require_writer(role)?;
    if mutations.is_empty() {
      return Ok(Vec::new());
    }
    let before = self.doc.oplog_vv();
    let mut text_cache = HashMap::new();
    for mutation in mutations {
      self.apply_granular_source_mutation_uncommitted(mutation, &mut text_cache)?;
    }
    self.doc.commit();
    validate_schema(&self.doc, Some(self.format_kind), Some(self.document_id))?;
    self
      .doc
      .export(ExportMode::updates(&before))
      .map_err(|error| CollabError::Loro(error.to_string()))
  }

  pub fn apply_granular_source_mutation(&self, role: Role, mutation: &GranularSourceMutation) -> CollabResult<Vec<u8>> {
    self.apply_granular_source_mutations(role, std::slice::from_ref(mutation))
  }

  pub fn insert_granular_text_utf8(&self, role: Role, text_id: &str, byte_offset: usize, text: &str) -> CollabResult<Vec<u8>> {
    require_writer(role)?;
    let before = self.doc.oplog_vv();
    let text_container = granular_text_container(&self.doc, text_id)?;
    validate_utf8_offset(&text_container.to_string(), byte_offset)?;
    text_container
      .insert_utf8(byte_offset, text)
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    self.doc.commit();
    validate_schema(&self.doc, Some(self.format_kind), Some(self.document_id))?;
    self
      .doc
      .export(ExportMode::updates(&before))
      .map_err(|error| CollabError::Loro(error.to_string()))
  }

  pub fn delete_granular_text_utf8(&self, role: Role, text_id: &str, byte_offset: usize, byte_len: usize) -> CollabResult<Vec<u8>> {
    require_writer(role)?;
    let before = self.doc.oplog_vv();
    let text_container = granular_text_container(&self.doc, text_id)?;
    let text_snapshot = text_container.to_string();
    let Some(byte_end) = byte_offset.checked_add(byte_len) else {
      return Err(CollabError::InvalidSchema("granular text range"));
    };
    validate_utf8_range(&text_snapshot, byte_offset..byte_end)?;
    text_container
      .delete_utf8(byte_offset, byte_len)
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    self.doc.commit();
    validate_schema(&self.doc, Some(self.format_kind), Some(self.document_id))?;
    self
      .doc
      .export(ExportMode::updates(&before))
      .map_err(|error| CollabError::Loro(error.to_string()))
  }

  pub fn mark_granular_text_utf8(
    &self,
    role: Role,
    text_id: &str,
    range: Range<usize>,
    key: &str,
    value: GranularValue,
  ) -> CollabResult<Vec<u8>> {
    require_writer(role)?;
    let before = self.doc.oplog_vv();
    let text_container = granular_text_container(&self.doc, text_id)?;
    let text_snapshot = text_container.to_string();
    validate_utf8_range(&text_snapshot, range.clone())?;
    text_container
      .mark_utf8(range, key, value.into_loro())
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    self.doc.commit();
    validate_schema(&self.doc, Some(self.format_kind), Some(self.document_id))?;
    self
      .doc
      .export(ExportMode::updates(&before))
      .map_err(|error| CollabError::Loro(error.to_string()))
  }

  pub fn unmark_granular_text_utf8(&self, role: Role, text_id: &str, range: Range<usize>, key: &str) -> CollabResult<Vec<u8>> {
    require_writer(role)?;
    let before = self.doc.oplog_vv();
    let text_container = granular_text_container(&self.doc, text_id)?;
    let text_snapshot = text_container.to_string();
    let unicode_range = utf8_range_to_unicode_range(&text_snapshot, range)?;
    text_container
      .unmark(unicode_range, key)
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    self.doc.commit();
    validate_schema(&self.doc, Some(self.format_kind), Some(self.document_id))?;
    self
      .doc
      .export(ExportMode::updates(&before))
      .map_err(|error| CollabError::Loro(error.to_string()))
  }

  pub fn set_granular_text_metadata(&self, role: Role, text_id: &str, metadata: &[u8]) -> CollabResult<Vec<u8>> {
    require_writer(role)?;
    let before = self.doc.oplog_vv();
    granular_text_record_map(&self.doc, text_id)?
      .insert(KEY_RECORD_METADATA, metadata)
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    self.doc.commit();
    validate_schema(&self.doc, Some(self.format_kind), Some(self.document_id))?;
    self
      .doc
      .export(ExportMode::updates(&before))
      .map_err(|error| CollabError::Loro(error.to_string()))
  }

  pub fn clear_granular_text_metadata(&self, role: Role, text_id: &str) -> CollabResult<Vec<u8>> {
    require_writer(role)?;
    let before = self.doc.oplog_vv();
    granular_text_record_map(&self.doc, text_id)?
      .delete(KEY_RECORD_METADATA)
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    self.doc.commit();
    validate_schema(&self.doc, Some(self.format_kind), Some(self.document_id))?;
    self
      .doc
      .export(ExportMode::updates(&before))
      .map_err(|error| CollabError::Loro(error.to_string()))
  }

  pub fn replace_granular_order(&self, role: Role, name: &str, ids: &[String]) -> CollabResult<Vec<u8>> {
    require_writer(role)?;
    let before = self.doc.oplog_vv();
    let orders = granular_orders_map(&self.doc)?;
    let order = orders
      .get_or_create_container(name, LoroMovableList::new())
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    order
      .clear()
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    for id in ids {
      order
        .push(id.as_str())
        .map_err(|error| CollabError::Loro(error.to_string()))?;
    }
    self.doc.commit();
    validate_schema(&self.doc, Some(self.format_kind), Some(self.document_id))?;
    self
      .doc
      .export(ExportMode::updates(&before))
      .map_err(|error| CollabError::Loro(error.to_string()))
  }

  fn cached_granular_text_container(&self, cache: &mut HashMap<String, LoroText>, text_id: &str) -> CollabResult<LoroText> {
    if let Some(text) = cache.get(text_id) {
      return Ok(text.clone());
    }
    let text = granular_text_container(&self.doc, text_id)?;
    cache.insert(text_id.to_string(), text.clone());
    Ok(text)
  }

  fn apply_granular_source_mutation_uncommitted(
    &self,
    mutation: &GranularSourceMutation,
    text_cache: &mut HashMap<String, LoroText>,
  ) -> CollabResult<()> {
    match mutation {
      GranularSourceMutation::InsertText { text_id, byte_offset, text } => {
        let text_container = self.cached_granular_text_container(text_cache, text_id)?;
        validate_utf8_offset(&text_container.to_string(), *byte_offset)?;
        text_container
          .insert_utf8(*byte_offset, text)
          .map_err(|error| CollabError::Loro(error.to_string()))?;
      },
      GranularSourceMutation::DeleteText {
        text_id,
        byte_offset,
        byte_len,
      } => {
        let text_container = self.cached_granular_text_container(text_cache, text_id)?;
        if byte_offset.checked_add(*byte_len).is_none() {
          return Err(CollabError::InvalidSchema("granular text range"));
        }
        text_container
          .delete_utf8(*byte_offset, *byte_len)
          .map_err(|error| CollabError::Loro(error.to_string()))?;
      },
      GranularSourceMutation::MarkText { text_id, range, key, value } => {
        let text_container = self.cached_granular_text_container(text_cache, text_id)?;
        let text_snapshot = text_container.to_string();
        validate_utf8_range(&text_snapshot, range.clone())?;
        text_container
          .mark_utf8(range.clone(), key, value.clone().into_loro())
          .map_err(|error| CollabError::Loro(error.to_string()))?;
      },
      GranularSourceMutation::UnmarkText { text_id, range, key } => {
        let text_container = self.cached_granular_text_container(text_cache, text_id)?;
        let text_snapshot = text_container.to_string();
        let unicode_range = utf8_range_to_unicode_range(&text_snapshot, range.clone())?;
        text_container
          .unmark(unicode_range, key)
          .map_err(|error| CollabError::Loro(error.to_string()))?;
      },
      GranularSourceMutation::SetTextMetadata { text_id, metadata } => {
        granular_text_record_map(&self.doc, text_id)?
          .insert(KEY_RECORD_METADATA, metadata.as_slice())
          .map_err(|error| CollabError::Loro(error.to_string()))?;
      },
      GranularSourceMutation::ClearTextMetadata { text_id } => {
        granular_text_record_map(&self.doc, text_id)?
          .delete(KEY_RECORD_METADATA)
          .map_err(|error| CollabError::Loro(error.to_string()))?;
      },
    }
    Ok(())
  }

  pub fn import_update_checked(&self, remote_role: Role, update: &[u8]) -> CollabResult<CollabImportOutcome> {
    require_writer(remote_role)?;

    let before_frontier = self.frontier()?;
    self
      .doc
      .import(update)
      .map_err(|error| CollabError::Loro(error.to_string()))?;

    #[cfg(debug_assertions)]
    validate_schema(&self.doc, Some(self.format_kind), Some(self.document_id))?;

    let after_frontier = self.frontier()?;
    let patch = (before_frontier != after_frontier).then_some(CollabProjectionPatch {
      old_projection_hash: [0; 32],
      new_projection_hash: [0; 32],
    });
    Ok(CollabImportOutcome {
      patch,
      frontier: after_frontier,
    })
  }

  #[must_use]
  pub fn shared_doc(&self) -> LoroDoc {
    self.doc.clone()
  }
}

#[derive(Clone, Debug)]
pub struct Db8CollabDocument {
  inner: CollabDocument,
}

impl Db8CollabDocument {
  pub fn from_projection_source(
    document_id: DocumentId,
    created_by_actor: ActorId,
    projection_cache: &[u8],
    asset_manifest: &[u8],
  ) -> CollabResult<Self> {
    Ok(Self {
      inner: CollabDocument::from_projection_source(FormatKind::Db8, document_id, created_by_actor, projection_cache, asset_manifest)?,
    })
  }

  pub fn from_granular_source(
    document_id: DocumentId,
    created_by_actor: ActorId,
    source: &GranularSource,
    projection_cache: &[u8],
    asset_manifest: &[u8],
  ) -> CollabResult<Self> {
    Ok(Self {
      inner: CollabDocument::from_granular_source(FormatKind::Db8, document_id, created_by_actor, source, projection_cache, asset_manifest)?,
    })
  }

  #[must_use]
  pub const fn inner(&self) -> &CollabDocument {
    &self.inner
  }

  pub fn into_inner(self) -> CollabDocument {
    self.inner
  }

  pub fn export_snapshot(&self) -> CollabResult<Vec<u8>> {
    self.inner.export_snapshot()
  }
}

#[derive(Clone, Debug)]
pub struct Fl0CollabDocument {
  inner: CollabDocument,
}

impl Fl0CollabDocument {
  pub fn from_projection_source(
    document_id: DocumentId,
    created_by_actor: ActorId,
    projection_cache: &[u8],
    asset_manifest: &[u8],
  ) -> CollabResult<Self> {
    Ok(Self {
      inner: CollabDocument::from_projection_source(FormatKind::Fl0, document_id, created_by_actor, projection_cache, asset_manifest)?,
    })
  }

  pub fn from_granular_source(
    document_id: DocumentId,
    created_by_actor: ActorId,
    source: &GranularSource,
    projection_cache: &[u8],
    asset_manifest: &[u8],
  ) -> CollabResult<Self> {
    Ok(Self {
      inner: CollabDocument::from_granular_source(FormatKind::Fl0, document_id, created_by_actor, source, projection_cache, asset_manifest)?,
    })
  }

  #[must_use]
  pub const fn inner(&self) -> &CollabDocument {
    &self.inner
  }

  pub fn into_inner(self) -> CollabDocument {
    self.inner
  }

  pub fn export_snapshot(&self) -> CollabResult<Vec<u8>> {
    self.inner.export_snapshot()
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ValidatedSchema {
  format_kind: FormatKind,
  document_id: DocumentId,
  source_model: SourceModel,
}

fn initialize_projection_root(
  doc: &LoroDoc,
  format_kind: FormatKind,
  document_id: DocumentId,
  created_by_actor: ActorId,
  role_policy: &CollabRolePolicy,
  projection_cache: &[u8],
  asset_manifest: &[u8],
) -> CollabResult<()> {
  initialize_common_root(doc, format_kind, document_id, created_by_actor, role_policy)?;
  write_source_model(doc, SourceModel::ProjectionPayload)?;
  write_projection_payload(doc, projection_cache, asset_manifest)?;
  doc.commit();
  Ok(())
}

fn initialize_granular_root(
  doc: &LoroDoc,
  format_kind: FormatKind,
  document_id: DocumentId,
  created_by_actor: ActorId,
  role_policy: &CollabRolePolicy,
  source: &GranularSource,
  projection_cache: &[u8],
  asset_manifest: &[u8],
) -> CollabResult<()> {
  initialize_common_root(doc, format_kind, document_id, created_by_actor, role_policy)?;
  write_source_model(doc, SourceModel::GranularRecords)?;
  write_projection_payload(doc, projection_cache, asset_manifest)?;
  write_granular_source(doc, source)?;
  doc.commit();
  Ok(())
}

fn initialize_common_root(
  doc: &LoroDoc,
  format_kind: FormatKind,
  document_id: DocumentId,
  created_by_actor: ActorId,
  role_policy: &CollabRolePolicy,
) -> CollabResult<()> {
  let root = doc.get_map(ROOT_MAP);
  root
    .insert(KEY_SCHEMA_VERSION, i64::from(COLLAB_SCHEMA_VERSION))
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  root
    .insert(KEY_FORMAT_KIND, i64::from(format_kind.as_u8()))
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  root
    .insert(KEY_DOCUMENT_ID, document_id.0.as_bytes().as_slice())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  root
    .insert(KEY_CREATED_BY_ACTOR, created_by_actor.0.as_bytes().as_slice())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  root
    .insert(KEY_ROLE_POLICY, postcard::to_stdvec(role_policy)?)
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  Ok(())
}

fn write_source_model(doc: &LoroDoc, model: SourceModel) -> CollabResult<()> {
  doc
    .get_map(ROOT_MAP)
    .insert(KEY_SOURCE_MODEL, model.code())
    .map_err(|error| CollabError::Loro(error.to_string()))
}

fn write_projection_payload(doc: &LoroDoc, projection_cache: &[u8], asset_manifest: &[u8]) -> CollabResult<()> {
  let root = doc.get_map(ROOT_MAP);
  let projection_hash = blake3_hash(projection_cache);
  root
    .insert(KEY_SOURCE_PAYLOAD, projection_cache)
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  root
    .insert(KEY_SOURCE_PAYLOAD_HASH, projection_hash.as_slice())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  root
    .insert(KEY_PROJECTION_HASH, projection_hash.as_slice())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  root
    .insert(KEY_ASSET_MANIFEST, asset_manifest)
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  root
    .insert(KEY_ASSET_MANIFEST_HASH, blake3_hash(asset_manifest).as_slice())
    .map_err(|error| CollabError::Loro(error.to_string()))
}

fn validate_schema(
  doc: &LoroDoc,
  expected_format: Option<FormatKind>,
  expected_document_id: Option<DocumentId>,
) -> CollabResult<ValidatedSchema> {
  let schema_version = root_i64(doc, KEY_SCHEMA_VERSION)?;
  if schema_version != i64::from(COLLAB_SCHEMA_VERSION) {
    return Err(CollabError::UnsupportedCollabSchema(u32::try_from(schema_version).unwrap_or(u32::MAX)));
  }

  let format_kind = FormatKind::from_u8(u8::try_from(root_i64(doc, KEY_FORMAT_KIND)?).unwrap_or(u8::MAX))?;
  if expected_format.is_some_and(|expected| expected != format_kind) {
    return Err(CollabError::InvalidSchema("Flowstate Loro format kind mismatch"));
  }

  let document_id = root_uuid(doc, KEY_DOCUMENT_ID).map(DocumentId)?;
  if expected_document_id.is_some_and(|expected| expected != document_id) {
    return Err(CollabError::InvalidSchema("Flowstate Loro document ID mismatch"));
  }

  let source_model = SourceModel::from_code(root_i64(doc, KEY_SOURCE_MODEL)?)?;
  let source_payload = root_binary(doc, KEY_SOURCE_PAYLOAD)?;
  let source_payload_hash = root_hash(doc, KEY_SOURCE_PAYLOAD_HASH).or_else(|_| root_hash(doc, KEY_PROJECTION_HASH))?;
  if blake3_hash(&source_payload) != source_payload_hash {
    return Err(CollabError::HashMismatch("Loro projection cache"));
  }
  let _ = root_hash(doc, KEY_PROJECTION_HASH).or_else(|_| root_hash(doc, KEY_SOURCE_PAYLOAD_HASH))?;
  let asset_manifest = root_binary(doc, KEY_ASSET_MANIFEST)?;
  let asset_manifest_hash = root_hash(doc, KEY_ASSET_MANIFEST_HASH)?;
  if blake3_hash(&asset_manifest) != asset_manifest_hash {
    return Err(CollabError::HashMismatch("Loro asset manifest"));
  }

  let role_policy: CollabRolePolicy = postcard::from_bytes(&root_binary(doc, KEY_ROLE_POLICY)?)?;
  if role_policy.role_for_actor(role_policy.owner) != Some(Role::Owner) {
    return Err(CollabError::InvalidSchema("Flowstate Loro role policy has no owner"));
  }
  let _ = root_binary(doc, KEY_CREATED_BY_ACTOR)?;

  if matches!(source_model, SourceModel::GranularRecords) {
    validate_granular_source(doc)?;
  }

  Ok(ValidatedSchema {
    format_kind,
    document_id,
    source_model,
  })
}

fn configure_granular_text_styles(doc: &LoroDoc) {
  doc.config_default_text_style(Some(StyleConfig { expand: ExpandType::After }));
}

fn validate_granular_source(doc: &LoroDoc) -> CollabResult<()> {
  let _ = root_binary(doc, KEY_GRANULAR_METADATA)?;
  let _ = granular_orders_map(doc)?;
  let _ = granular_texts_map(doc)?;
  let _ = granular_binaries_map(doc)?;
  Ok(())
}

fn write_granular_source(doc: &LoroDoc, source: &GranularSource) -> CollabResult<()> {
  let source = validate_granular_source_records(source)?.canonicalized();
  let root = doc.get_map(ROOT_MAP);
  root
    .insert(KEY_GRANULAR_METADATA, source.metadata)
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  write_granular_orders(&root, &source.orders)?;
  write_granular_texts(&root, &source.texts)?;
  write_granular_binaries(&root, &source.binaries)
}

fn merge_granular_source(doc: &LoroDoc, current: &GranularSource, target: &GranularSource) -> CollabResult<()> {
  let current = validate_granular_source_records(current)?.canonicalized();
  let target = validate_granular_source_records(target)?.canonicalized();
  let root = doc.get_map(ROOT_MAP);
  root
    .insert(KEY_GRANULAR_METADATA, target.metadata)
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  merge_granular_orders(&root, &current.orders, &target.orders)?;
  merge_granular_texts(&root, &current.texts, &target.texts)?;
  merge_granular_binaries(&root, &current.binaries, &target.binaries)
}

fn validate_granular_source_records(source: &GranularSource) -> CollabResult<GranularSource> {
  let mut source = source.clone();
  for record in &mut source.texts {
    validate_granular_text_record(record)?;
    record
      .marks
      .sort_by(|left, right| (left.start_utf8, left.end_utf8, &left.key).cmp(&(right.start_utf8, right.end_utf8, &right.key)));
  }
  source
    .orders
    .sort_by(|left, right| left.name.cmp(&right.name));
  source.texts.sort_by(|left, right| left.id.cmp(&right.id));
  source
    .binaries
    .sort_by(|left, right| left.id.cmp(&right.id));
  Ok(source)
}

fn validate_granular_text_record(record: &GranularTextRecord) -> CollabResult<()> {
  for mark in &record.marks {
    validate_utf8_range(&record.text, mark.start_utf8..mark.end_utf8).map_err(|_| CollabError::InvalidSchema("granular text marks"))?;
  }
  Ok(())
}

fn write_granular_orders(root: &LoroMap, orders: &[GranularOrderRecord]) -> CollabResult<()> {
  let orders_map = root
    .get_or_create_container(KEY_GRANULAR_ORDERS, LoroMap::new())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  let existing = orders_map
    .keys()
    .map(|key| key.to_string())
    .collect::<Vec<_>>();
  for key in existing {
    if let Some(ValueOrContainer::Container(Container::MovableList(order))) = orders_map.get(&key) {
      order
        .clear()
        .map_err(|error| CollabError::Loro(error.to_string()))?;
    }
    orders_map
      .delete(&key)
      .map_err(|error| CollabError::Loro(error.to_string()))?;
  }
  for order in orders {
    let list = orders_map
      .get_or_create_container(order.name.as_str(), LoroMovableList::new())
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    list
      .clear()
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    for id in &order.ids {
      list
        .push(id.as_str())
        .map_err(|error| CollabError::Loro(error.to_string()))?;
    }
  }
  Ok(())
}

fn merge_granular_orders(root: &LoroMap, current: &[GranularOrderRecord], target: &[GranularOrderRecord]) -> CollabResult<()> {
  let current_by_id = granular_records_by_id(current.to_vec(), |record| record.name.as_str());
  let target_by_id = granular_records_by_id(target.to_vec(), |record| record.name.as_str());
  let orders_map = root
    .get_or_create_container(KEY_GRANULAR_ORDERS, LoroMap::new())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  for name in current_by_id.keys() {
    if !target_by_id.contains_key(name) {
      if let Some(ValueOrContainer::Container(Container::MovableList(order))) = orders_map.get(name) {
        order
          .clear()
          .map_err(|error| CollabError::Loro(error.to_string()))?;
      }
      orders_map
        .delete(name)
        .map_err(|error| CollabError::Loro(error.to_string()))?;
    }
  }
  for (name, order) in target_by_id {
    if current_by_id.get(&name) == Some(&order) {
      continue;
    }
    let list = orders_map
      .get_or_create_container(name.as_str(), LoroMovableList::new())
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    list
      .clear()
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    for id in &order.ids {
      list
        .push(id.as_str())
        .map_err(|error| CollabError::Loro(error.to_string()))?;
    }
  }
  Ok(())
}

fn write_granular_texts(root: &LoroMap, texts: &[GranularTextRecord]) -> CollabResult<()> {
  let text_map = root
    .get_or_create_container(KEY_GRANULAR_TEXTS, LoroMap::new())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  let existing = text_map
    .keys()
    .map(|key| key.to_string())
    .collect::<Vec<_>>();
  for key in existing {
    text_map
      .delete(&key)
      .map_err(|error| CollabError::Loro(error.to_string()))?;
  }
  for record in texts {
    write_granular_text_record(&text_map, record)?;
  }
  Ok(())
}

fn write_granular_text_record(text_map: &LoroMap, record: &GranularTextRecord) -> CollabResult<()> {
  validate_granular_text_record(record)?;
  let record_map = text_map
    .get_or_create_container(record.id.as_str(), LoroMap::new())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  record_map
    .insert(KEY_RECORD_METADATA, record.metadata.as_slice())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  let text = record_map
    .get_or_create_container(KEY_RECORD_TEXT, LoroText::new())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  let current_len = text.to_string().len();
  if current_len > 0 {
    text
      .delete_utf8(0, current_len)
      .map_err(|error| CollabError::Loro(error.to_string()))?;
  }
  if !record.text.is_empty() {
    text
      .insert_utf8(0, &record.text)
      .map_err(|error| CollabError::Loro(error.to_string()))?;
  }
  for mark in &record.marks {
    text
      .mark_utf8(mark.start_utf8..mark.end_utf8, &mark.key, mark.value.clone().into_loro())
      .map_err(|error| CollabError::Loro(error.to_string()))?;
  }
  Ok(())
}

fn merge_granular_texts(root: &LoroMap, current: &[GranularTextRecord], target: &[GranularTextRecord]) -> CollabResult<()> {
  let current_by_id = granular_records_by_id(current.to_vec(), |record| record.id.as_str());
  let target_by_id = granular_records_by_id(target.to_vec(), |record| record.id.as_str());
  let text_map = root
    .get_or_create_container(KEY_GRANULAR_TEXTS, LoroMap::new())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  for id in current_by_id.keys() {
    if !target_by_id.contains_key(id) {
      text_map
        .delete(id)
        .map_err(|error| CollabError::Loro(error.to_string()))?;
    }
  }
  for (id, record) in target_by_id {
    match current_by_id.get(&id) {
      Some(current_record) if current_record == &record => {},
      Some(current_record) => merge_granular_text_record(&text_map, current_record, &record)?,
      None => write_granular_text_record(&text_map, &record)?,
    }
  }
  Ok(())
}

fn merge_granular_text_record(text_map: &LoroMap, current: &GranularTextRecord, target: &GranularTextRecord) -> CollabResult<()> {
  validate_granular_text_record(target)?;
  let record_map = text_map
    .get_or_create_container(target.id.as_str(), LoroMap::new())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  record_map
    .insert(KEY_RECORD_METADATA, target.metadata.as_slice())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  let text = record_map
    .get_or_create_container(KEY_RECORD_TEXT, LoroText::new())
    .map_err(|error| CollabError::Loro(error.to_string()))?;

  sync_granular_text_marks(&text, current, &[])?;
  sync_granular_text_content(&text, &current.text, &target.text)?;
  sync_granular_text_marks(
    &text,
    &GranularTextRecord {
      marks: Vec::new(),
      ..target.clone()
    },
    &target.marks,
  )
}

fn sync_granular_text_content(text: &LoroText, current: &str, target: &str) -> CollabResult<()> {
  if current == target {
    return Ok(());
  }
  let prefix_len = common_prefix_boundary(current, target);
  let suffix_len = common_suffix_boundary(&current[prefix_len..], &target[prefix_len..]);
  let current_delete_end = current.len().saturating_sub(suffix_len);
  if current_delete_end > prefix_len {
    text
      .delete_utf8(prefix_len, current_delete_end - prefix_len)
      .map_err(|error| CollabError::Loro(error.to_string()))?;
  }
  let target_insert_end = target.len().saturating_sub(suffix_len);
  if target_insert_end > prefix_len {
    text
      .insert_utf8(prefix_len, &target[prefix_len..target_insert_end])
      .map_err(|error| CollabError::Loro(error.to_string()))?;
  }
  Ok(())
}

fn sync_granular_text_marks(text: &LoroText, current: &GranularTextRecord, target_marks: &[GranularTextMark]) -> CollabResult<()> {
  for mark in &current.marks {
    if target_marks.iter().any(|target| target == mark) {
      continue;
    }
    text
      .unmark(utf8_range_to_unicode_range(&current.text, mark.start_utf8..mark.end_utf8)?, &mark.key)
      .map_err(|error| CollabError::Loro(error.to_string()))?;
  }
  for mark in target_marks {
    if current.marks.iter().any(|current| current == mark) {
      continue;
    }
    text
      .mark_utf8(mark.start_utf8..mark.end_utf8, &mark.key, mark.value.clone().into_loro())
      .map_err(|error| CollabError::Loro(error.to_string()))?;
  }
  Ok(())
}

fn common_prefix_boundary(left: &str, right: &str) -> usize {
  let mut boundary = 0;
  for ((left_ix, left_char), (right_ix, right_char)) in left.char_indices().zip(right.char_indices()) {
    if left_char != right_char {
      break;
    }
    boundary = left_ix + left_char.len_utf8();
    debug_assert_eq!(boundary, right_ix + right_char.len_utf8());
  }
  boundary
}

fn common_suffix_boundary(left: &str, right: &str) -> usize {
  let mut len = 0;
  for (left_char, right_char) in left.chars().rev().zip(right.chars().rev()) {
    if left_char != right_char {
      break;
    }
    len += left_char.len_utf8();
  }
  len
}

fn write_granular_binaries(root: &LoroMap, binaries: &[GranularBinaryRecord]) -> CollabResult<()> {
  let binary_map = root
    .get_or_create_container(KEY_GRANULAR_BINARIES, LoroMap::new())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  let existing = binary_map
    .keys()
    .map(|key| key.to_string())
    .collect::<Vec<_>>();
  for key in existing {
    binary_map
      .delete(&key)
      .map_err(|error| CollabError::Loro(error.to_string()))?;
  }
  for record in binaries {
    binary_map
      .insert(record.id.as_str(), record.metadata.as_slice())
      .map_err(|error| CollabError::Loro(error.to_string()))?;
  }
  Ok(())
}

fn merge_granular_binaries(root: &LoroMap, current: &[GranularBinaryRecord], target: &[GranularBinaryRecord]) -> CollabResult<()> {
  let current_by_id = granular_records_by_id(current.to_vec(), |record| record.id.as_str());
  let target_by_id = granular_records_by_id(target.to_vec(), |record| record.id.as_str());
  let binary_map = root
    .get_or_create_container(KEY_GRANULAR_BINARIES, LoroMap::new())
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  for id in current_by_id.keys() {
    if !target_by_id.contains_key(id) {
      binary_map
        .delete(id)
        .map_err(|error| CollabError::Loro(error.to_string()))?;
    }
  }
  for (id, record) in target_by_id {
    if current_by_id.get(&id) == Some(&record) {
      continue;
    }
    binary_map
      .insert(id.as_str(), record.metadata.as_slice())
      .map_err(|error| CollabError::Loro(error.to_string()))?;
  }
  Ok(())
}

fn read_granular_source(doc: &LoroDoc) -> CollabResult<GranularSource> {
  Ok(
    GranularSource {
      metadata: root_binary(doc, KEY_GRANULAR_METADATA)?,
      orders: read_granular_orders(doc)?,
      texts: read_granular_texts(doc)?,
      binaries: read_granular_binaries(doc)?,
    }
    .canonicalized(),
  )
}

fn read_granular_orders(doc: &LoroDoc) -> CollabResult<Vec<GranularOrderRecord>> {
  let orders = granular_orders_map(doc)?;
  let mut records = Vec::new();
  for name in orders.keys() {
    let name = name.to_string();
    let order = match orders.get(&name) {
      Some(ValueOrContainer::Container(Container::MovableList(order))) => order,
      _ => return Err(CollabError::InvalidSchema(KEY_GRANULAR_ORDERS)),
    };
    let ids = order
      .to_vec()
      .into_iter()
      .map(loro_string)
      .collect::<CollabResult<Vec<_>>>()?;
    records.push(GranularOrderRecord { name, ids });
  }
  records.sort_by(|left, right| left.name.cmp(&right.name));
  Ok(records)
}

fn read_granular_texts(doc: &LoroDoc) -> CollabResult<Vec<GranularTextRecord>> {
  let texts = granular_texts_map(doc)?;
  let mut records = Vec::new();
  for id in texts.keys() {
    let id = id.to_string();
    let record = match texts.get(&id) {
      Some(ValueOrContainer::Container(Container::Map(record))) => record,
      _ => return Err(CollabError::InvalidSchema(KEY_GRANULAR_TEXTS)),
    };
    let metadata = map_binary(&record, KEY_RECORD_METADATA)?;
    let text = match record.get(KEY_RECORD_TEXT) {
      Some(ValueOrContainer::Container(Container::Text(text))) => text,
      _ => return Err(CollabError::InvalidSchema(KEY_RECORD_TEXT)),
    };
    records.push(GranularTextRecord {
      id,
      text: text.to_string(),
      metadata,
      marks: text_marks(&text),
    });
  }
  records.sort_by(|left, right| left.id.cmp(&right.id));
  Ok(records)
}

fn read_granular_binaries(doc: &LoroDoc) -> CollabResult<Vec<GranularBinaryRecord>> {
  let binaries = granular_binaries_map(doc)?;
  let mut records = Vec::new();
  for id in binaries.keys() {
    let id = id.to_string();
    let metadata = match binaries.get(&id) {
      Some(ValueOrContainer::Value(LoroValue::Binary(value))) => value.unwrap(),
      _ => return Err(CollabError::InvalidSchema(KEY_GRANULAR_BINARIES)),
    };
    records.push(GranularBinaryRecord { id, metadata });
  }
  records.sort_by(|left, right| left.id.cmp(&right.id));
  Ok(records)
}

fn text_marks(text: &LoroText) -> Vec<GranularTextMark> {
  let mut offset = 0;
  let mut marks = Vec::new();
  for delta in text.to_delta() {
    let (insert, attributes) = match delta {
      loro::TextDelta::Insert { insert, attributes } => (insert, attributes),
      loro::TextDelta::Retain { retain, .. } | loro::TextDelta::Delete { delete: retain } => {
        offset += retain;
        continue;
      },
    };
    let start = offset;
    offset += insert.len();
    let Some(attributes) = attributes else {
      continue;
    };
    for (key, value) in attributes {
      if let Some(value) = GranularValue::from_loro(&value) {
        marks.push(GranularTextMark {
          start_utf8: start,
          end_utf8: offset,
          key,
          value,
        });
      }
    }
  }
  marks
}

fn granular_text_container(doc: &LoroDoc, text_id: &str) -> CollabResult<LoroText> {
  let texts = granular_texts_map(doc)?;
  let record = match texts.get(text_id) {
    Some(ValueOrContainer::Container(Container::Map(record))) => record,
    _ => return Err(CollabError::MissingRootValue(KEY_RECORD_TEXT)),
  };
  match record.get(KEY_RECORD_TEXT) {
    Some(ValueOrContainer::Container(Container::Text(text))) => Ok(text),
    _ => Err(CollabError::InvalidSchema(KEY_RECORD_TEXT)),
  }
}
fn granular_text_record_map(doc: &LoroDoc, text_id: &str) -> CollabResult<LoroMap> {
  let texts = granular_texts_map(doc)?;
  match texts.get(text_id) {
    Some(ValueOrContainer::Container(Container::Map(record))) => Ok(record),
    _ => Err(CollabError::MissingRootValue(KEY_RECORD_TEXT)),
  }
}

fn validate_utf8_offset(text: &str, offset: usize) -> CollabResult<()> {
  validate_utf8_range(text, offset..offset)
}

fn validate_utf8_range(text: &str, range: Range<usize>) -> CollabResult<()> {
  if range.start > range.end {
    return Err(CollabError::InvalidSchema("granular text range"));
  }
  let Some(end) = range
    .start
    .checked_add(range.end.saturating_sub(range.start))
  else {
    return Err(CollabError::InvalidSchema("granular text range"));
  };
  if end > text.len() || !text.is_char_boundary(range.start) || !text.is_char_boundary(range.end) {
    return Err(CollabError::InvalidSchema("granular text range"));
  }
  Ok(())
}

fn granular_orders_map(doc: &LoroDoc) -> CollabResult<LoroMap> {
  root_container_map(doc, KEY_GRANULAR_ORDERS)
}

fn granular_texts_map(doc: &LoroDoc) -> CollabResult<LoroMap> {
  root_container_map(doc, KEY_GRANULAR_TEXTS)
}

fn granular_binaries_map(doc: &LoroDoc) -> CollabResult<LoroMap> {
  root_container_map(doc, KEY_GRANULAR_BINARIES)
}

fn root_container_map(doc: &LoroDoc, key: &'static str) -> CollabResult<LoroMap> {
  let root = doc
    .try_get_map(ROOT_MAP)
    .ok_or(CollabError::MissingRootValue(ROOT_MAP))?;
  match root.get(key) {
    Some(ValueOrContainer::Container(Container::Map(map))) => Ok(map),
    _ => Err(CollabError::MissingRootValue(key)),
  }
}

fn root_value(doc: &LoroDoc, key: &'static str) -> CollabResult<LoroValue> {
  let root = doc
    .try_get_map(ROOT_MAP)
    .ok_or(CollabError::MissingRootValue(ROOT_MAP))?;
  match root.get(key) {
    Some(ValueOrContainer::Value(value)) => Ok(value),
    _ => Err(CollabError::MissingRootValue(key)),
  }
}

fn root_binary(doc: &LoroDoc, key: &'static str) -> CollabResult<Vec<u8>> {
  match root_value(doc, key)? {
    LoroValue::Binary(value) => Ok(value.unwrap()),
    _ => Err(CollabError::InvalidSchema(key)),
  }
}

fn map_binary(map: &LoroMap, key: &'static str) -> CollabResult<Vec<u8>> {
  match map.get(key) {
    Some(ValueOrContainer::Value(LoroValue::Binary(value))) => Ok(value.unwrap()),
    _ => Err(CollabError::InvalidSchema(key)),
  }
}

fn require_writer(role: Role) -> CollabResult<()> {
  if role == Role::Owner || role == Role::Editor {
    Ok(())
  } else {
    Err(CollabError::Unauthorized("writer role required"))
  }
}

fn loro_string(value: LoroValue) -> CollabResult<String> {
  match value {
    LoroValue::String(value) => Ok(value.to_string()),
    _ => Err(CollabError::InvalidSchema(KEY_GRANULAR_ORDERS)),
  }
}

fn root_i64(doc: &LoroDoc, key: &'static str) -> CollabResult<i64> {
  match root_value(doc, key)? {
    LoroValue::I64(value) => Ok(value),
    _ => Err(CollabError::InvalidSchema(key)),
  }
}

fn root_uuid(doc: &LoroDoc, key: &'static str) -> CollabResult<Uuid> {
  let bytes = root_binary(doc, key)?;
  Uuid::from_slice(&bytes).map_err(|_| CollabError::InvalidSchema(key))
}

fn root_hash(doc: &LoroDoc, key: &'static str) -> CollabResult<[u8; 32]> {
  let bytes = root_binary(doc, key)?;
  bytes
    .as_slice()
    .try_into()
    .map_err(|_| CollabError::InvalidSchema(key))
}

#[cfg(test)]
mod tests {
  use super::*;

  fn source_truth_fixture() -> CollabDocument {
    CollabDocument::from_projection_source(FormatKind::Db8, DocumentId::new(), ActorId::new(), b"projection", b"manifest").unwrap()
  }

  fn granular_source_fixture() -> CollabDocument {
    let actor = ActorId::new();
    let source = GranularSource {
      metadata: b"root".to_vec(),
      orders: vec![GranularOrderRecord {
        name: "paragraphs".to_string(),
        ids: vec!["p1".to_string()],
      }],
      texts: vec![GranularTextRecord {
        id: "p1".to_string(),
        text: "éa".to_string(),
        metadata: b"meta".to_vec(),
        marks: vec![GranularTextMark {
          start_utf8: 0,
          end_utf8: 2,
          key: "style".to_string(),
          value: GranularValue::String("bold".to_string()),
        }],
      }],
      binaries: vec![],
    };
    CollabDocument::from_granular_source(FormatKind::Db8, DocumentId::new(), actor, &source, b"projection", b"manifest").unwrap()
  }

  #[test]
  fn materialization_sizes_report_payload_lengths() {
    let doc = source_truth_fixture();
    let sizes = doc.materialization_sizes().unwrap();
    assert_eq!(sizes.projection_cache, doc.materialize_projection_cache().unwrap().len());
    assert_eq!(sizes.asset_manifest, doc.asset_manifest_bytes().unwrap().len());
    assert_eq!(sizes.frontier, doc.frontier().unwrap().len());
    assert!(sizes.projection_cache > 0);
  }

  #[test]
  fn granular_text_mutations_change_frontier_hash_and_export_updates() {
    let doc = granular_source_fixture();
    let before_frontier = doc.frontier().unwrap();
    let before_hash = doc.projection_hash().unwrap();

    let insert = doc
      .insert_granular_text_utf8(Role::Owner, "p1", 3, "!")
      .unwrap();
    assert!(!insert.is_empty());
    let after_insert_frontier = doc.frontier().unwrap();
    let after_insert_hash = doc.projection_hash().unwrap();
    assert_ne!(after_insert_frontier, before_frontier);
    assert_ne!(after_insert_hash, before_hash);

    let delete = doc
      .delete_granular_text_utf8(Role::Owner, "p1", 3, 1)
      .unwrap();
    assert!(!delete.is_empty());

    let mark = doc
      .mark_granular_text_utf8(Role::Owner, "p1", 0..2, "tone", GranularValue::Bool(true))
      .unwrap();
    assert!(!mark.is_empty());

    let unmark = doc
      .unmark_granular_text_utf8(Role::Owner, "p1", 0..2, "tone")
      .unwrap();
    assert!(!unmark.is_empty());

    let metadata = doc
      .set_granular_text_metadata(Role::Owner, "p1", b"updated")
      .unwrap();
    assert!(!metadata.is_empty());

    let cleared = doc.clear_granular_text_metadata(Role::Owner, "p1").unwrap();
    assert!(!cleared.is_empty());
  }
  #[test]
  fn granular_text_updates_refresh_source_provenance() {
    let left = granular_source_fixture();
    let right = CollabDocument::from_snapshot(&left.export_snapshot().unwrap(), Some(FormatKind::Db8), Some(left.document_id())).unwrap();
    let before = right.source_provenance().unwrap();

    let update = left
      .insert_granular_text_utf8(Role::Owner, "p1", 3, "!")
      .unwrap();
    let outcome = right.import_update_checked(Role::Editor, &update).unwrap();
    let after = right.source_provenance().unwrap();
    let materialized = right.materialize_granular_source().unwrap().unwrap();

    assert_ne!(after.source_hash, before.source_hash);
    assert_ne!(after.frontier, before.frontier);
    assert_eq!(outcome.frontier, after.frontier);
    assert_eq!(materialized.texts[0].text, "éa!");
  }

  #[test]
  fn granular_source_mutation_batches_export_one_convergent_update() {
    let left = granular_source_fixture();
    let right = CollabDocument::from_snapshot(&left.export_snapshot().unwrap(), Some(FormatKind::Db8), Some(left.document_id())).unwrap();

    let update = left
      .apply_granular_source_mutations(
        Role::Owner,
        &[
          GranularSourceMutation::InsertText {
            text_id: "p1".to_string(),
            byte_offset: 3,
            text: "!".to_string(),
          },
          GranularSourceMutation::MarkText {
            text_id: "p1".to_string(),
            range: 0..2,
            key: "tone".to_string(),
            value: GranularValue::Bool(true),
          },
        ],
      )
      .unwrap();
    assert!(!update.is_empty());

    right.import_update_checked(Role::Editor, &update).unwrap();
    let materialized = right.materialize_granular_source().unwrap().unwrap();
    assert_eq!(materialized.texts[0].text, "éa!");
    assert!(
      materialized.texts[0]
        .marks
        .iter()
        .any(|mark| mark.key == "tone")
    );
  }

  #[test]
  fn db8_granular_source_rejects_invalid_utf8_mark_range() {
    let source = GranularSource {
      metadata: vec![],
      orders: vec![],
      texts: vec![GranularTextRecord {
        id: "p1".to_string(),
        text: "éa".to_string(),
        metadata: vec![],
        marks: vec![GranularTextMark {
          start_utf8: 0,
          end_utf8: 1,
          key: "style".to_string(),
          value: GranularValue::Bool(true),
        }],
      }],
      binaries: vec![],
    };
    let error = validate_granular_source_records(&source).unwrap_err();
    assert!(matches!(error, CollabError::InvalidSchema("granular text marks")));
  }

  #[test]
  fn db8_granular_source_rejects_invalid_mark_order_range() {
    let source = GranularSource {
      metadata: vec![],
      orders: vec![],
      texts: vec![GranularTextRecord {
        id: "p1".to_string(),
        text: "abc".to_string(),
        metadata: vec![],
        marks: vec![GranularTextMark {
          start_utf8: 2,
          end_utf8: 1,
          key: "style".to_string(),
          value: GranularValue::Bool(true),
        }],
      }],
      binaries: vec![],
    };
    let error = validate_granular_source_records(&source).unwrap_err();
    assert!(matches!(error, CollabError::InvalidSchema("granular text marks")));
  }

  #[test]
  fn granular_text_mutations_reject_invalid_utf8_ranges() {
    let doc = granular_source_fixture();
    assert!(matches!(
      doc.insert_granular_text_utf8(Role::Owner, "p1", 1, "!"),
      Err(CollabError::InvalidSchema("granular text range"))
    ));
    assert!(matches!(
      doc.delete_granular_text_utf8(Role::Owner, "p1", 1, 1),
      Err(CollabError::InvalidSchema("granular text range"))
    ));
    assert!(matches!(
      doc.mark_granular_text_utf8(Role::Owner, "p1", 1..2, "tone", GranularValue::Bool(true)),
      Err(CollabError::InvalidSchema("granular text range"))
    ));
  }

  #[test]
  fn importing_the_same_update_twice_keeps_source_hash_stable() {
    let left = source_truth_fixture();
    let right = CollabDocument::from_snapshot(&left.export_snapshot().unwrap(), Some(FormatKind::Db8), Some(left.document_id())).unwrap();

    let update = left
      .replace_projection_source(Role::Owner, b"changed", b"manifest")
      .unwrap();
    let first = right.import_update_checked(Role::Editor, &update).unwrap();
    let hash_after_first = right.projection_hash().unwrap();
    let materialized_after_first = right.materialize_projection_cache().unwrap();

    let second = right.import_update_checked(Role::Editor, &update).unwrap();
    assert_eq!(right.projection_hash().unwrap(), hash_after_first);
    assert_eq!(right.materialize_projection_cache().unwrap(), materialized_after_first);
    assert!(second.patch.is_none());
    assert_eq!(first.frontier, second.frontier);
  }

  #[test]
  fn projection_cache_provenance_matches_current_source_and_frontier() {
    let doc = source_truth_fixture();
    let provenance = doc.projection_cache_provenance().unwrap();
    assert_eq!(provenance.source_hash, doc.source_hash().unwrap());
    assert_eq!(provenance.projection_cache_hash, doc.projection_cache_hash().unwrap());
    assert!(doc.can_reuse_projection_cache(&provenance).unwrap());
    assert_eq!(doc.projection_hash().unwrap(), doc.source_hash().unwrap());
  }

  #[test]
  fn projection_cache_reuse_rejects_stale_source_or_frontier() {
    let left = source_truth_fixture();
    let stale = left.projection_cache_provenance().unwrap();
    let right = CollabDocument::from_snapshot(&left.export_snapshot().unwrap(), Some(FormatKind::Db8), Some(left.document_id())).unwrap();
    assert!(right.can_reuse_projection_cache(&stale).unwrap());

    let update = right
      .replace_projection_source(Role::Owner, b"changed", b"manifest")
      .unwrap();
    let imported = left.import_update_checked(Role::Editor, &update).unwrap();
    assert!(!left.can_reuse_projection_cache(&stale).unwrap());
    assert_eq!(imported.frontier, left.frontier().unwrap());
  }

  #[test]
  #[ignore = "future-target: a no-op edit with only application metadata must not be acked as correctness-bearing source work"]
  fn accepted_empty_update_or_hint_metadata_must_not_count_as_ackable_source_change() {
    let doc = source_truth_fixture();
    let update = doc
      .replace_projection_source(Role::Owner, b"projection", b"manifest")
      .unwrap();
    let _ = update;
    let _ = doc.frontier().unwrap();
    let _ = doc.projection_hash().unwrap();
    panic!("source edits that only carry UI-hint metadata still need a distinct non-empty/update-or-hash signal");
  }

  #[test]
  #[ignore = "future-target: the import API should distinguish Duplicate from Applied"]
  fn import_update_checked_should_distinguish_duplicate_from_applied() {
    let doc = source_truth_fixture();
    let update = doc
      .replace_projection_source(Role::Owner, b"changed", b"manifest")
      .unwrap();
    let _ = doc.import_update_checked(Role::Editor, &update).unwrap();
    let _ = doc.import_update_checked(Role::Editor, &update).unwrap();
    panic!("duplicate imports still collapse into the same return shape");
  }

  #[test]
  #[ignore = "future-target: replica B should materialize A's acked edit directly from source truth"]
  fn replica_b_exports_materialized_edit_after_ack_without_projection_cache_fallback() {
    let left = source_truth_fixture();
    let snapshot = left.export_snapshot().unwrap();
    let right = CollabDocument::from_snapshot(&snapshot, Some(FormatKind::Db8), Some(left.document_id())).unwrap();
    let update = left
      .replace_projection_source(Role::Owner, b"acked-edit", b"manifest")
      .unwrap();
    let _ = right.import_update_checked(Role::Editor, &update).unwrap();
    panic!("the exported source path still needs a source-truth-only materialization assertion");
  }

  #[test]
  #[ignore = "future-target: corrupted or stale projection cache must not affect source truth and recovery should be reportable"]
  fn stale_projection_cache_must_not_change_source_truth_and_recovery_should_be_reportable() {
    let doc = source_truth_fixture();
    let _ = doc.export_snapshot().unwrap();
    let _ = doc.materialize_projection_cache().unwrap();
    panic!("projection cache corruption/staleness should become an explicit recoverable state");
  }
}
fn configure_peer_id(doc: &LoroDoc, actor_id: ActorId) -> CollabResult<()> {
  let mut bytes = [0; 8];
  bytes.copy_from_slice(&actor_id.0.as_bytes()[..8]);
  let mut peer_id = PeerID::from_le_bytes(bytes);
  if peer_id == 0 {
    peer_id = 1;
  }
  doc
    .set_peer_id(peer_id)
    .map_err(|error| CollabError::Loro(error.to_string()))
}

fn role_includes(granted: Role, requested: Role) -> bool {
  matches!(
    (granted, requested),
    (Role::Owner, Role::Owner | Role::Editor | Role::Viewer) | (Role::Editor, Role::Editor | Role::Viewer) | (Role::Viewer, Role::Viewer)
  )
}

#[must_use]
pub fn granular_record_id_u128(id: u128) -> String {
  format!("{id:032x}")
}

pub fn granular_record_id_to_u128(id: &str) -> CollabResult<u128> {
  if id.len() != 32 || !id.bytes().all(|byte| byte.is_ascii_hexdigit()) {
    return Err(CollabError::InvalidSchema("granular record id"));
  }
  u128::from_str_radix(id, 16).map_err(|_| CollabError::InvalidSchema("granular record id"))
}

fn utf8_range_to_unicode_range(text: &str, range: Range<usize>) -> CollabResult<Range<usize>> {
  if range.start > range.end || range.end > text.len() || !text.is_char_boundary(range.start) || !text.is_char_boundary(range.end) {
    return Err(CollabError::InvalidSchema("granular text range"));
  }
  let start = text[..range.start].chars().count();
  let end = start + text[range].chars().count();
  Ok(start..end)
}

#[must_use]
pub fn granular_record_id_with_suffix(id: impl AsRef<str>, suffix: &str) -> String {
  let id = id.as_ref();
  let mut record_id = String::with_capacity(id.len() + suffix.len() + 1);
  record_id.push_str(id);
  record_id.push(':');
  record_id.push_str(suffix);
  record_id
}

#[must_use]
pub fn granular_records_by_id<T, F>(records: Vec<T>, mut id: F) -> BTreeMap<String, T>
where
  F: FnMut(&T) -> &str,
{
  records
    .into_iter()
    .map(|record| (id(&record).to_string(), record))
    .collect()
}
