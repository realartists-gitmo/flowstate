use std::{
  collections::{HashMap, HashSet},
  sync::{Arc, OnceLock, RwLock as StdRwLock},
  time::Duration,
};

use anyhow::{Context as _, Result, anyhow, bail, ensure};
use async_channel::Sender;
use iroh::{
  Endpoint, EndpointId,
  endpoint::{Connection, PathId, RecvStream, SendStream},
  protocol::{AcceptError, ProtocolHandler},
};
use tokio::{
  sync::{OwnedSemaphorePermit, RwLock, Semaphore},
  time::timeout,
};

use crate::{
  admission::SessionAdmission,
  ids::{BlobId, SessionId},
  proto_direct::{
    AssetBytes, DIRECT_ALPN, DirectRequest, DirectResponseHeader, DiscoveryAdmissionGrant, MAX_FRAME_LEN, MAX_PAYLOAD_CHUNK_LEN,
    MAX_PAYLOAD_LEN, WireCodec, decode_frame, encode_frame,
  },
  sync_io::SyncIoHandle,
};

use super::{PullProgress, auth::SessionAuthRegistry, blobs::BlobOutbox};

const DIRECT_SERVE_CONCURRENCY: usize = 4;
/// RTT below which the path is treated as "fast" (LAN/localhost) and wire
/// compression is skipped, because zstd decode (~700–900 MB/s) would become the
/// bottleneck rather than the link. See [`link_is_fast`].
const FAST_LINK_RTT: Duration = Duration::from_millis(1);
const DIRECT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
static CLIENT_ENDPOINT: OnceLock<StdRwLock<Option<Endpoint>>> = OnceLock::new();

#[derive(Clone)]
pub struct DirectSessionHandler {
  requests: Sender<DirectServeRequest>,
  /// Handle to the session's document I/O service. When present, snapshot /
  /// update pulls are served through it (see [`serve_snapshot_via_io`]): the
  /// I/O thread answers them under the write gate (Loro-first spec I-9a — a
  /// raw ungated `LoroDoc` read is a commit barrier that can force-commit
  /// mid-intent state, so the old shared read-handle path is outlawed), and
  /// snapshot exports fork under the gate + export off it, so a peer's
  /// recovery pull still cannot be starved behind local edits. `None` falls
  /// back to the session-served request channel.
  io: Option<SyncIoHandle>,
}

impl DirectSessionHandler {
  #[must_use]
  pub fn new(requests: Sender<DirectServeRequest>, io: Option<SyncIoHandle>) -> Self {
    Self { requests, io }
  }
}

impl std::fmt::Debug for DirectSessionHandler {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("DirectSessionHandler")
      .field("has_io", &self.io.is_some())
      .finish_non_exhaustive()
  }
}

#[derive(Debug)]
pub enum DirectServeRequest {
  Snapshot { reply: Sender<Result<Vec<u8>>> },
  Updates { have_vv: Vec<u8>, reply: Sender<Result<Vec<u8>>> },
  Asset { asset: u128, reply: Sender<Result<AssetBytes>> },
}

#[derive(Clone, Debug, Default)]
pub struct DirectServeState {
  inner: Arc<RwLock<DirectServeInner>>,
  auth: SessionAuthRegistry,
  standing_access: Arc<RwLock<HashMap<SessionId, StandingAccessGrant>>>,
  /// Host-side notice channel (None in contexts that only join).
  events: Option<async_channel::Sender<super::NetEvent>>,
}

#[derive(Clone, Debug)]
struct StandingAccessGrant {
  document_fingerprint: [u8; 32],
  title: String,
  document: crate::ticket::DocumentKind,
  identities: HashSet<iroh::PublicKey>,
}

#[derive(Debug, Default)]
struct DirectServeInner {
  attached: HashSet<SessionId>,
  blobs: HashMap<SessionId, BlobOutbox>,
  handlers: HashMap<SessionId, DirectSessionHandler>,
}

impl DirectServeState {
  /// Attach the host-side notice channel (admission refusals etc.).
  #[must_use]
  pub fn with_events(mut self, events: async_channel::Sender<super::NetEvent>) -> Self {
    self.events = Some(events);
    self
  }

  /// Session admission state shared by all direct requests.
  #[must_use]
  pub fn auth(&self) -> &SessionAuthRegistry {
    &self.auth
  }

