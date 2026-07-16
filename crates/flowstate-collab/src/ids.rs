use std::{fmt, str::FromStr};

use rand::{TryRngCore as _, rngs::OsRng};
use serde::{Deserialize, Serialize};

pub type PeerId = iroh::EndpointId;

pub const SESSION_ID_LEN: usize = 32;

pub const PALETTE: [u32; 8] = [0x3b82f6, 0xef4444, 0x22c55e, 0xf59e0b, 0x8b5cf6, 0x06b6d4, 0xec4899, 0x84cc16];

#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize, Deserialize)]
pub struct SessionId([u8; SESSION_ID_LEN]);

impl SessionId {
  #[must_use]
  pub fn new() -> Self {
    let mut bytes = [0; SESSION_ID_LEN];
    OsRng
      .try_fill_bytes(&mut bytes)
      .expect("OS randomness must be available to create collaboration session IDs");
    Self(bytes)
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
    Self::color_index_for_peer_bytes(peer.as_bytes())
  }

  /// C-S5: the same palette keyed by a durable AUTHOR user id (comment
  /// authorship survives sessions, so its accent color must too — one id, one
  /// rendering, online or off).
  #[must_use]
  pub fn color_for_user(user_id: u128) -> u32 {
    PALETTE[Self::color_index_for_peer_bytes(&user_id.to_le_bytes())]
  }

  #[must_use]
  pub fn color_index_for_peer_bytes(bytes: &[u8]) -> usize {
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
    // §perf: manual nibble→hex lookup avoids invoking the fmt machinery per byte.
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut buf = String::with_capacity(self.0.len() * 2);
    for &byte in &self.0 {
      buf.push(HEX[(byte >> 4) as usize] as char);
      buf.push(HEX[(byte & 0x0f) as usize] as char);
    }
    f.write_str(&buf)
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
    f.write_str("expected a 64-character hex session id")
  }
}

#[cfg(test)]
mod tests {
  use super::SessionId;

  #[test]
  fn parses_uppercase_session_id_hex() {
    let parsed = "0123456789ABCDEFFEDCBA98765432100123456789ABCDEFFEDCBA9876543210"
      .parse::<SessionId>()
      .expect("uppercase hex should parse");
    assert_eq!(parsed.to_string(), "0123456789abcdeffedcba98765432100123456789abcdeffedcba9876543210");
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
