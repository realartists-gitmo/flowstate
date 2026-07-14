use anyhow::{Context as _, Result, ensure};
use iroh::EndpointAddr;
use iroh_tickets::{ParseError, Ticket};
use serde::{Deserialize, Serialize};

use crate::{SessionId, admission::SessionAdmission, proto_gossip::PROTOCOL_VERSION};

pub const TICKET_KIND: &str = "fscollab";
pub const INVITE_URL_PREFIX: &str = "flowstate://join#";

/// Symmetric editor invite for one ephemeral live session (wire version 3).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionTicket {
  pub version: u16,
  pub session: SessionId,
  pub bootstrap: Vec<EndpointAddr>,
  pub title: String,
  pub admission: SessionAdmission,
  tag: [u8; 32],
}

impl SessionTicket {
  #[must_use]
  pub fn new(session: SessionId, bootstrap: Vec<EndpointAddr>, title: String, admission: SessionAdmission) -> Self {
    let mut ticket = Self {
      version: PROTOCOL_VERSION,
      session,
      bootstrap,
      title,
      admission,
      tag: [0; 32],
    };
    ticket.tag = ticket
      .expected_tag()
      .expect("ticket metadata serialization should not fail");
    ticket
  }

  #[must_use]
  pub const fn is_supported_version(&self) -> bool {
    self.version == PROTOCOL_VERSION
  }

  /// Authenticate all user-visible and routing metadata before any dial.
  pub fn verify_for_join(&self) -> Result<()> {
    ensure!(self.is_supported_version(), "unsupported collaboration protocol version");
    ensure!(!self.bootstrap.is_empty(), "collaboration invite has no reachable participants");
    ensure!(self.tag == self.expected_tag()?, "collaboration invite metadata is invalid");
    Ok(())
  }

  fn expected_tag(&self) -> Result<[u8; 32]> {
    let payload =
      postcard::to_stdvec(&(self.version, self.session, &self.bootstrap, &self.title)).context("encoding collaboration invite metadata")?;
    Ok(self.admission.tag(&payload))
  }

  #[must_use]
  pub fn encode_text(&self) -> String {
    self.encode_string()
  }

  /// User-facing deep link. The bearer ticket lives in the URL fragment so an
  /// eventual HTTPS bridge never sends it to a web server in the request path.
  #[must_use]
  pub fn encode_invite_link(&self) -> String {
    format!("{INVITE_URL_PREFIX}{}", self.encode_text())
  }

  pub fn decode_text(text: &str) -> Result<Self, ParseError> {
    let text = text.trim();
    let ticket = text.strip_prefix(INVITE_URL_PREFIX).unwrap_or(text);
    Self::decode_string(ticket)
  }
}

impl Ticket for SessionTicket {
  const KIND: &'static str = TICKET_KIND;

  fn encode_bytes(&self) -> Vec<u8> {
    postcard::to_stdvec(self).expect("SessionTicket serialization should not fail")
  }

  fn decode_bytes(bytes: &[u8]) -> Result<Self, ParseError> {
    let ticket: Self = postcard::from_bytes(bytes).map_err(|_| ParseError::verification_failed("invalid collaboration ticket payload"))?;
    ticket
      .verify_for_join()
      .map_err(|_| ParseError::verification_failed("invalid collaboration ticket authentication"))?;
    Ok(ticket)
  }
}

#[cfg(test)]
mod tests {
  use iroh::{EndpointAddr, SecretKey};

  use super::SessionTicket;
  use crate::{SessionAdmission, SessionId};

  fn ticket() -> SessionTicket {
    SessionTicket::new(
      SessionId::from_bytes([1; 32]),
      vec![EndpointAddr::new(SecretKey::from_bytes(&[2; 32]).public())],
      "Shared brief".to_string(),
      SessionAdmission::from_bytes([3; 32]),
    )
  }

  #[test]
  fn ticket_round_trips_and_authenticates_metadata() {
    let encoded = ticket().encode_text();
    let decoded = SessionTicket::decode_text(&encoded).expect("ticket should decode");
    decoded
      .verify_for_join()
      .expect("ticket should authenticate");
  }

  #[test]
  fn deep_link_and_raw_ticket_are_both_accepted() {
    let ticket = ticket();
    assert_eq!(
      SessionTicket::decode_text(&ticket.encode_text())
        .unwrap()
        .session,
      ticket.session
    );
    assert_eq!(
      SessionTicket::decode_text(&ticket.encode_invite_link())
        .unwrap()
        .session,
      ticket.session
    );
  }

  #[test]
  fn tampered_metadata_is_rejected() {
    let mut ticket = ticket();
    ticket.title.push_str(" (forged)");
    assert!(ticket.verify_for_join().is_err());
  }

  #[test]
  fn any_participant_endpoint_can_be_the_bootstrap() {
    let mut ticket = ticket();
    ticket.bootstrap = vec![EndpointAddr::new(SecretKey::from_bytes(&[9; 32]).public())];
    ticket.tag = ticket.expected_tag().expect("tag");
    assert!(ticket.verify_for_join().is_ok());
  }
}
