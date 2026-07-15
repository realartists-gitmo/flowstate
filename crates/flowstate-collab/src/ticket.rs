use std::fmt;

use anyhow::{Context as _, Result, ensure};
use iroh::EndpointAddr;
use iroh_tickets::{ParseError, Ticket};
use serde::{Deserialize, Serialize};

use crate::{SessionId, admission::SessionAdmission, proto_gossip::PROTOCOL_VERSION};

pub const TICKET_KIND: &str = "fscollab";
pub const INVITE_URL_PREFIX: &str = "flowstate://join#";

/// Which document family a session synchronizes. Carried in the ticket
/// (inside the authenticated metadata — kind-flipping a ticket is tamper) so
/// the joiner spawns the right runtime BEFORE any bytes arrive; each
/// runtime's `from_snapshot` schema check stays as defense-in-depth.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub enum DocumentKind {
  RichText,
  Flow,
}

impl fmt::Display for DocumentKind {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    match self {
      Self::RichText => write!(f, "rich-text"),
      Self::Flow => write!(f, "flow"),
    }
  }
}

/// Symmetric editor invite for one ephemeral live session (wire version 4).
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionTicket {
  pub version: u16,
  pub session: SessionId,
  pub bootstrap: Vec<EndpointAddr>,
  pub title: String,
  pub document: DocumentKind,
  pub admission: SessionAdmission,
  tag: [u8; 32],
}

impl SessionTicket {
  #[must_use]
  pub fn new(session: SessionId, bootstrap: Vec<EndpointAddr>, title: String, document: DocumentKind, admission: SessionAdmission) -> Self {
    let mut ticket = Self {
      version: PROTOCOL_VERSION,
      session,
      bootstrap,
      title,
      document,
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
    ensure!(self.is_supported_version(), "{}", version_mismatch_message(self.version));
    ensure!(!self.bootstrap.is_empty(), "collaboration invite has no reachable participants");
    ensure!(self.tag == self.expected_tag()?, "collaboration invite metadata is invalid");
    Ok(())
  }

  fn expected_tag(&self) -> Result<[u8; 32]> {
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

const NEWER_VERSION_MESSAGE: &str = "this invite was created by a newer Flowstate — update to join";
const OLDER_VERSION_MESSAGE: &str = "this invite is from an outdated Flowstate; ask for a fresh invite";

/// Version-mismatch UX: the leading field of the postcard payload is the
/// wire version, so even an undecodable foreign-version ticket tells us which
/// direction the mismatch runs.
fn version_mismatch_message(version: u16) -> String {
  if version > PROTOCOL_VERSION {
    NEWER_VERSION_MESSAGE.to_string()
  } else {
    format!("this invite is from an outdated Flowstate (protocol {version}); ask for a fresh invite")
  }
}

impl Ticket for SessionTicket {
  const KIND: &'static str = TICKET_KIND;

  fn encode_bytes(&self) -> Vec<u8> {
    postcard::to_stdvec(self).expect("SessionTicket serialization should not fail")
  }

  fn decode_bytes(bytes: &[u8]) -> Result<Self, ParseError> {
    let Ok(ticket) = postcard::from_bytes::<Self>(bytes) else {
      // A foreign-version ticket usually fails structural decode outright.
      // Pre-decode the leading varint version field for a direction-aware
      // message instead of a generic "invalid payload". (`ParseError` only
      // carries `&'static str`, so this path drops the numeric version; the
      // richer message lives in `verify_for_join`.)
      let message = match postcard::from_bytes::<u16>(bytes) {
        Ok(version) if version > PROTOCOL_VERSION => NEWER_VERSION_MESSAGE,
        Ok(version) if version < PROTOCOL_VERSION => OLDER_VERSION_MESSAGE,
        _ => "invalid collaboration ticket payload",
      };
      return Err(ParseError::verification_failed(message));
    };
    ticket.verify_for_join().map_err(|_| {
      let message = match ticket.version.cmp(&PROTOCOL_VERSION) {
        std::cmp::Ordering::Greater => NEWER_VERSION_MESSAGE,
        std::cmp::Ordering::Less => OLDER_VERSION_MESSAGE,
        std::cmp::Ordering::Equal => "invalid collaboration ticket authentication",
      };
      ParseError::verification_failed(message)
    })?;
    Ok(ticket)
  }
}

#[cfg(test)]
mod tests {
  use iroh::{EndpointAddr, SecretKey};
  use iroh_tickets::Ticket as _;

  use super::{DocumentKind, SessionTicket};
  use crate::{SessionAdmission, SessionId};

  fn ticket_with_kind(document: DocumentKind) -> SessionTicket {
    SessionTicket::new(
      SessionId::from_bytes([1; 32]),
      vec![EndpointAddr::new(SecretKey::from_bytes(&[2; 32]).public())],
      "Shared brief".to_string(),
      document,
      SessionAdmission::from_bytes([3; 32]),
    )
  }

  fn ticket() -> SessionTicket {
    ticket_with_kind(DocumentKind::RichText)
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
  fn document_kind_round_trips_for_both_kinds() {
    for kind in [DocumentKind::RichText, DocumentKind::Flow] {
      let decoded = SessionTicket::decode_text(&ticket_with_kind(kind).encode_text()).expect("ticket should decode");
      assert_eq!(decoded.document, kind);
    }
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
  fn kind_flipped_ticket_is_rejected() {
    // Kind-flipping is tamper: the document kind is inside the HMAC'd
    // metadata, so a flow invite can never be replayed as a rich-text one.
    let mut ticket = ticket_with_kind(DocumentKind::Flow);
    ticket.document = DocumentKind::RichText;
    assert!(ticket.verify_for_join().is_err());
    let bytes = ticket.encode_bytes();
    assert!(SessionTicket::decode_bytes(&bytes).is_err());
  }

  #[test]
  fn foreign_version_gets_a_direction_aware_message() {
    let mut newer = ticket();
    newer.version = super::PROTOCOL_VERSION + 1;
    let error = newer.verify_for_join().unwrap_err().to_string();
    assert!(error.contains("newer Flowstate"), "got: {error}");

    let mut older = ticket();
    older.version = 3;
    let error = older.verify_for_join().unwrap_err().to_string();
    assert!(error.contains("outdated Flowstate"), "got: {error}");
  }

  #[test]
  fn any_participant_endpoint_can_be_the_bootstrap() {
    let mut ticket = ticket();
    ticket.bootstrap = vec![EndpointAddr::new(SecretKey::from_bytes(&[9; 32]).public())];
    ticket.tag = ticket.expected_tag().expect("tag");
    assert!(ticket.verify_for_join().is_ok());
  }
}
