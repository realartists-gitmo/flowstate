use std::collections::HashMap;

use anyhow::{Context as _, Result, anyhow, ensure};
use async_channel::Receiver;
use flowstate_collab::{
  SessionId,
  ids::PeerId,
  net::{
    NetCommand, TicketSeed,
    runtime::{self, CommandSender},
  },
  ticket::SessionTicket,
};
use gpui::{App, AppContext, BorrowAppContext, Context, Entity, Global};
use uuid::Uuid;

use crate::rich_text_element::RichTextEditor;

use super::session::{CollabSession, DetachReason, JoinedDocument, SessionPhase, SessionRosterEntry};

pub struct JoinRequest {
  pub session: SessionId,
  pub completed: Receiver<Result<JoinedDocument>>,
}

#[derive(Default)]
pub struct CollabManager {
  runtime: Option<CollabRuntime>,
  pub(super) sessions_by_id: HashMap<SessionId, Entity<CollabSession>>,
  session_by_panel: HashMap<Uuid, SessionId>,
  own_endpoint_id: Option<PeerId>,
  pub(super) endpoint_online: bool,
  pub(super) event_pump_started: bool,
}

impl Global for CollabManager {}

#[derive(Clone)]
struct CollabRuntime {
  commands: CommandSender,
}

impl CollabManager {
  pub fn init<C>(cx: &mut C)
  where
    C: BorrowAppContext,
  {
    cx.update_default_global::<Self, _>(|_, _| {});
  }

  pub(super) fn shutdown_runtime(&mut self) {
    tracing::info!(open_sessions = self.sessions_by_id.len(), runtime_running = self.runtime.is_some(), "shutting down collaboration runtime");
    if let Some(runtime) = self.runtime.take()
      && let Err(error) = runtime.commands.try_send(NetCommand::Shutdown)
    {
      tracing::warn!(error = %error, "queueing collaboration runtime shutdown failed");
    }
    self.sessions_by_id.clear();
    self.session_by_panel.clear();
    self.own_endpoint_id = None;
    self.event_pump_started = false;
    self.endpoint_online = false;
    tracing::debug!("collaboration runtime shutdown state cleared");
  }

  pub fn start_session_for_panel<T>(
    &mut self,
    panel_id: Uuid,
    editor: Entity<RichTextEditor>,
    title: String,
    cx: &mut Context<T>,
  ) -> Result<SessionId>
  where
    T: 'static,
  {
    if let Some(session) = self.session_by_panel.get(&panel_id).copied() {
      tracing::debug!(%panel_id, %session, "collaboration session already exists for panel");
      return Ok(session);
    }

    let commands = self.ensure_runtime(cx)?;
    let session = SessionId::new();
    let document = editor.read(cx).document().clone();
    tracing::info!(
      %panel_id,
      %session,
      title = %title,
      paragraphs = document.paragraphs.len(),
      blocks = document.blocks.len(),
      assets = document.assets.assets.len(),
      "starting local collaboration session",
    );
    let collab = CollabSession::from_local_document(session, panel_id, editor, title, document, commands.clone())?;
    let direct_handler = collab.direct_handler();
    let entity = cx.new(|_| collab);
    entity.update(cx, |session, cx| session.attach(cx));

    self.register_session(entity.clone(), cx);
    if let Err(error) = commands.try_send(NetCommand::RegisterDirectHandler {
      session,
      handler: direct_handler,
    }) {
      tracing::error!(%panel_id, %session, error = %error, "registering collaboration direct handler failed");
      Self::detach_entity(&entity, DetachReason::Fatal(format!("registering collaboration direct handler failed: {error}")), cx);
      self.unregister_session(session);
      return Err(error).context("registering collaboration direct handler failed");
    }
    tracing::debug!(%panel_id, %session, "registered collaboration direct handler for local session");

    let (reply_tx, reply_rx) = async_channel::bounded(1);
    if let Err(error) = commands.try_send(NetCommand::CreateSession { session, reply: reply_tx }) {
      tracing::error!(%panel_id, %session, error = %error, "queueing collaboration create-session command failed");
      Self::detach_entity(&entity, DetachReason::Fatal(format!("creating collaboration network session failed: {error}")), cx);
      self.unregister_session(session);
      return Err(error).context("creating collaboration network session failed");
    }
    tracing::debug!(%panel_id, %session, "queued collaboration create-session command");
    Self::finish_create_session(entity, reply_rx, cx);
    Ok(session)
  }

