//! Symmetric editor admission for live collaboration sessions.

use rand::{TryRngCore as _, rngs::OsRng};
use serde::{Deserialize, Serialize};

pub const SESSION_SECRET_LEN: usize = 32;

/// Bearer secret shared by every admitted editor in one ephemeral session.
/// It is deliberately not persisted: the session and all of its links die
/// when the last participant disconnects.
#[derive(Clone, Serialize, Deserialize)]
pub struct SessionAdmission([u8; SESSION_SECRET_LEN]);

impl std::fmt::Debug for SessionAdmission {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("SessionAdmission([REDACTED])")
  }
}

impl PartialEq for SessionAdmission {
  fn eq(&self, other: &Self) -> bool {
    self
      .0
      .iter()
      .zip(other.0.iter())
      .fold(0u8, |difference, (left, right)| difference | (left ^ right))
      == 0
  }
}

impl Eq for SessionAdmission {}

impl SessionAdmission {
  #[must_use]
  pub fn generate() -> Self {
    let mut bytes = [0; SESSION_SECRET_LEN];
    OsRng
      .try_fill_bytes(&mut bytes)
      .expect("OS randomness must be available to create a collaboration admission secret");
    Self(bytes)
  }

  #[must_use]
  pub const fn from_bytes(bytes: [u8; SESSION_SECRET_LEN]) -> Self {
    Self(bytes)
  }

  #[must_use]
  pub const fn as_bytes(&self) -> &[u8; SESSION_SECRET_LEN] {
    &self.0
  }

  #[must_use]
  pub fn tag(&self, payload: &[u8]) -> [u8; 32] {
    *blake3::keyed_hash(&self.0, payload).as_bytes()
  }
}
