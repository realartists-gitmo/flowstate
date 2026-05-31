use std::{
  collections::{BTreeMap, HashSet},
  fmt,
  ops::Range,
  sync::{Arc, Mutex},
  time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result as AnyResult, bail, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
use flowstate_collab::{
  ActorId, AssetChunkMessage, AssetHaveMessage, AssetNeedMessage, AuthorizeMessage, COLLAB_SCHEMA_VERSION, CollabDocument, DocumentId,
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
pub const FLOWSTATE_INVITE_PREFIX: &str = "flowstate://collab/";

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
  pub expires_unix_secs: Option<u64>,
  pub label: Option<String>,
  pub multi_use: bool,
}

impl InviteTicket {
  #[must_use]
  pub fn new(endpoint_addr: EndpointAddr, document_id: DocumentId, format_kind: FormatKind, invited_role: Role) -> Self {
    Self::new_with_options(endpoint_addr, document_id, format_kind, invited_role, None, None, true)
  }

  #[must_use]
  pub fn new_with_options(
    endpoint_addr: EndpointAddr,
    document_id: DocumentId,
    format_kind: FormatKind,
    invited_role: Role,
    expires_unix_secs: Option<u64>,
    label: Option<String>,
    multi_use: bool,
  ) -> Self {
    let mut seed = Vec::with_capacity(96);
    let first_nonce = ActorId::new();
    let second_nonce = ActorId::new();
    seed.extend_from_slice(document_id.0.as_bytes());
    seed.extend_from_slice(first_nonce.0.as_bytes());
    seed.extend_from_slice(second_nonce.0.as_bytes());
    seed.extend_from_slice(&[format_kind.as_u8(), role_wire_code(invited_role)]);
    let capability = blake3_hash(&seed).to_vec();
    Self {
      endpoint_addr,
      document_id,
      format_kind,
      invited_role,
      capability,
      expires_unix_secs,
      label,
      multi_use,
    }
  }

  #[must_use]
  pub fn redacted(&self) -> RedactedInviteTicket {
    RedactedInviteTicket {
      endpoint_addr: self.endpoint_addr.clone(),
      document_id: self.document_id,
      format_kind: self.format_kind,
      invited_role: self.invited_role,
      expires_unix_secs: self.expires_unix_secs,
      label: self.label.clone(),
      capability_hash: blake3_hash(&self.capability),
      multi_use: self.multi_use,
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RedactedInviteTicket {
  pub endpoint_addr: EndpointAddr,
  pub document_id: DocumentId,
  pub format_kind: FormatKind,
  pub invited_role: Role,
  pub expires_unix_secs: Option<u64>,
  pub label: Option<String>,
  pub capability_hash: [u8; 32],
  pub multi_use: bool,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
struct InviteLinkPayload {
  protocol_version: u32,
  endpoint_addr: EndpointAddr,
  document_id: DocumentId,
  format_kind: FormatKind,
  invited_role: Role,
  capability: Vec<u8>,
  expires_unix_secs: Option<u64>,
  label: Option<String>,
  multi_use: bool,
}

impl From<&InviteTicket> for InviteLinkPayload {
  fn from(ticket: &InviteTicket) -> Self {
    Self {
      protocol_version: FLOWSTATE_PROTOCOL_VERSION,
      endpoint_addr: ticket.endpoint_addr.clone(),
      document_id: ticket.document_id,
      format_kind: ticket.format_kind,
      invited_role: ticket.invited_role,
      capability: ticket.capability.clone(),
      expires_unix_secs: ticket.expires_unix_secs,
      label: ticket.label.clone(),
      multi_use: ticket.multi_use,
    }
  }
}

impl TryFrom<InviteLinkPayload> for InviteTicket {
  type Error = anyhow::Error;

  fn try_from(payload: InviteLinkPayload) -> AnyResult<Self> {
    ensure!(payload.protocol_version == FLOWSTATE_PROTOCOL_VERSION, SyncError::ProtocolMismatch);
    ensure!(!payload.capability.is_empty(), "invite capability is empty");
    Ok(Self {
      endpoint_addr: payload.endpoint_addr,
      document_id: payload.document_id,
      format_kind: payload.format_kind,
      invited_role: payload.invited_role,
      capability: payload.capability,
      expires_unix_secs: payload.expires_unix_secs,
      label: payload.label,
      multi_use: payload.multi_use,
    })
  }
}

pub fn encode_invite_link(ticket: &InviteTicket) -> AnyResult<String> {
  let payload = postcard::to_stdvec(&InviteLinkPayload::from(ticket)).context("failed to encode Flowstate invite payload")?;
  Ok(format!("{FLOWSTATE_INVITE_PREFIX}{}", URL_SAFE_NO_PAD.encode(payload)))
}

pub fn decode_invite_link(link: &str) -> AnyResult<InviteTicket> {
  let payload = link
    .strip_prefix(FLOWSTATE_INVITE_PREFIX)
    .context("Flowstate invite link has the wrong scheme")?;
  let bytes = URL_SAFE_NO_PAD
    .decode(payload)
    .context("Flowstate invite link payload is not base64url")?;
  postcard::from_bytes::<InviteLinkPayload>(&bytes)
    .context("Flowstate invite link payload is invalid")?
    .try_into()
}

#[derive(Clone, Debug, Default)]
pub struct InviteRegistry {
  tickets: Arc<Mutex<BTreeMap<Vec<u8>, InviteTicket>>>,
}

impl InviteRegistry {
  #[must_use]
  pub fn new() -> Self {
    Self::default()
  }

  pub fn issue(
    &self,
    endpoint_addr: EndpointAddr,
    document_id: DocumentId,
    format_kind: FormatKind,
    invited_role: Role,
    expires_unix_secs: Option<u64>,
    label: Option<String>,
    multi_use: bool,
  ) -> AnyResult<InviteTicket> {
    let ticket = InviteTicket::new_with_options(
      endpoint_addr,
      document_id,
      format_kind,
      invited_role,
      expires_unix_secs,
      label,
      multi_use,
    );
    self.insert(ticket.clone())?;
    Ok(ticket)
  }

  pub fn insert(&self, ticket: InviteTicket) -> AnyResult<()> {
    self
      .tickets
      .lock()
      .map_err(|_| anyhow::anyhow!("Flowstate invite registry lock is poisoned"))?
      .insert(ticket.capability.clone(), ticket);
    Ok(())
  }

  pub fn revoke(&self, capability: &[u8]) -> AnyResult<bool> {
    Ok(
      self
        .tickets
        .lock()
        .map_err(|_| anyhow::anyhow!("Flowstate invite registry lock is poisoned"))?
        .remove(capability)
        .is_some(),
    )
  }

  #[must_use]
  pub fn is_empty(&self) -> bool {
    self
      .tickets
      .lock()
      .map(|tickets| tickets.is_empty())
      .unwrap_or(true)
  }

  pub fn authorize(&self, hello: &HelloMessage) -> Option<AuthorizeMessage> {
    if hello.invite_capability.is_empty() {
      return None;
    }
    let ticket = match self.tickets.lock() {
      Ok(tickets) => tickets.get(&hello.invite_capability).cloned(),
      Err(_) => {
        return Some(AuthorizeMessage {
          accepted: false,
          role: Role::Viewer,
          reason: Some("invite registry is unavailable".to_string()),
        });
      },
    };
    let Some(ticket) = ticket else {
      return Some(AuthorizeMessage {
        accepted: false,
        role: Role::Viewer,
        reason: Some("invite capability is invalid or revoked".to_string()),
      });
    };
    if ticket.expires_unix_secs.is_some_and(|expiry| now_unix_secs() > expiry) {
      let _ = self.revoke(&hello.invite_capability);
      return Some(AuthorizeMessage {
        accepted: false,
        role: Role::Viewer,
        reason: Some("invite capability has expired".to_string()),
      });
    }
    if ticket.document_id != hello.document_id || ticket.format_kind != hello.format_kind {
      return Some(AuthorizeMessage {
        accepted: false,
        role: Role::Viewer,
        reason: Some("invite target does not match requested document".to_string()),
      });
    }
    if !role_includes(ticket.invited_role, hello.role_request) {
      return Some(AuthorizeMessage {
        accepted: false,
        role: ticket.invited_role,
        reason: Some("requested role exceeds invite role".to_string()),
      });
    }
    if !ticket.multi_use {
      let _ = self.revoke(&hello.invite_capability);
    }
    Some(AuthorizeMessage {
      accepted: true,
      role: hello.role_request,
      reason: None,
    })
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
  pub invite_registry: InviteRegistry,
  pub document_state: Option<SessionDocumentState>,
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
      self
        .invite_registry
        .authorize(&hello)
        .unwrap_or_else(|| self.role_policy.authorize(&hello))
    } else {
      AuthorizeMessage {
        accepted: false,
        role: Role::Viewer,
        reason: Some("protocol, schema, document, or format mismatch".to_string()),
      }
    };
    write_wire_message(&mut send, &WireMessage::Authorize(authorization.clone()), self.config.max_message_bytes)
      .await
      .map_err(accept_error)?;

    if authorization.accepted
      && let Some(state) = &self.document_state
    {
      send_snapshot_and_have(&mut send, state, &self.config).await.map_err(accept_error)?;
      serve_live_stream(&mut send, &mut recv, &hello, authorization.role, state, &self.config)
        .await
        .map_err(accept_error)?;
    }
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
  router_with_invites(endpoint, config, role_policy, InviteRegistry::default(), None)
}

pub fn router_with_invites(
  endpoint: Endpoint,
  config: FlowstateSyncConfig,
  role_policy: RolePolicy,
  invite_registry: InviteRegistry,
  document_state: Option<SessionDocumentState>,
) -> Router {
  iroh::protocol::Router::builder(endpoint)
    .accept(
      FLOWSTATE_ALPN,
      FlowstateProtocol {
        config,
        role_policy,
        invite_registry,
        document_state,
      },
    )
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

#[derive(Clone, Debug)]
pub struct SessionDocumentState {
  pub document: Arc<Mutex<CollabDocument>>,
  pub assets: Arc<Mutex<AssetStore>>,
}

impl SessionDocumentState {
  #[must_use]
  pub fn new(document: CollabDocument, assets: AssetStore) -> Self {
    Self {
      document: Arc::new(Mutex::new(document)),
      assets: Arc::new(Mutex::new(assets)),
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum SessionState {
  Idle,
  Hosting,
  Joining,
  SyncingSnapshot,
  Live,
  Reconnecting,
  Closed,
  Failed,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SessionEvent {
  StateChanged(SessionState),
  PeerAuthorized { actor_id: ActorId, session_id: SessionId, role: Role },
  SnapshotApplied { document_id: DocumentId, hash: [u8; 32] },
  UpdateApplied { document_id: DocumentId, hash: [u8; 32] },
  Presence(PeerPresence),
  AssetReceived { document_id: DocumentId, hash: [u8; 32], byte_len: u64 },
  Error(String),
}

#[derive(Clone, Debug)]
pub struct JoinedSnapshot {
  pub authorization: AuthorizeMessage,
  pub document: CollabDocument,
  pub assets_available: Vec<[u8; 32]>,
}

pub async fn connect_and_receive_snapshot(
  endpoint: &Endpoint,
  invite: &InviteTicket,
  config: &FlowstateSyncConfig,
) -> AnyResult<JoinedSnapshot> {
  let connection = endpoint
    .connect(invite.endpoint_addr.clone(), FLOWSTATE_ALPN)
    .await
    .context("failed to connect to Flowstate peer")?;
  let (mut send, mut recv) = connection.open_bi().await.context("failed to open Flowstate sync stream")?;
  let hello = config.hello(Vec::new(), invite.capability.clone());
  write_wire_message(&mut send, &WireMessage::Hello(hello), config.max_message_bytes).await?;

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

  let mut document = None;
  let mut assets_available = Vec::new();
  let mut saw_have = false;
  while document.is_none() || !saw_have {
    match read_wire_message(&mut recv, config.max_message_bytes).await? {
      WireMessage::Snapshot { document_id, bytes, hash } => {
        ensure!(document_id == config.document_id, SyncError::ProtocolMismatch);
        ensure!(blake3_hash(&bytes) == hash, "snapshot hash mismatch");
        document = Some(CollabDocument::from_snapshot(
          &bytes,
          Some(config.format_kind),
          Some(config.document_id),
        )?);
      },
      WireMessage::Have { document_id, assets, .. } => {
        ensure!(document_id == config.document_id, SyncError::ProtocolMismatch);
        assets_available = assets;
        saw_have = true;
      },
      WireMessage::AssetHave(message) => {
        ensure!(message.document_id == config.document_id, SyncError::ProtocolMismatch);
        assets_available = message.assets;
        saw_have = true;
      },
      WireMessage::Error { message, .. } => bail!(message),
      _ => bail!(SyncError::UnexpectedMessage("Snapshot")),
    }
  }

  send.finish().context("failed to finish Flowstate sync stream")?;
  Ok(JoinedSnapshot {
    authorization,
    document: document.expect("document checked above"),
    assets_available,
  })
}

pub async fn write_wire_message(send: &mut SendStream, message: &WireMessage, max_message_bytes: usize) -> AnyResult<()> {
  write_frame(send, &encode_wire_message(message)?, max_message_bytes).await
}

pub async fn read_wire_message(recv: &mut RecvStream, max_message_bytes: usize) -> AnyResult<WireMessage> {
  decode_wire_message(&read_frame(recv, max_message_bytes).await?).context("failed to decode Flowstate wire message")
}

async fn send_snapshot_and_have(send: &mut SendStream, state: &SessionDocumentState, config: &FlowstateSyncConfig) -> AnyResult<()> {
  let snapshot = {
    state
      .document
      .lock()
      .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?
      .export_snapshot()?
  };
  write_wire_message(
    send,
    &WireMessage::Snapshot {
      document_id: config.document_id,
      hash: blake3_hash(&snapshot),
      bytes: snapshot,
    },
    config.max_message_bytes,
  )
  .await?;
  let assets = state
    .assets
    .lock()
    .map_err(|_| anyhow::anyhow!("Flowstate asset state lock is poisoned"))?
    .hashes();
  let frontier = {
    state
      .document
      .lock()
      .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?
      .frontier()?
  };
  write_wire_message(
    send,
    &WireMessage::Have {
      document_id: config.document_id,
      frontier,
      assets,
    },
    config.max_message_bytes,
  )
  .await
}

async fn serve_live_stream(
  send: &mut SendStream,
  recv: &mut RecvStream,
  hello: &HelloMessage,
  remote_role: Role,
  state: &SessionDocumentState,
  config: &FlowstateSyncConfig,
) -> AnyResult<()> {
  loop {
    let message = match read_wire_message(recv, config.max_message_bytes).await {
      Ok(message) => message,
      Err(_) => break,
    };
    match message {
      WireMessage::Update { document_id, bytes, hash, .. } => {
        ensure!(document_id == config.document_id, SyncError::ProtocolMismatch);
        ensure!(blake3_hash(&bytes) == hash, "update hash mismatch");
        let frontier = {
          state
            .document
            .lock()
            .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?
            .import_update_checked(remote_role, &bytes)?
            .frontier
        };
        write_wire_message(
          send,
          &WireMessage::Ack {
            document_id: config.document_id,
            frontier,
          },
          config.max_message_bytes,
        )
        .await?;
      },
      WireMessage::Need { document_id, frontier, snapshot } => {
        ensure!(document_id == config.document_id, SyncError::ProtocolMismatch);
        if snapshot {
          send_snapshot_and_have(send, state, config).await?;
        } else {
          let update = state
            .document
            .lock()
            .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?
            .export_update_since_frontier(&frontier)?;
          if !update.is_empty() {
            write_wire_message(
              send,
              &WireMessage::Update {
                document_id,
                actor_id: config.actor_id,
                hash: blake3_hash(&update),
                bytes: update,
              },
              config.max_message_bytes,
            )
            .await?;
          }
        }
      },
      WireMessage::AssetNeed(request) => {
        ensure!(request.document_id == config.document_id, SyncError::ProtocolMismatch);
        let chunk = state
          .assets
          .lock()
          .map_err(|_| anyhow::anyhow!("Flowstate asset state lock is poisoned"))?
          .chunk(&request, config.max_asset_chunk_bytes)?;
        write_wire_message(send, &WireMessage::AssetChunk(chunk), config.max_message_bytes).await?;
      },
      WireMessage::Presence(_) => {
        let frontier = state
          .document
          .lock()
          .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?
          .frontier()?;
        write_wire_message(
          send,
          &WireMessage::Ack {
            document_id: hello.document_id,
            frontier,
          },
          config.max_message_bytes,
        )
        .await?;
      },
      WireMessage::Hello(_)
      | WireMessage::Authorize(_)
      | WireMessage::Have { .. }
      | WireMessage::Snapshot { .. }
      | WireMessage::AssetHave(_)
      | WireMessage::AssetChunk(_)
      | WireMessage::Ack { .. }
      | WireMessage::Error { .. } => {},
    }
  }
  Ok(())
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

fn now_unix_secs() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|duration| duration.as_secs())
    .unwrap_or_default()
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

  #[test]
  fn invite_link_round_trips() {
    let endpoint_addr = EndpointAddr::new(iroh::SecretKey::generate().public());
    let ticket = InviteTicket::new(endpoint_addr, DocumentId::new(), FormatKind::Db8, Role::Editor);
    let link = encode_invite_link(&ticket).unwrap();
    let decoded = decode_invite_link(&link).unwrap();
    assert_eq!(decoded.document_id, ticket.document_id);
    assert_eq!(decoded.format_kind, ticket.format_kind);
    assert_eq!(decoded.invited_role, ticket.invited_role);
    assert_eq!(decoded.capability, ticket.capability);
  }

  #[test]
  fn invite_registry_rejects_tampered_role() {
    let endpoint_addr = EndpointAddr::new(iroh::SecretKey::generate().public());
    let document_id = DocumentId::new();
    let registry = InviteRegistry::new();
    let ticket = registry
      .issue(endpoint_addr, document_id, FormatKind::Fl0, Role::Viewer, None, None, true)
      .unwrap();
    let config = FlowstateSyncConfig::new(document_id, FormatKind::Fl0, Role::Editor);
    let hello = config.hello(Vec::new(), ticket.capability);
    let authorization = registry.authorize(&hello).unwrap();
    assert!(!authorization.accepted);
    assert_eq!(authorization.role, Role::Viewer);
  }

  #[tokio::test]
  async fn host_join_receives_snapshot_through_iroh() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Fl0, document_id, owner, b"projection", &[]).unwrap();
    let host_endpoint = bind_endpoint().await.unwrap();
    let host_config = FlowstateSyncConfig::new(document_id, FormatKind::Fl0, Role::Owner);
    let registry = InviteRegistry::new();
    let state = SessionDocumentState::new(host_doc, AssetStore::default());
    let router = router_with_invites(
      host_endpoint,
      host_config.clone(),
      RolePolicy::owner_only(host_config.actor_id),
      registry.clone(),
      Some(state),
    );
    router.endpoint().online().await;

    let ticket = registry
      .issue(router.endpoint().addr(), document_id, FormatKind::Fl0, Role::Editor, None, None, true)
      .unwrap();
    let client_endpoint = bind_endpoint().await.unwrap();
    let client_config = FlowstateSyncConfig::new(document_id, FormatKind::Fl0, Role::Editor);
    let joined = connect_and_receive_snapshot(&client_endpoint, &ticket, &client_config)
      .await
      .unwrap();

    assert_eq!(joined.authorization.role, Role::Editor);
    assert_eq!(joined.document.materialize_projection_cache().unwrap(), b"projection");
    router.shutdown().await.unwrap();
    client_endpoint.close().await;
  }
}
