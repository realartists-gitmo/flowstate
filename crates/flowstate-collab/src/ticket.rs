use anyhow::{Context as _, Result, ensure};
use iroh::EndpointAddr;
use iroh_tickets::{ParseError, Ticket};
use serde::{Deserialize, Serialize};

use crate::{SessionId, admission::SessionAdmission, proto_gossip::PROTOCOL_VERSION};

pub const TICKET_KIND: &str = "fscollab";
pub const INVITE_URL_PREFIX: &str = "flowstate://join#";

/// What kind of document a live session shares. Rides the invite ticket
/// UNDER the HMAC tag (wire v4), so a ticket's kind cannot be flipped in
/// transit — a joiner spins up the right runtime before the first byte of
/// snapshot arrives (snapshot schema sniffing stays as defense-in-depth).
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum DocumentKind {
  #[default]
  RichText,
  Flow,
}

/// Symmetric editor invite for one ephemeral live session (wire version 4).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionTicket {
  pub version: u16,
  pub session: SessionId,
  pub bootstrap: Vec<EndpointAddr>,
  pub title: String,
  pub admission: SessionAdmission,
  pub document: DocumentKind,
  tag: [u8; 32],
}

impl SessionTicket {
  #[must_use]
  pub fn new(session: SessionId, bootstrap: Vec<EndpointAddr>, title: String, admission: SessionAdmission, document: DocumentKind) -> Self {
    let mut ticket = Self {
      version: PROTOCOL_VERSION,
      session,
      bootstrap,
      title,
      admission,
      document,
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

  /// Version-mismatch UX: a specific, actionable message per direction
  /// instead of a generic failure.
  fn version_mismatch_message(version: u16) -> Option<&'static str> {
    match version.cmp(&PROTOCOL_VERSION) {
      std::cmp::Ordering::Less => Some("this collaboration invite is outdated and can no longer be joined — ask for a fresh invite"),
      std::cmp::Ordering::Greater => Some("this collaboration invite was created by a newer Flowstate — update Flowstate to join"),
      std::cmp::Ordering::Equal => None,
    }
  }

  /// Authenticate all user-visible and routing metadata before any dial.
  pub fn verify_for_join(&self) -> Result<()> {
    if let Some(message) = Self::version_mismatch_message(self.version) {
      anyhow::bail!(message);
    }
    ensure!(!self.bootstrap.is_empty(), "collaboration invite has no reachable participants");
    ensure!(self.tag == self.expected_tag()?, "collaboration invite metadata is invalid");
    Ok(())
  }

  fn expected_tag(&self) -> Result<[u8; 32]> {
    // The document KIND is tag-covered (v4): flipping it would route a joiner
    // into the wrong runtime kind.
    let payload = postcard::to_stdvec(&(self.version, self.session, &self.bootstrap, &self.title, self.document))
      .context("encoding collaboration invite metadata")?;
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
    // Leading-varint pre-decode: the version is the first field, so a ticket
    // from a DIFFERENT wire version (whose full struct layout won't parse)
    // still yields the right UX message instead of a generic decode failure.
    if let Ok((version, _)) = postcard::take_from_bytes::<u16>(bytes)
      && let Some(message) = Self::version_mismatch_message(version)
    {
      return Err(ParseError::verification_failed(message));
    }
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
      super::DocumentKind::Flow,
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
  fn document_kind_round_trips_and_is_tag_covered() {
    let encoded = ticket().encode_text();
    let decoded = SessionTicket::decode_text(&encoded).expect("decode");
    assert_eq!(decoded.document, super::DocumentKind::Flow);
    // Kind-flipping is an HMAC failure, not a silent reroute.
    let mut flipped = ticket();
    flipped.document = super::DocumentKind::RichText;
    assert!(flipped.verify_for_join().is_err());
  }

  #[test]
  fn version_mismatch_yields_actionable_messages() {
    use iroh_tickets::Ticket as _;
    let mut old = ticket();
    old.version = 3;
    let bytes = old.encode_bytes();
    let error = SessionTicket::decode_bytes(&bytes).expect_err("v3 must be rejected");
    assert!(error.to_string().contains("outdated"), "{error}");
    let mut newer = ticket();
    newer.version = 99;
    let bytes = newer.encode_bytes();
    let error = SessionTicket::decode_bytes(&bytes).expect_err("future version must be rejected");
    assert!(error.to_string().contains("newer Flowstate"), "{error}");
  }

  #[test]
  fn any_participant_endpoint_can_be_the_bootstrap() {
    let mut ticket = ticket();
    ticket.bootstrap = vec![EndpointAddr::new(SecretKey::from_bytes(&[9; 32]).public())];
    ticket.tag = ticket.expected_tag().expect("tag");
    assert!(ticket.verify_for_join().is_ok());
  }
}