  pub fn join_session<T>(&mut self, ticket: SessionTicket, cx: &mut Context<T>) -> Result<JoinRequest>
  where
    T: 'static,
  {
    tracing::info!(session = %ticket.session, inviter = %ticket.inviter.id, title = %ticket.title, "joining collaboration session from ticket");
    if !ticket.is_supported_version() {
      tracing::warn!(session = %ticket.session, "unsupported collaboration protocol version in ticket");
    }
    ensure!(ticket.is_supported_version(), "unsupported collaboration protocol version");
    let commands = self.ensure_runtime(cx)?;
    commands
      .try_send(NetCommand::EnsureUp)
      .context("starting collaboration networking failed")?;
    ensure!(
      self.own_endpoint_id != Some(ticket.inviter.id),
      "That's your own invite — open the Share dialog instead."
    );
    if self.sessions_by_id.contains_key(&ticket.session) {
      tracing::warn!(session = %ticket.session, "collaboration session is already open");
    }
    ensure!(
      !self.sessions_by_id.contains_key(&ticket.session),
      "collaboration session is already open"
    );

    let session = ticket.session;
    let inviter = ticket.inviter.id;
    let collab = CollabSession::joining(session, ticket.title.clone(), commands.clone(), vec![ticket.inviter.clone()]);
    let entity = cx.new(|_| collab);
    self.register_session(entity.clone(), cx);
    let neighbor_rx = entity.update(cx, |session, cx| session.prepare_join_neighbor_wait(cx));

    if let Err(error) = commands.try_send(NetCommand::JoinSession {
      session,
      bootstrap: vec![ticket.inviter],
    }) {
      tracing::error!(%session, error = %error, "queueing collaboration join-session command failed");
      entity.update(cx, |session, cx| {
        session.detach(
          DetachReason::JoinFailed(format!("joining collaboration network session failed: {error}")),
          cx,
        );
      });
      self.unregister_session(session);
      return Err(error).context("joining collaboration network session failed");
    }
    tracing::debug!(%session, inviter = %inviter, "queued collaboration join-session command");
    let completed = entity.update(cx, |session, cx| session.begin_join_bootstrap(inviter, neighbor_rx, cx));
    Ok(JoinRequest { session, completed })
  }

  pub fn attach_joined_session<T>(
    &mut self,
    session_id: SessionId,
    panel_id: Uuid,
    editor: Entity<RichTextEditor>,
    cx: &mut Context<T>,
  ) -> Result<()>
  where
    T: 'static,
  {
    tracing::info!(%session_id, %panel_id, "attaching joined collaboration session to editor");
    let commands = self.ensure_runtime(cx)?;
    let session = self
      .sessions_by_id
      .get(&session_id)
      .cloned()
      .ok_or_else(|| anyhow!("collaboration session is not registered"))?;
    let direct_handler = session.read(cx).direct_handler();
    if let Err(error) = commands.try_send(NetCommand::RegisterDirectHandler {
      session: session_id,
      handler: direct_handler,
    }) {
      tracing::error!(%session_id, %panel_id, error = %error, "registering collaboration direct handler for joined session failed");
      Self::detach_entity(&session, DetachReason::Fatal(format!("registering collaboration direct handler failed: {error}")), cx);
      self.unregister_session(session_id);
      return Err(error).context("registering collaboration direct handler failed");
    }
    tracing::debug!(%session_id, %panel_id, "registered collaboration direct handler for joined session");
    if let Err(error) = Self::establish_joined_peer(session.clone(), commands, cx) {
      Self::detach_entity(&session, DetachReason::Fatal(format!("collaboration endpoint address unavailable: {error:#}")), cx);
      self.unregister_session(session_id);
      return Err(error);
    }
    if let Err(error) = session.update(cx, |session, cx| session.attach_joined_editor(panel_id, editor, cx)) {
      Self::detach_entity(&session, DetachReason::Fatal(format!("attaching joined collaboration editor failed: {error:#}")), cx);
      self.unregister_session(session_id);
      return Err(error);
    }
    self.session_by_panel.insert(panel_id, session_id);
    Ok(())
  }

