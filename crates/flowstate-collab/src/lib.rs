// Collaboration authority chain: workspace UI and editors write into flowstate-document / gpui-flowtext,
// flowstate-collab owns the durable Loro source, and flowstate-sync only transports persisted updates/snapshots.
use std::{
  collections::BTreeMap,
  fmt,
  io::{Cursor, Read as _},
};

use serde::{Deserialize, Serialize};
use uuid::Uuid;

mod source;

pub use source::*;

pub const COLLAB_SCHEMA_VERSION: u32 = 2;
pub const NATIVE_ENVELOPE_SCHEMA_VERSION: u32 = 1;
pub const DB8_COLLAB_MAGIC: &[u8; 5] = b"DB8C\0";
pub const FL0_COLLAB_MAGIC: &[u8; 5] = b"FL0C\0";
pub const FLOWSTATE_ALPN: &[u8] = b"flowstate/collab/2";

const CHUNK_MANIFEST: u16 = 1;
const CHUNK_LORO_SNAPSHOT: u16 = 2;
const CHUNK_RECENT_UPDATES: u16 = 3;
const CHUNK_PROJECTION_CACHE: u16 = 4;
const CHUNK_ASSET_MANIFEST: u16 = 5;
const CHUNK_ASSET_INLINE_DATA: u16 = 6;
const CHUNK_INTEGRITY: u16 = 7;
const CHUNK_TABLE_ENTRY_LEN: usize = 2 + 2 + 8 + 8 + 32;

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum FormatKind {
  Db8,
  Fl0,
}