  pub async fn attach_session(&self, session: SessionId) {
    let mut inner = self.inner.write().await;
    let inserted = inner.attached.insert(session);
    tracing::debug!(%session, inserted, attached_sessions = inner.attached.len(), "attached collaboration direct session");
  }

  pub async fn detach_session(&self, session: SessionId) {
    let mut inner = self.inner.write().await;
    let removed = inner.attached.remove(&session);
    let blob_count = inner
      .blobs
      .remove(&session)
      .map_or(0, |outbox| outbox.len());
    let handler_removed = inner.handlers.remove(&session).is_some();
    self.standing_access.write().await.remove(&session);
    tracing::debug!(%session, removed, blob_count, handler_removed, attached_sessions = inner.attached.len(), "detached collaboration direct session");
  }

  pub async fn register_handler(&self, session: SessionId, handler: DirectSessionHandler) {
    let mut inner = self.inner.write().await;
    inner.attached.insert(session);
    let replaced = inner.handlers.insert(session, handler).is_some();
    tracing::debug!(%session, replaced, handler_count = inner.handlers.len(), "registered collaboration direct session handler");
  }

  pub async fn configure_standing_access(
    &self,
    session: SessionId,
    document_fingerprint: [u8; 32],
    title: String,
    document: crate::ticket::DocumentKind,
    identities: HashSet<iroh::PublicKey>,
  ) {
    self.standing_access.write().await.insert(
      session,
      StandingAccessGrant {
        document_fingerprint,
        title,
        document,
        identities,
      },
    );
  }

