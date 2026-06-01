use std::{collections::BTreeMap, ops::Range};

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

  fn source_hash(&self) -> CollabResult<[u8; 32]> {
    postcard::to_stdvec(&self.clone().canonicalized())
      .map(|bytes| blake3_hash(&bytes))
      .map_err(Into::into)
  }
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

  #[must_use]
  pub fn peer_id(&self) -> PeerID {
    self.doc.peer_id()
  }

  pub fn role_policy(&self) -> CollabResult<CollabRolePolicy> {
    postcard::from_bytes(&root_binary(&self.doc, KEY_ROLE_POLICY)?).map_err(Into::into)
  }

  pub fn materialize_projection_cache(&self) -> CollabResult<Vec<u8>> {
    root_binary(&self.doc, KEY_SOURCE_PAYLOAD)
  }

  pub fn materialize_granular_source(&self) -> CollabResult<Option<GranularSource>> {
    if !self.is_granular() {
      return Ok(None);
    }
    read_granular_source(&self.doc).map(Some)
  }

  pub fn projection_hash(&self) -> CollabResult<[u8; 32]> {
    if self.is_granular() {
      return read_granular_source(&self.doc)?.source_hash();
    }
    root_hash(&self.doc, KEY_PROJECTION_HASH)
  }

  pub fn frontier(&self) -> CollabResult<Vec<u8>> {
    postcard::to_stdvec(&self.doc.oplog_vv()).map_err(Into::into)
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
    write_source_model(&self.doc, SourceModel::GranularRecords)?;
    write_projection_payload(&self.doc, projection_cache, asset_manifest)?;
    write_granular_source(&self.doc, source)?;
    self.doc.commit();
    validate_schema(&self.doc, Some(self.format_kind), Some(self.document_id))?;
    self
      .doc
      .export(ExportMode::updates(&before))
      .map_err(|error| CollabError::Loro(error.to_string()))
  }

  pub fn insert_granular_text_utf8(&self, role: Role, text_id: &str, byte_offset: usize, text: &str) -> CollabResult<Vec<u8>> {
    require_writer(role)?;
    let before = self.doc.oplog_vv();
    let text_container = granular_text_container(&self.doc, text_id)?;
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

  pub fn import_update_checked(&self, remote_role: Role, update: &[u8]) -> CollabResult<CollabImportOutcome> {
    require_writer(remote_role)?;

    let before_hash = self.projection_hash()?;
    let before_snapshot = self.export_snapshot()?;
    let candidate = LoroDoc::from_snapshot(&before_snapshot).map_err(|error| CollabError::Loro(error.to_string()))?;
    candidate
      .import(update)
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    validate_schema(&candidate, Some(self.format_kind), Some(self.document_id))?;

    self
      .doc
      .import(update)
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    validate_schema(&self.doc, Some(self.format_kind), Some(self.document_id))?;

    let after_hash = self.projection_hash()?;
    let patch = (before_hash != after_hash).then_some(CollabProjectionPatch {
      old_projection_hash: before_hash,
      new_projection_hash: after_hash,
    });
    Ok(CollabImportOutcome {
      patch,
      frontier: self.frontier()?,
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
  let source_payload_hash = root_hash(doc, KEY_SOURCE_PAYLOAD_HASH)?;
  if blake3_hash(&source_payload) != source_payload_hash {
    return Err(CollabError::HashMismatch("Loro projection cache"));
  }
  let _ = root_hash(doc, KEY_PROJECTION_HASH)?;

  let role_policy: CollabRolePolicy = postcard::from_bytes(&root_binary(doc, KEY_ROLE_POLICY)?)?;
  if role_policy.role_for_actor(role_policy.owner) != Some(Role::Owner) {
    return Err(CollabError::InvalidSchema("Flowstate Loro role policy has no owner"));
  }
  let _ = root_binary(doc, KEY_CREATED_BY_ACTOR)?;
  let _ = root_hash(doc, KEY_ASSET_MANIFEST_HASH)?;

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
  let source = source.clone().canonicalized();
  let root = doc.get_map(ROOT_MAP);
  root
    .insert(KEY_GRANULAR_METADATA, source.metadata)
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  write_granular_orders(&root, &source.orders)?;
  write_granular_texts(&root, &source.texts)?;
  write_granular_binaries(&root, &source.binaries)
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
      if mark.start_utf8 < mark.end_utf8 {
        text
          .mark_utf8(mark.start_utf8..mark.end_utf8, &mark.key, mark.value.clone().into_loro())
          .map_err(|error| CollabError::Loro(error.to_string()))?;
      }
    }
  }
  Ok(())
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

fn root_binary(doc: &LoroDoc, key: &'static str) -> CollabResult<Vec<u8>> {
  match root_value(doc, key)? {
    LoroValue::Binary(bytes) => Ok(bytes.unwrap()),
    _ => Err(CollabError::InvalidSchema(key)),
  }
}

fn root_value(doc: &LoroDoc, key: &'static str) -> CollabResult<LoroValue> {
  let root = doc
    .try_get_map(ROOT_MAP)
    .ok_or(CollabError::MissingRootValue(ROOT_MAP))?;
  let value = root.get(key).ok_or(CollabError::MissingRootValue(key))?;
  match value {
    ValueOrContainer::Value(value) => Ok(value),
    ValueOrContainer::Container(_) => Err(CollabError::InvalidSchema(key)),
  }
}

fn map_binary(map: &LoroMap, key: &'static str) -> CollabResult<Vec<u8>> {
  match map.get(key) {
    Some(ValueOrContainer::Value(LoroValue::Binary(bytes))) => Ok(bytes.unwrap()),
    _ => Err(CollabError::InvalidSchema(key)),
  }
}

fn loro_string(value: LoroValue) -> CollabResult<String> {
  match value {
    LoroValue::String(value) => Ok(value.unwrap()),
    _ => Err(CollabError::InvalidSchema("granular order value")),
  }
}

fn require_writer(role: Role) -> CollabResult<()> {
  if role.can_write() {
    Ok(())
  } else {
    Err(CollabError::Unauthorized("viewer cannot create durable document updates"))
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
