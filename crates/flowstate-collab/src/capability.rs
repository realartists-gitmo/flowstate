//! Signed capability grants for collaboration sessions (FS-080).
//!
//! A [`SessionCapability`] is an ed25519-signed bearer grant issued by the
//! session owner's endpoint secret key. It binds a role (edit vs. view), an
//! expiry time, and a revocation epoch to a session id. The session id doubles
//! as the document lineage id on the wire (anti-entropy digests compare the
//! same id), so signing the session id covers both.
//!
//! Trust model, stated honestly:
//! - Capabilities are *bearer* grants: whoever holds the invite ticket holds
//!   the capability. The signature proves the grant came from the owner and
//!   has not been altered; it does not pin the grant to one endpoint key.
//! - Every peer that knows the owner public key (all ticket holders, and the
//!   owner itself) can verify capabilities, so enforcement does not depend on
//!   the honesty of the joining client.
//! - Revocation is by epoch: the owner bumps [`SessionCapability::capability_epoch`]'s
//!   session-wide floor and gossips a signed control frame; capabilities minted
//!   before the bump verify as stale and are rejected.

use std::{
  fmt,
  time::{SystemTime, UNIX_EPOCH},
};

use iroh::{PublicKey, SecretKey, Signature};
use serde::{Deserialize, Serialize};

use crate::ids::SessionId;

/// Domain separation for capability signatures. The `v2` suffix tracks
/// `proto_gossip::PROTOCOL_VERSION`; bumping the protocol version invalidates
/// old signatures by construction.
const CAPABILITY_SIGNING_CONTEXT: &[u8] = b"flowstate-collab/capability/v2";
/// Domain separation for signed capability-epoch bump control frames.
const EPOCH_SIGNING_CONTEXT: &[u8] = b"flowstate-collab/capability-epoch/v2";

/// Default lifetime of an invite ticket before it expires.
pub const DEFAULT_TICKET_TTL_SECS: u64 = 24 * 60 * 60;

/// Role granted to a joiner by an invite ticket.
///
/// This mirrors `gpui_flowtext::CollaborationRole` for the invitable subset
/// (the `Owner` role is never granted by a ticket; the owner is whoever holds
/// the session owner secret key).
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum CapabilityRole {
  Editor,
  Viewer,
}

impl CapabilityRole {
  #[must_use]
  pub const fn can_write(self) -> bool {
    matches!(self, Self::Editor)
  }

  #[must_use]
  pub const fn label(self) -> &'static str {
    match self {
      Self::Editor => "editor",
      Self::Viewer => "viewer",
    }
  }

  const fn wire_byte(self) -> u8 {
    match self {
      Self::Editor => 1,
      Self::Viewer => 2,
    }
  }
}

impl From<CapabilityRole> for gpui_flowtext::CollaborationRole {
  fn from(value: CapabilityRole) -> Self {
    match value {
      CapabilityRole::Editor => Self::Editor,
      CapabilityRole::Viewer => Self::Viewer,
    }
  }
}

/// An owner-signed grant carried inside an invite ticket and presented by
/// joiners during the direct-connection handshake.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SessionCapability {
  pub role: CapabilityRole,
  /// Unix seconds; the capability is invalid at and after this instant.
  pub expires_at: u64,
  /// Revocation epoch this capability was minted under. Capabilities with an
  /// epoch below the session's current epoch are rejected.
  pub capability_epoch: u64,
  /// The session owner's public key, so any ticket holder can verify.
  pub owner: PublicKey,
  /// ed25519 signature by `owner` over the canonical signing payload.
  pub signature: Signature,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CapabilityError {
  BadSignature,
  Expired { expires_at: u64, now: u64 },
  StaleEpoch { epoch: u64, current: u64 },
}