  #[allow(
    clippy::significant_drop_tightening,
    reason = "write guard scope is intentionally tight and no await occurs while held"
  )]
  pub async fn insert_blob(&self, session: SessionId, bytes: Vec<u8>) -> Result<BlobId> {
    let byte_len = bytes.len();
    let (blob, outbox_len, outbox_bytes) = {
      let mut inner = self.inner.write().await;
      inner.attached.insert(session);
      let outbox = inner.blobs.entry(session).or_default();
      let blob = BlobId::new();
      ensure!(outbox.insert_with_id(blob, bytes), "collaboration direct blob exceeds outbox capacity");
      let outbox_len = outbox.len();
      let outbox_bytes = outbox.total_bytes();
      (blob, outbox_len, outbox_bytes)
    };
    tracing::debug!(
      %session,
      ?blob,
      byte_len,
      outbox_len,
      outbox_bytes,
      "stored collaboration direct blob for peer pull",
    );
    Ok(blob)
  }

  async fn serve(&self, request: DirectRequest, remote: EndpointId) -> ServeOutcome {
    let session = request.session();
    let request_kind = direct_request_kind(&request);
    let request_detail_bytes = direct_request_detail_bytes(&request);
    tracing::trace!(%session, remote = %remote, request_kind, request_detail_bytes, "serving collaboration direct request");

    if let DirectRequest::RequestAdmission { request } = &request {
      let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
      if request.verify(now).is_err() || request.session != session {
        return ServeOutcome::Header(DirectResponseHeader::Unauthorized);
      }
      let title = self
        .standing_access
        .read()
        .await
        .get(&session)
        .and_then(|grant| {
          (grant.document_fingerprint == request.document_fingerprint && grant.identities.contains(&request.identity))
            .then(|| (grant.title.clone(), grant.document))
        });
      let Some((title, document)) = title else {
        // CO-S1: tell the host someone knocked and wasn't on the list — the
        // refusal itself stays exactly as strict as before.
        if let Some(events) = &self.events {
          let _ = events.try_send(super::NetEvent::AdmissionRefused {
            session,
            identity: request.identity.to_string(),
          });
        }
        return ServeOutcome::Header(DirectResponseHeader::Unauthorized);
      };
      let Some(admission) = self.auth.admission(session) else {
        return ServeOutcome::Header(DirectResponseHeader::NotAttached);
      };
      // S12: the grant carries the SESSION's document kind so a discovered
      // joiner spawns the right runtime before any bytes arrive.
      let payload = match postcard::to_stdvec(&DiscoveryAdmissionGrant { admission, title, document }) {
        Ok(payload) => payload,
        Err(_) => return ServeOutcome::Header(DirectResponseHeader::NotFound),
      };
      return ServeOutcome::Payload(payload);
    }

    // The admission handshake and authorization gate live here,
    // on the serving side, so enforcement never depends on the client.
    if let DirectRequest::Authenticate { session, admission } = &request {
      return match self.auth.authenticate_peer(*session, remote, admission) {
        Ok(()) => {
          tracing::debug!(%session, remote = %remote, "collaboration direct admission handshake accepted");
          ServeOutcome::Payload(Vec::new())
        },
        Err(error) => {
          tracing::warn!(%session, remote = %remote, error = %format_args!("{error:#}"), "collaboration direct admission handshake rejected");
          ServeOutcome::Header(DirectResponseHeader::Unauthorized)
        },
      };
    }
    if !self.auth.authorize_direct(session, remote) {
      tracing::warn!(%session, remote = %remote, request_kind, "collaboration direct request rejected without valid admission");
      return ServeOutcome::Header(DirectResponseHeader::Unauthorized);
    }

    let handler = {
      let inner = self.inner.read().await;
      if !inner.attached.contains(&session) {
        tracing::warn!(%session, request_kind, "collaboration direct request rejected because session is not attached");
        return ServeOutcome::Header(DirectResponseHeader::NotAttached);
      }
      if let DirectRequest::Blob { blob, .. } = request {
        return inner
          .blobs
          .get(&session)
          .and_then(|outbox| outbox.get(blob))
          .map_or_else(
            || {
              tracing::warn!(%session, ?blob, "collaboration direct blob request missed outbox");
              ServeOutcome::Header(DirectResponseHeader::NotFound)
            },
            |bytes| {
              tracing::debug!(%session, ?blob, bytes = bytes.len(), "collaboration direct blob request hit outbox");
              ServeOutcome::Payload(bytes.to_vec())
            },
          );
      }
      inner.handlers.get(&session).cloned()
    };

    let Some(handler) = handler else {
      tracing::warn!(%session, request_kind, "collaboration direct request rejected because handler is missing");
      return ServeOutcome::Header(DirectResponseHeader::NotFound);
    };

    match request {
      DirectRequest::Snapshot { .. } => {
        if let Some(io) = handler.io.clone() {
          serve_snapshot_via_io(session, io).await
        } else {
          request_payload(handler.requests, session, request_kind, |reply| DirectServeRequest::Snapshot { reply }).await
        }
      },
      DirectRequest::Updates { have_vv, .. } => {
        tracing::trace!(%session, have_vv_bytes = have_vv.len(), "serving collaboration direct updates request");
        if let Some(io) = handler.io.clone() {
          serve_updates_via_io(session, io, have_vv).await
        } else {
          request_payload(handler.requests, session, request_kind, |reply| DirectServeRequest::Updates {
            have_vv,
            reply,
          })
          .await
        }
      },
      DirectRequest::Asset { asset, .. } => request_asset(handler.requests, session, asset).await,
      DirectRequest::Blob { .. } | DirectRequest::Authenticate { .. } | DirectRequest::RequestAdmission { .. } => {
        ServeOutcome::Header(DirectResponseHeader::NotFound)
      },
    }
  }
}

#[derive(Debug)]
enum ServeOutcome {
  Header(DirectResponseHeader),
  Payload(Vec<u8>),
}

impl ServeOutcome {
  fn kind(&self) -> &'static str {
    match self {
      Self::Header(header) => direct_response_header_kind(header),
      Self::Payload(_) => "ok",
    }
  }

  fn payload_len(&self) -> usize {
    match self {
      Self::Header(_) => 0,
      Self::Payload(payload) => payload.len(),
    }
  }
}

#[derive(Clone, Debug)]
pub struct DirectProto {
  state: DirectServeState,
  permits: Arc<Semaphore>,
}

impl DirectProto {
  #[must_use]
  pub fn new(state: DirectServeState) -> Self {
    Self {
      state,
      permits: Arc::new(Semaphore::new(DIRECT_SERVE_CONCURRENCY)),
    }
  }

  async fn handle_stream(
    &self,
    mut send: SendStream,
    mut recv: RecvStream,
    remote: EndpointId,
    link_is_fast: bool,
    _permit: OwnedSemaphorePermit,
  ) -> Result<()> {
    let request = read_frame::<DirectRequest>(&mut recv).await?;
    let session = request.session();
    let request_kind = direct_request_kind(&request);
    // Only the CRDT payloads (snapshots, update batches) benefit from zstd; assets
    // are already compressed and blobs are opaque, so those stream verbatim.
    let compressible = matches!(request, DirectRequest::Snapshot { .. } | DirectRequest::Updates { .. });
    tracing::debug!(%session, remote = %remote, request_kind, "received collaboration direct stream request");
    let outcome = self.state.serve(request, remote).await;
    tracing::debug!(%session, request_kind, outcome = outcome.kind(), payload_bytes = outcome.payload_len(), "collaboration direct request served");
    write_response(&mut send, outcome, compressible, link_is_fast).await?;
    Ok(())
  }
}

