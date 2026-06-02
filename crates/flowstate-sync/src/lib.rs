use std::{
  collections::{BTreeMap, HashMap, HashSet, VecDeque},
  fmt,
  future::Future,
  ops::Range,
  sync::{Arc, Mutex},
  time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context as _, Result as AnyResult, bail, ensure};
use base64::{Engine as _, engine::general_purpose::URL_SAFE_NO_PAD};
pub use flowstate_collab::{ActorId, DocumentId, FormatKind, Role, SessionId};
use flowstate_collab::{
  AssetChunkMessage, AssetHaveMessage, AssetNeedMessage, AuthorizeMessage, COLLAB_SCHEMA_VERSION, CollabDocument, FLOWSTATE_ALPN, HelloMessage,
  NativeAssetRecord, PeerEventKind, PeerEventMessage, PresenceMessage, UpdateApplication, WireMessage, blake3_hash, decode_wire_message,
  encode_wire_message,
};
use iroh::{
  Endpoint, EndpointAddr,
  endpoint::{Connection, RecvStream, SendStream, presets},
  protocol::{AcceptError, ProtocolHandler, Router},
};
use tokio::runtime::Runtime;
use tokio::sync::broadcast;

pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_MAX_SNAPSHOT_BYTES: usize = DEFAULT_MAX_MESSAGE_BYTES;
pub const DEFAULT_MAX_UPDATE_BYTES: usize = DEFAULT_MAX_MESSAGE_BYTES;
pub const DEFAULT_MAX_ASSET_CHUNK_BYTES: usize = 1024 * 1024;
pub const DEFAULT_MAX_PEER_COUNT: usize = 64;
pub const DEFAULT_MAX_PRESENCE_MESSAGES_PER_MINUTE: usize = 600;
pub const DEFAULT_MAX_ASSET_REQUESTS_PER_MINUTE: usize = 120;
const RATE_LIMIT_WINDOW_MILLIS: u64 = 60_000;
pub const FLOWSTATE_PROTOCOL_VERSION: u32 = 1;
pub const FLOWSTATE_INVITE_PREFIX: &str = "flowstate://collab/";

static SYNC_RUNTIME: std::sync::LazyLock<Runtime> = std::sync::LazyLock::new(|| {
  tokio::runtime::Builder::new_multi_thread()
    .enable_all()
    .thread_name("flowstate-sync")
    .build()
    .expect("failed to initialize Flowstate sync runtime")
});

pub fn run_on_sync_runtime<F>(future: F) -> F::Output
where
  F: Future,
{
  SYNC_RUNTIME.block_on(future)
}

#[derive(Clone, Debug)]
pub struct FlowstateSyncConfig {
  pub app_build_id: String,
  pub document_id: DocumentId,
  pub format_kind: FormatKind,
  pub actor_id: ActorId,
  pub session_id: SessionId,
  pub role_request: Role,
  pub max_message_bytes: usize,
  pub max_snapshot_bytes: usize,
  pub max_update_bytes: usize,
  pub max_asset_chunk_bytes: usize,
  pub max_peer_count: usize,
  pub max_presence_messages_per_minute: usize,
  pub max_asset_requests_per_minute: usize,
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
      max_snapshot_bytes: DEFAULT_MAX_SNAPSHOT_BYTES,
      max_update_bytes: DEFAULT_MAX_UPDATE_BYTES,
      max_asset_chunk_bytes: DEFAULT_MAX_ASSET_CHUNK_BYTES,
      max_peer_count: DEFAULT_MAX_PEER_COUNT,
      max_presence_messages_per_minute: DEFAULT_MAX_PRESENCE_MESSAGES_PER_MINUTE,
      max_asset_requests_per_minute: DEFAULT_MAX_ASSET_REQUESTS_PER_MINUTE,
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

  pub fn set_actor_role(&mut self, actor_id: ActorId, role: Role) {
    self.editors.remove(&actor_id);
    self.viewers.remove(&actor_id);
    match role {
      Role::Owner => self.owner = actor_id,
      Role::Editor => {
        self.editors.insert(actor_id);
      },
      Role::Viewer => {
        self.viewers.insert(actor_id);
      },
    }
  }