impl fmt::Display for CapabilityError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::BadSignature => f.write_str("collaboration capability signature is invalid"),
      Self::Expired { expires_at, now } => {
        write!(f, "collaboration capability expired at {expires_at} (now {now})")
      },
      Self::StaleEpoch { epoch, current } => {
        write!(f, "collaboration capability epoch {epoch} was revoked (current epoch {current})")
      },
    }
  }
}

impl std::error::Error for CapabilityError {}

impl SessionCapability {
  /// Issue a capability signed by the session owner's endpoint secret key.
  #[must_use]
  pub fn issue(owner_secret: &SecretKey, session: SessionId, role: CapabilityRole, expires_at: u64, capability_epoch: u64) -> Self {
    let owner = owner_secret.public();
    let signature = owner_secret.sign(&signing_payload(session, &owner, role, expires_at, capability_epoch));
    Self {
      role,
      expires_at,
      capability_epoch,
      owner,
      signature,
    }
  }

  /// Verify the owner signature, expiry, and revocation epoch.
  ///
  /// `min_epoch` is the verifier's current session capability epoch; pass `0`
  /// when the current epoch is unknown (e.g. a fresh joiner verifying its own
  /// ticket client-side — the owner re-checks the epoch at the handshake).
  pub fn verify(&self, session: SessionId, now_unix: u64, min_epoch: u64) -> Result<(), CapabilityError> {
    let payload = signing_payload(session, &self.owner, self.role, self.expires_at, self.capability_epoch);
    self
      .owner
      .verify(&payload, &self.signature)
      .map_err(|_| CapabilityError::BadSignature)?;
    if now_unix >= self.expires_at {
      return Err(CapabilityError::Expired {
        expires_at: self.expires_at,
        now: now_unix,
      });
    }
    if self.capability_epoch < min_epoch {
      return Err(CapabilityError::StaleEpoch {
        epoch: self.capability_epoch,
        current: min_epoch,
      });
    }
    Ok(())
  }
}

/// Canonical byte encoding covered by capability signatures:
/// context || session/lineage id || owner key || role || `expires_at` || epoch.
fn signing_payload(session: SessionId, owner: &PublicKey, role: CapabilityRole, expires_at: u64, capability_epoch: u64) -> Vec<u8> {
  let mut payload = Vec::with_capacity(CAPABILITY_SIGNING_CONTEXT.len() + 32 + 32 + 1 + 8 + 8);
  payload.extend_from_slice(CAPABILITY_SIGNING_CONTEXT);
  payload.extend_from_slice(session.as_bytes());
  payload.extend_from_slice(owner.as_bytes());
  payload.push(role.wire_byte());
  payload.extend_from_slice(&expires_at.to_le_bytes());
  payload.extend_from_slice(&capability_epoch.to_le_bytes());
  payload
}

/// Sign a capability-epoch bump control frame for `session`.
#[must_use]
pub fn sign_epoch_bump(owner_secret: &SecretKey, session: SessionId, epoch: u64) -> Signature {
  owner_secret.sign(&epoch_payload(session, epoch))
}

/// Verify a capability-epoch bump control frame against the owner key.
#[must_use]
pub fn verify_epoch_bump(owner: &PublicKey, session: SessionId, epoch: u64, signature: &Signature) -> bool {
  owner.verify(&epoch_payload(session, epoch), signature).is_ok()
}

fn epoch_payload(session: SessionId, epoch: u64) -> Vec<u8> {
  let mut payload = Vec::with_capacity(EPOCH_SIGNING_CONTEXT.len() + 32 + 8);
  payload.extend_from_slice(EPOCH_SIGNING_CONTEXT);
  payload.extend_from_slice(session.as_bytes());
  payload.extend_from_slice(&epoch.to_le_bytes());
  payload
}

/// Current unix time in seconds.
#[must_use]
pub fn unix_now() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .map_or(0, |elapsed| elapsed.as_secs())
}

#[cfg(test)]
mod tests {
  use iroh::SecretKey;