impl ProtocolHandler for DirectProto {
  async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
    // The remote endpoint id comes from the connection's TLS handshake, so it
    // is an authenticated sender identity, not self-reported data.
    let remote = connection.remote_id();
    // Decide once per connection whether the path is fast enough that zstd decode
    // would be the bottleneck rather than the link (then skip compression).
    let link_is_fast = link_is_fast(&connection);
    tracing::trace!(remote = %remote, link_is_fast, "accepted collaboration direct connection");
    while let Ok((send, recv)) = connection.accept_bi().await {
      let Ok(permit) = self.permits.clone().try_acquire_owned() else {
        let mut send = send;
        if let Err(error) = write_response(&mut send, ServeOutcome::Header(DirectResponseHeader::Busy), false, link_is_fast).await {
          tracing::warn!("flowstate collab direct busy response failed: {error:#}");
        }
        continue;
      };
      let proto = self.clone();
      tokio::spawn(async move {
        if let Err(error) = proto
          .handle_stream(send, recv, remote, link_is_fast, permit)
          .await
        {
          tracing::error!(error = %format_args!("{error:#}"), "collaboration direct stream failed");
        }
      });
    }
    tracing::trace!("collaboration direct connection closed");
    Ok(())
  }
}

pub(crate) fn install_endpoint(endpoint: Endpoint) {
  let cache = CLIENT_ENDPOINT.get_or_init(|| StdRwLock::new(None));
  if let Ok(mut cached) = cache.write() {
    *cached = Some(endpoint);
    tracing::debug!(installed = true, "installed collaboration direct client endpoint");
  } else {
    tracing::warn!("failed to install collaboration direct client endpoint");
  }
}

pub async fn pull_with_fallback(req: DirectRequest, candidates: Vec<EndpointId>, per_peer_timeout: Duration) -> Result<Vec<u8>> {
  pull_with_fallback_progress(req, candidates, per_peer_timeout, None).await
}

pub async fn pull_with_fallback_progress(
  req: DirectRequest,
  candidates: Vec<EndpointId>,
  per_peer_timeout: Duration,
  progress: Option<Sender<PullProgress>>,
) -> Result<Vec<u8>> {
  let endpoint = CLIENT_ENDPOINT
    .get()
    .and_then(|cache| cache.read().ok().and_then(|cached| cached.clone()))
    .ok_or_else(|| anyhow!("collaboration direct client endpoint is not running"))?;
  ensure!(!endpoint.is_closed(), "collaboration direct client endpoint is not running");
  pull_with_endpoint_progress(&endpoint, req, candidates, per_peer_timeout, progress).await
}

pub async fn pull_with_endpoint(
  endpoint: &Endpoint,
  req: DirectRequest,
  candidates: Vec<EndpointId>,
  per_peer_timeout: Duration,
) -> Result<Vec<u8>> {
  pull_with_endpoint_progress(endpoint, req, candidates, per_peer_timeout, None).await
}

