use anyhow::{Result, anyhow, ensure};
use serde::{Deserialize, Serialize};

use crate::ids::{BlobId, SessionId};

pub const PROTOCOL_VERSION: u16 = 1;
pub const DIRECT_ALPN: &[u8] = b"flowstate/collab-direct/0";
pub const GOSSIP_INLINE_LIMIT: usize = 2 * 1024;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum GossipMsg {
  Update(Vec<u8>),
  UpdateAvailable { blob: BlobId, len: u64 },
  Presence(Vec<u8>),
  Digest { session: SessionId, vv: Vec<u8> },
}

pub fn encode(msg: &GossipMsg) -> Result<Vec<u8>> {
  let mut bytes = PROTOCOL_VERSION.to_le_bytes().to_vec();
  bytes.extend(postcard::to_stdvec(msg)?);
  Ok(bytes)
}

pub fn encode_inline(msg: &GossipMsg) -> Result<Vec<u8>> {
  let bytes = encode(msg)?;
  ensure!(bytes.len() <= GOSSIP_INLINE_LIMIT, "gossip frame exceeds {GOSSIP_INLINE_LIMIT} bytes");
  Ok(bytes)
}

pub fn encoded_len(msg: &GossipMsg) -> Result<usize> {
  Ok(encode(msg)?.len())
}

pub fn decode(bytes: &[u8]) -> Result<GossipMsg> {
  ensure!(bytes.len() >= 2, "gossip frame is missing protocol version");
  let version = u16::from_le_bytes([bytes[0], bytes[1]]);
  if version != PROTOCOL_VERSION {
    return Err(anyhow!("unsupported gossip protocol version {version}"));
  }
  Ok(postcard::from_bytes(&bytes[2..])?)
}