  pub fn leave_session_for_panel<T>(&mut self, panel_id: Uuid, cx: &mut Context<T>) -> bool
  where
    T: 'static,
  {
    let Some(session_id) = self.session_by_panel.get(&panel_id).copied() else {
      tracing::debug!(%panel_id, "leave requested for panel without collaboration session");
      return false;
    };
    let Some(session) = self.sessions_by_id.get(&session_id).cloned() else {
      tracing::warn!(%panel_id, %session_id, "leave requested for missing registered collaboration session");
      self.unregister_session(session_id);
      return false;
    };

    tracing::info!(%panel_id, %session_id, "leaving collaboration session for panel");
    let _ = session.update(cx, |session, cx| session.detach(DetachReason::UserLeft, cx));
    self.unregister_session(session_id);
    true
  }

  pub fn request_ticket_for_panel<T>(&mut self, panel_id: Uuid, cx: &mut Context<T>) -> Option<Receiver<Result<SessionTicket>>>
  where
    T: 'static,
  {
    let session_id = self.session_by_panel.get(&panel_id).copied()?;
    let session = self.sessions_by_id.get(&session_id)?.clone();
    let title = session.read(cx).title().to_string();
    let commands = self.ensure_runtime(cx).ok()?;
    let (ticket_tx, ticket_rx) = async_channel::bounded(1);
    let (reply_tx, reply_rx) = async_channel::bounded(1);
    if let Err(error) = commands.try_send(NetCommand::MintTicketAddr { reply: reply_tx }) {
      tracing::error!(%panel_id, %session_id, error = %error, "queueing collaboration ticket address command failed");
      return None;
    }
    tracing::debug!(%panel_id, %session_id, "requested collaboration ticket address");

    cx.spawn(async move |_, cx| {
      let ticket = match reply_rx.recv().await {
        Ok(addr) => {
          tracing::info!(%session_id, inviter = %addr.id, "created collaboration share ticket");
          let own_endpoint_id = addr.id;
          let _ = cx.update_global::<CollabManager, _>(|manager, _| manager.own_endpoint_id = Some(own_endpoint_id));
          Ok(SessionTicket::new(session_id, addr, title))
        },
        Err(error) => {
          tracing::error!(%session_id, error = %error, "collaboration endpoint address unavailable for ticket");
          Err(anyhow!("collaboration endpoint address unavailable: {error}"))
        },
      };
      let _ = ticket_tx.send(ticket).await;
    })
    .detach();
    Some(ticket_rx)
  }

  pub fn session_for_panel(&self, panel_id: Uuid) -> Option<Entity<CollabSession>> {
    self
      .session_by_panel
      .get(&panel_id)
      .and_then(|session| self.sessions_by_id.get(session))
      .cloned()
  }

  pub fn phase_for_panel(&self, panel_id: Uuid, cx: &App) -> Option<SessionPhase> {
    self
      .session_for_panel(panel_id)
      .map(|session| session.read(cx).phase().clone())
  }

  pub fn phase_for_session(&self, session_id: SessionId, cx: &App) -> Option<SessionPhase> {
    self
      .sessions_by_id
      .get(&session_id)
      .map(|session| session.read(cx).phase().clone())
  }

  pub fn session_for_id(&self, session_id: SessionId) -> Option<Entity<CollabSession>> {
    self.sessions_by_id.get(&session_id).cloned()
  }

  pub fn roster_for_panel(&self, panel_id: Uuid, cx: &App) -> Vec<SessionRosterEntry> {
    self
      .session_for_panel(panel_id)
      .map_or_else(Vec::new, |session| session.read(cx).roster())
  }

  fn register_session<T>(&mut self, session: Entity<CollabSession>, cx: &mut Context<T>) {
    let id = session.read(cx).session_id();
    if let Some(panel_id) = session.read(cx).panel_id() {
      self.session_by_panel.insert(panel_id, id);
      tracing::debug!(%id, %panel_id, "registered collaboration session panel mapping");
    }
    self.sessions_by_id.insert(id, session);
    tracing::debug!(%id, open_sessions = self.sessions_by_id.len(), "registered collaboration session");
  }