pub async fn pull_with_endpoint_progress(
  endpoint: &Endpoint,
  req: DirectRequest,
  candidates: Vec<EndpointId>,
  per_peer_timeout: Duration,
  progress: Option<Sender<PullProgress>>,
) -> Result<Vec<u8>> {
  let session = req.session();
  let request_kind = direct_request_kind(&req);
  let candidate_count = candidates.len();
  if candidates.is_empty() {
    tracing::warn!(%session, request_kind, "collaboration direct pull has no candidate peers");
  }
  ensure!(!candidates.is_empty(), "direct pull has no candidate peers");
  tracing::debug!(%session, request_kind, candidate_count, ?per_peer_timeout, "starting collaboration direct pull");

  let mut errors = Vec::new();
  for peer in candidates {
    tracing::trace!(%session, request_kind, peer = %peer, "attempting collaboration direct pull peer");
    // §perf: pull_once uses req only by reference; avoid cloning DirectRequest
    // (whose `have_vv` grows with document size) per candidate peer.
    match timeout(per_peer_timeout, pull_once(endpoint, peer, &req, progress.as_ref())).await {
      Ok(Ok(bytes)) => {
        tracing::debug!(%session, request_kind, peer = %peer, bytes = bytes.len(), "collaboration direct pull peer succeeded");
        return Ok(bytes);
      },
      Ok(Err(error)) => {
        tracing::warn!(%session, request_kind, peer = %peer, error = %format_args!("{error:#}"), "collaboration direct pull peer failed");
        errors.push(format!("{peer}: {error:#}"));
      },
      Err(_) => {
        tracing::warn!(%session, request_kind, peer = %peer, ?per_peer_timeout, "collaboration direct pull peer timed out");
        errors.push(format!("{peer}: timed out after {per_peer_timeout:?}"));
      },
    }
  }

  tracing::error!(%session, request_kind, candidate_count, "collaboration direct pull failed for all candidates");
  Err(anyhow!("direct pull failed for all candidates: {}", errors.join("; ")))
}

async fn pull_once(endpoint: &Endpoint, peer: EndpointId, req: &DirectRequest, progress: Option<&Sender<PullProgress>>) -> Result<Vec<u8>> {
  let session = req.session();
  let request_kind = direct_request_kind(req);
  tracing::trace!(%session, request_kind, peer = %peer, "dialing collaboration direct peer");
  let connection = endpoint
    .connect(peer, DIRECT_ALPN)
    .await
    .context("direct dial failed")?;
  if let Some(admission) = super::auth::local_admission(session) {
    authenticate_connection(&connection, session, admission)
      .await
      .context("collaboration admission handshake failed")?;
  }
  request_on_connection(&connection, req, progress).await
}

async fn authenticate_connection(connection: &Connection, session: SessionId, admission: SessionAdmission) -> Result<()> {
  tracing::trace!(%session, "sending collaboration direct admission handshake");
  let _ = request_on_connection(connection, &DirectRequest::Authenticate { session, admission }, None).await?;
  Ok(())
}

async fn request_on_connection(connection: &Connection, req: &DirectRequest, progress: Option<&Sender<PullProgress>>) -> Result<Vec<u8>> {
  let session = req.session();
  let request_kind = direct_request_kind(req);
  tracing::trace!(%session, request_kind, "opening collaboration direct stream");
  let (mut send, mut recv) = connection
    .open_bi()
    .await
    .context("opening direct request stream failed")?;
  let frame = encode_frame(req)?;
  tracing::trace!(%session, request_kind, frame_bytes = frame.len(), "sending collaboration direct request frame");
  write_frame(&mut send, &frame).await?;
  send.finish()?;

  let header = read_frame::<DirectResponseHeader>(&mut recv).await?;
  tracing::trace!(%session, request_kind, response = direct_response_header_kind(&header), "received collaboration direct response header");
  match header {
    DirectResponseHeader::Ok {
      codec,
      wire_len,
      uncompressed_len,
    } => {
      let wire = read_payload(&mut recv, wire_len, progress).await?;
      let uncompressed_len = usize::try_from(uncompressed_len).context("direct payload is too large for this platform")?;
      let payload = super::wire_compression::decompress_from_wire(codec, wire, uncompressed_len).context("decoding direct response payload")?;
      tracing::trace!(%session, request_kind, wire_bytes = wire_len, bytes = payload.len(), codec = ?codec, "read collaboration direct response payload");
      Ok(payload)
    },
    DirectResponseHeader::NotAttached => Err(anyhow!("peer is not attached to this session")),
    DirectResponseHeader::NotFound => Err(anyhow!("peer does not have the requested collaboration data")),
    DirectResponseHeader::Busy => Err(anyhow!("peer is busy serving collaboration data")),
    DirectResponseHeader::Unauthorized => Err(anyhow!("peer rejected our collaboration session admission")),
  }
}

