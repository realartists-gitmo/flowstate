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
    AssetBytes, DIRECT_ALPN, DirectRequest, DirectResponseHeader, MAX_FRAME_LEN, MAX_PAYLOAD_CHUNK_LEN, decode_frame,
    encode_frame,
  },
};

use super::{PullProgress, blobs::BlobOutbox};

const DIRECT_SERVE_CONCURRENCY: usize = 4;
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
    self.inner.write().await.attached.insert(session);
  }

  pub async fn detach_session(&self, session: SessionId) {
    let mut inner = self.inner.write().await;
    inner.attached.remove(&session);
    inner.blobs.remove(&session);
    inner.handlers.remove(&session);
  }

  pub async fn register_handler(&self, session: SessionId, handler: DirectSessionHandler) {
    let mut inner = self.inner.write().await;
    inner.attached.insert(session);
    inner.handlers.insert(session, handler);
  }

  pub async fn insert_blob(&self, session: SessionId, bytes: Vec<u8>) -> BlobId {
    let mut inner = self.inner.write().await;
    inner.attached.insert(session);
    inner.blobs.entry(session).or_default().insert(bytes)
  }

  async fn serve(&self, request: DirectRequest) -> ServeOutcome {
    let session = request.session();
    let handler = {
      let inner = self.inner.read().await;
      if !inner.attached.contains(&session) {
        return ServeOutcome::Header(DirectResponseHeader::NotAttached);
      }
      if let DirectRequest::Blob { blob, .. } = request {
        return inner
          .blobs
          .get(&session)
          .and_then(|outbox| outbox.get(blob))
          .map_or(ServeOutcome::Header(DirectResponseHeader::NotFound), |bytes| {
            ServeOutcome::Payload(bytes.to_vec())
          });
      }
      inner.handlers.get(&session).cloned()
    };

    let Some(handler) = handler else {
      return ServeOutcome::Header(DirectResponseHeader::NotFound);
    };

    match request {
      DirectRequest::Snapshot { .. } => request_payload(handler.requests, |reply| DirectServeRequest::Snapshot { reply }).await,
      DirectRequest::Updates { have_vv, .. } => request_payload(handler.requests, |reply| DirectServeRequest::Updates { have_vv, reply }).await,
      DirectRequest::Asset { asset, .. } => request_asset(handler.requests, asset).await,
      DirectRequest::Blob { .. } => ServeOutcome::Header(DirectResponseHeader::NotFound),
    }
  }
}

#[derive(Debug)]
enum ServeOutcome {
  Header(DirectResponseHeader),
  Payload(Vec<u8>),
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
    let outcome = self.state.serve(request).await;
    write_response(&mut send, outcome).await?;
    Ok(())
  }
}

impl ProtocolHandler for DirectProto {
  async fn accept(&self, connection: Connection) -> Result<(), AcceptError> {
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
          tracing::warn!("flowstate collab direct stream failed: {error:#}");
        }
      });
    }
    Ok(())
  }
}

pub(crate) fn install_endpoint(endpoint: Endpoint) {
  let cache = CLIENT_ENDPOINT.get_or_init(|| StdRwLock::new(None));
  if let Ok(mut cached) = cache.write() {
    *cached = Some(endpoint);
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
  ensure!(!candidates.is_empty(), "direct pull has no candidate peers");

  let mut errors = Vec::new();
  for peer in candidates {
    match timeout(per_peer_timeout, pull_once(endpoint, peer, req.clone(), progress.as_ref())).await {
      Ok(Ok(bytes)) => return Ok(bytes),
      Ok(Err(error)) => errors.push(format!("{peer}: {error:#}")),
      Err(_) => errors.push(format!("{peer}: timed out after {per_peer_timeout:?}")),
    }
  }

  Err(anyhow!("direct pull failed for all candidates: {}", errors.join("; ")))
}

async fn pull_once(endpoint: &Endpoint, peer: EndpointId, req: DirectRequest, progress: Option<&Sender<PullProgress>>) -> Result<Vec<u8>> {
  let connection = endpoint
    .connect(peer, DIRECT_ALPN)
    .await
    .context("direct dial failed")?;
  let (mut send, mut recv) = connection
    .open_bi()
    .await
    .context("opening direct request stream failed")?;
  write_frame(&mut send, &encode_frame(&req)?).await?;
  send.finish()?;

  let header = read_frame::<DirectResponseHeader>(&mut recv).await?;
  match header {
    DirectResponseHeader::Ok { total_len } => read_payload(&mut recv, total_len, progress).await,
    DirectResponseHeader::NotAttached => Err(anyhow!("peer is not attached to this session")),
    DirectResponseHeader::NotFound => Err(anyhow!("peer does not have the requested collaboration data")),
    DirectResponseHeader::Busy => Err(anyhow!("peer is busy serving collaboration data")),
  }
}

async fn request_payload<F>(requests: Sender<DirectServeRequest>, make_request: F) -> ServeOutcome
where
  F: FnOnce(Sender<Result<Vec<u8>>>) -> DirectServeRequest,
{
  let (reply_tx, reply_rx) = async_channel::bounded(1);
  if requests.send(make_request(reply_tx)).await.is_err() {
    return ServeOutcome::Header(DirectResponseHeader::NotAttached);
  }
  match reply_rx.recv().await {
    Ok(Ok(bytes)) => ServeOutcome::Payload(bytes),
    Ok(Err(_)) | Err(_) => ServeOutcome::Header(DirectResponseHeader::NotFound),
  }
}

async fn request_asset(requests: Sender<DirectServeRequest>, asset: u128) -> ServeOutcome {
  let (reply_tx, reply_rx) = async_channel::bounded(1);
  if requests
    .send(DirectServeRequest::Asset { asset, reply: reply_tx })
    .await
    .is_err()
  {
    return ServeOutcome::Header(DirectResponseHeader::NotAttached);
  }
  match reply_rx.recv().await {
    Ok(Ok(asset)) => ServeOutcome::Payload(asset.bytes),
    Ok(Err(_)) | Err(_) => ServeOutcome::Header(DirectResponseHeader::NotFound),
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
  send.write_all(frame).await?;
  Ok(())
}

async fn write_payload(send: &mut SendStream, payload: &[u8]) -> Result<()> {
  for chunk in payload.chunks(MAX_PAYLOAD_CHUNK_LEN) {
    send.write_all(chunk).await?;
  }
  Ok(())
}

async fn read_payload(recv: &mut RecvStream, total_len: u64, progress: Option<&Sender<PullProgress>>) -> Result<Vec<u8>> {
  let total_len_usize = usize::try_from(total_len).context("direct payload is too large for this platform")?;
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
      let _ = progress.try_send(PullProgress { got: offset as u64, total: total_len });
    }
  }
  Ok(payload)
}

impl DirectRequest {
  fn session(&self) -> SessionId {
    match self {
      Self::Snapshot { session } | Self::Updates { session, .. } | Self::Blob { session, .. } | Self::Asset { session, .. } => *session,
    }
  }
}