  pub(super) fn unregister_session(&mut self, session: SessionId) {
    let removed = self.sessions_by_id.remove(&session).is_some();
    self.session_by_panel.retain(|_, active| *active != session);
    tracing::debug!(%session, removed, open_sessions = self.sessions_by_id.len(), "unregistered collaboration session");
  }

  fn ensure_runtime<T>(&mut self, cx: &mut Context<T>) -> Result<CommandSender>
  where
    T: 'static,
  {
    if let Some(runtime) = &self.runtime {
      tracing::trace!("collaboration runtime already available");
      return Ok(runtime.commands.clone());
    }

    tracing::info!("starting collaboration runtime for manager");
    let (commands, events) = runtime::start()?;
    self.runtime = Some(CollabRuntime { commands: commands.clone() });
    self.start_event_pump(events, cx);
    Ok(commands)
  }

  fn finish_create_session<T>(session: Entity<CollabSession>, reply_rx: Receiver<Result<TicketSeed>>, cx: &mut Context<T>)
  where
    T: 'static,
  {
    let session_id = session.read(cx).session_id();
    cx.spawn(async move |_, cx| match reply_rx.recv().await {
      Ok(Ok(seed)) => {
        tracing::info!(%session_id, inviter = %seed.inviter.id, "collaboration create-session completed");
        let own_endpoint_id = seed.inviter.id;
        let _ = cx.update_global::<CollabManager, _>(|manager, _| manager.own_endpoint_id = Some(own_endpoint_id));
        let _ = session.update(cx, |session, cx| session.establish_local_peer(&seed.inviter.id, cx));
      },
      Ok(Err(error)) => {
        tracing::error!(%session_id, error = %format_args!("{error:#}"), "collaboration create-session failed");
        let _ = session.update(cx, |session, cx| {
          session.detach(DetachReason::Fatal(format!("creating collaboration session failed: {error:#}")), cx);
        });
      },
      Err(error) => {
        tracing::error!(%session_id, error = %error, "collaboration create-session reply channel closed");
        let _ = session.update(cx, |session, cx| {
          session.detach(DetachReason::Fatal(format!("creating collaboration session failed: {error}")), cx);
        });
      },
    })
    .detach();
  }

  fn establish_joined_peer<T>(session: Entity<CollabSession>, commands: CommandSender, cx: &mut Context<T>) -> Result<()>
  where
    T: 'static,
  {
    let session_id = session.read(cx).session_id();
    let (reply_tx, reply_rx) = async_channel::bounded(1);
    if let Err(error) = commands.try_send(NetCommand::MintTicketAddr { reply: reply_tx }) {
      tracing::error!(%session_id, error = %error, "queueing collaboration endpoint address command failed");
      return Err(error).context("requesting collaboration endpoint address failed");
    }
    tracing::debug!(%session_id, "requested collaboration endpoint address for joined session");
    cx.spawn(async move |_, cx| match reply_rx.recv().await {
      Ok(addr) => {
        tracing::info!(%session_id, peer = %addr.id, "collaboration joined session local peer established");
        let own_endpoint_id = addr.id;
        let _ = cx.update_global::<CollabManager, _>(|manager, _| manager.own_endpoint_id = Some(own_endpoint_id));
        let _ = session.update(cx, |session, cx| session.establish_local_peer(&addr.id, cx));
      },
      Err(error) => {
        tracing::error!(%session_id, error = %error, "collaboration endpoint address unavailable for joined session");
        let _ = session.update(cx, |session, cx| {
          session.detach(DetachReason::Fatal(format!("collaboration endpoint address unavailable: {error}")), cx);
        });
      },
    })
    .detach();
    Ok(())
  }

  fn detach_entity<T>(entity: &Entity<CollabSession>, reason: DetachReason, cx: &mut Context<T>)
  where
    T: 'static,
  {
    entity.update(cx, |session, cx| {
      session.detach(reason, cx);
    });
  }
}