async fn request_payload<F>(
  requests: Sender<DirectServeRequest>,
  session: SessionId,
  request_kind: &'static str,
  make_request: F,
) -> ServeOutcome
where
  F: FnOnce(Sender<Result<Vec<u8>>>) -> DirectServeRequest,
{
  let (reply_tx, reply_rx) = async_channel::bounded(1);
  if requests.send(make_request(reply_tx)).await.is_err() {
    tracing::warn!(%session, request_kind, "collaboration direct session request channel closed");
    return ServeOutcome::Header(DirectResponseHeader::NotAttached);
  }
  match timeout(DIRECT_RESPONSE_TIMEOUT, reply_rx.recv()).await {
    Ok(Ok(Ok(bytes))) => {
      tracing::trace!(%session, request_kind, bytes = bytes.len(), "collaboration direct session returned payload");
      ServeOutcome::Payload(bytes)
    },
    Ok(Ok(Err(error))) => {
      tracing::warn!(%session, request_kind, error = %format_args!("{error:#}"), "collaboration direct session failed to produce payload");
      ServeOutcome::Header(DirectResponseHeader::NotFound)
    },
    Ok(Err(error)) => {
      tracing::warn!(%session, request_kind, error = %error, "collaboration direct session payload reply channel closed");
      ServeOutcome::Header(DirectResponseHeader::NotFound)
    },
    Err(_) => {
      tracing::warn!(%session, request_kind, ?DIRECT_RESPONSE_TIMEOUT, "collaboration direct session payload timed out");
      ServeOutcome::Header(DirectResponseHeader::NotFound)
    },
  }
}

/// Serve a snapshot pull through the document I/O service. The gate-held part
/// is a brief `fork()`; the actual snapshot encode happens off-gate on the I/O
/// thread (spec I-9a long-export rule), so a large snapshot neither stalls
/// typing nor gets starved behind local edits.
async fn serve_snapshot_via_io(session: SessionId, io: SyncIoHandle) -> ServeOutcome {
  match io.snapshot_bytes().await {
    Ok(bytes) => {
      tracing::trace!(%session, bytes = bytes.len(), "served collaboration snapshot via the document I/O service");
      ServeOutcome::Payload(bytes)
    },
    Err(error) => {
      tracing::warn!(%session, error = %format_args!("{error:#}"), "exporting collaboration snapshot failed");
      ServeOutcome::Header(DirectResponseHeader::NotFound)
    },
  }
}

/// Serve an incremental-updates pull through the document I/O service (see
/// [`serve_snapshot_via_io`]); the version-vector decode and gate-held export
/// happen on the I/O thread.
async fn serve_updates_via_io(session: SessionId, io: SyncIoHandle, have_vv: Vec<u8>) -> ServeOutcome {
  match io.export_updates_for(have_vv).await {
    Ok(bytes) => {
      tracing::trace!(%session, bytes = bytes.len(), "served collaboration updates via the document I/O service");
      ServeOutcome::Payload(bytes)
    },
    Err(error) => {
      tracing::warn!(%session, error = %format_args!("{error:#}"), "exporting collaboration updates failed");
      ServeOutcome::Header(DirectResponseHeader::NotFound)
    },
  }
}

async fn request_asset(requests: Sender<DirectServeRequest>, session: SessionId, asset: u128) -> ServeOutcome {
  let (reply_tx, reply_rx) = async_channel::bounded(1);
  if requests
    .send(DirectServeRequest::Asset { asset, reply: reply_tx })
    .await
    .is_err()
  {
    tracing::warn!(%session, asset, "collaboration direct asset request channel closed");
    return ServeOutcome::Header(DirectResponseHeader::NotAttached);
  }
  match timeout(DIRECT_RESPONSE_TIMEOUT, reply_rx.recv()).await {
    Ok(Ok(Ok(asset_bytes))) => {
      tracing::trace!(%session, asset, bytes = asset_bytes.bytes.len(), "collaboration direct session returned asset");
      ServeOutcome::Payload(asset_bytes.bytes)
    },
    Ok(Ok(Err(error))) => {
      tracing::warn!(%session, asset, error = %format_args!("{error:#}"), "collaboration direct session failed to produce asset");
      ServeOutcome::Header(DirectResponseHeader::NotFound)
    },
    Ok(Err(error)) => {
      tracing::warn!(%session, asset, error = %error, "collaboration direct asset reply channel closed");
      ServeOutcome::Header(DirectResponseHeader::NotFound)
    },
    Err(_) => {
      tracing::warn!(%session, asset, ?DIRECT_RESPONSE_TIMEOUT, "collaboration direct asset timed out");
      ServeOutcome::Header(DirectResponseHeader::NotFound)
    },
  }
}