  use super::{CapabilityError, CapabilityRole, SessionCapability, sign_epoch_bump, verify_epoch_bump};
  use crate::ids::SessionId;

  fn owner_secret(seed: u8) -> SecretKey {
    SecretKey::from_bytes(&[seed; 32])
  }

  #[test]
  fn capability_sign_verify_round_trip() {
    let secret = owner_secret(1);
    let session = SessionId::from_bytes([7; 32]);
    let capability = SessionCapability::issue(&secret, session, CapabilityRole::Editor, 1_000, 3);

    assert_eq!(capability.owner, secret.public());
    assert_eq!(capability.verify(session, 999, 3), Ok(()));

    let encoded = postcard::to_stdvec(&capability).expect("capability should serialize");
    let decoded: SessionCapability = postcard::from_bytes(&encoded).expect("capability should deserialize");
    assert_eq!(decoded, capability);
    assert_eq!(decoded.verify(session, 999, 3), Ok(()));
  }

  #[test]
  fn expired_capability_is_rejected() {
    let secret = owner_secret(2);
    let session = SessionId::from_bytes([8; 32]);
    let capability = SessionCapability::issue(&secret, session, CapabilityRole::Viewer, 500, 0);

    assert_eq!(
      capability.verify(session, 500, 0),
      Err(CapabilityError::Expired { expires_at: 500, now: 500 })
    );
    assert_eq!(capability.verify(session, 499, 0), Ok(()));
  }

  #[test]
  fn revoked_epoch_is_rejected_and_newer_epoch_is_accepted() {
    let secret = owner_secret(3);
    let session = SessionId::from_bytes([9; 32]);
    let stale = SessionCapability::issue(&secret, session, CapabilityRole::Editor, 1_000, 1);
    let fresh = SessionCapability::issue(&secret, session, CapabilityRole::Editor, 1_000, 2);

    assert_eq!(stale.verify(session, 10, 2), Err(CapabilityError::StaleEpoch { epoch: 1, current: 2 }));
    assert_eq!(fresh.verify(session, 10, 2), Ok(()));
  }

  #[test]
  fn tampered_capability_fails_signature_verification() {
    let secret = owner_secret(4);
    let session = SessionId::from_bytes([10; 32]);
    let capability = SessionCapability::issue(&secret, session, CapabilityRole::Viewer, 1_000, 0);

    let mut role_tampered = capability.clone();
    role_tampered.role = CapabilityRole::Editor;
    assert_eq!(role_tampered.verify(session, 10, 0), Err(CapabilityError::BadSignature));

    let mut expiry_tampered = capability.clone();
    expiry_tampered.expires_at = u64::MAX;
    assert_eq!(expiry_tampered.verify(session, 10, 0), Err(CapabilityError::BadSignature));

    let mut epoch_tampered = capability.clone();
    epoch_tampered.capability_epoch += 1;
    assert_eq!(epoch_tampered.verify(session, 10, 1), Err(CapabilityError::BadSignature));

    let mut owner_swapped = capability.clone();
    owner_swapped.owner = owner_secret(5).public();
    assert_eq!(owner_swapped.verify(session, 10, 0), Err(CapabilityError::BadSignature));

    let other_session = SessionId::from_bytes([11; 32]);
    assert_eq!(capability.verify(other_session, 10, 0), Err(CapabilityError::BadSignature));
  }

  #[test]
  fn epoch_bump_signature_round_trip() {
    let secret = owner_secret(6);
    let session = SessionId::from_bytes([12; 32]);
    let signature = sign_epoch_bump(&secret, session, 4);

    assert!(verify_epoch_bump(&secret.public(), session, 4, &signature));
    assert!(!verify_epoch_bump(&secret.public(), session, 5, &signature));
    assert!(!verify_epoch_bump(&owner_secret(7).public(), session, 4, &signature));
    assert!(!verify_epoch_bump(&secret.public(), SessionId::from_bytes([13; 32]), 4, &signature));
  }
}
