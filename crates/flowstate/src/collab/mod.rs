mod manager;
pub(crate) mod notify;
mod presence_view;
mod pump;
mod session;
pub mod share_dialog;
mod shutdown;
pub mod status;

use anyhow::Result;
use async_channel::Receiver;
use flowstate_collab::{SessionId, crdt_runtime_actor::CrdtRuntimeHandle, ticket::SessionTicket};
use gpui::{App, BorrowAppContext, Context, Entity, ReadGlobal};
use uuid::Uuid;

use crate::rich_text_element::RichTextEditor;

pub use manager::{CollabManager, JoinRequest};
pub use session::{
  Attachment, CollabSession, Connectivity, DetachReason, JoinStage, JoinedDocument, SessionNotice, SessionPhase, SessionRosterEntry,
};
pub use shutdown::shutdown;

pub fn init<C>(cx: &mut C)
where
  C: BorrowAppContext,
{
  CollabManager::init(cx);
}

pub fn start_session_for_panel<T>(
  panel_id: Uuid,
  editor: Entity<RichTextEditor>,
  title: String,
  runtime: CrdtRuntimeHandle,
  cx: &mut Context<T>,
) -> Result<SessionId>
where
  T: 'static,
{
  cx.update_default_global::<CollabManager, _>(|manager, cx| {
    manager.start_session_for_panel(panel_id, editor, title, runtime, cx)
  })
}

pub fn runtime_for_session(session_id: SessionId, cx: &App) -> Option<CrdtRuntimeHandle> {
  CollabManager::global(cx)
    .session_for_id(session_id)
    .and_then(|session| session.read(cx).runtime_handle())
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