async fn read_frame<T>(recv: &mut RecvStream) -> Result<T>
where
  T: for<'de> serde::Deserialize<'de>,
{
  let mut len_bytes = [0; 4];
  recv.read_exact(&mut len_bytes).await?;
  let payload_len = u32::from_le_bytes(len_bytes) as usize;
  ensure!(payload_len <= MAX_FRAME_LEN, "direct frame exceeds {MAX_FRAME_LEN} bytes");
  let mut frame = len_bytes.to_vec();
  frame.resize(4 + payload_len, 0);
  recv.read_exact(&mut frame[4..]).await?;
  decode_frame(&frame)
}

/// Whether the connection's path is fast enough that zstd decode would be the
/// bottleneck rather than the link, in which case compression is skipped. A very
/// low RTT means LAN/localhost; higher-RTT WAN/relay paths gain ~2.5–3× effective
/// goodput from compression, so they keep it.
fn link_is_fast(connection: &Connection) -> bool {
  // `PathId::ZERO` is the primary path (iroh's own net-report reads rtt the same
  // way). An unknown rtt is treated as NOT fast, so we compress when in doubt.
  connection
    .rtt(PathId::ZERO)
    .is_some_and(|rtt| rtt < FAST_LINK_RTT)
}

async fn write_response(send: &mut SendStream, outcome: ServeOutcome, compressible: bool, link_is_fast: bool) -> Result<()> {
  tracing::trace!(
    outcome = outcome.kind(),
    payload_bytes = outcome.payload_len(),
    "writing collaboration direct response"
  );
  match outcome {
    ServeOutcome::Header(header) => write_frame(send, &encode_frame(&header)?).await?,
    ServeOutcome::Payload(payload) => {
      // Snapshots/updates are compressed by size (dictionary for small, long mode
      // for big); non-CRDT payloads (assets) and fast links stream verbatim.
      // §perf: WireBytes hands the caller the SAME Arc as the cache (or a borrow of
      // `payload`), avoiding the 1-2 full compressed-payload memcpys the old Cow path took.
      let (codec, wire): (WireCodec, super::wire_compression::WireBytes<'_>) = if compressible {
        super::wire_compression::compress_for_wire(&payload, link_is_fast)
      } else {
        (WireCodec::None, super::wire_compression::WireBytes::Borrowed(payload.as_slice()))
      };
      let header = DirectResponseHeader::Ok {
        codec,
        wire_len: wire.as_slice().len() as u64,
        uncompressed_len: payload.len() as u64,
      };
      tracing::trace!(codec = ?codec, wire_bytes = wire.as_slice().len(), payload_bytes = payload.len(), "wrote collaboration direct payload for the wire");
      write_frame(send, &encode_frame(&header)?).await?;
      write_payload(send, wire.as_slice()).await?;
    },
  }
  send.finish()?;
  Ok(())
}

async fn write_frame(send: &mut SendStream, frame: &[u8]) -> Result<()> {
  tracing::trace!(frame_bytes = frame.len(), "writing collaboration direct frame");
  send.write_all(frame).await?;
  Ok(())
}

async fn write_payload(send: &mut SendStream, payload: &[u8]) -> Result<()> {
  tracing::trace!(
    payload_bytes = payload.len(),
    chunk_bytes = MAX_PAYLOAD_CHUNK_LEN,
    "writing collaboration direct payload"
  );
  for chunk in payload.chunks(MAX_PAYLOAD_CHUNK_LEN) {
    send.write_all(chunk).await?;
  }
  Ok(())
}

async fn read_payload(recv: &mut RecvStream, total_len: u64, progress: Option<&Sender<PullProgress>>) -> Result<Vec<u8>> {
  let total_len_usize = usize::try_from(total_len).context("direct payload is too large for this platform")?;
  ensure!(total_len_usize <= MAX_PAYLOAD_LEN, "direct payload exceeds {MAX_PAYLOAD_LEN} bytes");
  tracing::trace!(
    payload_bytes = total_len_usize,
    chunk_bytes = MAX_PAYLOAD_CHUNK_LEN,
    "reading collaboration direct payload"
  );
  // Reserve the exact buffer up front so the whole payload lands in one allocation with
  // no reallocation on the happy path, but do NOT zero-fill it (as `vec![0; len]` would):
  // the reserved pages are only committed as chunks are copied in, which keeps large
  // snapshots — up to 1 GiB here — off the hot path's zeroing cost. `with_capacity` is a
  // virtual reservation, so a peer that overstates `total_len` cannot force a physical
  // commit beyond the bytes it actually sends.
  let mut payload = Vec::with_capacity(total_len_usize);
  if let Some(progress) = progress {
    let _ = progress.try_send(PullProgress { got: 0, total: total_len });
  }
  while payload.len() < total_len_usize {
    let remaining = total_len_usize - payload.len();
    match recv.read_chunk(remaining).await? {
      Some(bytes) => {
        payload.extend_from_slice(&bytes);
        if let Some(progress) = progress {
          let _ = progress.try_send(PullProgress {
            got: payload.len() as u64,
            total: total_len,
          });
        }
      },
      None => bail!("direct payload ended early: expected {total_len_usize} bytes, received {}", payload.len()),
    }
  }
  Ok(payload)
}

