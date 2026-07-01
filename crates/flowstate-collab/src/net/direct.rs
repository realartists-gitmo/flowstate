use std::{
  collections::{HashMap, HashSet},
  sync::{Arc, OnceLock, RwLock as StdRwLock},
  time::Duration,
};

use anyhow::{Context as _, Result, anyhow, ensure};
use async_channel::Sender;
use iroh::{
  Endpoint, EndpointId,
  endpoint::{Connection, RecvStream, SendStream},
  protocol::{AcceptError, ProtocolHandler},
};
use tokio::{
  sync::{OwnedSemaphorePermit, RwLock, Semaphore},
  time::timeout,
};

use crate::{
  ids::{BlobId, SessionId},
  proto_direct::{
    AssetBytes, DIRECT_ALPN, DirectRequest, DirectResponseHeader, MAX_FRAME_LEN, MAX_PAYLOAD_CHUNK_LEN, decode_frame, encode_frame,
  },
};

use super::{PullProgress, blobs::BlobOutbox};

const DIRECT_SERVE_CONCURRENCY: usize = 4;
const DIRECT_RESPONSE_TIMEOUT: Duration = Duration::from_secs(10);
static CLIENT_ENDPOINT: OnceLock<StdRwLock<Option<Endpoint>>> = OnceLock::new();

#[derive(Clone, Debug)]
pub struct DirectSessionHandler {
  requests: Sender<DirectServeRequest>,
}

impl DirectSessionHandler {
  #[must_use]
  pub fn new(requests: Sender<DirectServeRequest>) -> Self {
    Self { requests }
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
}

#[derive(Debug, Default)]
struct DirectServeInner {
  attached: HashSet<SessionId>,
  blobs: HashMap<SessionId, BlobOutbox>,
  handlers: HashMap<SessionId, DirectSessionHandler>,
}

impl DirectServeState {
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
    tracing::debug!(%session, removed, blob_count, handler_removed, attached_sessions = inner.attached.len(), "detached collaboration direct session");
  }

