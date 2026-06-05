use super::*;

pub(crate) fn live_update_wire_message(document_id: DocumentId, current_session_id: SessionId, update: LiveUpdate) -> Option<WireMessage> {
  if update.source_session_id == Some(current_session_id) {
    return None;
  }
  match update.kind {
    LiveUpdateKind::Wire(message) => Some(message),
    LiveUpdateKind::Event(event) => session_event_wire_message(document_id, event),
  }
}

pub(crate) fn session_event_wire_message(document_id: DocumentId, event: SessionEvent) -> Option<WireMessage> {
  match event {
    SessionEvent::PeerAuthorized { actor_id, session_id, role } => Some(peer_event_message(
      document_id,
      actor_id,
      session_id,
      Some(role),
      PeerEventKind::Authorized,
    )),
    SessionEvent::PeerRoleChanged { actor_id, session_id, role } => Some(peer_event_message(
      document_id,
      actor_id,
      session_id,
      Some(role),
      PeerEventKind::RoleChanged,
    )),
    SessionEvent::PeerLeft { actor_id, session_id } => Some(peer_event_message(document_id, actor_id, session_id, None, PeerEventKind::Left)),
    _ => None,
  }
}

pub(crate) fn peer_authorized_message(document_id: DocumentId, peer: LivePeer) -> WireMessage {
  peer_event_message(document_id, peer.actor_id, peer.session_id, Some(peer.role), PeerEventKind::Authorized)
}

pub(crate) fn peer_event_message(
  document_id: DocumentId,
  actor_id: ActorId,
  session_id: SessionId,
  role: Option<Role>,
  kind: PeerEventKind,
) -> WireMessage {
  WireMessage::PeerEvent(PeerEventMessage {
    document_id,
    actor_id,
    session_id,
    role,
    kind,
  })
}
