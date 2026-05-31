use std::{
  collections::{BTreeMap, HashSet},
  fmt,
  ops::Range,
};

use anyhow::{Context as _, Result as AnyResult, bail, ensure};
use flowstate_collab::{
  ActorId, AssetChunkMessage, AssetHaveMessage, AssetNeedMessage, AuthorizeMessage, COLLAB_SCHEMA_VERSION, DocumentId,
  FLOWSTATE_ALPN, FormatKind, HelloMessage, PresenceMessage, Role, SessionId, WireMessage, blake3_hash, decode_wire_message,
  encode_wire_message,
};
use iroh::{
  Endpoint, EndpointAddr,
  endpoint::{Connection, RecvStream, SendStream, presets},
  protocol::{AcceptError, ProtocolHandler, Router},
};

pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_MAX_ASSET_CHUNK_BYTES: usize = 1024 * 1024;
pub const FLOWSTATE_PROTOCOL_VERSION: u32 = 1;

#[derive(Clone, Debug)]
pub struct FlowstateSyncConfig {
  pub app_build_id: String,
  pub document_id: DocumentId,
  pub format_kind: FormatKind,
  pub actor_id: ActorId,
  pub session_id: SessionId,
  pub role_request: Role,
  pub max_message_bytes: usize,
  pub max_asset_chunk_bytes: usize,
}

impl FlowstateSyncConfig {
  #[must_use]
  pub fn new(document_id: DocumentId, format_kind: FormatKind, role_request: Role) -> Self {
    Self {
      app_build_id: "flowstate-dev".to_string(),
      document_id,
      format_kind,
      actor_id: ActorId::new(),
      session_id: SessionId::new(),
      role_request,
      max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
      max_asset_chunk_bytes: DEFAULT_MAX_ASSET_CHUNK_BYTES,
    }
  }

  #[must_use]
  pub fn hello(&self, known_frontier: Vec<u8>, invite_capability: Vec<u8>) -> HelloMessage {
    HelloMessage {
      protocol_version: FLOWSTATE_PROTOCOL_VERSION,
      app_build_id: self.app_build_id.clone(),
      document_id: self.document_id,
      format_kind: self.format_kind,
      collab_schema: COLLAB_SCHEMA_VERSION,
      crdt_engine: "loro".to_string(),
      actor_id: self.actor_id,
      session_id: self.session_id,
      role_request: self.role_request,
      known_frontier,
      invite_capability,
    }
  }
}

#[derive(Clone, Debug)]
pub struct RolePolicy {
  pub owner: ActorId,
  pub editors: HashSet<ActorId>,
  pub viewers: HashSet<ActorId>,
}

