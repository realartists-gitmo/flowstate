mod discovery_runtime;
pub(crate) mod dropbox_checkpoint;
pub mod dropbox_oauth;
mod manager;
pub(crate) mod notify;
mod presence_view;
mod pump;
mod session;
pub mod share_dialog;
mod shutdown;
pub mod status;

use std::{borrow::BorrowMut, sync::Arc};

use anyhow::Result;
use async_channel::Receiver;
use flowstate_collab::{SessionId, doc_io::DocIoHandle, local_write::LocalDocHandle, ticket::SessionTicket};
use gpui::{App, BorrowAppContext, Context, Entity, ReadGlobal};
use uuid::Uuid;

use crate::rich_text_element::RichTextEditor;

pub use discovery_runtime::DiscoveryScanResult;
pub use manager::{CollabManager, JoinRequest};
pub use session::{
  Attachment, CollabEditor, CollabSession, Connectivity, DetachReason, JoinStage, JoinedDocument, SessionNotice, SessionPhase,
  SessionRosterEntry,
};
pub use shutdown::shutdown;

pub fn init<C>(cx: &mut C)
where
  C: BorrowAppContext,
{
  CollabManager::init(cx);
}

pub fn start_flow_session_for_panel<T>(
  panel_id: Uuid,
  editor: Entity<crate::flow::FlowEditor>,
  title: String,
  io: flowstate_collab::flow::FlowIoHandle,
  cx: &mut Context<T>,
) -> Result<SessionId>
where
  T: 'static,
{
  cx.update_default_global::<CollabManager, _>(|manager, cx| manager.start_flow_session_for_panel(panel_id, editor, title, io, cx))
}

pub fn start_session_for_panel<T>(
  panel_id: Uuid,
  editor: Entity<RichTextEditor>,
  title: String,
  io: DocIoHandle,
  cx: &mut Context<T>,
) -> Result<SessionId>
where
  T: 'static,
{
  cx.update_default_global::<CollabManager, _>(|manager, cx| manager.start_session_for_panel(panel_id, editor, title, io, cx))
}

/// JOIN handoff (Loro-first spec §3): take the document services the joining
/// session constructed from the initial snapshot — the write authority for the
/// editor and the I/O handle for saves/exports. One-shot: the write authority
/// moves out of the session (the session is transport-only, invariant 5);
/// subsequent calls return `None`.
pub fn take_joined_document_services_for_session<T>(session_id: SessionId, cx: &mut Context<T>) -> Option<(Arc<LocalDocHandle>, DocIoHandle)>
where
  T: 'static,
{
  cx.update_default_global::<CollabManager, _>(|manager, cx| {
    let session = manager.session_for_id(session_id)?;
    session.update(cx, |session, _| session.take_joined_document_services())
  })
}

pub fn join_session<T>(ticket: SessionTicket, cx: &mut Context<T>) -> Result<JoinRequest>
where
  T: 'static,
{
  cx.update_default_global::<CollabManager, _>(|manager, cx| manager.join_session(ticket, cx))
}

pub fn attach_joined_session<T>(session_id: SessionId, panel_id: Uuid, editor: Entity<RichTextEditor>, cx: &mut Context<T>) -> Result<()>
where
  T: 'static,
{
  cx.update_default_global::<CollabManager, _>(|manager, cx| manager.attach_joined_session(session_id, panel_id, editor, cx))
}

pub fn leave_session_for_panel<T>(panel_id: Uuid, cx: &mut Context<T>) -> bool
where
  T: 'static,
{
  cx.update_default_global::<CollabManager, _>(|manager, cx| manager.leave_session_for_panel(panel_id, cx))
}

pub fn request_ticket_for_panel<T>(panel_id: Uuid, cx: &mut Context<T>) -> Option<Receiver<Result<SessionTicket>>>
where
  T: 'static,
{
  cx.update_default_global::<CollabManager, _>(|manager, cx| manager.request_ticket_for_panel(panel_id, cx))
}

/// The document kind of a panel's live session (S9: what its tickets carry).
pub fn session_kind_for_panel(panel_id: Uuid, cx: &App) -> Option<flowstate_collab::DocumentKind> {
  let manager = CollabManager::global(cx);
  let session = manager.session_for_panel(panel_id)?;
  Some(session.read(cx).kind())
}

pub fn phase_for_panel(panel_id: Uuid, cx: &App) -> Option<SessionPhase> {
  CollabManager::global(cx).phase_for_panel(panel_id, cx)
}

pub fn phase_for_session(session_id: SessionId, cx: &App) -> Option<SessionPhase> {
  CollabManager::global(cx).phase_for_session(session_id, cx)
}

pub fn session_for_panel(panel_id: Uuid, cx: &App) -> Option<Entity<CollabSession>> {
  CollabManager::global(cx).session_for_panel(panel_id)
}

pub fn session_for_id(session_id: SessionId, cx: &App) -> Option<Entity<CollabSession>> {
  CollabManager::global(cx).session_for_id(session_id)
}

pub fn roster_for_panel(panel_id: Uuid, cx: &App) -> Vec<SessionRosterEntry> {
  CollabManager::global(cx).roster_for_panel(panel_id, cx)
}

pub fn scan_document_discovery<T>(document_id: u128, cx: &mut Context<T>) -> Receiver<DiscoveryScanResult>
where
  T: 'static,
{
  cx.update_default_global::<CollabManager, _>(|manager, cx| manager.scan_document_discovery(document_id, cx))
}

pub fn reconfigure_discovery<C>(cx: &mut C)
where
  C: BorrowAppContext + BorrowMut<App>,
{
  cx.update_default_global::<CollabManager, _>(|manager, cx| manager.reconfigure_discovery(cx));
}

pub fn request_discovered_ticket<T>(
  advertisement: flowstate_collab::discovery::DiscoveryAdvertisement,
  cx: &mut Context<T>,
) -> Receiver<Result<SessionTicket>>
where
  T: 'static,
{
  cx.update_default_global::<CollabManager, _>(|manager, cx| manager.request_discovered_ticket(advertisement, cx))
}

/// Notify the collaboration session for `panel_id` that the document's runtime
/// was just checkpointed (saved) outside the session.
///
/// A save records a named revision into Loro via the runtime's save hook, which
/// advances the canonical version vector but cannot route the resulting
/// `LocalUpdate` events back to the session (the save hook future has no GPUI
/// context). This refreshes the session's cached runtime version vector and
/// re-publishes an anti-entropy digest so peers converge on the new revision op
/// promptly. Returns `false` (a harmless no-op) when the panel is not part of an
/// active collaboration session, so solo-document saves are unaffected.
pub fn refresh_after_external_checkpoint<T>(panel_id: Uuid, cx: &mut Context<T>) -> bool
where
  T: 'static,
{
  let Some(session) = session_for_panel(panel_id, cx) else {
    return false;
  };
  session.update(cx, |session, cx| session.refresh_after_external_checkpoint(cx));
  true
}
