use loro::{ExportMode, LoroDoc, LoroValue, ValueOrContainer, VersionVector};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use crate::{ActorId, COLLAB_SCHEMA_VERSION, CollabError, CollabResult, DocumentId, FormatKind, Role, blake3_hash};

const ROOT_MAP: &str = "flowstate";
const KEY_SCHEMA_VERSION: &str = "schema_version";
const KEY_FORMAT_KIND: &str = "format_kind";
const KEY_DOCUMENT_ID: &str = "document_id";
const KEY_CREATED_BY_ACTOR: &str = "created_by_actor";
const KEY_ROLE_POLICY: &str = "role_policy";
const KEY_SOURCE_PAYLOAD: &str = "source_payload";
const KEY_SOURCE_PAYLOAD_HASH: &str = "source_payload_hash";
const KEY_PROJECTION_HASH: &str = "projection_hash";
const KEY_ASSET_MANIFEST_HASH: &str = "asset_manifest_hash";

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
    let role_policy = CollabRolePolicy::owner_only(created_by_actor);
    initialize_root(&doc, format_kind, document_id, created_by_actor, &role_policy, projection_cache, asset_manifest)?;
    Self::from_doc(doc, Some(format_kind), Some(document_id))
  }

  pub fn from_snapshot(snapshot: &[u8], expected_format: Option<FormatKind>, expected_document_id: Option<DocumentId>) -> CollabResult<Self> {
    let doc = LoroDoc::from_snapshot(snapshot).map_err(|error| CollabError::Loro(error.to_string()))?;
    Self::from_doc(doc, expected_format, expected_document_id)
  }

  fn from_doc(doc: LoroDoc, expected_format: Option<FormatKind>, expected_document_id: Option<DocumentId>) -> CollabResult<Self> {
    let schema = validate_schema(&doc, expected_format, expected_document_id)?;
    Ok(Self {
      doc,
      format_kind: schema.format_kind,
      document_id: schema.document_id,
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

  pub fn role_policy(&self) -> CollabResult<CollabRolePolicy> {
    postcard::from_bytes(&root_binary(&self.doc, KEY_ROLE_POLICY)?).map_err(Into::into)
  }

  pub fn materialize_projection_cache(&self) -> CollabResult<Vec<u8>> {
    root_binary(&self.doc, KEY_SOURCE_PAYLOAD)
  }

  pub fn projection_hash(&self) -> CollabResult<[u8; 32]> {
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

  pub fn replace_projection_source(
    &self,
    role: Role,
    projection_cache: &[u8],
    asset_manifest: &[u8],
  ) -> CollabResult<Vec<u8>> {
    if !role.can_write() {
      return Err(CollabError::Unauthorized("viewer cannot create durable document updates"));
    }
    let before = self.doc.oplog_vv();
    let root = self.doc.get_map(ROOT_MAP);
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
      .map_err(|error| CollabError::Loro(error.to_string()))?;
    self.doc.commit();
    validate_schema(&self.doc, Some(self.format_kind), Some(self.document_id))?;
    self
      .doc
      .export(ExportMode::updates(&before))
      .map_err(|error| CollabError::Loro(error.to_string()))
  }

  pub fn import_update_checked(&self, remote_role: Role, update: &[u8]) -> CollabResult<CollabImportOutcome> {
    if !remote_role.can_write() {
      return Err(CollabError::Unauthorized("viewer cannot import durable document updates"));
    }

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
      inner: CollabDocument::from_projection_source(
        FormatKind::Db8,
        document_id,
        created_by_actor,
        projection_cache,
        asset_manifest,
      )?,
    })
  }

  #[must_use]
  pub fn inner(&self) -> &CollabDocument {
    &self.inner
  }

  #[must_use]
  pub fn into_inner(self) -> CollabDocument {
    self.inner
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
      inner: CollabDocument::from_projection_source(
        FormatKind::Fl0,
        document_id,
        created_by_actor,
        projection_cache,
        asset_manifest,
      )?,
    })
  }

  #[must_use]
  pub fn inner(&self) -> &CollabDocument {
    &self.inner
  }

  #[must_use]
  pub fn into_inner(self) -> CollabDocument {
    self.inner
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ValidatedSchema {
  format_kind: FormatKind,
  document_id: DocumentId,
}

fn initialize_root(
  doc: &LoroDoc,
  format_kind: FormatKind,
  document_id: DocumentId,
  created_by_actor: ActorId,
  role_policy: &CollabRolePolicy,
  projection_cache: &[u8],
  asset_manifest: &[u8],
) -> CollabResult<()> {
  let root = doc.get_map(ROOT_MAP);
  let projection_hash = blake3_hash(projection_cache);
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
    .map_err(|error| CollabError::Loro(error.to_string()))?;
  doc.commit();
  Ok(())
}

fn validate_schema(doc: &LoroDoc, expected_format: Option<FormatKind>, expected_document_id: Option<DocumentId>) -> CollabResult<ValidatedSchema> {
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

  let source_payload = root_binary(doc, KEY_SOURCE_PAYLOAD)?;
  let source_payload_hash = root_hash(doc, KEY_SOURCE_PAYLOAD_HASH)?;
  if blake3_hash(&source_payload) != source_payload_hash {
    return Err(CollabError::HashMismatch("Loro source payload"));
  }
  let projection_hash = root_hash(doc, KEY_PROJECTION_HASH)?;
  if blake3_hash(&source_payload) != projection_hash {
    return Err(CollabError::HashMismatch("Loro projection materialization"));
  }

  let role_policy: CollabRolePolicy = postcard::from_bytes(&root_binary(doc, KEY_ROLE_POLICY)?)?;
  if role_policy.role_for_actor(role_policy.owner) != Some(Role::Owner) {
    return Err(CollabError::InvalidSchema("Flowstate Loro role policy has no owner"));
  }
  let _ = root_binary(doc, KEY_CREATED_BY_ACTOR)?;
  let _ = root_hash(doc, KEY_ASSET_MANIFEST_HASH)?;

  Ok(ValidatedSchema {
    format_kind,
    document_id,
  })
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

fn role_includes(granted: Role, requested: Role) -> bool {
  matches!(
    (granted, requested),
    (Role::Owner, Role::Owner | Role::Editor | Role::Viewer)
      | (Role::Editor, Role::Editor | Role::Viewer)
      | (Role::Viewer, Role::Viewer)
  )
}