impl RolePolicy {
  #[must_use]
  pub fn owner_only(owner: ActorId) -> Self {
    Self {
      owner,
      editors: HashSet::new(),
      viewers: HashSet::new(),
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
  pub fn authorize(&self, hello: &HelloMessage) -> AuthorizeMessage {
    let Some(granted) = self.role_for_actor(hello.actor_id) else {
      return AuthorizeMessage {
        accepted: false,
        role: Role::Viewer,
        reason: Some("actor is not authorized for this document".to_string()),
      };
    };
    if !role_includes(granted, hello.role_request) {
      return AuthorizeMessage {
        accepted: false,
        role: granted,
        reason: Some("requested role exceeds granted document role".to_string()),
      };
    }
    AuthorizeMessage {
      accepted: true,
      role: hello.role_request,
      reason: None,
    }
  }
}

#[derive(Clone, Debug)]
pub struct InviteTicket {
  pub endpoint_addr: EndpointAddr,
  pub document_id: DocumentId,
  pub format_kind: FormatKind,
  pub invited_role: Role,
  pub capability: Vec<u8>,
}

impl InviteTicket {
  #[must_use]
  pub fn new(endpoint_addr: EndpointAddr, document_id: DocumentId, format_kind: FormatKind, invited_role: Role) -> Self {
    let mut capability = Vec::with_capacity(48);
    capability.extend_from_slice(document_id.0.as_bytes());
    capability.extend_from_slice(&[format_kind.as_u8(), role_wire_code(invited_role)]);
    capability.extend_from_slice(&blake3_hash(&capability));
    Self {
      endpoint_addr,
      document_id,
      format_kind,
      invited_role,
      capability,
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct VerifiedAsset {
  pub hash: [u8; 32],
  pub bytes: Vec<u8>,
}

#[derive(Clone, Debug, Default)]
pub struct AssetStore {
  assets: BTreeMap<[u8; 32], Vec<u8>>,
}

impl AssetStore {
  #[must_use]
  pub fn hashes(&self) -> Vec<[u8; 32]> {
    self.assets.keys().copied().collect()
  }

  pub fn insert_verified(&mut self, bytes: Vec<u8>) -> VerifiedAsset {
    let hash = blake3_hash(&bytes);
    self.assets.insert(hash, bytes.clone());
    VerifiedAsset { hash, bytes }
  }

  #[must_use]
  pub fn contains(&self, hash: &[u8; 32]) -> bool {
    self.assets.contains_key(hash)
  }

  pub fn chunk(&self, request: &AssetNeedMessage, max_chunk_bytes: usize) -> AnyResult<AssetChunkMessage> {
    let bytes = self
      .assets
      .get(&request.blake3_hash)
      .context("requested asset is not available")?;
    let range = bounded_range(request.offset, request.len, bytes.len(), max_chunk_bytes)?;
    Ok(AssetChunkMessage {
      document_id: request.document_id,
      blake3_hash: request.blake3_hash,
      offset: range.start as u64,
      bytes: bytes[range].to_vec(),
    })
  }

  pub fn insert_complete_chunk(&mut self, chunk: AssetChunkMessage, expected_len: u64) -> AnyResult<VerifiedAsset> {
    ensure!(chunk.offset == 0, "complete asset chunks must start at offset 0");
    ensure!(chunk.bytes.len() as u64 == expected_len, "complete asset chunk length mismatch");
    ensure!(blake3_hash(&chunk.bytes) == chunk.blake3_hash, "asset chunk hash mismatch");
    self.assets.insert(chunk.blake3_hash, chunk.bytes.clone());
    Ok(VerifiedAsset {
      hash: chunk.blake3_hash,
      bytes: chunk.bytes,
    })
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerPresence {
  pub actor_id: ActorId,
  pub session_id: SessionId,
  pub role: Role,
  pub user_label: String,
  pub cursor: Option<String>,
  pub focus: Option<String>,
  pub viewport_hint: Option<String>,
  pub last_known_frontier: Vec<u8>,
  pub monotonic_millis: u64,
}

impl PeerPresence {
  #[must_use]
  pub fn message(&self, document_id: DocumentId) -> PresenceMessage {
    PresenceMessage {
      document_id,
      session_id: self.session_id,
      user_label: self.user_label.clone(),
      role: self.role,
      cursor: self.cursor.clone(),
      focus: self.focus.clone(),
      viewport_hint: self.viewport_hint.clone(),
      last_known_frontier: self.last_known_frontier.clone(),
      monotonic_millis: self.monotonic_millis,
    }
  }
}

#[derive(Debug)]
pub enum SyncError {
  FrameTooLarge { len: usize, max: usize },
  UnexpectedMessage(&'static str),
  ProtocolMismatch,
  Unauthorized(String),
}

impl fmt::Display for SyncError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::FrameTooLarge { len, max } => write!(f, "Flowstate sync frame length {len} exceeds limit {max}"),
      Self::UnexpectedMessage(expected) => write!(f, "unexpected Flowstate sync message; expected {expected}"),
      Self::ProtocolMismatch => f.write_str("Flowstate sync protocol mismatch"),
      Self::Unauthorized(reason) => write!(f, "Flowstate sync authorization failed: {reason}"),
    }
  }
}

impl std::error::Error for SyncError {}

#[derive(Clone, Debug)]
pub struct FlowstateProtocol {
  pub config: FlowstateSyncConfig,
  pub role_policy: RolePolicy,
}

impl ProtocolHandler for FlowstateProtocol {
  async fn accept(&self, connection: Connection) -> std::result::Result<(), AcceptError> {
    if connection.alpn() != FLOWSTATE_ALPN {
      return Err(AcceptError::from_err(SyncError::ProtocolMismatch));
    }

    let (mut send, mut recv) = connection.accept_bi().await?;
    let message = read_wire_message(&mut recv, self.config.max_message_bytes)
      .await
      .map_err(accept_error)?;
    let WireMessage::Hello(hello) = message else {
      return Err(AcceptError::from_err(SyncError::UnexpectedMessage("Hello")));
    };
    let authorization = if validate_hello(&hello, &self.config).is_ok() {
      self.role_policy.authorize(&hello)
    } else {
      AuthorizeMessage {
        accepted: false,
        role: Role::Viewer,
        reason: Some("protocol, schema, document, or format mismatch".to_string()),
      }
    };
    write_wire_message(&mut send, &WireMessage::Authorize(authorization), self.config.max_message_bytes)
      .await
      .map_err(accept_error)?;
    send.finish()?;
    Ok(())
  }
}

pub fn endpoint_builder() -> iroh::endpoint::Builder {
  Endpoint::builder(presets::N0).alpns(vec![FLOWSTATE_ALPN.to_vec()])
}

pub async fn bind_endpoint() -> AnyResult<Endpoint> {
  endpoint_builder().bind().await.context("failed to bind Flowstate Iroh endpoint")
}

pub fn router(endpoint: Endpoint, config: FlowstateSyncConfig, role_policy: RolePolicy) -> Router {
  iroh::protocol::Router::builder(endpoint)
    .accept(FLOWSTATE_ALPN, FlowstateProtocol { config, role_policy })
    .spawn()
}

pub async fn connect_and_authorize(endpoint: &Endpoint, invite: &InviteTicket, config: &FlowstateSyncConfig) -> AnyResult<AuthorizeMessage> {
  let connection = endpoint
    .connect(invite.endpoint_addr.clone(), FLOWSTATE_ALPN)
    .await
    .context("failed to connect to Flowstate peer")?;
  let (mut send, mut recv) = connection.open_bi().await.context("failed to open Flowstate handshake stream")?;
  let hello = config.hello(Vec::new(), invite.capability.clone());
  write_wire_message(&mut send, &WireMessage::Hello(hello), config.max_message_bytes).await?;
  send.finish().context("failed to finish Flowstate hello stream")?;
  let message = read_wire_message(&mut recv, config.max_message_bytes).await?;
  let WireMessage::Authorize(authorization) = message else {
    bail!(SyncError::UnexpectedMessage("Authorize"));
  };
  if !authorization.accepted {
    bail!(SyncError::Unauthorized(
      authorization
        .reason
        .clone()
        .unwrap_or_else(|| "peer rejected session".to_string())
    ));
  }
  Ok(authorization)
}

pub async fn write_wire_message(send: &mut SendStream, message: &WireMessage, max_message_bytes: usize) -> AnyResult<()> {
  write_frame(send, &encode_wire_message(message)?, max_message_bytes).await
}

pub async fn read_wire_message(recv: &mut RecvStream, max_message_bytes: usize) -> AnyResult<WireMessage> {
  decode_wire_message(&read_frame(recv, max_message_bytes).await?).context("failed to decode Flowstate wire message")
}

pub async fn write_frame(send: &mut SendStream, bytes: &[u8], max_message_bytes: usize) -> AnyResult<()> {
  ensure!(
    bytes.len() <= max_message_bytes,
    SyncError::FrameTooLarge {
      len: bytes.len(),
      max: max_message_bytes,
    }
  );
  let len = u32::try_from(bytes.len()).context("Flowstate frame length exceeds u32")?;
  send
    .write_all(&len.to_le_bytes())
    .await
    .context("failed to write Flowstate frame length")?;
  send
    .write_all(bytes)
    .await
    .context("failed to write Flowstate frame payload")
}

pub async fn read_frame(recv: &mut RecvStream, max_message_bytes: usize) -> AnyResult<Vec<u8>> {
  let mut len = [0; 4];
  recv
    .read_exact(&mut len)
    .await
    .context("failed to read Flowstate frame length")?;
  let len = u32::from_le_bytes(len) as usize;
  ensure!(len <= max_message_bytes, SyncError::FrameTooLarge { len, max: max_message_bytes });
  let mut bytes = vec![0; len];
  recv
    .read_exact(&mut bytes)
    .await
    .context("failed to read Flowstate frame payload")?;
  Ok(bytes)
}

pub fn validate_hello(hello: &HelloMessage, config: &FlowstateSyncConfig) -> AnyResult<()> {
  ensure!(hello.protocol_version == FLOWSTATE_PROTOCOL_VERSION, SyncError::ProtocolMismatch);
  ensure!(hello.collab_schema == COLLAB_SCHEMA_VERSION, SyncError::ProtocolMismatch);
  ensure!(hello.crdt_engine == "loro", SyncError::ProtocolMismatch);
  ensure!(hello.document_id == config.document_id, SyncError::ProtocolMismatch);
  ensure!(hello.format_kind == config.format_kind, SyncError::ProtocolMismatch);
  Ok(())
}

#[must_use]
pub fn asset_have(document_id: DocumentId, store: &AssetStore) -> WireMessage {
  WireMessage::AssetHave(AssetHaveMessage {
    document_id,
    assets: store.hashes(),
  })
}

#[must_use]
pub fn asset_need(document_id: DocumentId, blake3_hash: [u8; 32], offset: u64, len: u64) -> WireMessage {
  WireMessage::AssetNeed(AssetNeedMessage {
    document_id,
    blake3_hash,
    offset,
    len,
  })
}

fn bounded_range(offset: u64, len: u64, available: usize, max_chunk_bytes: usize) -> AnyResult<Range<usize>> {
  let start = usize::try_from(offset).context("asset offset overflows usize")?;
  let requested = usize::try_from(len).context("asset length overflows usize")?;
  let len = requested.min(max_chunk_bytes);
  let end = start.checked_add(len).context("asset range overflows usize")?;
  ensure!(start <= available && end <= available, "asset range is out of bounds");
  Ok(start..end)
}

fn role_includes(granted: Role, requested: Role) -> bool {
  matches!(
    (granted, requested),
    (Role::Owner, Role::Owner | Role::Editor | Role::Viewer)
      | (Role::Editor, Role::Editor | Role::Viewer)
      | (Role::Viewer, Role::Viewer)
  )
}

const fn role_wire_code(role: Role) -> u8 {
  match role {
    Role::Owner => 1,
    Role::Editor => 2,
    Role::Viewer => 3,
  }
}

fn accept_error(error: anyhow::Error) -> AcceptError {
  AcceptError::from_err(std::io::Error::other(error.to_string()))
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn role_policy_rejects_escalation() {
    let owner = ActorId::new();
    let editor = ActorId::new();
    let mut policy = RolePolicy::owner_only(owner);
    policy.editors.insert(editor);
    let config = FlowstateSyncConfig::new(DocumentId::new(), FormatKind::Db8, Role::Owner);
    let mut hello = config.hello(Vec::new(), Vec::new());
    hello.actor_id = editor;
    let authorization = policy.authorize(&hello);
    assert!(!authorization.accepted);
    assert_eq!(authorization.role, Role::Editor);
  }

  #[test]
  fn asset_chunks_are_hash_verified() {
    let mut store = AssetStore::default();
    let asset = store.insert_verified(b"abcdef".to_vec());
    let request = AssetNeedMessage {
      document_id: DocumentId::new(),
      blake3_hash: asset.hash,
      offset: 0,
      len: 6,
    };
    let chunk = store.chunk(&request, DEFAULT_MAX_ASSET_CHUNK_BYTES).unwrap();
    let mut receiving = AssetStore::default();
    receiving.insert_complete_chunk(chunk, 6).unwrap();
    assert!(receiving.contains(&asset.hash));
  }
}