impl FormatKind {
  #[must_use]
  pub const fn magic(self) -> &'static [u8; 5] {
    match self {
      Self::Db8 => DB8_COLLAB_MAGIC,
      Self::Fl0 => FL0_COLLAB_MAGIC,
    }
  }

  #[must_use]
  pub const fn as_u8(self) -> u8 {
    match self {
      Self::Db8 => 1,
      Self::Fl0 => 2,
    }
  }

  pub fn from_u8(value: u8) -> Result<Self, CollabError> {
    match value {
      1 => Ok(Self::Db8),
      2 => Ok(Self::Fl0),
      _ => Err(CollabError::InvalidFormatKind(value)),
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum Role {
  Owner,
  Editor,
  Viewer,
}

impl Role {
  #[must_use]
  pub const fn can_write(self) -> bool {
    matches!(self, Self::Owner | Self::Editor)
  }

  #[must_use]
  pub const fn can_invite(self) -> bool {
    matches!(self, Self::Owner)
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct DocumentId(pub Uuid);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct ActorId(pub Uuid);

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct SessionId(pub Uuid);

impl DocumentId {
  #[must_use]
  pub fn new() -> Self {
    Self(Uuid::new_v4())
  }
}

impl Default for DocumentId {
  fn default() -> Self {
    Self::new()
  }
}

impl ActorId {
  #[must_use]
  pub fn new() -> Self {
    Self(Uuid::new_v4())
  }
}

impl Default for ActorId {
  fn default() -> Self {
    Self::new()
  }
}

impl SessionId {
  #[must_use]
  pub fn new() -> Self {
    Self(Uuid::new_v4())
  }
}

impl Default for SessionId {
  fn default() -> Self {
    Self::new()
  }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeAssetRecord {
  pub asset_id: u128,
  pub blake3_hash: [u8; 32],
  pub byte_len: u64,
  pub mime_type: String,
  pub original_name: Option<String>,
  pub created_by_actor: ActorId,
  pub inline: bool,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeManifest {
  pub document_id: DocumentId,
  pub format_kind: FormatKind,
  pub envelope_schema: u32,
  pub collab_schema: u32,
  pub crdt_engine: String,
  pub created_by_actor: ActorId,
  pub projection_hash: [u8; 32],
  pub snapshot_hash: [u8; 32],
  pub asset_manifest_hash: [u8; 32],
  pub role_model: Vec<Role>,
  pub capabilities: Vec<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct NativeIntegrity {
  pub header_hash: [u8; 32],
  pub manifest_hash: [u8; 32],
  pub snapshot_hash: [u8; 32],
  pub recent_updates_hash: [u8; 32],
  pub projection_cache_hash: [u8; 32],
  pub asset_manifest_hash: [u8; 32],
  pub asset_inline_data_hash: [u8; 32],
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct NativeFileInput {
  pub format_kind: FormatKind,
  pub document_id: DocumentId,
  pub created_by_actor: ActorId,
  pub projection_cache: Vec<u8>,
  pub recent_updates: Vec<Vec<u8>>,
  pub asset_manifest: Vec<NativeAssetRecord>,
  pub asset_inline_data: Vec<u8>,
  pub source_snapshot: Option<Vec<u8>>,
}

impl NativeFileInput {
  #[must_use]
  pub fn new(format_kind: FormatKind, projection_cache: Vec<u8>) -> Self {
    Self {
      format_kind,
      document_id: DocumentId::new(),
      created_by_actor: ActorId::new(),
      projection_cache,
      recent_updates: Vec::new(),
      asset_manifest: Vec::new(),
      source_snapshot: None,
      asset_inline_data: Vec::new(),
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DecodedNativeFile {
  pub manifest: NativeManifest,
  pub snapshot: Vec<u8>,
  pub recent_updates: Vec<Vec<u8>>,
  pub projection_cache: Vec<u8>,
  pub projection_cache_recovery: ProjectionCacheRecovery,
  pub asset_manifest: Vec<NativeAssetRecord>,
  pub asset_inline_data: Vec<u8>,
  pub integrity: NativeIntegrity,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct HelloMessage {
  pub protocol_version: u32,
  pub app_build_id: String,
  pub document_id: DocumentId,
  pub format_kind: FormatKind,
  pub collab_schema: u32,
  pub crdt_engine: String,
  pub actor_id: ActorId,
  pub session_id: SessionId,
  pub role_request: Role,
  pub known_frontier: Vec<u8>,
  pub invite_capability: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AuthorizeMessage {
  pub accepted: bool,
  pub role: Role,
  pub reason: Option<String>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PresenceMessage {
  pub document_id: DocumentId,
  pub actor_id: ActorId,
  pub session_id: SessionId,
  pub user_label: String,
  pub role: Role,
  pub cursor: Option<String>,
  pub focus: Option<String>,
  pub viewport_hint: Option<String>,
  pub last_known_frontier: Vec<u8>,
  pub monotonic_millis: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum PeerEventKind {
  Authorized,
  RoleChanged,
  Left,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PeerEventMessage {
  pub document_id: DocumentId,
  pub actor_id: ActorId,
  pub session_id: SessionId,
  pub role: Option<Role>,
  pub kind: PeerEventKind,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssetHaveMessage {
  pub document_id: DocumentId,
  pub assets: Vec<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssetNeedMessage {
  pub document_id: DocumentId,
  pub blake3_hash: [u8; 32],
  pub offset: u64,
  pub len: u64,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct AssetChunkMessage {
  pub document_id: DocumentId,
  pub blake3_hash: [u8; 32],
  pub offset: u64,
  pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SnapshotChunkMessage {
  pub document_id: DocumentId,
  pub hash: [u8; 32],
  pub offset: u64,
  pub total_len: u64,
  pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum UpdateApplication {
  Db8CanonicalOperations(Vec<u8>),
  Fl0ActionBundle(Vec<u8>),
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum WireMessage {
  Hello(HelloMessage),
  Authorize(AuthorizeMessage),
  Have {
    document_id: DocumentId,
    frontier: Vec<u8>,
    assets: Vec<[u8; 32]>,
  },
  Need {
    document_id: DocumentId,
    frontier: Vec<u8>,
    snapshot: bool,
  },
  Update {
    document_id: DocumentId,
    actor_id: ActorId,
    bytes: Vec<u8>,
    hash: [u8; 32],
    application: Option<UpdateApplication>,
  },
  Snapshot {
    document_id: DocumentId,
    bytes: Vec<u8>,
    hash: [u8; 32],
  },
  SnapshotChunk(SnapshotChunkMessage),
  AssetHave(AssetHaveMessage),
  AssetNeed(AssetNeedMessage),
  AssetChunk(AssetChunkMessage),
  Presence(PresenceMessage),
  PeerEvent(PeerEventMessage),
  Ack {
    document_id: DocumentId,
    frontier: Vec<u8>,
  },
  Error {
    document_id: Option<DocumentId>,
    message: String,
  },
}

#[derive(Debug)]
pub enum CollabError {
  Io(std::io::Error),
  Postcard(postcard::Error),
  Loro(String),
  InvalidMagic,
  InvalidFormatKind(u8),
  UnsupportedEnvelopeSchema(u32),
  MissingChunk(&'static str),
  DuplicateChunk(u16),
  ChunkOutOfBounds(u16),
  HashMismatch(&'static str),
  InvalidIntegrity,
  InvalidSchema(&'static str),
  MissingRootValue(&'static str),
  Unauthorized(&'static str),
  UnsupportedCollabSchema(u32),
}

impl fmt::Display for CollabError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::Io(error) => write!(f, "I/O error: {error}"),
      Self::Postcard(error) => write!(f, "postcard error: {error}"),
      Self::Loro(error) => write!(f, "Loro error: {error}"),
      Self::InvalidMagic => f.write_str("invalid Flowstate collaboration envelope magic"),
      Self::InvalidFormatKind(value) => write!(f, "invalid Flowstate format kind {value}"),
      Self::UnsupportedEnvelopeSchema(schema) => write!(f, "unsupported Flowstate collaboration envelope schema {schema}"),
      Self::MissingChunk(name) => write!(f, "missing Flowstate collaboration chunk {name}"),
      Self::DuplicateChunk(kind) => write!(f, "duplicate Flowstate collaboration chunk {kind}"),
      Self::ChunkOutOfBounds(kind) => write!(f, "Flowstate collaboration chunk {kind} is out of bounds"),
      Self::HashMismatch(name) => write!(f, "Flowstate collaboration hash mismatch for {name}"),
      Self::InvalidIntegrity => f.write_str("invalid Flowstate collaboration integrity chunk"),
      Self::InvalidSchema(name) => write!(f, "invalid Flowstate Loro schema at {name}"),
      Self::MissingRootValue(name) => write!(f, "missing Flowstate Loro root value {name}"),
      Self::Unauthorized(reason) => write!(f, "unauthorized Flowstate collaboration update: {reason}"),
      Self::UnsupportedCollabSchema(schema) => write!(f, "unsupported Flowstate collaboration schema {schema}"),
    }
  }
}

impl std::error::Error for CollabError {}

impl From<std::io::Error> for CollabError {
  fn from(error: std::io::Error) -> Self {
    Self::Io(error)
  }
}

impl From<postcard::Error> for CollabError {
  fn from(error: postcard::Error) -> Self {
    Self::Postcard(error)
  }
}

pub type CollabResult<T> = Result<T, CollabError>;

#[must_use]
pub fn blake3_hash(bytes: &[u8]) -> [u8; 32] {
  *blake3::hash(bytes).as_bytes()
}

pub fn encode_wire_message(message: &WireMessage) -> CollabResult<Vec<u8>> {
  postcard::to_stdvec(message).map_err(Into::into)
}

pub fn decode_wire_message(bytes: &[u8]) -> CollabResult<WireMessage> {
  postcard::from_bytes(bytes).map_err(Into::into)
}

pub fn encode_native_file(input: NativeFileInput) -> CollabResult<Vec<u8>> {
  let asset_manifest = postcard::to_stdvec(&input.asset_manifest)?;
  let recent_updates = postcard::to_stdvec(&input.recent_updates)?;
  let snapshot = if let Some(snapshot) = input.source_snapshot {
    snapshot
  } else {
    source_snapshot(
      input.format_kind,
      input.document_id,
      input.created_by_actor,
      &input.projection_cache,
      &asset_manifest,
    )?
  };
  let manifest = NativeManifest {
    document_id: input.document_id,
    format_kind: input.format_kind,
    envelope_schema: NATIVE_ENVELOPE_SCHEMA_VERSION,
    collab_schema: COLLAB_SCHEMA_VERSION,
    crdt_engine: "loro".to_string(),
    created_by_actor: input.created_by_actor,
    projection_hash: blake3_hash(&input.projection_cache),
    snapshot_hash: blake3_hash(&snapshot),
    asset_manifest_hash: blake3_hash(&asset_manifest),
    role_model: vec![Role::Owner, Role::Editor, Role::Viewer],
    capabilities: vec![
      "loro-snapshot".to_string(),
      "projection-cache".to_string(),
      "document-level-assets".to_string(),
      "granular-source-records".to_string(),
      "ephemeral-presence".to_string(),
    ],
  };
  let manifest_bytes = postcard::to_stdvec(&manifest)?;
  let mut chunks = vec![
    (CHUNK_MANIFEST, manifest_bytes),
    (CHUNK_LORO_SNAPSHOT, snapshot),
    (CHUNK_RECENT_UPDATES, recent_updates),
    (CHUNK_PROJECTION_CACHE, input.projection_cache),
    (CHUNK_ASSET_MANIFEST, asset_manifest),
    (CHUNK_ASSET_INLINE_DATA, input.asset_inline_data),
  ];
  let final_chunk_count = chunks.len() + 1;
  let integrity = NativeIntegrity {
    header_hash: envelope_header_hash(input.format_kind, final_chunk_count),
    manifest_hash: blake3_hash(chunk_bytes(&chunks, CHUNK_MANIFEST).unwrap_or(&[])),
    snapshot_hash: blake3_hash(chunk_bytes(&chunks, CHUNK_LORO_SNAPSHOT).unwrap_or(&[])),
    recent_updates_hash: blake3_hash(chunk_bytes(&chunks, CHUNK_RECENT_UPDATES).unwrap_or(&[])),
    projection_cache_hash: blake3_hash(chunk_bytes(&chunks, CHUNK_PROJECTION_CACHE).unwrap_or(&[])),
    asset_manifest_hash: blake3_hash(chunk_bytes(&chunks, CHUNK_ASSET_MANIFEST).unwrap_or(&[])),
    asset_inline_data_hash: blake3_hash(chunk_bytes(&chunks, CHUNK_ASSET_INLINE_DATA).unwrap_or(&[])),
  };
  chunks.push((CHUNK_INTEGRITY, postcard::to_stdvec(&integrity)?));
  write_envelope(input.format_kind, chunks)
}

pub fn decode_native_file(bytes: &[u8], expected_format: FormatKind) -> CollabResult<DecodedNativeFile> {
  let chunks = read_envelope(bytes, expected_format)?;
  let manifest_bytes = required_chunk(&chunks, CHUNK_MANIFEST, "manifest")?;
  let snapshot = required_chunk(&chunks, CHUNK_LORO_SNAPSHOT, "loro snapshot")?.to_vec();
  let recent_updates_bytes = required_chunk(&chunks, CHUNK_RECENT_UPDATES, "recent updates")?;
  let stored_projection_cache = required_chunk(&chunks, CHUNK_PROJECTION_CACHE, "projection cache")?.to_vec();
  let asset_manifest_bytes = required_chunk(&chunks, CHUNK_ASSET_MANIFEST, "asset manifest")?;
  let asset_inline_data = required_chunk(&chunks, CHUNK_ASSET_INLINE_DATA, "asset inline data")?.to_vec();
  let integrity_bytes = required_chunk(&chunks, CHUNK_INTEGRITY, "integrity")?;

  let manifest: NativeManifest = postcard::from_bytes(manifest_bytes)?;
  if manifest.envelope_schema != NATIVE_ENVELOPE_SCHEMA_VERSION {
    return Err(CollabError::UnsupportedEnvelopeSchema(manifest.envelope_schema));
  }
  if manifest.format_kind != expected_format {
    return Err(CollabError::InvalidMagic);
  }

  verify_hash("manifest snapshot hash", &snapshot, manifest.snapshot_hash)?;
  verify_hash("manifest asset manifest hash", asset_manifest_bytes, manifest.asset_manifest_hash)?;

  let recent_updates: Vec<Vec<u8>> = postcard::from_bytes(recent_updates_bytes)?;
  let asset_manifest: Vec<NativeAssetRecord> = postcard::from_bytes(asset_manifest_bytes)?;
  let integrity: NativeIntegrity = postcard::from_bytes(integrity_bytes)?;
  verify_integrity(expected_format, &chunks, &integrity)?;

  let source = CollabDocument::from_snapshot(&snapshot, Some(expected_format), Some(manifest.document_id))?;
  let materialized_projection = source.materialize_projection_cache()?;
  verify_hash("manifest projection hash", &materialized_projection, manifest.projection_hash)?;
  let (projection_cache, projection_cache_recovery) = if blake3_hash(&stored_projection_cache) == manifest.projection_hash {
    (stored_projection_cache, ProjectionCacheRecovery::Reused)
  } else {
    (materialized_projection, ProjectionCacheRecovery::Rebuilt)
  };

  Ok(DecodedNativeFile {
    manifest,
    snapshot,
    recent_updates,
    projection_cache,
    projection_cache_recovery,
    asset_manifest,
    asset_inline_data,
    integrity,
  })
}

pub fn projection_snapshot(
  format_kind: FormatKind,
  document_id: DocumentId,
  created_by_actor: ActorId,
  projection_cache: &[u8],
  asset_manifest: &[u8],
) -> CollabResult<Vec<u8>> {
  source_snapshot(format_kind, document_id, created_by_actor, projection_cache, asset_manifest)
}

pub fn source_snapshot(
  format_kind: FormatKind,
  document_id: DocumentId,
  created_by_actor: ActorId,
  projection_cache: &[u8],
  asset_manifest: &[u8],
) -> CollabResult<Vec<u8>> {
  CollabDocument::from_projection_source(format_kind, document_id, created_by_actor, projection_cache, asset_manifest)?.export_snapshot()
}

fn write_envelope(format_kind: FormatKind, chunks: Vec<(u16, Vec<u8>)>) -> CollabResult<Vec<u8>> {
  let header_len = format_kind.magic().len() + 4 + 1 + 4 + chunks.len() * CHUNK_TABLE_ENTRY_LEN;
  let payload_len = chunks.iter().map(|(_, bytes)| bytes.len()).sum::<usize>();
  let mut bytes = Vec::with_capacity(header_len + payload_len);
  bytes.extend_from_slice(format_kind.magic());
  bytes.extend_from_slice(&NATIVE_ENVELOPE_SCHEMA_VERSION.to_le_bytes());
  bytes.push(format_kind.as_u8());
  bytes.extend_from_slice(
    &u32::try_from(chunks.len())
      .unwrap_or(u32::MAX)
      .to_le_bytes(),
  );
  let mut offset = header_len;
  for (kind, payload) in &chunks {
    bytes.extend_from_slice(&kind.to_le_bytes());
    bytes.extend_from_slice(&0_u16.to_le_bytes());
    bytes.extend_from_slice(&(offset as u64).to_le_bytes());
    bytes.extend_from_slice(&(payload.len() as u64).to_le_bytes());
    bytes.extend_from_slice(&blake3_hash(payload));
    offset += payload.len();
  }
  for (_, payload) in chunks {
    bytes.extend_from_slice(&payload);
  }
  Ok(bytes)
}

fn envelope_header_hash(format_kind: FormatKind, chunk_count: usize) -> [u8; 32] {
  let mut bytes = Vec::with_capacity(format_kind.magic().len() + 4 + 1 + 4);
  bytes.extend_from_slice(format_kind.magic());
  bytes.extend_from_slice(&NATIVE_ENVELOPE_SCHEMA_VERSION.to_le_bytes());
  bytes.push(format_kind.as_u8());
  bytes.extend_from_slice(&u32::try_from(chunk_count).unwrap_or(u32::MAX).to_le_bytes());
  blake3_hash(&bytes)
}

fn read_envelope(bytes: &[u8], expected_format: FormatKind) -> CollabResult<BTreeMap<u16, Vec<u8>>> {
  let mut cursor = Cursor::new(bytes);
  let mut magic = [0; 5];
  cursor.read_exact(&mut magic)?;
  if &magic != expected_format.magic() {
    return Err(CollabError::InvalidMagic);
  }
  let schema = read_u32(&mut cursor)?;
  if schema != NATIVE_ENVELOPE_SCHEMA_VERSION {
    return Err(CollabError::UnsupportedEnvelopeSchema(schema));
  }
  let format_kind = FormatKind::from_u8(read_u8(&mut cursor)?)?;
  if format_kind != expected_format {
    return Err(CollabError::InvalidMagic);
  }
  let chunk_count = read_u32(&mut cursor)? as usize;
  let mut table = Vec::with_capacity(chunk_count);
  for _ in 0..chunk_count {
    let kind = read_u16(&mut cursor)?;
    let flags = read_u16(&mut cursor)?;
    if flags != 0 {
      return Err(CollabError::ChunkOutOfBounds(kind));
    }
    let offset = read_u64(&mut cursor)? as usize;
    let len = read_u64(&mut cursor)? as usize;
    let mut hash = [0; 32];
    cursor.read_exact(&mut hash)?;
    table.push((kind, offset, len, hash));
  }

  let mut chunks = BTreeMap::new();
  for (kind, offset, len, hash) in table {
    let end = offset
      .checked_add(len)
      .ok_or(CollabError::ChunkOutOfBounds(kind))?;
    if end > bytes.len() {
      return Err(CollabError::ChunkOutOfBounds(kind));
    }
    let payload = &bytes[offset..end];
    if kind != CHUNK_PROJECTION_CACHE && blake3_hash(payload) != hash {
      return Err(CollabError::HashMismatch("chunk"));
    }
    if chunks.insert(kind, payload.to_vec()).is_some() {
      return Err(CollabError::DuplicateChunk(kind));
    }
  }
  Ok(chunks)
}

fn verify_integrity(format_kind: FormatKind, chunks: &BTreeMap<u16, Vec<u8>>, integrity: &NativeIntegrity) -> CollabResult<()> {
  if envelope_header_hash(format_kind, chunks.len()) != integrity.header_hash {
    return Err(CollabError::HashMismatch("integrity header"));
  }
  verify_hash(
    "integrity manifest",
    required_chunk(chunks, CHUNK_MANIFEST, "manifest")?,
    integrity.manifest_hash,
  )?;
  verify_hash(
    "integrity snapshot",
    required_chunk(chunks, CHUNK_LORO_SNAPSHOT, "loro snapshot")?,
    integrity.snapshot_hash,
  )?;
  verify_hash(
    "integrity recent updates",
    required_chunk(chunks, CHUNK_RECENT_UPDATES, "recent updates")?,
    integrity.recent_updates_hash,
  )?;
  let _ = required_chunk(chunks, CHUNK_PROJECTION_CACHE, "projection cache")?;
  let _ = integrity.projection_cache_hash;
  verify_hash(
    "integrity asset manifest",
    required_chunk(chunks, CHUNK_ASSET_MANIFEST, "asset manifest")?,
    integrity.asset_manifest_hash,
  )?;
  verify_hash(
    "integrity asset inline data",
    required_chunk(chunks, CHUNK_ASSET_INLINE_DATA, "asset inline data")?,
    integrity.asset_inline_data_hash,
  )?;
  Ok(())
}

fn required_chunk<'chunks>(chunks: &'chunks BTreeMap<u16, Vec<u8>>, kind: u16, name: &'static str) -> CollabResult<&'chunks [u8]> {
  chunks
    .get(&kind)
    .map(Vec::as_slice)
    .ok_or(CollabError::MissingChunk(name))
}

fn chunk_bytes(chunks: &[(u16, Vec<u8>)], kind: u16) -> Option<&[u8]> {
  chunks
    .iter()
    .find(|(candidate, _)| *candidate == kind)
    .map(|(_, bytes)| bytes.as_slice())
}

fn verify_hash(name: &'static str, bytes: &[u8], expected: [u8; 32]) -> CollabResult<()> {
  if blake3_hash(bytes) == expected {
    Ok(())
  } else {
    Err(CollabError::HashMismatch(name))
  }
}

fn read_u8(cursor: &mut Cursor<&[u8]>) -> std::io::Result<u8> {
  let mut bytes = [0; 1];
  cursor.read_exact(&mut bytes)?;
  Ok(bytes[0])
}

fn read_u16(cursor: &mut Cursor<&[u8]>) -> std::io::Result<u16> {
  let mut bytes = [0; 2];
  cursor.read_exact(&mut bytes)?;
  Ok(u16::from_le_bytes(bytes))
}

fn read_u32(cursor: &mut Cursor<&[u8]>) -> std::io::Result<u32> {
  let mut bytes = [0; 4];
  cursor.read_exact(&mut bytes)?;
  Ok(u32::from_le_bytes(bytes))
}

fn read_u64(cursor: &mut Cursor<&[u8]>) -> std::io::Result<u64> {
  let mut bytes = [0; 8];
  cursor.read_exact(&mut bytes)?;
  Ok(u64::from_le_bytes(bytes))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn native_envelope_round_trips_reuse_projection_cache() {
    let input = NativeFileInput::new(FormatKind::Db8, b"projection".to_vec());
    let bytes = encode_native_file(input).unwrap();
    let decoded = decode_native_file(&bytes, FormatKind::Db8).unwrap();
    assert_eq!(decoded.projection_cache, b"projection");
    assert_eq!(decoded.projection_cache_recovery, ProjectionCacheRecovery::Reused);
    assert_eq!(decoded.manifest.format_kind, FormatKind::Db8);
  }

  #[test]
  fn corrupt_projection_cache_is_rebuilt_from_loro_source() {
    let input = NativeFileInput::new(FormatKind::Db8, b"projection".to_vec());
    let bytes = corrupt_projection_cache(encode_native_file(input).unwrap());
    let decoded = decode_native_file(&bytes, FormatKind::Db8).unwrap();
    assert_eq!(decoded.projection_cache, b"projection");
    assert_eq!(decoded.projection_cache_recovery, ProjectionCacheRecovery::Rebuilt);
  }

  #[test]
  fn rejects_wrong_magic() {
    let input = NativeFileInput::new(FormatKind::Db8, b"projection".to_vec());
    let bytes = encode_native_file(input).unwrap();
    assert!(matches!(decode_native_file(&bytes, FormatKind::Fl0), Err(CollabError::InvalidMagic)));
  }

  #[test]
  fn wire_messages_round_trip() {
    let message = WireMessage::Hello(HelloMessage {
      protocol_version: 1,
      app_build_id: "dev".to_string(),
      document_id: DocumentId::new(),
      format_kind: FormatKind::Fl0,
      collab_schema: COLLAB_SCHEMA_VERSION,
      crdt_engine: "loro".to_string(),
      actor_id: ActorId::new(),
      session_id: SessionId::new(),
      role_request: Role::Editor,
      known_frontier: Vec::new(),
      invite_capability: vec![1, 2, 3],
    });
    let bytes = encode_wire_message(&message).unwrap();
    assert_eq!(decode_wire_message(&bytes).unwrap(), message);
  }

  #[test]
  fn in_memory_collab_replicas_converge_from_updates() {
    let document_id = DocumentId::new();
    let actor = ActorId::new();
    let left = CollabDocument::from_projection_source(FormatKind::Fl0, document_id, actor, b"one", &[]).unwrap();
    let snapshot = left.export_snapshot().unwrap();
    let right = CollabDocument::from_snapshot(&snapshot, Some(FormatKind::Fl0), Some(document_id)).unwrap();

    let update = left
      .replace_projection_source(Role::Owner, b"two", &[])
      .unwrap();
    let outcome = right.import_update_checked(Role::Editor, &update).unwrap();
    assert!(outcome.patch.is_some());
    assert_eq!(right.materialize_projection_cache().unwrap(), b"two");
    assert_eq!(left.projection_hash().unwrap(), right.projection_hash().unwrap());
  }

  #[test]
  fn asset_manifest_is_durable_source_data() {
    let document_id = DocumentId::new();
    let actor = ActorId::new();
    let manifest = b"asset-manifest".to_vec();
    let left = CollabDocument::from_projection_source(FormatKind::Db8, document_id, actor, b"one", &manifest).unwrap();
    let right = CollabDocument::from_snapshot(&left.export_snapshot().unwrap(), Some(FormatKind::Db8), Some(document_id)).unwrap();

    assert_eq!(right.asset_manifest_bytes().unwrap(), manifest);
    let update = left
      .replace_projection_source(Role::Owner, b"two", b"replacement-manifest")
      .unwrap();
    right.import_update_checked(Role::Editor, &update).unwrap();
    assert_eq!(right.asset_manifest_bytes().unwrap(), b"replacement-manifest");
  }

  #[test]
  fn viewer_update_import_is_rejected_before_mutation() {
    let document_id = DocumentId::new();
    let actor = ActorId::new();
    let left = CollabDocument::from_projection_source(FormatKind::Db8, document_id, actor, b"one", &[]).unwrap();
    let right = CollabDocument::from_snapshot(&left.export_snapshot().unwrap(), Some(FormatKind::Db8), Some(document_id)).unwrap();
    let update = left
      .replace_projection_source(Role::Owner, b"two", &[])
      .unwrap();

    assert!(matches!(
      right.import_update_checked(Role::Viewer, &update),
      Err(CollabError::Unauthorized(_))
    ));
    assert_eq!(right.materialize_projection_cache().unwrap(), b"one");
  }

  #[test]
  fn granular_text_replicas_converge_from_concurrent_updates() {
    let document_id = DocumentId::new();
    let actor = ActorId::new();
    let source = GranularSource {
      metadata: b"template".to_vec(),
      orders: vec![GranularOrderRecord {
        name: "paragraph_order".to_string(),
        ids: vec!["p1".to_string()],
      }],
      texts: vec![GranularTextRecord {
        id: "p1".to_string(),
        text: "ab".to_string(),
        metadata: b"normal".to_vec(),
        marks: Vec::new(),
      }],
      binaries: Vec::new(),
    };
    let left = CollabDocument::from_granular_source(FormatKind::Db8, document_id, actor, &source, b"cache", &[]).unwrap();
    let right = CollabDocument::from_snapshot(&left.export_snapshot().unwrap(), Some(FormatKind::Db8), Some(document_id)).unwrap();

    let left_update = left
      .insert_granular_text_utf8(Role::Owner, "p1", 1, "L")
      .unwrap();
    let right_update = right
      .insert_granular_text_utf8(Role::Editor, "p1", 1, "R")
      .unwrap();
    left
      .import_update_checked(Role::Editor, &right_update)
      .unwrap();
    right
      .import_update_checked(Role::Owner, &left_update)
      .unwrap();

    let left_source = left.materialize_granular_source().unwrap().unwrap();
    let right_source = right.materialize_granular_source().unwrap().unwrap();
    assert_eq!(left_source, right_source);
    assert_eq!(left.projection_hash().unwrap(), right.projection_hash().unwrap());
    assert!(left_source.texts[0].text.contains('L'));
    assert!(left_source.texts[0].text.contains('R'));
  }

  #[test]
  fn concurrent_granular_source_replacements_preserve_independent_text_edits() {
    let document_id = DocumentId::new();
    let actor = ActorId::new();
    let base = GranularSource {
      metadata: b"cache".to_vec(),
      orders: vec![GranularOrderRecord {
        name: "paragraph_order".to_string(),
        ids: vec![granular_record_id_u128(1), granular_record_id_u128(2)],
      }],
      texts: vec![
        GranularTextRecord {
          id: granular_record_id_u128(1),
          text: "A".to_string(),
          metadata: Vec::new(),
          marks: Vec::new(),
        },
        GranularTextRecord {
          id: granular_record_id_u128(2),
          text: "B".to_string(),
          metadata: Vec::new(),
          marks: Vec::new(),
        },
      ],
      binaries: Vec::new(),
    };
    let left = CollabDocument::from_granular_source(FormatKind::Db8, document_id, actor, &base, b"cache", &[]).unwrap();
    let right = CollabDocument::from_snapshot(&left.export_snapshot().unwrap(), Some(FormatKind::Db8), Some(document_id)).unwrap();

    let mut left_source = base.clone();
    left_source.texts[0].text = "left A".to_string();
    let left_update = left
      .replace_granular_source(Role::Owner, &left_source, b"left cache", &[])
      .unwrap();

    let mut right_source = base;
    right_source.texts[1].text = "right B".to_string();
    let right_update = right
      .replace_granular_source(Role::Editor, &right_source, b"right cache", &[])
      .unwrap();

    left
      .import_update_checked(Role::Editor, &right_update)
      .unwrap();
    right
      .import_update_checked(Role::Owner, &left_update)
      .unwrap();

    let left_source = left.materialize_granular_source().unwrap().unwrap();
    let right_source = right.materialize_granular_source().unwrap().unwrap();
    assert_eq!(left_source, right_source);
    let by_id = granular_records_by_id(left_source.texts, |record| record.id.as_str());
    assert_eq!(by_id[&granular_record_id_u128(1)].text, "left A");
    assert_eq!(by_id[&granular_record_id_u128(2)].text, "right B");
  }

  #[test]
  fn concurrent_style_only_source_replacements_do_not_duplicate_text() {
    let document_id = DocumentId::new();
    let actor = ActorId::new();
    let text_id = granular_record_id_u128(1);
    let base = GranularSource {
      metadata: b"cache".to_vec(),
      orders: Vec::new(),
      texts: vec![GranularTextRecord {
        id: text_id.clone(),
        text: "unchanged text".to_string(),
        metadata: Vec::new(),
        marks: Vec::new(),
      }],
      binaries: Vec::new(),
    };
    let left = CollabDocument::from_granular_source(FormatKind::Db8, document_id, actor, &base, b"cache", &[]).unwrap();
    let right = CollabDocument::from_snapshot(&left.export_snapshot().unwrap(), Some(FormatKind::Db8), Some(document_id)).unwrap();

    let mut left_source = base.clone();
    left_source.texts[0].metadata = b"left-style".to_vec();
    let left_update = left
      .replace_granular_source(Role::Owner, &left_source, b"left-cache", &[])
      .unwrap();

    let mut right_source = base;
    right_source.texts[0].marks = vec![GranularTextMark {
      start_utf8: 0,
      end_utf8: "unchanged".len(),
      key: "semantic".to_string(),
      value: GranularValue::I64(1),
    }];
    let right_update = right
      .replace_granular_source(Role::Editor, &right_source, b"right-cache", &[])
      .unwrap();

    left
      .import_update_checked(Role::Editor, &right_update)
      .unwrap();
    right
      .import_update_checked(Role::Owner, &left_update)
      .unwrap();

    let left_source = left.materialize_granular_source().unwrap().unwrap();
    let right_source = right.materialize_granular_source().unwrap().unwrap();
    assert_eq!(left_source, right_source);
    assert_eq!(left_source.texts[0].text, "unchanged text");
  }

  #[test]
  fn granular_viewer_text_update_is_rejected_before_mutation() {
    let document_id = DocumentId::new();
    let actor = ActorId::new();
    let source = GranularSource {
      metadata: Vec::new(),
      orders: Vec::new(),
      texts: vec![GranularTextRecord {
        id: "p1".to_string(),
        text: "locked".to_string(),
        metadata: Vec::new(),
        marks: Vec::new(),
      }],
      binaries: Vec::new(),
    };
    let document = CollabDocument::from_granular_source(FormatKind::Db8, document_id, actor, &source, b"cache", &[]).unwrap();
    assert!(matches!(
      document.insert_granular_text_utf8(Role::Viewer, "p1", 0, "x"),
      Err(CollabError::Unauthorized(_))
    ));
    assert_eq!(
      document
        .materialize_granular_source()
        .unwrap()
        .unwrap()
        .texts[0]
        .text,
      "locked"
    );
  }

  #[test]
  fn local_actors_get_distinct_loro_peer_ids() {
    let document_id = DocumentId::new();
    let left = CollabDocument::from_projection_source(FormatKind::Db8, document_id, ActorId::new(), b"one", &[]).unwrap();
    let right = CollabDocument::from_snapshot(&left.export_snapshot().unwrap(), Some(FormatKind::Db8), Some(document_id)).unwrap();
    let peer_before = right.peer_id();
    right.set_local_actor(ActorId::new()).unwrap();
    assert_ne!(peer_before, right.peer_id());
  }

  #[test]
  fn granular_non_ascii_marks_stay_on_utf8_boundaries() {
    let document_id = DocumentId::new();
    let actor = ActorId::new();
    let source = GranularSource {
      metadata: Vec::new(),
      orders: Vec::new(),
      texts: vec![GranularTextRecord {
        id: "p1".to_string(),
        text: "éa".to_string(),
        metadata: Vec::new(),
        marks: vec![GranularTextMark {
          start_utf8: 0,
          end_utf8: "é".len(),
          key: "semantic".to_string(),
          value: GranularValue::I64(1),
        }],
      }],
      binaries: Vec::new(),
    };
    let document = CollabDocument::from_granular_source(FormatKind::Db8, document_id, actor, &source, b"cache", &[]).unwrap();
    let materialized = document.materialize_granular_source().unwrap().unwrap();
    assert_eq!(materialized.texts[0].text, "éa");
    assert_eq!(materialized.texts[0].marks[0].start_utf8, 0);
    assert_eq!(materialized.texts[0].marks[0].end_utf8, "é".len());
  }

  #[test]
  fn granular_record_ids_must_be_canonical() {
    assert!(granular_record_id_to_u128("1").is_err());
    assert_eq!(granular_record_id_to_u128("00000000000000000000000000000001").unwrap(), 1);
  }
  fn corrupt_projection_cache(mut bytes: Vec<u8>) -> Vec<u8> {
    let chunk_count_offset = DB8_COLLAB_MAGIC.len() + 4 + 1;
    let chunk_count = u32::from_le_bytes(
      bytes[chunk_count_offset..chunk_count_offset + 4]
        .try_into()
        .unwrap(),
    ) as usize;
    let table_start = chunk_count_offset + 4;
    for ix in 0..chunk_count {
      let entry = table_start + ix * CHUNK_TABLE_ENTRY_LEN;
      let kind = u16::from_le_bytes(bytes[entry..entry + 2].try_into().unwrap());
      if kind != CHUNK_PROJECTION_CACHE {
        continue;
      }
      let offset = u64::from_le_bytes(bytes[entry + 4..entry + 12].try_into().unwrap()) as usize;
      let len = u64::from_le_bytes(bytes[entry + 12..entry + 20].try_into().unwrap()) as usize;
      if len > 0 {
        bytes[offset] ^= 0xff;
      }
      break;
    }
    bytes
  }
}