  pub fn remove_actor(&mut self, actor_id: ActorId) -> bool {
    if actor_id == self.owner {
      return false;
    }
    self.editors.remove(&actor_id) || self.viewers.remove(&actor_id)
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
    let ticket = InviteTicket::new_with_options(endpoint_addr, document_id, format_kind, invited_role, expires_unix_secs, label, multi_use);
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

  pub fn revoke_all(&self) -> AnyResult<usize> {
    let revoked = {
      let mut tickets = self
        .tickets
        .lock()
        .map_err(|_| anyhow::anyhow!("Flowstate invite registry lock is poisoned"))?;
      let revoked = tickets.len();
      tickets.clear();
      revoked
    };
    Ok(revoked)
  }

  pub fn redacted_tickets(&self) -> AnyResult<Vec<RedactedInviteTicket>> {
    Ok(
      self
        .tickets
        .lock()
        .map_err(|_| anyhow::anyhow!("Flowstate invite registry lock is poisoned"))?
        .values()
        .map(InviteTicket::redacted)
        .collect(),
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
    if ticket
      .expires_unix_secs
      .is_some_and(|expiry| now_unix_secs() > expiry)
    {
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

  #[must_use]
  pub fn get_verified(&self, hash: &[u8; 32]) -> Option<VerifiedAsset> {
    self.assets.get(hash).map(|bytes| VerifiedAsset {
      hash: *hash,
      bytes: bytes.clone(),
    })
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
      actor_id: self.actor_id,
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

#[derive(Clone, Debug)]
pub struct LiveUpdate {
  pub source_session_id: Option<SessionId>,
  pub kind: LiveUpdateKind,
}

#[derive(Clone, Debug)]
pub enum LiveUpdateKind {
  Wire(WireMessage),
  Event(SessionEvent),
}

impl LiveUpdate {
  #[must_use]
  pub fn wire(source_session_id: Option<SessionId>, message: WireMessage) -> Self {
    Self {
      source_session_id,
      kind: LiveUpdateKind::Wire(message),
    }
  }

  #[must_use]
  pub fn event(source_session_id: Option<SessionId>, event: SessionEvent) -> Self {
    Self {
      source_session_id,
      kind: LiveUpdateKind::Event(event),
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct LivePeer {
  pub actor_id: ActorId,
  pub session_id: SessionId,
  pub role: Role,
}

#[derive(Clone, Debug)]
struct RateWindow {
  max_events: usize,
  window_millis: u64,
  events: VecDeque<u64>,
}

impl RateWindow {
  fn new(max_events: usize, window_millis: u64) -> Self {
    Self {
      max_events,
      window_millis,
      events: VecDeque::new(),
    }
  }

  fn check(&mut self, now_millis: u64) -> bool {
    let window_start = now_millis.saturating_sub(self.window_millis);
    while self
      .events
      .front()
      .is_some_and(|event| *event <= window_start)
    {
      self.events.pop_front();
    }
    if self.events.len() >= self.max_events {
      return false;
    }
    self.events.push_back(now_millis);
    true
  }
}

#[derive(Clone, Debug)]
pub struct LiveUpdateHub {
  sender: broadcast::Sender<LiveUpdate>,
  peers: Arc<Mutex<HashMap<SessionId, LivePeer>>>,
}

impl Default for LiveUpdateHub {
  fn default() -> Self {
    Self::new(1024)
  }
}

impl LiveUpdateHub {
  #[must_use]
  pub fn new(capacity: usize) -> Self {
    let (sender, _) = broadcast::channel(capacity.max(1));
    Self {
      sender,
      peers: Arc::new(Mutex::new(HashMap::new())),
    }
  }

  pub fn subscribe(&self) -> broadcast::Receiver<LiveUpdate> {
    self.sender.subscribe()
  }

  pub fn upsert_peer(&self, peer: LivePeer) -> AnyResult<()> {
    self
      .peers
      .lock()
      .map_err(|_| anyhow::anyhow!("Flowstate live peer roster lock is poisoned"))?
      .insert(peer.session_id, peer);
    Ok(())
  }

  #[allow(
    clippy::significant_drop_tightening,
    reason = "the mutex guard must stay live while mutating and copying the selected peer"
  )]
  pub fn update_peer_role(&self, session_id: SessionId, role: Role) -> AnyResult<Option<LivePeer>> {
    let peer = {
      let mut peers = self
        .peers
        .lock()
        .map_err(|_| anyhow::anyhow!("Flowstate live peer roster lock is poisoned"))?;
      let Some(peer) = peers.get_mut(&session_id) else {
        return Ok(None);
      };
      peer.role = role;
      *peer
    };
    Ok(Some(peer))
  }

  pub fn remove_peer(&self, session_id: SessionId) -> AnyResult<Option<LivePeer>> {
    Ok(
      self
        .peers
        .lock()
        .map_err(|_| anyhow::anyhow!("Flowstate live peer roster lock is poisoned"))?
        .remove(&session_id),
    )
  }

  pub fn peers(&self) -> AnyResult<Vec<LivePeer>> {
    Ok(
      self
        .peers
        .lock()
        .map_err(|_| anyhow::anyhow!("Flowstate live peer roster lock is poisoned"))?
        .values()
        .copied()
        .collect(),
    )
  }

  pub fn peer_count(&self) -> AnyResult<usize> {
    Ok(
      self
        .peers
        .lock()
        .map_err(|_| anyhow::anyhow!("Flowstate live peer roster lock is poisoned"))?
        .len(),
    )
  }

  pub fn publish(&self, update: LiveUpdate) -> AnyResult<()> {
    match self.sender.send(update) {
      Ok(_) => Ok(()),
      Err(_) if self.sender.receiver_count() == 0 => Ok(()),
      Err(error) => Err(error).context("failed to publish Flowstate live update"),
    }
  }
}

impl std::error::Error for SyncError {}

#[derive(Clone, Debug)]
pub struct FlowstateProtocol {
  pub config: FlowstateSyncConfig,
  pub role_policy: RolePolicy,
  pub live_updates: Option<LiveUpdateHub>,
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
    let mut authorization = if validate_hello(&hello, &self.config).is_ok() {
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
    if authorization.accepted
      && let Some(hub) = &self.live_updates
      && hub.peer_count().map_err(accept_error)? >= self.config.max_peer_count
    {
      authorization = AuthorizeMessage {
        accepted: false,
        role: authorization.role,
        reason: Some("peer limit reached".to_string()),
      };
    }
    write_wire_message(&mut send, &WireMessage::Authorize(authorization.clone()), self.config.max_message_bytes)
      .await
      .map_err(accept_error)?;

    if authorization.accepted
      && let Some(state) = &self.document_state
    {
      let live_rx = self.live_updates.as_ref().map(LiveUpdateHub::subscribe);
      if let Some(hub) = &self.live_updates {
        hub
          .upsert_peer(LivePeer {
            actor_id: hello.actor_id,
            session_id: hello.session_id,
            role: authorization.role,
          })
          .map_err(accept_error)?;
        hub
          .publish(LiveUpdate::event(
            Some(hello.session_id),
            SessionEvent::PeerAuthorized {
              actor_id: hello.actor_id,
              session_id: hello.session_id,
              role: authorization.role,
            },
          ))
          .map_err(accept_error)?;
      }
      let stream_result = async {
        send_snapshot_and_have(&mut send, state, &self.config).await?;
        if let Some(hub) = &self.live_updates {
          send_peer_roster(&mut send, hub, hello.session_id, self.config.document_id, self.config.max_message_bytes).await?;
        }
        serve_live_stream(
          &mut send,
          &mut recv,
          &hello,
          authorization.role,
          state,
          &self.config,
          live_rx,
          self.live_updates.as_ref(),
        )
        .await
      }
      .await;
      if let Some(hub) = &self.live_updates
        && let Some(peer) = hub.remove_peer(hello.session_id).map_err(accept_error)?
      {
        hub
          .publish(LiveUpdate::event(
            Some(hello.session_id),
            SessionEvent::PeerLeft {
              actor_id: peer.actor_id,
              session_id: peer.session_id,
            },
          ))
          .map_err(accept_error)?;
      }
      stream_result.map_err(accept_error)?;
    }
    send.finish()?;
    Ok(())
  }
}

pub fn endpoint_builder() -> iroh::endpoint::Builder {
  Endpoint::builder(presets::N0).alpns(vec![FLOWSTATE_ALPN.to_vec()])
}

pub async fn bind_endpoint() -> AnyResult<Endpoint> {
  endpoint_builder()
    .bind()
    .await
    .context("failed to bind Flowstate Iroh endpoint")
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
  router_with_live_updates(endpoint, config, role_policy, invite_registry, document_state, None)
}

pub fn router_with_live_updates(
  endpoint: Endpoint,
  config: FlowstateSyncConfig,
  role_policy: RolePolicy,
  invite_registry: InviteRegistry,
  document_state: Option<SessionDocumentState>,
  live_updates: Option<LiveUpdateHub>,
) -> Router {
  iroh::protocol::Router::builder(endpoint)
    .accept(
      FLOWSTATE_ALPN,
      FlowstateProtocol {
        config,
        role_policy,
        live_updates,
        invite_registry,
        document_state,
      },
    )
    .spawn()
}

pub struct HostedCollaboration {
  router: Router,
  registry: InviteRegistry,
  config: FlowstateSyncConfig,
  document_state: SessionDocumentState,
  live_updates: LiveUpdateHub,
}

impl HostedCollaboration {
  pub async fn start(document: CollabDocument, assets: AssetStore, local_role: Role) -> AnyResult<Self> {
    let config = FlowstateSyncConfig::new(document.document_id(), document.format_kind(), local_role);
    Self::start_with_config(document, assets, config).await
  }

  pub async fn start_with_config(document: CollabDocument, assets: AssetStore, config: FlowstateSyncConfig) -> AnyResult<Self> {
    ensure!(
      document.document_id() == config.document_id,
      "host document id does not match sync config"
    );
    ensure!(
      document.format_kind() == config.format_kind,
      "host document format does not match sync config"
    );
    let endpoint = bind_endpoint().await?;
    let registry = InviteRegistry::new();
    let state = SessionDocumentState::new(document, assets);
    let live_updates = LiveUpdateHub::default();
    let router = router_with_live_updates(
      endpoint,
      config.clone(),
      RolePolicy::owner_only(config.actor_id),
      registry.clone(),
      Some(state.clone()),
      Some(live_updates.clone()),
    );
    Ok(Self {
      router,
      registry,
      config,
      document_state: state,
      live_updates,
    })
  }

  #[must_use]
  pub const fn document_id(&self) -> DocumentId {
    self.config.document_id
  }

  #[must_use]
  pub const fn format_kind(&self) -> FormatKind {
    self.config.format_kind
  }

  pub fn issue_invite_link(&self, invited_role: Role, label: Option<String>, multi_use: bool) -> AnyResult<String> {
    let ticket = self.registry.issue(
      self.router.endpoint().addr(),
      self.config.document_id,
      self.config.format_kind,
      invited_role,
      None,
      label,
      multi_use,
    )?;
    encode_invite_link(&ticket)
  }

  pub fn revoke_invite_link(&self, link: &str) -> AnyResult<bool> {
    let ticket = decode_invite_link(link)?;
    self.registry.revoke(&ticket.capability)
  }

  pub fn revoke_all_invites(&self) -> AnyResult<usize> {
    self.registry.revoke_all()
  }

  pub fn document_hash(&self) -> AnyResult<[u8; 32]> {
    self
      .document_state
      .document
      .lock()
      .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?
      .projection_hash()
      .map_err(Into::into)
  }

  #[must_use]
  pub fn document_state(&self) -> SessionDocumentState {
    self.document_state.clone()
  }

  pub fn subscribe_live_updates(&self) -> broadcast::Receiver<LiveUpdate> {
    self.live_updates.subscribe()
  }

  pub fn peers(&self) -> AnyResult<Vec<LivePeer>> {
    self.live_updates.peers()
  }

  pub fn set_peer_role(&self, session_id: SessionId, role: Role) -> AnyResult<bool> {
    ensure!(role != Role::Owner, "live peer ownership transfer is not supported");
    let Some(peer) = self.live_updates.update_peer_role(session_id, role)? else {
      return Ok(false);
    };
    self.live_updates.publish(LiveUpdate::event(
      None,
      SessionEvent::PeerRoleChanged {
        actor_id: peer.actor_id,
        session_id: peer.session_id,
        role: peer.role,
      },
    ))?;
    Ok(true)
  }

  pub fn kick_peer(&self, session_id: SessionId) -> AnyResult<bool> {
    let Some(peer) = self.live_updates.remove_peer(session_id)? else {
      return Ok(false);
    };
    self.live_updates.publish(LiveUpdate::event(
      None,
      SessionEvent::PeerLeft {
        actor_id: peer.actor_id,
        session_id: peer.session_id,
      },
    ))?;
    Ok(true)
  }

  pub fn apply_local_update(&self, bytes: Vec<u8>) -> AnyResult<()> {
    ensure!(self.config.role_request.can_write(), "local role is not allowed to publish updates");
    ensure_update_size(&bytes, self.config.max_update_bytes)?;
    let frontier = {
      self
        .document_state
        .document
        .lock()
        .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?
        .import_update_checked(self.config.role_request, &bytes)?
        .frontier
    };
    self.live_updates.publish(LiveUpdate::wire(
      None,
      WireMessage::Update {
        document_id: self.config.document_id,

        actor_id: self.config.actor_id,
        hash: blake3_hash(&bytes),
        bytes,
        application: None,
      },
    ))?;
    self.live_updates.publish(LiveUpdate::wire(
      None,
      WireMessage::Ack {
        document_id: self.config.document_id,
        frontier,
      },
    ))
  }

  pub fn publish_presence(
    &self,
    user_label: impl Into<String>,
    cursor: Option<String>,
    focus: Option<String>,
    viewport_hint: Option<String>,
  ) -> AnyResult<()> {
    let frontier = self
      .document_state
      .document
      .lock()
      .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?
      .frontier()?;
    let presence = PeerPresence {
      actor_id: self.config.actor_id,
      session_id: self.config.session_id,
      role: self.config.role_request,
      user_label: user_label.into(),
      cursor,
      focus,
      viewport_hint,
      last_known_frontier: frontier,
      monotonic_millis: now_unix_millis(),
    };
    self
      .live_updates
      .publish(LiveUpdate::event(Some(self.config.session_id), SessionEvent::Presence(presence)))?;
    Ok(())
  }

  pub fn publish_application_update(&self, application: UpdateApplication) -> AnyResult<()> {
    ensure!(
      self.config.role_request.can_write(),
      "local role is not allowed to publish application updates"
    );
    self.publish_update(Vec::new(), Some(application))
  }

  pub fn replace_source_from(&self, source: &CollabDocument) -> AnyResult<()> {
    self.publish_update_from_source(source, None)
  }

  pub fn publish_update_from_source(&self, source: &CollabDocument, application: Option<UpdateApplication>) -> AnyResult<()> {
    ensure!(self.config.role_request.can_write(), "local role is not allowed to publish updates");
    ensure!(source.document_id() == self.config.document_id, "replacement source document mismatch");
    ensure!(source.format_kind() == self.config.format_kind, "replacement source format mismatch");
    let projection_cache = source.materialize_projection_cache()?;
    let asset_manifest = source.asset_manifest_bytes()?;
    let update = if let Ok(Some(granular_source)) = source.materialize_granular_source() {
      self
        .document_state
        .document
        .lock()
        .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?
        .replace_granular_source(self.config.role_request, &granular_source, &projection_cache, &asset_manifest)?
    } else {
      self
        .document_state
        .document
        .lock()
        .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?
        .replace_projection_source(self.config.role_request, &projection_cache, &asset_manifest)?
    };
    self.publish_update(update, application)
  }

  pub fn publish_update(&self, update: Vec<u8>, application: Option<UpdateApplication>) -> AnyResult<()> {
    ensure_update_size(&update, self.config.max_update_bytes)?;
    self.live_updates.publish(LiveUpdate::wire(
      None,
      WireMessage::Update {
        document_id: self.config.document_id,
        actor_id: self.config.actor_id,
        hash: blake3_hash(&update),
        bytes: update,
        application,
      },
    ))
  }
  pub async fn shutdown(self) -> AnyResult<()> {
    self
      .router
      .shutdown()
      .await
      .context("failed to shut down Flowstate collaboration host")
  }
}

pub async fn join_invite_snapshot(link: &str) -> AnyResult<JoinedSnapshot> {
  let ticket = decode_invite_link(link)?;
  let endpoint = bind_endpoint().await?;
  let config = FlowstateSyncConfig::new(ticket.document_id, ticket.format_kind, ticket.invited_role);
  let result = connect_and_receive_snapshot(&endpoint, &ticket, &config).await;
  endpoint.close().await;
  result
}

pub async fn connect_and_authorize(endpoint: &Endpoint, invite: &InviteTicket, config: &FlowstateSyncConfig) -> AnyResult<AuthorizeMessage> {
  let connection = endpoint
    .connect(invite.endpoint_addr.clone(), FLOWSTATE_ALPN)
    .await
    .context("failed to connect to Flowstate peer")?;
  let (mut send, mut recv) = connection
    .open_bi()
    .await
    .context("failed to open Flowstate handshake stream")?;
  let hello = config.hello(Vec::new(), invite.capability.clone());
  write_wire_message(&mut send, &WireMessage::Hello(hello), config.max_message_bytes).await?;
  send
    .finish()
    .context("failed to finish Flowstate hello stream")?;
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
  PeerAuthorized {
    actor_id: ActorId,
    session_id: SessionId,
    role: Role,
  },
  PeerLeft {
    actor_id: ActorId,
    session_id: SessionId,
  },
  PeerRoleChanged {
    actor_id: ActorId,
    session_id: SessionId,
    role: Role,
  },
  SnapshotApplied {
    document_id: DocumentId,
    hash: [u8; 32],
  },
  UpdateApplied {
    document_id: DocumentId,
    hash: [u8; 32],
  },
  UpdateRejected {
    document_id: DocumentId,
    actor_id: ActorId,
    reason: String,
  },
  Presence(PeerPresence),
  AssetReceived {
    document_id: DocumentId,
    hash: [u8; 32],
    byte_len: u64,
  },
  AssetTransferFailed {
    document_id: DocumentId,
    hash: [u8; 32],
    reason: String,
  },
  Reconnecting,
  FatalError(String),
  Error(String),
}

#[derive(Clone, Debug)]
pub struct JoinedSnapshot {
  pub authorization: AuthorizeMessage,
  pub document: CollabDocument,
  pub assets_available: Vec<[u8; 32]>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PeerConnection {
  pub actor_id: ActorId,
  pub session_id: SessionId,
  pub role: Role,
  pub known_frontier: Vec<u8>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct OutboundUpdate {
  pub document_id: DocumentId,
  pub actor_id: ActorId,
  pub bytes: Vec<u8>,
  pub hash: [u8; 32],
}

#[derive(Clone, Debug)]
pub struct CollabSession {
  config: FlowstateSyncConfig,
  document_state: SessionDocumentState,
  state: SessionState,
  local_role: Role,
  peers: HashMap<SessionId, PeerConnection>,
  events: VecDeque<SessionEvent>,
  outbound_updates: VecDeque<OutboundUpdate>,
  presence_queue: VecDeque<PresenceMessage>,
}

impl CollabSession {
  #[must_use]
  pub fn new(config: FlowstateSyncConfig, local_role: Role, document_state: SessionDocumentState) -> Self {
    let mut events = VecDeque::new();
    events.push_back(SessionEvent::StateChanged(SessionState::Idle));
    Self {
      config,
      document_state,
      state: SessionState::Idle,
      local_role,
      peers: HashMap::new(),
      events,
      outbound_updates: VecDeque::new(),
      presence_queue: VecDeque::new(),
    }
  }

  #[must_use]
  pub const fn state(&self) -> SessionState {
    self.state
  }

  #[must_use]
  pub const fn local_role(&self) -> Role {
    self.local_role
  }

  #[must_use]
  pub fn peer_count(&self) -> usize {
    self.peers.len()
  }

  pub fn peers(&self) -> impl Iterator<Item = &PeerConnection> {
    self.peers.values()
  }

  pub fn set_state(&mut self, state: SessionState) {
    if self.state != state {
      self.state = state;
      self.events.push_back(SessionEvent::StateChanged(state));
    }
  }

  pub fn upsert_peer(&mut self, hello: &HelloMessage, role: Role) {
    let peer = PeerConnection {
      actor_id: hello.actor_id,
      session_id: hello.session_id,
      role,
      known_frontier: hello.known_frontier.clone(),
    };
    let event = if self.peers.insert(peer.session_id, peer.clone()).is_some() {
      SessionEvent::PeerRoleChanged {
        actor_id: peer.actor_id,
        session_id: peer.session_id,
        role,
      }
    } else {
      SessionEvent::PeerAuthorized {
        actor_id: peer.actor_id,
        session_id: peer.session_id,
        role,
      }
    };
    self.events.push_back(event);
  }

  pub fn remove_peer(&mut self, session_id: SessionId) {
    if let Some(peer) = self.peers.remove(&session_id) {
      self.events.push_back(SessionEvent::PeerLeft {
        actor_id: peer.actor_id,
        session_id,
      });
    }
  }

  pub fn queue_local_update(&mut self, bytes: Vec<u8>) {
    let update = OutboundUpdate {
      document_id: self.config.document_id,
      actor_id: self.config.actor_id,
      hash: blake3_hash(&bytes),
      bytes,
    };
    self.outbound_updates.push_back(update.clone());
    self.events.push_back(SessionEvent::UpdateApplied {
      document_id: update.document_id,
      hash: update.hash,
    });
  }

  pub fn apply_remote_update(&mut self, actor_id: ActorId, remote_role: Role, bytes: &[u8]) -> AnyResult<()> {
    let import_result = {
      let document = self
        .document_state
        .document
        .lock()
        .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?;
      document.import_update_checked(remote_role, bytes)
    };
    match import_result {
      Ok(outcome) => {
        let hash = blake3_hash(bytes);
        if outcome.patch.is_some() {
          self.outbound_updates.push_back(OutboundUpdate {
            document_id: self.config.document_id,
            actor_id,
            bytes: bytes.to_vec(),
            hash,
          });
          self.events.push_back(SessionEvent::UpdateApplied {
            document_id: self.config.document_id,
            hash,
          });
        }
        for peer in self
          .peers
          .values_mut()
          .filter(|peer| peer.actor_id == actor_id)
        {
          peer.known_frontier.clone_from(&outcome.frontier);
        }
        Ok(())
      },
      Err(error) => {
        self.events.push_back(SessionEvent::UpdateRejected {
          document_id: self.config.document_id,
          actor_id,
          reason: error.to_string(),
        });
        Err(error.into())
      },
    }
  }

  pub fn queue_presence(&mut self, presence: PeerPresence) {
    self
      .events
      .push_back(SessionEvent::Presence(presence.clone()));
    self
      .presence_queue
      .push_back(presence.message(self.config.document_id));
  }

  pub fn pop_outbound_update(&mut self) -> Option<OutboundUpdate> {
    self.outbound_updates.pop_front()
  }

  pub fn pop_presence(&mut self) -> Option<PresenceMessage> {
    self.presence_queue.pop_front()
  }

  pub fn drain_events(&mut self) -> Vec<SessionEvent> {
    self.events.drain(..).collect()
  }
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
  let (mut send, mut recv) = connection
    .open_bi()
    .await
    .context("failed to open Flowstate sync stream")?;
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
        ensure_snapshot_size(&bytes, config.max_snapshot_bytes)?;
        ensure!(blake3_hash(&bytes) == hash, "snapshot hash mismatch");
        document = Some(CollabDocument::from_snapshot(&bytes, Some(config.format_kind), Some(config.document_id))?);
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
      WireMessage::PeerEvent(message) => {
        ensure!(message.document_id == config.document_id, SyncError::ProtocolMismatch);
      },
      WireMessage::Error { message, .. } => bail!(message),
      _ => bail!(SyncError::UnexpectedMessage("Snapshot")),
    }
  }

  send
    .finish()
    .context("failed to finish Flowstate sync stream")?;
  let document = document.expect("document checked above");
  document.set_local_actor(config.actor_id)?;
  Ok(JoinedSnapshot {
    authorization,
    document,
    assets_available,
  })
}

pub struct LiveCollaborationClient {
  endpoint: Endpoint,
  send: SendStream,
  pub last_application: Option<UpdateApplication>,
  recv: RecvStream,
  pub authorization: AuthorizeMessage,
  pub document: CollabDocument,
  pub assets_available: Vec<[u8; 32]>,
  pub assets: AssetStore,
  pending_events: VecDeque<(SessionEvent, Option<UpdateApplication>)>,
  config: FlowstateSyncConfig,
}

enum LiveClientMessage {
  Event(SessionEvent, Option<UpdateApplication>),
  AssetChunk(AssetChunkMessage),
  Continue,
}

impl LiveCollaborationClient {
  pub async fn publish_update(&mut self, bytes: Vec<u8>) -> AnyResult<()> {
    ensure!(self.authorization.role.can_write(), "local role is not allowed to publish updates");
    ensure_update_size(&bytes, self.config.max_update_bytes)?;
    let hash = blake3_hash(&bytes);
    self
      .document
      .import_update_checked(self.authorization.role, &bytes)?;
    write_wire_message(
      &mut self.send,
      &WireMessage::Update {
        document_id: self.config.document_id,
        actor_id: self.config.actor_id,
        bytes,
        hash,
        application: None,
      },
      self.config.max_message_bytes,
    )
    .await
  }

  pub async fn publish_application_update(&mut self, application: UpdateApplication) -> AnyResult<()> {
    ensure!(
      self.authorization.role.can_write(),
      "local role is not allowed to publish application updates"
    );
    let bytes = Vec::new();
    write_wire_message(
      &mut self.send,
      &WireMessage::Update {
        document_id: self.config.document_id,
        actor_id: self.config.actor_id,
        hash: blake3_hash(&bytes),
        bytes,
        application: Some(application),
      },
      self.config.max_message_bytes,
    )
    .await
  }
  pub async fn replace_source_from(&mut self, source: &CollabDocument, application: Option<UpdateApplication>) -> AnyResult<()> {
    ensure!(self.authorization.role.can_write(), "local role is not allowed to publish updates");
    ensure!(source.document_id() == self.config.document_id, "replacement source document mismatch");
    ensure!(source.format_kind() == self.config.format_kind, "replacement source format mismatch");
    let projection_cache = source.materialize_projection_cache()?;
    let asset_manifest = source.asset_manifest_bytes()?;
    let update = if let Ok(Some(granular_source)) = source.materialize_granular_source() {
      self
        .document
        .replace_granular_source(self.authorization.role, &granular_source, &projection_cache, &asset_manifest)?
    } else {
      self
        .document
        .replace_projection_source(self.authorization.role, &projection_cache, &asset_manifest)?
    };
    ensure_update_size(&update, self.config.max_update_bytes)?;
    write_wire_message(
      &mut self.send,
      &WireMessage::Update {
        document_id: self.config.document_id,
        actor_id: self.config.actor_id,
        hash: blake3_hash(&update),
        bytes: update,
        application,
      },
      self.config.max_message_bytes,
    )
    .await
  }

  pub async fn publish_presence(
    &mut self,
    user_label: impl Into<String>,
    cursor: Option<String>,
    focus: Option<String>,
    viewport_hint: Option<String>,
  ) -> AnyResult<()> {
    let presence = PeerPresence {
      actor_id: self.config.actor_id,
      session_id: self.config.session_id,
      role: self.authorization.role,
      user_label: user_label.into(),
      cursor,
      focus,
      viewport_hint,
      last_known_frontier: self.document.frontier()?,
      monotonic_millis: now_unix_millis(),
    };
    write_wire_message(
      &mut self.send,
      &WireMessage::Presence(presence.message(self.config.document_id)),
      self.config.max_message_bytes,
    )
    .await
  }

  pub async fn receive_next_update(&mut self) -> AnyResult<Option<SessionEvent>> {
    if let Some((event, application)) = self.pending_events.pop_front() {
      self.last_application = application;
      return Ok(Some(event));
    }
    loop {
      let message = match read_wire_message(&mut self.recv, self.config.max_message_bytes).await {
        Ok(message) => message,
        Err(_) => return Ok(None),
      };
      match self.handle_live_message(message).await? {
        LiveClientMessage::Event(event, application) => {
          self.last_application = application;
          return Ok(Some(event));
        },
        LiveClientMessage::AssetChunk(_) | LiveClientMessage::Continue => {},
      }
    }
  }

  async fn handle_live_message(&mut self, message: WireMessage) -> AnyResult<LiveClientMessage> {
    match message {
      WireMessage::Update {
        document_id,
        application,
        actor_id: _,
        bytes,
        hash,
      } => {
        ensure!(document_id == self.config.document_id, SyncError::ProtocolMismatch);
        ensure_update_size(&bytes, self.config.max_update_bytes)?;
        ensure!(blake3_hash(&bytes) == hash, "update hash mismatch");
        if bytes.is_empty() && application.is_some() {
          let frontier = self.document.frontier()?;
          write_wire_message(&mut self.send, &WireMessage::Ack { document_id, frontier }, self.config.max_message_bytes).await?;
          return Ok(LiveClientMessage::Event(SessionEvent::UpdateApplied { document_id, hash }, application));
        }
        let outcome = self.document.import_update_checked(Role::Editor, &bytes)?;
        write_wire_message(
          &mut self.send,
          &WireMessage::Ack {
            document_id,
            frontier: outcome.frontier,
          },
          self.config.max_message_bytes,
        )
        .await?;
        if outcome.patch.is_none() {
          return Ok(LiveClientMessage::Event(SessionEvent::UpdateApplied { document_id, hash }, None));
        }
        Ok(LiveClientMessage::Event(SessionEvent::UpdateApplied { document_id, hash }, application))
      },
      WireMessage::Snapshot { document_id, bytes, hash } => {
        ensure!(document_id == self.config.document_id, SyncError::ProtocolMismatch);
        ensure_snapshot_size(&bytes, self.config.max_snapshot_bytes)?;
        ensure!(blake3_hash(&bytes) == hash, "snapshot hash mismatch");
        self.document = CollabDocument::from_snapshot(&bytes, Some(self.config.format_kind), Some(self.config.document_id))?;
        self.document.set_local_actor(self.config.actor_id)?;
        Ok(LiveClientMessage::Event(SessionEvent::SnapshotApplied { document_id, hash }, None))
      },
      WireMessage::Have { assets, .. } | WireMessage::AssetHave(AssetHaveMessage { assets, .. }) => {
        self.assets_available = assets;
        Ok(LiveClientMessage::Continue)
      },
      WireMessage::AssetChunk(chunk) => {
        ensure!(chunk.document_id == self.config.document_id, SyncError::ProtocolMismatch);
        Ok(LiveClientMessage::AssetChunk(chunk))
      },
      WireMessage::PeerEvent(message) => {
        ensure!(message.document_id == self.config.document_id, SyncError::ProtocolMismatch);
        let event = match message.kind {
          PeerEventKind::Authorized => SessionEvent::PeerAuthorized {
            actor_id: message.actor_id,
            session_id: message.session_id,
            role: message
              .role
              .context("peer authorized event is missing role")?,
          },
          PeerEventKind::RoleChanged => SessionEvent::PeerRoleChanged {
            actor_id: message.actor_id,
            session_id: message.session_id,
            role: message
              .role
              .context("peer role-changed event is missing role")?,
          },
          PeerEventKind::Left => SessionEvent::PeerLeft {
            actor_id: message.actor_id,
            session_id: message.session_id,
          },
        };
        if let SessionEvent::PeerRoleChanged { session_id, role, .. } = event
          && session_id == self.config.session_id
        {
          self.authorization.role = role;
        }
        Ok(LiveClientMessage::Event(event, None))
      },
      WireMessage::Presence(message) => {
        ensure!(message.document_id == self.config.document_id, SyncError::ProtocolMismatch);
        Ok(LiveClientMessage::Event(
          SessionEvent::Presence(PeerPresence {
            actor_id: message.actor_id,
            session_id: message.session_id,
            role: message.role,
            user_label: message.user_label,
            cursor: message.cursor,
            focus: message.focus,
            viewport_hint: message.viewport_hint,
            last_known_frontier: message.last_known_frontier,
            monotonic_millis: message.monotonic_millis,
          }),
          None,
        ))
      },
      WireMessage::Ack { .. } => Ok(LiveClientMessage::Continue),
      WireMessage::Error { message, .. } => bail!(message),
      WireMessage::Hello(_) | WireMessage::Authorize(_) | WireMessage::Need { .. } | WireMessage::AssetNeed(_) => {
        Ok(LiveClientMessage::Continue)
      },
    }
  }

  pub async fn request_asset(&mut self, hash: [u8; 32], expected_len: u64) -> AnyResult<VerifiedAsset> {
    if let Some(asset) = self.assets.get_verified(&hash) {
      return Ok(asset);
    }
    ensure!(self.assets_available.contains(&hash), "asset is not advertised by the collaboration host");
    let expected_len_usize = usize::try_from(expected_len).context("asset length overflows usize")?;
    let mut bytes = Vec::with_capacity(expected_len_usize);
    while bytes.len() < expected_len_usize {
      write_wire_message(
        &mut self.send,
        &asset_need(
          self.config.document_id,
          hash,
          bytes.len() as u64,
          (expected_len_usize - bytes.len()) as u64,
        ),
        self.config.max_message_bytes,
      )
      .await?;
      loop {
        let message = read_wire_message(&mut self.recv, self.config.max_message_bytes).await?;
        match self.handle_live_message(message).await? {
          LiveClientMessage::AssetChunk(chunk) => {
            ensure!(chunk.blake3_hash == hash, "asset chunk hash mismatch");
            let chunk_offset = usize::try_from(chunk.offset).context("asset chunk offset overflows usize")?;
            ensure!(chunk_offset == bytes.len(), "asset chunk offset mismatch");
            ensure!(!chunk.bytes.is_empty(), "asset chunk is empty");
            bytes.extend_from_slice(&chunk.bytes);
            break;
          },
          LiveClientMessage::Event(event, application) => self.pending_events.push_back((event, application)),
          LiveClientMessage::Continue => {},
        }
      }
    }
    ensure!(bytes.len() == expected_len_usize, "asset length mismatch");
    ensure!(blake3_hash(&bytes) == hash, "asset hash mismatch");
    Ok(self.assets.insert_verified(bytes))
  }

  pub async fn request_assets(&mut self, assets: impl IntoIterator<Item = ([u8; 32], u64)>) -> AnyResult<Vec<VerifiedAsset>> {
    let mut verified = Vec::new();
    for (hash, byte_len) in assets {
      verified.push(self.request_asset(hash, byte_len).await?);
    }
    Ok(verified)
  }

  pub async fn request_document_assets(&mut self) -> AnyResult<Vec<VerifiedAsset>> {
    let manifest_bytes = self.document.asset_manifest_bytes()?;
    if manifest_bytes.is_empty() {
      return Ok(Vec::new());
    }
    let manifest: Vec<NativeAssetRecord> = postcard::from_bytes(&manifest_bytes).context("failed to decode Flowstate asset manifest")?;
    self
      .request_assets(
        manifest
          .into_iter()
          .map(|asset| (asset.blake3_hash, asset.byte_len)),
      )
      .await
  }

  pub async fn shutdown(mut self) -> AnyResult<()> {
    self
      .send
      .finish()
      .context("failed to finish Flowstate live stream")?;
    self.endpoint.close().await;
    Ok(())
  }
}

pub async fn connect_live_invite(link: &str) -> AnyResult<LiveCollaborationClient> {
  let ticket = decode_invite_link(link)?;
  let endpoint = bind_endpoint().await?;
  let config = FlowstateSyncConfig::new(ticket.document_id, ticket.format_kind, ticket.invited_role);
  let connection = endpoint
    .connect(ticket.endpoint_addr.clone(), FLOWSTATE_ALPN)
    .await
    .context("failed to connect to Flowstate peer")?;
  let (mut send, mut recv) = connection
    .open_bi()
    .await
    .context("failed to open Flowstate live stream")?;
  let hello = config.hello(Vec::new(), ticket.capability.clone());
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
        ensure_snapshot_size(&bytes, config.max_snapshot_bytes)?;
        ensure!(blake3_hash(&bytes) == hash, "snapshot hash mismatch");
        document = Some(CollabDocument::from_snapshot(&bytes, Some(config.format_kind), Some(config.document_id))?);
      },
      WireMessage::Have { document_id, assets, .. } | WireMessage::AssetHave(AssetHaveMessage { document_id, assets }) => {
        ensure!(document_id == config.document_id, SyncError::ProtocolMismatch);
        assets_available = assets;
        saw_have = true;
      },
      WireMessage::PeerEvent(message) => {
        ensure!(message.document_id == config.document_id, SyncError::ProtocolMismatch);
      },
      WireMessage::Error { message, .. } => bail!(message),
      _ => bail!(SyncError::UnexpectedMessage("Snapshot")),
    }
  }
  let document = document.expect("document checked above");
  document.set_local_actor(config.actor_id)?;
  Ok(LiveCollaborationClient {
    endpoint,
    send,
    last_application: None,
    recv,
    authorization,
    document,
    assets_available,
    assets: AssetStore::default(),
    pending_events: VecDeque::new(),
    config,
  })
}

pub async fn write_wire_message(send: &mut SendStream, message: &WireMessage, max_message_bytes: usize) -> AnyResult<()> {
  write_frame(send, &encode_wire_message(message)?, max_message_bytes).await
}

pub async fn read_wire_message(recv: &mut RecvStream, max_message_bytes: usize) -> AnyResult<WireMessage> {
  decode_wire_message(&read_frame(recv, max_message_bytes).await?).context("failed to decode Flowstate wire message")
}

async fn send_snapshot_and_have(send: &mut SendStream, state: &SessionDocumentState, config: &FlowstateSyncConfig) -> AnyResult<()> {
  let (snapshot, frontier) = {
    let document = state
      .document
      .lock()
      .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?;
    (document.export_snapshot()?, document.frontier()?)
  };
  ensure_snapshot_size(&snapshot, config.max_snapshot_bytes)?;
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
  let assets = advertised_asset_hashes(state)?;
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

fn document_asset_manifest_records(state: &SessionDocumentState) -> AnyResult<Vec<NativeAssetRecord>> {
  let manifest_bytes = state
    .document
    .lock()
    .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?
    .asset_manifest_bytes()?;
  if manifest_bytes.is_empty() {
    return Ok(Vec::new());
  }
  postcard::from_bytes(&manifest_bytes).context("failed to decode Flowstate asset manifest")
}

fn advertised_asset_hashes(state: &SessionDocumentState) -> AnyResult<Vec<[u8; 32]>> {
  let manifest = match document_asset_manifest_records(state) {
    Ok(manifest) => manifest,
    Err(_) => return Ok(Vec::new()),
  };
  if manifest.is_empty() {
    return Ok(Vec::new());
  }
  let available = state
    .assets
    .lock()
    .map_err(|_| anyhow::anyhow!("Flowstate asset state lock is poisoned"))?
    .hashes()
    .into_iter()
    .collect::<HashSet<_>>();
  let mut advertised = Vec::new();
  let mut seen = HashSet::new();
  for asset in manifest {
    if available.contains(&asset.blake3_hash) && seen.insert(asset.blake3_hash) {
      advertised.push(asset.blake3_hash);
    }
  }
  Ok(advertised)
}

fn ensure_asset_referenced(state: &SessionDocumentState, hash: [u8; 32]) -> AnyResult<()> {
  ensure!(
    document_asset_manifest_records(state)?
      .into_iter()
      .any(|asset| asset.blake3_hash == hash),
    "requested asset is not referenced by document manifest"
  );
  Ok(())
}

async fn send_peer_roster(
  send: &mut SendStream,
  hub: &LiveUpdateHub,
  current_session_id: SessionId,
  document_id: DocumentId,
  max_message_bytes: usize,
) -> AnyResult<()> {
  for peer in hub.peers()? {
    if peer.session_id != current_session_id {
      write_wire_message(send, &peer_authorized_message(document_id, peer), max_message_bytes).await?;
    }
  }
  Ok(())
}

async fn serve_live_stream(
  send: &mut SendStream,
  recv: &mut RecvStream,
  hello: &HelloMessage,
  remote_role: Role,
  state: &SessionDocumentState,
  config: &FlowstateSyncConfig,
  mut live_rx: Option<broadcast::Receiver<LiveUpdate>>,
  live_updates: Option<&LiveUpdateHub>,
) -> AnyResult<()> {
  let mut remote_role = remote_role;
  let mut presence_rate = RateWindow::new(config.max_presence_messages_per_minute, RATE_LIMIT_WINDOW_MILLIS);
  let mut asset_request_rate = RateWindow::new(config.max_asset_requests_per_minute, RATE_LIMIT_WINDOW_MILLIS);
  loop {
    let message = if let Some(rx) = live_rx.as_mut() {
      tokio::select! {
        message = read_wire_message(recv, config.max_message_bytes) => match message {
          Ok(message) => message,
          Err(_) => break,
        },
        update = rx.recv() => match update {
          Ok(update) => {
            if let LiveUpdateKind::Event(event) = &update.kind {
              match event {
                SessionEvent::PeerLeft { session_id, .. } if *session_id == hello.session_id => {
                  write_wire_message(
                    send,
                    &WireMessage::Error {
                      document_id: Some(config.document_id),
                      message: "peer was removed from the collaboration session".to_string(),
                    },
                    config.max_message_bytes,
                  )
                  .await?;
                  break;
                },
                SessionEvent::PeerRoleChanged { session_id, role, .. } if *session_id == hello.session_id => {
                  remote_role = *role;
                },
                _ => {},
              }
            }
            if let Some(message) = live_update_wire_message(config.document_id, hello.session_id, update) {
              write_wire_message(send, &message, config.max_message_bytes).await?;
            }
            continue;
          },
          Err(broadcast::error::RecvError::Lagged(_)) => {
            send_snapshot_and_have(send, state, config).await?;
            continue;
          },
          Err(broadcast::error::RecvError::Closed) => {
            live_rx = None;
            continue;
          },
        },
      }
    } else {
      match read_wire_message(recv, config.max_message_bytes).await {
        Ok(message) => message,
        Err(_) => break,
      }
    };
    match message {
      WireMessage::Update {
        document_id,
        actor_id,
        bytes,
        hash,
        application,
      } => {
        ensure!(document_id == config.document_id, SyncError::ProtocolMismatch);
        ensure!(actor_id == hello.actor_id, "update actor does not match authorized peer");
        ensure_update_size(&bytes, config.max_update_bytes)?;
        ensure!(blake3_hash(&bytes) == hash, "update hash mismatch");
        if bytes.is_empty() && application.is_some() {
          let frontier = state
            .document
            .lock()
            .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?
            .frontier()?;
          if let Some(hub) = live_updates {
            let message = WireMessage::Update {
              document_id,
              actor_id,
              hash,
              bytes,
              application,
            };
            hub.publish(LiveUpdate::wire(Some(hello.session_id), message))?;
          }
          write_wire_message(
            send,
            &WireMessage::Ack {
              document_id: config.document_id,
              frontier,
            },
            config.max_message_bytes,
          )
          .await?;
          continue;
        }
        let outcome = {
          state
            .document
            .lock()
            .map_err(|_| anyhow::anyhow!("Flowstate document state lock is poisoned"))?
            .import_update_checked(remote_role, &bytes)?
        };
        if outcome.patch.is_some()
          && let Some(hub) = live_updates
        {
          let message = WireMessage::Update {
            document_id,
            actor_id,
            hash,
            bytes,
            application,
          };
          hub.publish(LiveUpdate::wire(Some(hello.session_id), message))?;
        }
        write_wire_message(
          send,
          &WireMessage::Ack {
            document_id: config.document_id,
            frontier: outcome.frontier,
          },
          config.max_message_bytes,
        )
        .await?;
      },
      WireMessage::Need {
        document_id,
        frontier,
        snapshot,
      } => {
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
            if update.len() > config.max_update_bytes {
              send_snapshot_and_have(send, state, config).await?;
            } else {
              write_wire_message(
                send,
                &WireMessage::Update {
                  document_id,
                  actor_id: config.actor_id,
                  hash: blake3_hash(&update),
                  bytes: update,
                  application: None,
                },
                config.max_message_bytes,
              )
              .await?;
            }
          }
        }
      },
      WireMessage::AssetNeed(request) => {
        ensure!(asset_request_rate.check(now_unix_millis()), "asset request rate limit exceeded");
        ensure!(role_includes(remote_role, Role::Viewer), "peer is not authorized to receive assets");
        ensure!(request.document_id == config.document_id, SyncError::ProtocolMismatch);
        ensure_asset_referenced(state, request.blake3_hash)?;
        let chunk = state
          .assets
          .lock()
          .map_err(|_| anyhow::anyhow!("Flowstate asset state lock is poisoned"))?
          .chunk(&request, config.max_asset_chunk_bytes)?;
        write_wire_message(send, &WireMessage::AssetChunk(chunk), config.max_message_bytes).await?;
      },
      WireMessage::Presence(mut presence) => {
        ensure!(presence_rate.check(now_unix_millis()), "presence rate limit exceeded");
        ensure!(presence.document_id == config.document_id, SyncError::ProtocolMismatch);
        ensure!(presence.actor_id == hello.actor_id, "presence actor does not match authorized peer");
        ensure!(presence.session_id == hello.session_id, "presence session does not match authorized peer");
        presence.role = remote_role;
        let peer_presence = PeerPresence {
          actor_id: presence.actor_id,
          session_id: presence.session_id,
          role: presence.role,
          user_label: presence.user_label.clone(),
          cursor: presence.cursor.clone(),
          focus: presence.focus.clone(),
          viewport_hint: presence.viewport_hint.clone(),
          last_known_frontier: presence.last_known_frontier.clone(),
          monotonic_millis: presence.monotonic_millis,
        };
        if let Some(hub) = live_updates {
          hub.publish(LiveUpdate::event(Some(hello.session_id), SessionEvent::Presence(peer_presence)))?;
          hub.publish(LiveUpdate::wire(Some(hello.session_id), WireMessage::Presence(presence)))?;
        }
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
      | WireMessage::PeerEvent(_)
      | WireMessage::Ack { .. }
      | WireMessage::Error { .. } => {},
    }
  }
  Ok(())
}

fn live_update_wire_message(document_id: DocumentId, current_session_id: SessionId, update: LiveUpdate) -> Option<WireMessage> {
  if update.source_session_id == Some(current_session_id) {
    return None;
  }
  match update.kind {
    LiveUpdateKind::Wire(message) => Some(message),
    LiveUpdateKind::Event(event) => session_event_wire_message(document_id, event),
  }
}

fn session_event_wire_message(document_id: DocumentId, event: SessionEvent) -> Option<WireMessage> {
  match event {
    SessionEvent::PeerAuthorized { actor_id, session_id, role } => Some(peer_event_message(
      document_id,
      actor_id,
      session_id,
      Some(role),
      PeerEventKind::Authorized,
    )),
    SessionEvent::PeerRoleChanged { actor_id, session_id, role } => Some(peer_event_message(
      document_id,
      actor_id,
      session_id,
      Some(role),
      PeerEventKind::RoleChanged,
    )),
    SessionEvent::PeerLeft { actor_id, session_id } => Some(peer_event_message(document_id, actor_id, session_id, None, PeerEventKind::Left)),
    _ => None,
  }
}

fn peer_authorized_message(document_id: DocumentId, peer: LivePeer) -> WireMessage {
  peer_event_message(document_id, peer.actor_id, peer.session_id, Some(peer.role), PeerEventKind::Authorized)
}

fn peer_event_message(
  document_id: DocumentId,
  actor_id: ActorId,
  session_id: SessionId,
  role: Option<Role>,
  kind: PeerEventKind,
) -> WireMessage {
  WireMessage::PeerEvent(PeerEventMessage {
    document_id,
    actor_id,
    session_id,
    role,
    kind,
  })
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
  let end = start
    .checked_add(len)
    .context("asset range overflows usize")?;
  ensure!(start <= available && end <= available, "asset range is out of bounds");
  Ok(start..end)
}

fn role_includes(granted: Role, requested: Role) -> bool {
  matches!(
    (granted, requested),
    (Role::Owner, Role::Owner | Role::Editor | Role::Viewer) | (Role::Editor, Role::Editor | Role::Viewer) | (Role::Viewer, Role::Viewer)
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

fn now_unix_millis() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map(|duration| u64::try_from(duration.as_millis()).unwrap_or(u64::MAX))
    .unwrap_or_default()
}

fn accept_error(error: anyhow::Error) -> AcceptError {
  AcceptError::from_err(std::io::Error::other(error.to_string()))
}

fn ensure_snapshot_size(bytes: &[u8], max_snapshot_bytes: usize) -> AnyResult<()> {
  ensure!(
    bytes.len() <= max_snapshot_bytes,
    "Flowstate snapshot length {} exceeds limit {}",
    bytes.len(),
    max_snapshot_bytes
  );
  Ok(())
}

fn ensure_update_size(bytes: &[u8], max_update_bytes: usize) -> AnyResult<()> {
  ensure!(
    bytes.len() <= max_update_bytes,
    "Flowstate update length {} exceeds limit {}",
    bytes.len(),
    max_update_bytes
  );
  Ok(())
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
  fn role_policy_changes_and_kicks_peers() {
    let owner = ActorId::new();
    let peer = ActorId::new();
    let mut policy = RolePolicy::owner_only(owner);
    policy.set_actor_role(peer, Role::Editor);
    assert_eq!(policy.role_for_actor(peer), Some(Role::Editor));
    policy.set_actor_role(peer, Role::Viewer);
    assert_eq!(policy.role_for_actor(peer), Some(Role::Viewer));
    assert!(policy.remove_actor(peer));
    assert_eq!(policy.role_for_actor(peer), None);
    assert!(!policy.remove_actor(owner));
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
    let chunk = store
      .chunk(&request, DEFAULT_MAX_ASSET_CHUNK_BYTES)
      .unwrap();
    let mut receiving = AssetStore::default();
    receiving.insert_complete_chunk(chunk, 6).unwrap();
    assert!(receiving.contains(&asset.hash));
  }

  #[test]
  fn asset_store_rejects_unknown_and_out_of_bounds_ranges() {
    let mut store = AssetStore::default();
    let asset = store.insert_verified(b"abcdef".to_vec());
    let unknown = AssetNeedMessage {
      document_id: DocumentId::new(),
      blake3_hash: [7; 32],
      offset: 0,
      len: 1,
    };
    assert!(
      store
        .chunk(&unknown, DEFAULT_MAX_ASSET_CHUNK_BYTES)
        .is_err()
    );

    let out_of_bounds = AssetNeedMessage {
      document_id: DocumentId::new(),
      blake3_hash: asset.hash,
      offset: 5,
      len: 2,
    };
    assert!(
      store
        .chunk(&out_of_bounds, DEFAULT_MAX_ASSET_CHUNK_BYTES)
        .is_err()
    );
  }

  #[test]
  fn live_update_hub_allows_publish_without_subscribers() {
    let hub = LiveUpdateHub::new(1);
    assert!(
      hub
        .publish(LiveUpdate::wire(
          None,
          WireMessage::Ack {
            document_id: DocumentId::new(),
            frontier: Vec::new(),
          },
        ))
        .is_ok()
    );
  }

  #[test]
  fn rate_window_rejects_events_until_window_expires() {
    let mut window = RateWindow::new(2, 1_000);

    assert!(window.check(1_000));
    assert!(window.check(1_500));
    assert!(!window.check(1_999));
    assert!(window.check(2_001));
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

  #[test]
  fn session_tracks_peers_updates_and_presence_events() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let document = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let config = FlowstateSyncConfig::new(document_id, FormatKind::Db8, Role::Owner);
    let mut session = CollabSession::new(config.clone(), Role::Owner, SessionDocumentState::new(document, AssetStore::default()));
    session.set_state(SessionState::Live);
    let peer_config = FlowstateSyncConfig::new(document_id, FormatKind::Db8, Role::Editor);
    let hello = peer_config.hello(Vec::new(), Vec::new());
    session.upsert_peer(&hello, Role::Editor);
    session.queue_presence(PeerPresence {
      actor_id: hello.actor_id,
      session_id: hello.session_id,
      role: Role::Editor,
      user_label: "peer".to_string(),
      cursor: Some("p:0".to_string()),
      focus: None,
      viewport_hint: None,
      last_known_frontier: Vec::new(),
      monotonic_millis: 7,
    });

    assert_eq!(session.peer_count(), 1);
    assert!(session.pop_presence().is_some());
    let events = session.drain_events();
    assert!(
      events
        .iter()
        .any(|event| matches!(event, SessionEvent::StateChanged(SessionState::Live)))
    );
    assert!(
      events
        .iter()
        .any(|event| matches!(event, SessionEvent::PeerAuthorized { role: Role::Editor, .. }))
    );
    assert!(
      events
        .iter()
        .any(|event| matches!(event, SessionEvent::Presence(_)))
    );
  }

  #[tokio::test]
  async fn hosted_collaboration_issues_invite_and_join_receives_snapshot() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"projection", b"asset-manifest").unwrap();
    let host = HostedCollaboration::start(host_doc, AssetStore::default(), Role::Owner)
      .await
      .unwrap();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let joined = join_invite_snapshot(&invite).await.unwrap();

    assert_eq!(joined.authorization.role, Role::Editor);
    assert_eq!(joined.document.materialize_projection_cache().unwrap(), b"projection");
    assert_eq!(joined.document.asset_manifest_bytes().unwrap(), b"asset-manifest");
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn hosted_collaboration_revoke_invite_blocks_new_live_join() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"projection", &[]).unwrap();
    let host = HostedCollaboration::start(host_doc, AssetStore::default(), Role::Owner)
      .await
      .unwrap();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let first = connect_live_invite(&invite).await.unwrap();

    assert!(host.revoke_invite_link(&invite).unwrap());
    assert!(connect_live_invite(&invite).await.is_err());

    first.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn hosted_collaboration_revoke_all_invites_blocks_new_live_join() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"projection", &[]).unwrap();
    let host = HostedCollaboration::start(host_doc, AssetStore::default(), Role::Owner)
      .await
      .unwrap();
    let invite = host
      .issue_invite_link(Role::Viewer, Some("viewer".to_string()), true)
      .unwrap();

    assert_eq!(host.revoke_all_invites().unwrap(), 1);
    assert!(connect_live_invite(&invite).await.is_err());
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_client_downloads_advertised_asset_in_ranges() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let asset_bytes = (0..DEFAULT_MAX_ASSET_CHUNK_BYTES + 17)
      .map(|index| (index % 251) as u8)
      .collect::<Vec<_>>();
    let mut assets = AssetStore::default();
    let asset = assets.insert_verified(asset_bytes.clone());
    let manifest = postcard::to_stdvec(&vec![NativeAssetRecord {
      asset_id: 7,
      blake3_hash: asset.hash,
      byte_len: asset.bytes.len() as u64,
      mime_type: "image/png".to_string(),
      original_name: Some("asset.png".to_string()),
      created_by_actor: owner,
      inline: true,
    }])
    .unwrap();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"projection", &manifest).unwrap();
    let host = HostedCollaboration::start(host_doc, assets, Role::Owner)
      .await
      .unwrap();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let mut client = connect_live_invite(&invite).await.unwrap();

    assert!(client.assets_available.contains(&asset.hash));
    let received = client.request_document_assets().await.unwrap();

    assert_eq!(received.len(), 1);
    assert_eq!(received[0].hash, asset.hash);
    assert_eq!(received[0].bytes, asset_bytes);
    assert!(client.assets.contains(&asset.hash));
    client.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_host_advertises_and_serves_only_manifest_assets() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let mut assets = AssetStore::default();
    let referenced = assets.insert_verified(b"referenced asset".to_vec());
    let unreferenced = assets.insert_verified(b"unreferenced asset".to_vec());
    let manifest = postcard::to_stdvec(&vec![NativeAssetRecord {
      asset_id: 11,
      blake3_hash: referenced.hash,
      byte_len: referenced.bytes.len() as u64,
      mime_type: "image/png".to_string(),
      original_name: Some("referenced.png".to_string()),
      created_by_actor: owner,
      inline: true,
    }])
    .unwrap();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"projection", &manifest).unwrap();
    let host = HostedCollaboration::start(host_doc, assets, Role::Owner)
      .await
      .unwrap();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let mut client = connect_live_invite(&invite).await.unwrap();

