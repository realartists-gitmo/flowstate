use std::{fmt, str::FromStr};

use serde::{Deserialize, Serialize};

pub type PeerId = iroh::EndpointId;

pub const SESSION_ID_LEN: usize = 32;

pub const PALETTE: [u32; 8] = [
  0x3b82f6, 0xef4444, 0x22c55e, 0xf59e0b, 0x8b5cf6, 0x06b6d4, 0xec4899, 0x84cc16,
];

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SessionId([u8; SESSION_ID_LEN]);

impl SessionId {
  #[must_use]
  pub fn new() -> Self {
    Self(rand::random())
  }

  #[must_use]
  pub const fn from_bytes(bytes: [u8; SESSION_ID_LEN]) -> Self {
    Self(bytes)
  }

  #[must_use]
  pub const fn as_bytes(&self) -> &[u8; SESSION_ID_LEN] {
    &self.0
  }

  #[must_use]
  pub fn color_index_for_peer(peer: &PeerId) -> usize {
    let bytes = peer.as_bytes();
    let mut hash = 0usize;
    for byte in bytes.iter().take(8) {
      hash = hash.wrapping_mul(257).wrapping_add(usize::from(*byte));
    }
    hash % PALETTE.len()
  }
}

impl Default for SessionId {
  fn default() -> Self {
    Self::new()
  }
}

impl fmt::Display for SessionId {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    for byte in self.0 {
      write!(f, "{byte:02x}")?;
    }
    Ok(())
  }
}

impl FromStr for SessionId {
  type Err = SessionIdParseError;

  fn from_str(value: &str) -> Result<Self, Self::Err> {
    if value.len() != SESSION_ID_LEN * 2 {
      return Err(SessionIdParseError);
    }

    let mut bytes = [0; SESSION_ID_LEN];
    for (ix, chunk) in value.as_bytes().chunks_exact(2).enumerate() {
      let text = std::str::from_utf8(chunk).map_err(|_| SessionIdParseError)?;
      bytes[ix] = u8::from_str_radix(text, 16).map_err(|_| SessionIdParseError)?;
    }
    Ok(Self(bytes))
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SessionIdParseError;

impl fmt::Display for SessionIdParseError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("expected a 64-character lowercase hex session id")
  }
}

impl std::error::Error for SessionIdParseError {}

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct BlobId(pub u128);

impl BlobId {
  #[must_use]
  pub fn new() -> Self {
    Self(uuid::Uuid::new_v4().as_u128())
  }
}

impl Default for BlobId {
  fn default() -> Self {
    Self::new()
  }
}