  pub async fn register_handler(&self, session: SessionId, handler: DirectSessionHandler) {
    let mut inner = self.inner.write().await;
    inner.attached.insert(session);
    let replaced = inner.handlers.insert(session, handler).is_some();
    tracing::debug!(%session, replaced, handler_count = inner.handlers.len(), "registered collaboration direct session handler");
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

  async fn serve(&self, request: DirectRequest) -> ServeOutcome {
    let session = request.session();
    let request_kind = direct_request_kind(&request);
    let request_detail_bytes = direct_request_detail_bytes(&request);
    tracing::trace!(%session, request_kind, request_detail_bytes, "serving collaboration direct request");
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
        request_payload(handler.requests, session, request_kind, |reply| DirectServeRequest::Snapshot { reply }).await
      },
      DirectRequest::Updates { have_vv, .. } => {
        tracing::trace!(%session, have_vv_bytes = have_vv.len(), "forwarding collaboration direct updates request to session");
        request_payload(handler.requests, session, request_kind, |reply| DirectServeRequest::Updates {
          have_vv,
          reply,
        })
        .await
      },
      DirectRequest::Asset { asset, .. } => request_asset(handler.requests, session, asset).await,
      DirectRequest::Blob { .. } => ServeOutcome::Header(DirectResponseHeader::NotFound),
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

  async fn handle_stream(&self, mut send: SendStream, mut recv: RecvStream, _permit: OwnedSemaphorePermit) -> Result<()> {
    let request = read_frame::<DirectRequest>(&mut recv).await?;
    let session = request.session();
    let request_kind = direct_request_kind(&request);
    tracing::debug!(%session, request_kind, "received collaboration direct stream request");
    let outcome = self.state.serve(request).await;
    tracing::debug!(%session, request_kind, outcome = outcome.kind(), payload_bytes = outcome.payload_len(), "collaboration direct request served");
    write_response(&mut send, outcome).await?;
    Ok(())
  }
}

impl ProtocolHandler for DirectProto {
  async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
    tracing::trace!("accepted collaboration direct connection");
    while let Ok((send, recv)) = connection.accept_bi().await {
      let Ok(permit) = self.permits.clone().try_acquire_owned() else {
        let mut send = send;
        if let Err(error) = write_response(&mut send, ServeOutcome::Header(DirectResponseHeader::Busy)).await {
          tracing::warn!("flowstate collab direct busy response failed: {error:#}");
        }
        continue;
      };
      let proto = self.clone();
      tokio::spawn(async move {
        if let Err(error) = proto.handle_stream(send, recv, permit).await {
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
    match timeout(per_peer_timeout, pull_once(endpoint, peer, req.clone(), progress.as_ref())).await {
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

async fn pull_once(endpoint: &Endpoint, peer: EndpointId, req: DirectRequest, progress: Option<&Sender<PullProgress>>) -> Result<Vec<u8>> {
  let session = req.session();
  let request_kind = direct_request_kind(&req);
  tracing::trace!(%session, request_kind, peer = %peer, "dialing collaboration direct peer");
  let connection = endpoint
    .connect(peer, DIRECT_ALPN)
    .await
    .context("direct dial failed")?;
  tracing::trace!(%session, request_kind, peer = %peer, "opening collaboration direct stream");
  let (mut send, mut recv) = connection
    .open_bi()
    .await
    .context("opening direct request stream failed")?;
  let frame = encode_frame(&req)?;
  tracing::trace!(%session, request_kind, peer = %peer, frame_bytes = frame.len(), "sending collaboration direct request frame");
  write_frame(&mut send, &frame).await?;
  send.finish()?;

  let header = read_frame::<DirectResponseHeader>(&mut recv).await?;
  tracing::trace!(%session, request_kind, peer = %peer, response = direct_response_header_kind(&header), "received collaboration direct response header");
  match header {
    DirectResponseHeader::Ok { total_len } => {
      let payload = read_payload(&mut recv, total_len, progress).await?;
      tracing::trace!(%session, request_kind, peer = %peer, bytes = payload.len(), "read collaboration direct response payload");
      Ok(payload)
    },
    DirectResponseHeader::NotAttached => Err(anyhow!("peer is not attached to this session")),
    DirectResponseHeader::NotFound => Err(anyhow!("peer does not have the requested collaboration data")),
    DirectResponseHeader::Busy => Err(anyhow!("peer is busy serving collaboration data")),
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

async fn write_response(send: &mut SendStream, outcome: ServeOutcome) -> Result<()> {
  tracing::trace!(
    outcome = outcome.kind(),
    payload_bytes = outcome.payload_len(),
    "writing collaboration direct response"
  );
  match outcome {
    ServeOutcome::Header(header) => write_frame(send, &encode_frame(&header)?).await?,
    ServeOutcome::Payload(payload) => {
      let header = DirectResponseHeader::Ok {
        total_len: payload.len() as u64,
      };
      write_frame(send, &encode_frame(&header)?).await?;
      write_payload(send, &payload).await?;
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
  ensure!(total_len_usize <= MAX_FRAME_LEN, "direct payload exceeds {MAX_FRAME_LEN} bytes");
  tracing::trace!(
    payload_bytes = total_len_usize,
    chunk_bytes = MAX_PAYLOAD_CHUNK_LEN,
    "reading collaboration direct payload"
  );
  let mut payload = vec![0; total_len_usize];
  let mut offset = 0;
  if let Some(progress) = progress {
    let _ = progress.try_send(PullProgress { got: 0, total: total_len });
  }
  while offset < total_len_usize {
    let next = (offset + MAX_PAYLOAD_CHUNK_LEN).min(total_len_usize);
    recv.read_exact(&mut payload[offset..next]).await?;
    offset = next;
    if let Some(progress) = progress {
      let _ = progress.try_send(PullProgress {
        got: offset as u64,
        total: total_len,
      });
    }
  }
  Ok(payload)
}

fn direct_request_kind(request: &DirectRequest) -> &'static str {
  match request {
    DirectRequest::Snapshot { .. } => "snapshot",
    DirectRequest::Updates { .. } => "updates",
    DirectRequest::Blob { .. } => "blob",
    DirectRequest::Asset { .. } => "asset",
  }
}

fn direct_request_detail_bytes(request: &DirectRequest) -> usize {
  match request {
    DirectRequest::Updates { have_vv, .. } => have_vv.len(),
    DirectRequest::Snapshot { .. } | DirectRequest::Blob { .. } | DirectRequest::Asset { .. } => 0,
  }
}

fn direct_response_header_kind(header: &DirectResponseHeader) -> &'static str {
  match header {
    DirectResponseHeader::Ok { .. } => "ok",
    DirectResponseHeader::NotAttached => "not_attached",
    DirectResponseHeader::NotFound => "not_found",
    DirectResponseHeader::Busy => "busy",
  }
}

impl DirectRequest {
  fn session(&self) -> SessionId {
    match self {
      Self::Snapshot { session } | Self::Updates { session, .. } | Self::Blob { session, .. } | Self::Asset { session, .. } => *session,
    }
  }
}
