use anyhow::{Context as _, Result, ensure};
use iroh::EndpointAddr;
use iroh_tickets::{ParseError, Ticket};
use serde::{Deserialize, Serialize};

use crate::{SessionId, capability::SessionCapability, proto_gossip::PROTOCOL_VERSION};

pub const TICKET_KIND: &str = "fscollab";

/// Invite ticket for a collaboration session (wire version 2, FS-080).
///
/// Version 2 replaces the unauthenticated version-1 payload: tickets now carry
/// a [`SessionCapability`] — an owner-signed grant binding role, expiry, and
/// revocation epoch to the session id. There is intentionally no back-compat
/// decode path for version-1 tickets (pre-release).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionTicket {
  pub version: u16,
  pub session: SessionId,
  pub inviter: EndpointAddr,
  pub title: String,
  pub capability: SessionCapability,
}

impl SessionTicket {
  #[must_use]
  pub fn new(session: SessionId, inviter: EndpointAddr, title: String, capability: SessionCapability) -> Self {
    Self {
      version: PROTOCOL_VERSION,
      session,
      inviter,
      title,
      capability,
    }
  }

  #[must_use]
  pub const fn is_supported_version(&self) -> bool {
    self.version == PROTOCOL_VERSION
  }

  /// Client-side verification before dialing: version, owner binding, owner
  /// signature, and expiry. The revocation epoch cannot be checked by a fresh
  /// joiner (it does not know the current epoch yet); the owner re-verifies it
  /// during the direct handshake, which is the enforcement point that does not
  /// rely on the joining client being honest.
  pub fn verify_for_join(&self, now_unix: u64) -> Result<()> {
    ensure!(self.is_supported_version(), "unsupported collaboration protocol version");
    ensure!(
      self.capability.owner == self.inviter.id,
      "collaboration ticket capability was not signed by the inviter"
    );
    self
      .capability
      .verify(self.session, now_unix, 0)
      .context("collaboration ticket capability is invalid")
  }

  #[must_use]
  pub fn encode_text(&self) -> String {
    self.encode_string()
  }

  pub fn decode_text(text: &str) -> Result<Self, ParseError> {
    Self::decode_string(text.trim())
  }
}

impl Ticket for SessionTicket {
  const KIND: &'static str = TICKET_KIND;

  /// `iroh_tickets::Ticket::encode_bytes` cannot return a `Result`; `SessionTicket`
  /// only contains postcard-serializable fields, so serialization failure would be a programmer error.
  fn encode_bytes(&self) -> Vec<u8> {
    postcard::to_stdvec(self).expect("SessionTicket serialization should not fail")
  }

  fn decode_bytes(bytes: &[u8]) -> Result<Self, ParseError> {
    let ticket: Self = postcard::from_bytes(bytes).map_err(|_| ParseError::verification_failed("invalid collaboration ticket payload"))?;
    if !ticket.is_supported_version() {
      return Err(ParseError::verification_failed("unsupported collaboration protocol version"));
    }
    Ok(ticket)
  }
}

#[cfg(test)]
mod tests {
  use iroh::{EndpointAddr, SecretKey};

  use super::SessionTicket;
  use crate::{
    SessionId,
    capability::{CapabilityRole, SessionCapability},
  };

  fn owner_secret(seed: u8) -> SecretKey {
    SecretKey::from_bytes(&[seed; 32])
  }

  fn ticket(owner: &SecretKey, session: SessionId, role: CapabilityRole, expires_at: u64, epoch: u64) -> SessionTicket {
    let capability = SessionCapability::issue(owner, session, role, expires_at, epoch);
    SessionTicket::new(session, EndpointAddr::new(owner.public()), "Doc".to_string(), capability)
  }

  #[test]
  fn ticket_encodes_and_decodes_with_a_verifiable_capability() {
    let owner = owner_secret(1);
    let session = SessionId::from_bytes([1; 32]);
    let ticket = ticket(&owner, session, CapabilityRole::Editor, 5_000, 2);

    let text = ticket.encode_text();
    let decoded = SessionTicket::decode_text(&text).expect("ticket text should decode");
    assert_eq!(decoded.session, session);
    assert_eq!(decoded.capability, ticket.capability);
    decoded
      .verify_for_join(4_999)
      .expect("unexpired signed ticket should verify");
  }

  #[test]
  fn expired_ticket_is_rejected_at_join() {
    let owner = owner_secret(2);
    let session = SessionId::from_bytes([2; 32]);
    let ticket = ticket(&owner, session, CapabilityRole::Viewer, 100, 0);

    assert!(ticket.verify_for_join(100).is_err());
    assert!(ticket.verify_for_join(99).is_ok());
  }

  #[test]
  fn ticket_signed_by_someone_other_than_the_inviter_is_rejected() {
    let owner = owner_secret(3);
    let imposter = owner_secret(4);
    let session = SessionId::from_bytes([3; 32]);
    let capability = SessionCapability::issue(&imposter, session, CapabilityRole::Editor, 5_000, 0);
    let ticket = SessionTicket::new(session, EndpointAddr::new(owner.public()), "Doc".to_string(), capability);

    assert!(ticket.verify_for_join(10).is_err());
  }

  #[test]
  fn tampered_ticket_role_is_rejected_at_join() {
    let owner = owner_secret(5);
    let session = SessionId::from_bytes([4; 32]);
    let mut ticket = ticket(&owner, session, CapabilityRole::Viewer, 5_000, 0);
    ticket.capability.role = CapabilityRole::Editor;

    assert!(ticket.verify_for_join(10).is_err());
  }
}
