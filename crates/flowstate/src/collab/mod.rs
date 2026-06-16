mod presence_view;
mod session;
pub mod share_dialog;
mod shutdown;
pub mod status;

use std::collections::HashMap;

use anyhow::{Context as _, Result, anyhow, ensure};
use async_channel::Receiver;
use flowstate_collab::{
  SessionId,
  net::{
    NetCommand, NetEvent, TicketSeed,
    runtime::{self, CommandSender},
  },
  ticket::SessionTicket,
};
use gpui::{App, AppContext, BorrowAppContext, Context, Entity, Global, ReadGlobal};
use uuid::Uuid;

use crate::rich_text_element::RichTextEditor;

pub use session::{Attachment, CollabSession, Connectivity, DetachReason, JoinStage, JoinedDocument, SessionPhase, SessionRosterEntry};
pub use shutdown::shutdown;

pub struct JoinRequest {
  pub session: SessionId,
  pub completed: Receiver<Result<JoinedDocument>>,
}

#[derive(Default)]
pub struct CollabManager {
  runtime: Option<CollabRuntime>,
  sessions_by_id: HashMap<SessionId, Entity<CollabSession>>,
  session_by_panel: HashMap<Uuid, SessionId>,
  endpoint_online: bool,
  event_pump_started: bool,
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
      return Err(error).context("registering collaboration direct handler failed");
    }
    tracing::debug!(%panel_id, %session, "registered collaboration direct handler for local session");

    let (reply_tx, reply_rx) = async_channel::bounded(1);
    if let Err(error) = commands.try_send(NetCommand::CreateSession { session, reply: reply_tx }) {
      tracing::error!(%panel_id, %session, error = %error, "queueing collaboration create-session command failed");
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
    if self.sessions_by_id.contains_key(&ticket.session) {
      tracing::warn!(session = %ticket.session, "collaboration session is already open");
    }
    ensure!(
      !self.sessions_by_id.contains_key(&ticket.session),
      "collaboration session is already open"
    );

    let commands = self.ensure_runtime(cx)?;
    let session = ticket.session;
    let inviter = ticket.inviter.id;
    let collab = CollabSession::joining(session, ticket.title.clone(), commands.clone(), vec![ticket.inviter.clone()]);
    let entity = cx.new(|_| collab);
    self.register_session(entity.clone(), cx);

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
    let completed = entity.update(cx, |session, cx| {
      session.set_join_stage(JoinStage::Subscribing, cx);
      session.begin_join_bootstrap(inviter, cx)
    });
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
    session.update(cx, |session, cx| session.attach_joined_editor(panel_id, editor, cx))?;
    self.session_by_panel.insert(panel_id, session_id);
    if let Err(error) = commands.try_send(NetCommand::RegisterDirectHandler {
        session: session_id,
        handler: direct_handler,
      }) {
      tracing::error!(%session_id, %panel_id, error = %error, "registering collaboration direct handler for joined session failed");
      return Err(error).context("registering collaboration direct handler failed");
    }
    tracing::debug!(%session_id, %panel_id, "registered collaboration direct handler for joined session");
    Self::establish_joined_peer(session, commands, cx)?;
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

    cx.spawn(async move |_, _| {
      let ticket = match reply_rx.recv().await {
        Ok(addr) => {
          tracing::info!(%session_id, inviter = %addr.id, "created collaboration share ticket");
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

  fn unregister_session(&mut self, session: SessionId) {
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

  fn start_event_pump<T>(&mut self, events: runtime::EventReceiver, cx: &mut Context<T>)
  where
    T: 'static,
  {
    if self.event_pump_started {
      tracing::trace!("collaboration network event pump already started");
      return;
    }
    self.event_pump_started = true;
    tracing::debug!("starting collaboration network event pump");
    cx.spawn(async move |_, cx| {
      while let Ok(event) = events.recv().await {
        if cx
          .update_global::<CollabManager, _>(|manager, cx| manager.handle_net_event(event, cx))
          .is_err()
        {
          tracing::warn!("collaboration network event pump could not update manager; stopping");
          break;
        }
      }
      tracing::debug!("collaboration network event pump stopped");
    })
    .detach();
  }

  fn handle_net_event(&mut self, event: NetEvent, cx: &mut App) {
    match event {
      NetEvent::EndpointOnline(online) => {
        tracing::info!(online, "collaboration endpoint status changed");
        self.endpoint_online = online;
        for session in self.sessions_by_id.values() {
          session.update(cx, |session, cx| session.set_endpoint_online(online, cx));
        }
      },
      NetEvent::Gossip { session, from, msg } => {
        tracing::trace!(%session, from = %from, gossip_kind = msg.kind(), gossip_payload_bytes = msg.payload_len(), "routing collaboration gossip event");
        self.update_session(session, cx, |session, cx| session.handle_gossip(from, msg, cx));
      },
      NetEvent::NeighborUp { session, peer } => {
        tracing::debug!(%session, peer = %peer, "routing collaboration neighbor-up event");
        self.update_session(session, cx, |session, cx| session.neighbor_up(peer, cx));
      },
      NetEvent::NeighborDown { session, peer } => {
        tracing::debug!(%session, peer = %peer, "routing collaboration neighbor-down event");
        self.update_session(session, cx, |session, cx| session.neighbor_down(peer, cx));
      },
      NetEvent::GossipLagged { session } => {
        tracing::warn!(%session, "routing collaboration gossip-lagged event");
        self.update_session(session, cx, |session, cx| session.handle_gossip_lagged(cx));
      },
      NetEvent::SubscribeFailed { session, error } => {
        tracing::error!(%session, error = %error, "collaboration gossip subscription failed");
        let detached = self.update_session(session, cx, |session, cx| session.detach(DetachReason::Fatal(error), cx));
        if detached.unwrap_or(false) {
          self.unregister_session(session);
        }
      },
    }
  }

  fn update_session<R>(
    &mut self,
    session: SessionId,
    cx: &mut App,
    update: impl FnOnce(&mut CollabSession, &mut Context<CollabSession>) -> R,
  ) -> Option<R> {
    self
      .sessions_by_id
      .get(&session)
      .cloned()
      .map(|session| session.update(cx, update))
      .or_else(|| {
        tracing::warn!(%session, "collaboration event targeted missing session");
        None
      })
  }

  fn finish_create_session<T>(session: Entity<CollabSession>, reply_rx: Receiver<Result<TicketSeed>>, cx: &mut Context<T>)
  where
    T: 'static,
  {
    let session_id = session.read(cx).session_id();
    cx.spawn(async move |_, cx| match reply_rx.recv().await {
      Ok(Ok(seed)) => {
        tracing::info!(%session_id, inviter = %seed.inviter.id, "collaboration create-session completed");
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
}

pub fn init<C>(cx: &mut C)
where
  C: BorrowAppContext,
{
  CollabManager::init(cx);
}

pub fn start_session_for_panel<T>(panel_id: Uuid, editor: Entity<RichTextEditor>, title: String, cx: &mut Context<T>) -> Result<SessionId>
where
  T: 'static,
{
  cx.update_default_global::<CollabManager, _>(|manager, cx| manager.start_session_for_panel(panel_id, editor, title, cx))
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

pub fn roster_for_panel(panel_id: Uuid, cx: &App) -> Vec<SessionRosterEntry> {
  CollabManager::global(cx).roster_for_panel(panel_id, cx)
}