    assert!(client.assets_available.contains(&referenced.hash));
    assert!(!client.assets_available.contains(&unreferenced.hash));
    assert!(
      client
        .request_asset(unreferenced.hash, unreferenced.bytes.len() as u64)
        .await
        .is_err()
    );

    write_wire_message(
      &mut client.send,
      &asset_need(document_id, unreferenced.hash, 0, unreferenced.bytes.len() as u64),
      client.config.max_message_bytes,
    )
    .await
    .unwrap();
    assert!(
      read_wire_message(&mut client.recv, client.config.max_message_bytes)
        .await
        .is_err()
    );

    let _ = client.shutdown().await;
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_host_rejects_asset_request_rate_limit() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let mut assets = AssetStore::default();
    let asset = assets.insert_verified(b"rate limited asset".to_vec());
    let manifest = postcard::to_stdvec(&vec![NativeAssetRecord {
      asset_id: 12,
      blake3_hash: asset.hash,
      byte_len: asset.bytes.len() as u64,
      mime_type: "image/png".to_string(),
      original_name: Some("rate.png".to_string()),
      created_by_actor: owner,
      inline: true,
    }])
    .unwrap();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"projection", &manifest).unwrap();
    let mut config = FlowstateSyncConfig::new(document_id, FormatKind::Db8, Role::Owner);
    config.max_asset_requests_per_minute = 1;
    let host = HostedCollaboration::start_with_config(host_doc, assets, config)
      .await
      .unwrap();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let mut client = connect_live_invite(&invite).await.unwrap();

    write_wire_message(
      &mut client.send,
      &asset_need(document_id, asset.hash, 0, asset.bytes.len() as u64),
      client.config.max_message_bytes,
    )
    .await
    .unwrap();
    assert!(matches!(
      read_wire_message(&mut client.recv, client.config.max_message_bytes)
        .await
        .unwrap(),
      WireMessage::AssetChunk(_)
    ));
    write_wire_message(
      &mut client.send,
      &asset_need(document_id, asset.hash, 0, asset.bytes.len() as u64),
      client.config.max_message_bytes,
    )
    .await
    .unwrap();
    assert!(
      read_wire_message(&mut client.recv, client.config.max_message_bytes)
        .await
        .is_err()
    );

    let _ = client.shutdown().await;
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_host_fans_out_peer_update_to_other_joiner() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let host = HostedCollaboration::start(host_doc, AssetStore::default(), Role::Owner)
      .await
      .unwrap();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let mut left = connect_live_invite(&invite).await.unwrap();
    let mut right = connect_live_invite(&invite).await.unwrap();
    let update = left
      .document
      .replace_projection_source(Role::Editor, b"two", &[])
      .unwrap();

    left.publish_update(update).await.unwrap();
    let event = loop {
      let event = right.receive_next_update().await.unwrap();
      if matches!(event, Some(SessionEvent::UpdateApplied { .. })) {
        break event;
      }
    };

    assert!(matches!(event, Some(SessionEvent::UpdateApplied { document_id: id, .. }) if id == document_id));
    assert_eq!(right.document.materialize_projection_cache().unwrap(), b"two");
    left.shutdown().await.unwrap();
    right.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_host_subscriber_receives_peer_update_for_owner_workspace() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let host = HostedCollaboration::start(host_doc, AssetStore::default(), Role::Owner)
      .await
      .unwrap();
    let mut host_updates = host.subscribe_live_updates();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let mut client = connect_live_invite(&invite).await.unwrap();
    let update = client
      .document
      .replace_projection_source(Role::Editor, b"two", &[])
      .unwrap();

    client.publish_update(update).await.unwrap();
    let update = loop {
      let update = host_updates.recv().await.unwrap();
      if matches!(update.kind, LiveUpdateKind::Wire(WireMessage::Update { .. })) {
        break update;
      }
    };

    assert!(update.source_session_id.is_some());
    assert!(matches!(update.kind, LiveUpdateKind::Wire(WireMessage::Update { document_id: id, .. }) if id == document_id));
    assert_eq!(
      host
        .document_state()
        .document
        .lock()
        .unwrap()
        .materialize_projection_cache()
        .unwrap(),
      b"two"
    );
    client.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_host_subscriber_receives_peer_lifecycle_events() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let host = HostedCollaboration::start(host_doc, AssetStore::default(), Role::Owner)
      .await
      .unwrap();
    let mut host_updates = host.subscribe_live_updates();
    let invite = host
      .issue_invite_link(Role::Viewer, Some("viewer".to_string()), true)
      .unwrap();
    let client = connect_live_invite(&invite).await.unwrap();
    let event = loop {
      let update = host_updates.recv().await.unwrap();
      if let LiveUpdateKind::Event(event @ SessionEvent::PeerAuthorized { .. }) = update.kind {
        break event;
      }
    };

    assert!(matches!(event, SessionEvent::PeerAuthorized { role: Role::Viewer, .. }));
    client.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_host_subscriber_receives_peer_left_after_rejected_update() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let host = HostedCollaboration::start(host_doc, AssetStore::default(), Role::Owner)
      .await
      .unwrap();
    let mut host_updates = host.subscribe_live_updates();
    let invite = host
      .issue_invite_link(Role::Viewer, Some("viewer".to_string()), true)
      .unwrap();
    let mut client = connect_live_invite(&invite).await.unwrap();
    let session_id = client.config.session_id;
    let actor_id = client.config.actor_id;
    let update = client
      .document
      .replace_projection_source(Role::Editor, b"illegal", &[])
      .unwrap();
    let hash = blake3_hash(&update);

    write_wire_message(
      &mut client.send,
      &WireMessage::Update {
        document_id,
        actor_id,
        bytes: update,
        hash,
        application: None,
      },
      client.config.max_message_bytes,
    )
    .await
    .unwrap();
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
      loop {
        let update = host_updates.recv().await.unwrap();
        if let LiveUpdateKind::Event(
          event @ SessionEvent::PeerLeft {
            session_id: left_session_id, ..
          },
        ) = update.kind
          && left_session_id == session_id
        {
          break event;
        }
      }
    })
    .await
    .unwrap();

    assert!(matches!(
      event,
      SessionEvent::PeerLeft {
        session_id: left_session_id,
        ..
      } if left_session_id == session_id
    ));
    let _ = client.shutdown().await;
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_joiner_receives_peer_roster_and_lifecycle_events() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let host = HostedCollaboration::start(host_doc, AssetStore::default(), Role::Owner)
      .await
      .unwrap();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let mut first = connect_live_invite(&invite).await.unwrap();
    let first_session_id = first.config.session_id;
    let mut second = connect_live_invite(&invite).await.unwrap();
    let second_session_id = second.config.session_id;

    let first_seen_by_second = tokio::time::timeout(std::time::Duration::from_secs(5), second.receive_next_update())
      .await
      .unwrap()
      .unwrap()
      .unwrap();
    assert!(matches!(
      first_seen_by_second,
      SessionEvent::PeerAuthorized {
        session_id,
        role: Role::Editor,
        ..
      } if session_id == first_session_id
    ));

    let second_seen_by_first = tokio::time::timeout(std::time::Duration::from_secs(5), first.receive_next_update())
      .await
      .unwrap()
      .unwrap()
      .unwrap();
    assert!(matches!(
      second_seen_by_first,
      SessionEvent::PeerAuthorized {
        session_id,
        role: Role::Editor,
        ..
      } if session_id == second_session_id
    ));

    first.shutdown().await.unwrap();
    let first_left = tokio::time::timeout(std::time::Duration::from_secs(5), second.receive_next_update())
      .await
      .unwrap()
      .unwrap()
      .unwrap();
    assert!(matches!(
      first_left,
      SessionEvent::PeerLeft {
        session_id,
        ..
      } if session_id == first_session_id
    ));

    second.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_host_rejects_join_when_peer_limit_is_reached() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let mut config = FlowstateSyncConfig::new(document_id, FormatKind::Db8, Role::Owner);
    config.max_peer_count = 1;
    let host = HostedCollaboration::start_with_config(host_doc, AssetStore::default(), config)
      .await
      .unwrap();
    let mut host_updates = host.subscribe_live_updates();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let first = connect_live_invite(&invite).await.unwrap();
    let first_session_id = first.config.session_id;
    let second = connect_live_invite(&invite).await;

    assert!(second.is_err(), "second join should be rejected by peer limit");
    first.shutdown().await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
      loop {
        let update = host_updates.recv().await.unwrap();
        if let LiveUpdateKind::Event(SessionEvent::PeerLeft { session_id, .. }) = update.kind
          && session_id == first_session_id
        {
          break;
        }
      }
    })
    .await
    .unwrap();
    let third = connect_live_invite(&invite).await.unwrap();
    third.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_host_rejects_oversized_peer_update_without_mutation() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let mut config = FlowstateSyncConfig::new(document_id, FormatKind::Db8, Role::Owner);
    config.max_update_bytes = 1;
    let host = HostedCollaboration::start_with_config(host_doc, AssetStore::default(), config)
      .await
      .unwrap();
    let mut host_updates = host.subscribe_live_updates();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let mut client = connect_live_invite(&invite).await.unwrap();
    let session_id = client.config.session_id;
    let update = client
      .document
      .replace_projection_source(Role::Editor, b"two", &[])
      .unwrap();
    assert!(update.len() > 1);

    client.publish_update(update).await.unwrap();
    tokio::time::timeout(std::time::Duration::from_secs(5), async {
      loop {
        let update = host_updates.recv().await.unwrap();
        if let LiveUpdateKind::Event(SessionEvent::PeerLeft {
          session_id: left_session_id, ..
        }) = update.kind
          && left_session_id == session_id
        {
          break;
        }
      }
    })
    .await
    .unwrap();

    assert_eq!(
      host
        .document_state()
        .document
        .lock()
        .unwrap()
        .materialize_projection_cache()
        .unwrap(),
      b"one"
    );
    let _ = client.shutdown().await;
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_host_rejects_snapshot_when_snapshot_limit_is_exceeded() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"large projection", &[]).unwrap();
    let mut config = FlowstateSyncConfig::new(document_id, FormatKind::Db8, Role::Owner);
    config.max_snapshot_bytes = 1;
    let host = HostedCollaboration::start_with_config(host_doc, AssetStore::default(), config)
      .await
      .unwrap();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();

    assert!(connect_live_invite(&invite).await.is_err());
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_presence_is_visible_to_host_and_other_joiners() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let host = HostedCollaboration::start(host_doc, AssetStore::default(), Role::Owner)
      .await
      .unwrap();
    let mut host_updates = host.subscribe_live_updates();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let mut left = connect_live_invite(&invite).await.unwrap();
    let left_session_id = left.config.session_id;
    let mut right = connect_live_invite(&invite).await.unwrap();

    left
      .publish_presence(
        "Left debater",
        Some("paragraph:3".to_string()),
        Some("case".to_string()),
        Some("visible:0-5".to_string()),
      )
      .await
      .unwrap();

    let host_presence = tokio::time::timeout(std::time::Duration::from_secs(5), async {
      loop {
        let update = host_updates.recv().await.unwrap();
        if let LiveUpdateKind::Event(SessionEvent::Presence(presence)) = update.kind
          && presence.session_id == left_session_id
        {
          break presence;
        }
      }
    })
    .await
    .unwrap();
    assert_eq!(host_presence.user_label, "Left debater");
    assert_eq!(host_presence.cursor.as_deref(), Some("paragraph:3"));
    assert_eq!(host_presence.focus.as_deref(), Some("case"));

    let joiner_presence = tokio::time::timeout(std::time::Duration::from_secs(5), async {
      loop {
        let event = right.receive_next_update().await.unwrap();
        if let Some(SessionEvent::Presence(presence)) = event
          && presence.session_id == left_session_id
        {
          break presence;
        }
      }
    })
    .await
    .unwrap();
    assert_eq!(joiner_presence.user_label, "Left debater");
    assert_eq!(joiner_presence.viewport_hint.as_deref(), Some("visible:0-5"));

    left.shutdown().await.unwrap();
    right.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_host_role_change_updates_client_permissions() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let host = HostedCollaboration::start(host_doc, AssetStore::default(), Role::Owner)
      .await
      .unwrap();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let mut client = connect_live_invite(&invite).await.unwrap();
    let session_id = client.config.session_id;

    assert!(host.set_peer_role(session_id, Role::Viewer).unwrap());
    let event = tokio::time::timeout(std::time::Duration::from_secs(5), client.receive_next_update())
      .await
      .unwrap()
      .unwrap()
      .unwrap();
    assert!(matches!(
      event,
      SessionEvent::PeerRoleChanged {
        session_id: changed_session_id,
        role: Role::Viewer,
        ..
      } if changed_session_id == session_id
    ));
    assert_eq!(client.authorization.role, Role::Viewer);
    let update = client
      .document
      .replace_projection_source(Role::Editor, b"two", &[])
      .unwrap();
    assert!(client.publish_update(update).await.is_err());

    let _ = client.shutdown().await;
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_host_kick_disconnects_target_and_notifies_other_joiners() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let host = HostedCollaboration::start(host_doc, AssetStore::default(), Role::Owner)
      .await
      .unwrap();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let mut kicked = connect_live_invite(&invite).await.unwrap();
    let kicked_session_id = kicked.config.session_id;
    let mut observer = connect_live_invite(&invite).await.unwrap();

    assert!(host.kick_peer(kicked_session_id).unwrap());
    let kicked_result = tokio::time::timeout(std::time::Duration::from_secs(5), async {
      loop {
        match kicked.receive_next_update().await {
          Ok(Some(_)) => {},
          other => break other,
        }
      }
    })
    .await
    .unwrap();
    assert!(matches!(kicked_result, Ok(None) | Err(_)));
    let observer_event = tokio::time::timeout(std::time::Duration::from_secs(5), async {
      loop {
        let event = observer.receive_next_update().await.unwrap();
        if let Some(SessionEvent::PeerLeft { session_id, .. }) = event
          && session_id == kicked_session_id
        {
          break;
        }
      }
    })
    .await;
    assert!(observer_event.is_ok());

    let _ = kicked.shutdown().await;
    observer.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_joiner_repairs_from_stale_frontier_with_need() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let host = HostedCollaboration::start(host_doc, AssetStore::default(), Role::Owner)
      .await
      .unwrap();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let mut client = connect_live_invite(&invite).await.unwrap();
    let update = client
      .document
      .replace_projection_source(Role::Editor, b"two", &[])
      .unwrap();
    host.apply_local_update(update).unwrap();

    write_wire_message(
      &mut client.send,
      &WireMessage::Need {
        document_id,
        frontier: Vec::new(),
        snapshot: false,
      },
      client.config.max_message_bytes,
    )
    .await
    .unwrap();
    let event = client.receive_next_update().await.unwrap();

    assert!(matches!(event, Some(SessionEvent::UpdateApplied { document_id: id, .. }) if id == document_id));
    assert_eq!(client.document.materialize_projection_cache().unwrap(), b"two");
    client.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
  }

  #[tokio::test]
  async fn live_snapshot_repair_clears_typed_application_metadata() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let host_doc = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let host = HostedCollaboration::start(host_doc, AssetStore::default(), Role::Owner)
      .await
      .unwrap();
    let invite = host
      .issue_invite_link(Role::Editor, Some("editor".to_string()), true)
      .unwrap();
    let mut client = connect_live_invite(&invite).await.unwrap();
    let replacement = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"two", b"asset-manifest").unwrap();
    host
      .publish_update_from_source(&replacement, Some(UpdateApplication::Db8CanonicalOperations(vec![1, 2, 3])))
      .unwrap();

    let event = client.receive_next_update().await.unwrap();
    assert!(matches!(event, Some(SessionEvent::UpdateApplied { document_id: id, .. }) if id == document_id));
    assert!(client.last_application.is_some());
    assert_eq!(client.document.asset_manifest_bytes().unwrap(), b"asset-manifest");

    write_wire_message(
      &mut client.send,
      &WireMessage::Need {
        document_id,
        frontier: Vec::new(),
        snapshot: true,
      },
      client.config.max_message_bytes,
    )
    .await
    .unwrap();
    let event = client.receive_next_update().await.unwrap();

    assert!(matches!(event, Some(SessionEvent::SnapshotApplied { document_id: id, .. }) if id == document_id));
    assert!(client.last_application.is_none());
    assert_eq!(client.document.materialize_projection_cache().unwrap(), b"two");
    client.shutdown().await.unwrap();
    host.shutdown().await.unwrap();
  }

  #[test]
  fn session_rejects_viewer_update_without_mutation() {
    let document_id = DocumentId::new();
    let owner = ActorId::new();
    let left = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let update = left
      .replace_projection_source(Role::Owner, b"two", &[])
      .unwrap();
    let right = CollabDocument::from_projection_source(FormatKind::Db8, document_id, owner, b"one", &[]).unwrap();
    let config = FlowstateSyncConfig::new(document_id, FormatKind::Db8, Role::Owner);
    let mut session = CollabSession::new(config, Role::Owner, SessionDocumentState::new(right.clone(), AssetStore::default()));

    assert!(
      session
        .apply_remote_update(ActorId::new(), Role::Viewer, &update)
        .is_err()
    );
    assert_eq!(right.materialize_projection_cache().unwrap(), b"one");
    assert!(
      session
        .drain_events()
        .iter()
        .any(|event| matches!(event, SessionEvent::UpdateRejected { .. }))
    );
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
