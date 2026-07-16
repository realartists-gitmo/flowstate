use std::fmt;

use anyhow::{Result, ensure};
use serde::{Deserialize, Serialize};

use crate::ids::{BlobId, SessionId};

/// Version 5 fences the B-S4 cell-flow schema (.db8 `LORO_SCHEMA_VERSION` 2):
/// an old binary in a v2 session would silently corrupt the doc — its
/// whole-cell writer destroys in-cell records and its repair passes fight the
/// new shape — so the wire refuses the pairing outright, with the existing
/// direction-aware upgrade message. COUPLED with
/// `flowstate_document::LORO_SCHEMA_VERSION` — bump both together (unit test
/// `wire_and_schema_versions_bump_together` enforces the pair).
/// Version 4 added the session's document kind (rich text vs flow) to the
/// ticket, inside the authenticated metadata. Version 3 used symmetric,
/// session-lifetime editor admission. Pre-release protocol versions are
/// intentionally not accepted.
pub const PROTOCOL_VERSION: u16 = 5;
pub const GOSSIP_INLINE_LIMIT: usize = 2 * 1024;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ProtocolVersionMismatch {
  pub version: u16,
}

impl fmt::Display for ProtocolVersionMismatch {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    write!(f, "unsupported gossip protocol version {}", self.version)
  }
}

impl std::error::Error for ProtocolVersionMismatch {}

#[must_use]
pub fn is_protocol_version_mismatch(error: &anyhow::Error) -> bool {
  error.is::<ProtocolVersionMismatch>()
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum GossipMsg {
  Update(Vec<u8>),
  UpdateAvailable { blob: BlobId, len: u64 },
  Presence(Vec<u8>),
  Digest { session: SessionId, vv: Vec<u8> },
}

impl GossipMsg {
  #[must_use]
  pub fn kind(&self) -> &'static str {
    match self {
      Self::Update(_) => "update",
      Self::UpdateAvailable { .. } => "update_available",
      Self::Presence(_) => "presence",
      Self::Digest { .. } => "digest",
    }
  }

  #[must_use]
  pub fn payload_len(&self) -> u64 {
    match self {
      Self::Update(bytes) | Self::Presence(bytes) => bytes.len() as u64,
      Self::UpdateAvailable { len, .. } => *len,
      Self::Digest { vv, .. } => vv.len() as u64,
    }
  }
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
  // This intentionally uses the encoder: postcard has no stable counting
  // serializer here, and callers only need a small inline/blob decision.
  Ok(encode(msg)?.len())
}

pub fn decode(bytes: &[u8]) -> Result<GossipMsg> {
  ensure!(bytes.len() >= 2, "gossip frame is missing protocol version");
  let version = u16::from_le_bytes([bytes[0], bytes[1]]);
  if version != PROTOCOL_VERSION {
    return Err(ProtocolVersionMismatch { version }.into());
  }
  Ok(postcard::from_bytes(&bytes[2..])?)
}

#[cfg(test)]
mod version_fence_tests {
  use super::PROTOCOL_VERSION;

  /// B-S4 THE FENCE PAIR: a .db8 schema bump without a wire bump lets an old
  /// binary join a session it will silently corrupt (whole-cell writer +
  /// old-shape repairs against the new schema). If this fails, you bumped one
  /// constant without the other — read the doc comments on BOTH.
  #[test]
  fn wire_and_schema_versions_bump_together() {
    assert_eq!(
      (PROTOCOL_VERSION, flowstate_document::LORO_SCHEMA_VERSION),
      (5, 2),
      "PROTOCOL_VERSION and LORO_SCHEMA_VERSION are a coupled pair — bump them together"
    );
  }
}
