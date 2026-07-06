use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

use crate::capability::SessionCapability;
use crate::ids::{BlobId, SessionId};
use crate::proto_gossip::PROTOCOL_VERSION;

pub const DIRECT_ALPN: &[u8] = b"flowstate/collab-direct/0";
pub const MAX_FRAME_LEN: usize = 2 * 1024 * 1024;
pub const MAX_PAYLOAD_CHUNK_LEN: usize = 256 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DirectRequest {
  /// Capability handshake (FS-080): presents the owner-signed grant from the
  /// invite ticket. Peers verify signature, expiry, and revocation epoch and
  /// record the sender endpoint with the granted role; data requests from
  /// endpoints that never authenticated are refused with `Unauthorized`.
  Authenticate { session: SessionId, capability: SessionCapability },
  Snapshot { session: SessionId },
  Updates { session: SessionId, have_vv: Vec<u8> },
  Blob { session: SessionId, blob: BlobId },
  Asset { session: SessionId, asset: u128 },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DirectResponseHeader {
  Ok { total_len: u64 },
  NotAttached,
  NotFound,
  Busy,
  /// The request was refused because the sender presented no valid capability
  /// (missing handshake, bad signature, expired, or revoked epoch).
  Unauthorized,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AssetBytes {
  pub bytes: Vec<u8>,
}

pub fn encode_frame<T: Serialize>(value: &T) -> Result<Vec<u8>> {
  let payload = postcard::to_stdvec(&(PROTOCOL_VERSION, value))?;
  ensure!(payload.len() <= MAX_FRAME_LEN, "direct frame exceeds {MAX_FRAME_LEN} bytes");
  let len = payload.len() as u32;
  let mut frame = len.to_le_bytes().to_vec();
  frame.extend(payload);
  Ok(frame)
}

pub fn decode_frame<T>(frame: &[u8]) -> Result<T>
where
  T: for<'de> Deserialize<'de>,
{
  ensure!(frame.len() >= 4, "direct frame is missing its length prefix");
  let payload_len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
  ensure!(payload_len <= MAX_FRAME_LEN, "direct frame exceeds {MAX_FRAME_LEN} bytes");
  let payload = frame
    .get(4..)
    .ok_or_else(|| anyhow!("direct frame payload is missing"))?;
  ensure!(payload.len() == payload_len, "direct frame length prefix does not match payload length");
  let (version, value): (u16, T) = postcard::from_bytes(payload)?;
  if version != PROTOCOL_VERSION {
    return Err(anyhow!("unsupported direct protocol version {version}"));
  }
  Ok(value)
}

pub fn split_frame(frame: &[u8]) -> Result<(usize, &[u8])> {
  ensure!(frame.len() >= 4, "direct frame is missing its length prefix");
  let payload_len = u32::from_le_bytes([frame[0], frame[1], frame[2], frame[3]]) as usize;
  ensure!(payload_len <= MAX_FRAME_LEN, "direct frame exceeds {MAX_FRAME_LEN} bytes");
  let payload = frame
    .get(4..)
    .ok_or_else(|| anyhow!("direct frame payload is missing"))?;
  ensure!(payload.len() == payload_len, "direct frame length prefix does not match payload length");
  Ok((payload_len, payload))
}