fn direct_request_kind(request: &DirectRequest) -> &'static str {
  match request {
    DirectRequest::Authenticate { .. } => "authenticate",
    DirectRequest::RequestAdmission { .. } => "request-admission",
    DirectRequest::Snapshot { .. } => "snapshot",
    DirectRequest::Updates { .. } => "updates",
    DirectRequest::Blob { .. } => "blob",
    DirectRequest::Asset { .. } => "asset",
  }
}

fn direct_request_detail_bytes(request: &DirectRequest) -> usize {
  match request {
    DirectRequest::Updates { have_vv, .. } => have_vv.len(),
    DirectRequest::Authenticate { .. }
    | DirectRequest::RequestAdmission { .. }
    | DirectRequest::Snapshot { .. }
    | DirectRequest::Blob { .. }
    | DirectRequest::Asset { .. } => 0,
  }
}

fn direct_response_header_kind(header: &DirectResponseHeader) -> &'static str {
  match header {
    DirectResponseHeader::Ok { .. } => "ok",
    DirectResponseHeader::NotAttached => "not_attached",
    DirectResponseHeader::NotFound => "not_found",
    DirectResponseHeader::Busy => "busy",
    DirectResponseHeader::Unauthorized => "unauthorized",
  }
}

impl DirectRequest {
  fn session(&self) -> SessionId {
    match self {
      Self::Authenticate { session, .. }
      | Self::RequestAdmission {
        request: crate::discovery::DiscoveryAdmissionRequest { session, .. },
      }
      | Self::Snapshot { session }
      | Self::Updates { session, .. }
      | Self::Blob { session, .. }
      | Self::Asset { session, .. } => *session,
    }
  }
}

// Class 6 — asset (and any direct) pull must FAIL CLEANLY when candidate peers are
// unreachable: return the "failed for all candidates" error within the per-peer timeout
// budget, never hang the caller. The field logs showed `direct pull failed for all
// candidates request_kind="asset"`; these pin that the failure path is bounded and
// well-formed, using a local-only (`presets::Minimal`, no relays/DNS) endpoint so the test
// is deterministic and never touches the network.
#[cfg(test)]
mod tests {
  use super::*;
  use iroh::{SecretKey, endpoint::presets};

  fn asset_request() -> DirectRequest {
    DirectRequest::Asset {
      session: SessionId::new(),
      asset: 42,
    }
  }

  #[tokio::test]
  async fn asset_pull_fails_cleanly_when_all_candidates_are_unreachable() -> Result<()> {
    let endpoint = Endpoint::builder(presets::Minimal).bind().await?;
    // A freshly-minted peer id that is not serving DIRECT_ALPN anywhere reachable.
    let unreachable = SecretKey::generate().public();
    let result = pull_with_endpoint(&endpoint, asset_request(), vec![unreachable], Duration::from_millis(250)).await;
    let error = result.expect_err("asset pull from an unreachable candidate must fail, not hang");
    assert!(
      error
        .to_string()
        .contains("direct pull failed for all candidates"),
      "expected the all-candidates-failed error, got: {error}"
    );
    Ok(())
  }

  #[tokio::test]
  async fn asset_pull_with_no_candidates_errors_without_reaching_the_network() -> Result<()> {
    let endpoint = Endpoint::builder(presets::Minimal).bind().await?;
    let result = pull_with_endpoint(&endpoint, asset_request(), Vec::new(), Duration::from_millis(250)).await;
    assert!(result.is_err(), "asset pull with no candidate peers must error cleanly");
    Ok(())
  }
}
