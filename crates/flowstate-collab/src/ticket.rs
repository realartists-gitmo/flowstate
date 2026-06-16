use iroh::EndpointAddr;
use iroh_tickets::{ParseError, Ticket};
use serde::{Deserialize, Serialize};

use crate::{SessionId, proto_gossip::PROTOCOL_VERSION};

pub const TICKET_KIND: &str = "fscollab";

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SessionTicket {
  pub version: u16,
  pub session: SessionId,
  pub inviter: EndpointAddr,
  pub title: String,
}

impl SessionTicket {
  #[must_use]
  pub fn new(session: SessionId, inviter: EndpointAddr, title: String) -> Self {
    Self {
      version: PROTOCOL_VERSION,
      session,
      inviter,
      title,
    }
  }

  #[must_use]
  pub const fn is_supported_version(&self) -> bool {
    self.version == PROTOCOL_VERSION
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
