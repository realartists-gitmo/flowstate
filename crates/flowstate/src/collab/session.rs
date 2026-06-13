use std::{collections::HashSet, rc::Rc, time::Instant};

use anyhow::{Context as _, Result, anyhow, bail};
use flowstate_collab::{
  SessionId,
  binding::DocBinding,
  local_apply::LocalApplier,
  net::{
    NetCommand, PeerAddr, PublishPayload,
    anti_entropy::AntiEntropyState,
    direct::{DirectServeRequest, DirectSessionHandler},
    runtime::CommandSender,
  },
  presence::PresenceStore,
  projection,
  proto_gossip::GossipMsg,
  schema,
};
use gpui::{Context, Entity, Subscription};
use loro::{LoroDoc, Subscription as LoroSubscription, UndoManager};
use uuid::Uuid;

use crate::app_settings::load_document_theme;
use crate::rich_text_element::{AssetId, CollabPatch, Document, EditorEvent, RichTextEditor, UndoRedirect};

use super::presence_view;

#[path = "asset_transfer.rs"]
mod asset_transfer;
#[path = "session_io.rs"]
mod session_io;
#[path = "session_presence.rs"]
mod session_presence;
#[path = "session_timers.rs"]
mod session_timers;

#[derive(Clone, Debug)]
pub enum SessionPhase {
  Creating,
  Joining(JoinStage),
  Attached(Attachment),
  Detached(DetachReason),
}

#[derive(Clone, Debug)]
pub struct Attachment {
  pub connectivity: Connectivity,
  pub peers_present: usize,
}

#[derive(Clone, Debug)]
pub enum Connectivity {
  Online,
  Offline { since: Instant, retries: u32 },
}

#[derive(Clone, Debug)]
pub enum JoinStage {
  Resolving,
  Subscribing,
  FetchingSnapshot { got: u64, total: Option<u64> },
  Building,
}

#[derive(Clone, Debug)]
pub enum DetachReason {
  UserLeft,
  JoinFailed(String),
  Fatal(String),
}

pub struct JoinedDocument {
  pub session: SessionId,
  pub title: String,
  pub document: Document,
}

#[derive(Clone, Debug)]
pub struct SessionRosterEntry {
  pub name: String,
  pub color_rgb: u32,
  pub is_self: bool,
}

pub struct CollabSession {
  session: SessionId,
  title: String,
  phase: SessionPhase,
  doc: Option<LoroDoc>,
  binding: Option<DocBinding>,
  editor: Option<Entity<RichTextEditor>>,
  panel_id: Option<Uuid>,
  pending_remote_patches: Vec<CollabPatch>,
  pending_remote_updates: Vec<Vec<u8>>,
  presence: Option<PresenceStore>,
  net_tx: CommandSender,
  direct_tx: async_channel::Sender<DirectServeRequest>,
  direct_rx: async_channel::Receiver<DirectServeRequest>,
  undo_tx: async_channel::Sender<UndoRedirect>,
  undo_rx: async_channel::Receiver<UndoRedirect>,
  editor_subscriptions: Vec<Subscription>,
  loro_subscriptions: Vec<LoroSubscription>,
  neighbors: HashSet<flowstate_collab::ids::PeerId>,
  bootstrap_addrs: Vec<PeerAddr>,
  asset_pulls_in_flight: HashSet<AssetId>,
  anti_entropy: AntiEntropyState,
  undo_manager: Option<UndoManager>,
  direct_pump_started: bool,
  undo_pump_started: bool,
  local_update_publish_attached: bool,
  timers_started: bool,
  endpoint_online: bool,
  zero_neighbors_since: Option<Instant>,
  inbound_since_last_digest: bool,
  quiet_digest_rounds: u8,
  next_recovery_at: Option<Instant>,
  last_document_activity: Instant,
}

impl CollabSession {
  pub fn from_local_document(
    session: SessionId,
    panel_id: Uuid,
    editor: Entity<RichTextEditor>,
    title: String,
    document: Document,
    net_tx: CommandSender,
  ) -> Result<Self> {
    let doc = schema::new_configured_doc();
    projection::populate_from_document(&doc, session, &title, &document)?;
    let binding = DocBinding::build(&doc, &document)?;
    let (direct_tx, direct_rx) = async_channel::unbounded();
    let (undo_tx, undo_rx) = async_channel::unbounded();
    let now = Instant::now();

    Ok(Self {
      session,
      title,
      phase: SessionPhase::Creating,
      doc: Some(doc),
      binding: Some(binding),
      editor: Some(editor),
      panel_id: Some(panel_id),
      pending_remote_patches: Vec::new(),
      pending_remote_updates: Vec::new(),
      presence: None,
      net_tx,
      direct_tx,
      direct_rx,
      undo_tx,
      undo_rx,
      editor_subscriptions: Vec::new(),
      loro_subscriptions: Vec::new(),
      neighbors: HashSet::new(),
      bootstrap_addrs: Vec::new(),
      asset_pulls_in_flight: HashSet::new(),
      anti_entropy: AntiEntropyState::new(session, now),
      undo_manager: None,
      direct_pump_started: false,
      undo_pump_started: false,
      local_update_publish_attached: false,
      timers_started: false,
      endpoint_online: true,
      zero_neighbors_since: Some(now),
      inbound_since_last_digest: false,
      quiet_digest_rounds: 0,
      next_recovery_at: None,
      last_document_activity: now,
    })
  }

  pub fn joining(session: SessionId, title: String, net_tx: CommandSender, bootstrap_addrs: Vec<PeerAddr>) -> Self {
    let (direct_tx, direct_rx) = async_channel::unbounded();
    let (undo_tx, undo_rx) = async_channel::unbounded();
    let now = Instant::now();
    Self {
      session,
      title,
      phase: SessionPhase::Joining(JoinStage::Resolving),
      doc: None,
      binding: None,
      editor: None,
      panel_id: None,
      pending_remote_patches: Vec::new(),
      pending_remote_updates: Vec::new(),
      presence: None,
      net_tx,
      direct_tx,
      direct_rx,
      undo_tx,
      undo_rx,
      editor_subscriptions: Vec::new(),
      loro_subscriptions: Vec::new(),
      neighbors: HashSet::new(),
      bootstrap_addrs,
      asset_pulls_in_flight: HashSet::new(),
      anti_entropy: AntiEntropyState::new(session, now),
      undo_manager: None,
      direct_pump_started: false,
      undo_pump_started: false,
      local_update_publish_attached: false,
      timers_started: false,
      endpoint_online: true,
      zero_neighbors_since: Some(now),
      inbound_since_last_digest: false,
      quiet_digest_rounds: 0,
      next_recovery_at: None,
      last_document_activity: now,
    }
  }

  pub fn session_id(&self) -> SessionId {
    self.session
  }

  pub fn panel_id(&self) -> Option<Uuid> {
    self.panel_id
  }

  pub fn title(&self) -> &str {
    &self.title
  }

  pub fn phase(&self) -> &SessionPhase {
    &self.phase
  }

  pub fn roster(&self) -> Vec<SessionRosterEntry> {
    let Some(presence) = &self.presence else {
      return Vec::new();
    };
    let self_key = presence.self_key().to_string();
    presence
      .roster()
      .into_iter()
      .map(|entry| SessionRosterEntry {
        is_self: entry.key == self_key,
        name: entry.name,
        color_rgb: entry.color_rgb,
      })
      .collect()
  }

  pub fn direct_handler(&self) -> DirectSessionHandler {
    DirectSessionHandler::new(self.direct_tx.clone())
  }

  pub(super) fn pull_candidates(&self, preferred: Option<flowstate_collab::ids::PeerId>) -> Vec<flowstate_collab::ids::PeerId> {
    let mut candidates = Vec::with_capacity(self.neighbors.len() + usize::from(preferred.is_some()));
    if let Some(peer) = preferred {
      candidates.push(peer);
    }
    candidates.extend(
      self
        .neighbors
        .iter()
        .copied()
        .filter(|peer| Some(*peer) != preferred),
    );
    candidates
  }

  pub fn set_join_stage(&mut self, stage: JoinStage, cx: &mut Context<Self>) {
    self.phase = SessionPhase::Joining(stage);
    cx.notify();
  }

  pub fn begin_join_bootstrap(
    &mut self,
    inviter: flowstate_collab::ids::PeerId,
    cx: &mut Context<Self>,
  ) -> async_channel::Receiver<Result<JoinedDocument>> {
    let (result_tx, result_rx) = async_channel::bounded(1);
    self.phase = SessionPhase::Joining(JoinStage::FetchingSnapshot { got: 0, total: None });
    cx.notify();

    let (reply_tx, reply_rx) = async_channel::bounded(1);
    if let Err(error) = self.net_tx.try_send(NetCommand::PullSnapshot {
      session: self.session,
      candidates: self.pull_candidates(Some(inviter)),
      reply: reply_tx,
    }) {
      let detail = format!("requesting collaboration snapshot failed: {error}");
      self.detach(DetachReason::JoinFailed(detail.clone()), cx);
      let _ = result_tx.try_send(Err(anyhow!(detail)));
      return result_rx;
    }

    cx.spawn(async move |session, cx| {
      let joined = match reply_rx.recv().await {
        Ok(Ok(bytes)) => session
          .update(cx, |session, cx| match session.finish_join_snapshot(&bytes, cx) {
            Ok(joined) => Ok(joined),
            Err(error) => {
              let detail = format!("building collaboration document failed: {error:#}");
              session.detach(DetachReason::JoinFailed(detail), cx);
              Err(error.context("building collaboration document failed"))
            },
          })
          .unwrap_or_else(|error| Err(anyhow!("collaboration join session disappeared: {error}"))),
        Ok(Err(error)) => {
          let detail = format!("pulling collaboration snapshot failed: {error:#}");
          let _ = session.update(cx, |session, cx| {
            session.detach(DetachReason::JoinFailed(detail.clone()), cx);
          });
          Err(anyhow!(detail))
        },
        Err(error) => {
          let detail = format!("collaboration snapshot reply channel closed: {error}");
          let _ = session.update(cx, |session, cx| {
            session.detach(DetachReason::JoinFailed(detail.clone()), cx);
          });
          Err(anyhow!(detail))
        },
      };
      let _ = result_tx.send(joined).await;
    })
    .detach();

    result_rx
  }

  pub fn attach_joined_editor(&mut self, panel_id: Uuid, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) -> Result<()> {
    if self.doc.is_none() || self.binding.is_none() {
      bail!("collaboration snapshot has not finished loading");
    }

    self.panel_id = Some(panel_id);
    self.editor = Some(editor);
    self.attach(cx);

    let pending_updates = std::mem::take(&mut self.pending_remote_updates);
    for update in pending_updates {
      self.import_update_bytes(&update, cx)?;
    }
    self.flush_pending_remote_patches(cx);
    asset_transfer::schedule_missing_assets(self, None, cx);
    self.publish_digest();
    cx.notify();
    Ok(())
  }

  pub fn attach(&mut self, cx: &mut Context<Self>) {
    self.attach_undo_manager();
    self.attach_editor_hooks(cx);
    self.attach_loro_publish_hook();
    self.attach_direct_request_pump(cx);
    self.attach_undo_request_pump(cx);
    self.attach_timers(cx);
  }

  pub fn establish_local_peer(&mut self, peer: &flowstate_collab::ids::PeerId, cx: &mut Context<Self>) {
    if self.presence.is_none() {
      let presence = PresenceStore::new(peer);
      let session = self.session;
      let net_tx = self.net_tx.clone();
      self
        .loro_subscriptions
        .push(presence.subscribe_local_updates(move |bytes| {
          let _ = net_tx.try_send(NetCommand::Publish {
            session,
            payload: PublishPayload::Presence(bytes.clone()),
          });
          true
        }));
      self.presence = Some(presence);
    }
    self.refresh_own_presence(cx);
    self.endpoint_online = true;
    self.phase = SessionPhase::Attached(Attachment {
      connectivity: Connectivity::Online,
      peers_present: self.peers_present(),
    });
    self.publish_presence_snapshot();
    self.publish_digest();
    asset_transfer::schedule_missing_assets(self, None, cx);
    cx.notify();
  }

  fn finish_join_snapshot(&mut self, snapshot: &[u8], cx: &mut Context<Self>) -> Result<JoinedDocument> {
    self.phase = SessionPhase::Joining(JoinStage::Building);
    cx.notify();

    let doc = schema::new_configured_doc();
    doc
      .import_with(snapshot, "remote")
      .context("importing collaboration snapshot failed")?;
    projection::verify_lineage(&doc, self.session)?;
    let document = projection::document_from_loro(&doc, load_document_theme())?;
    let binding = DocBinding::build(&doc, &document)?;

    self.doc = Some(doc);
    self.binding = Some(binding);
    Ok(JoinedDocument {
      session: self.session,
      title: self.title.clone(),
      document,
    })
  }

  pub fn detach(&mut self, reason: DetachReason, cx: &mut Context<Self>) -> bool {
    if matches!(self.phase, SessionPhase::Detached(_)) {
      return false;
    }

    if let Some(presence) = &self.presence {
      presence.delete_self();
      self.publish_presence_bytes(presence.encode_self());
    }
    let _ = self
      .net_tx
      .try_send(NetCommand::LeaveSession { session: self.session });
    self.flush_pending_remote_patches(cx);

    if let Some(editor) = self.editor.clone() {
      editor.update(cx, |editor, cx| {
        editor.set_collab_undo_redirect(None);
        editor.set_collab_capture(false);
        editor.clear_collab_history();
        let _ = editor.take_pending_collab_edits();
        editor.set_external_carets(Vec::new(), cx);
      });
    }

    self.editor_subscriptions.clear();
    self.loro_subscriptions.clear();
    self.undo_manager = None;
    self.presence = None;
    self.binding = None;
    self.doc = None;
    self.pending_remote_patches.clear();
    self.pending_remote_updates.clear();
    self.neighbors.clear();
    self.asset_pulls_in_flight.clear();
    self.zero_neighbors_since = Some(Instant::now());
    self.inbound_since_last_digest = false;
    self.quiet_digest_rounds = 0;
    self.next_recovery_at = None;
    self.local_update_publish_attached = false;
    self.phase = SessionPhase::Detached(reason);
    cx.notify();
    true
  }

  pub fn flush_local_edits(&mut self, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) -> Result<()> {
    if matches!(self.phase, SessionPhase::Detached(_) | SessionPhase::Joining(_)) {
      return Ok(());
    }

    let document = editor.read(cx).document().clone();
    let edits = editor.update(cx, |editor, _| {
      let edits = editor.take_pending_collab_edits();
      editor.clear_collab_history();
      edits
    });
    let Some(doc) = &self.doc else {
      return Ok(());
    };
    let Some(binding) = &mut self.binding else {
      return Ok(());
    };

    let mut applied = false;
    for edit in edits {
      if edit.operations.is_empty() {
        continue;
      }
      LocalApplier { doc, binding }.apply(&document, &edit.operations)?;
      applied = true;
    }
    if applied {
      self.last_document_activity = Instant::now();
    }
    Ok(())
  }

  pub fn handle_gossip(&mut self, from: flowstate_collab::ids::PeerId, msg: GossipMsg, cx: &mut Context<Self>) {
    self.note_inbound_traffic(cx);
    let result = match msg {
      GossipMsg::Update(bytes) => self
        .import_update_bytes(&bytes, cx)
        .map(|()| asset_transfer::schedule_missing_assets(self, Some(from), cx)),
      GossipMsg::UpdateAvailable { blob, .. } => {
        self.pull_blob(from, blob, cx);
        Ok(())
      },
      GossipMsg::Presence(bytes) => {
        self.apply_presence(&bytes, cx);
        Ok(())
      },
      GossipMsg::Digest { session, vv } => self.handle_digest(from, session, &vv, cx),
    };
    if let Err(error) = result {
      self.detach(DetachReason::Fatal(format!("collaboration update failed: {error:#}")), cx);
    }
  }

  pub fn neighbor_up(&mut self, peer: flowstate_collab::ids::PeerId, cx: &mut Context<Self>) {
    self.neighbors.insert(peer);
    self.zero_neighbors_since = None;
    self.anti_entropy.on_neighbor_up();
    self.mark_online(cx);
    self.publish_digest();
    asset_transfer::schedule_missing_assets(self, Some(peer), cx);
    cx.notify();
  }

  pub fn neighbor_down(&mut self, peer: flowstate_collab::ids::PeerId, cx: &mut Context<Self>) {
    self.neighbors.remove(&peer);
    if self.neighbors.is_empty() && self.zero_neighbors_since.is_none() {
      self.zero_neighbors_since = Some(Instant::now());
    }
    self.evaluate_connectivity(cx);
    cx.notify();
  }

  pub fn handle_gossip_lagged(&mut self, cx: &mut Context<Self>) {
    self.publish_digest();
    let peer = self.neighbors.iter().next().copied();
    let vv = self
      .doc
      .as_ref()
      .map(|doc| doc.oplog_vv().encode())
      .unwrap_or_default();
    let action = self.anti_entropy.on_lagged(peer, vv);
    self.handle_gap_action(action, cx);
  }

  pub fn set_endpoint_online(&mut self, online: bool, cx: &mut Context<Self>) {
    self.endpoint_online = online;
    if online {
      if self.peers_present() == 0 || !self.neighbors.is_empty() {
        self.mark_online(cx);
      }
    } else {
      self.evaluate_connectivity(cx);
    }
    cx.notify();
  }

  fn attach_editor_hooks(&mut self, cx: &mut Context<Self>) {
    if !self.editor_subscriptions.is_empty() {
      return;
    }
    let Some(editor) = self.editor.clone() else {
      return;
    };

    editor.update(cx, |editor, cx| {
      if editor.document_path().is_none() {
        editor.set_recovery_path(Some(presence_view::collaboration_recovery_path(self.session, &self.title)), cx);
      }
      editor.clear_collab_history();
      editor.set_collab_capture(true);
      let undo_tx = self.undo_tx.clone();
      editor.set_collab_undo_redirect(Some(Rc::new(move |redirect: UndoRedirect| {
        let _ = undo_tx.try_send(redirect);
      })));
    });

    self
      .editor_subscriptions
      .push(cx.observe(&editor, |session, editor, cx| {
        if let Err(error) = session.flush_local_edits(editor.clone(), cx) {
          session.detach(DetachReason::Fatal(format!("capturing local collaboration edit failed: {error:#}")), cx);
          return;
        }
        session.flush_pending_remote_patches(cx);
      }));

    self
      .editor_subscriptions
      .push(cx.subscribe(&editor, |session, _, event: &EditorEvent, cx| {
        if matches!(event, EditorEvent::SelectionChanged { .. }) {
          session.refresh_own_presence(cx);
        }
      }));
  }

  fn attach_loro_publish_hook(&mut self) {
    if self.doc.is_none() || self.local_update_publish_attached {
      return;
    }
    let Some(doc) = &self.doc else {
      return;
    };
    let session = self.session;
    let net_tx = self.net_tx.clone();
    self
      .loro_subscriptions
      .push(doc.subscribe_local_update(Box::new(move |bytes| {
        let _ = net_tx.try_send(NetCommand::Publish {
          session,
          payload: PublishPayload::Update(bytes.clone()),
        });
        true
      })));
    self.local_update_publish_attached = true;
  }

  fn attach_undo_manager(&mut self) {
    if self.undo_manager.is_some() {
      return;
    }
    let Some(doc) = &self.doc else {
      return;
    };
    let mut undo_manager = UndoManager::new(doc);
    undo_manager.set_merge_interval(500);
    undo_manager.set_max_undo_steps(300);
    self.undo_manager = Some(undo_manager);
  }

  fn attach_undo_request_pump(&mut self, cx: &mut Context<Self>) {
    if self.undo_pump_started {
      return;
    }
    self.undo_pump_started = true;
    let requests = self.undo_rx.clone();
    cx.spawn(async move |session, cx| {
      while let Ok(redirect) = requests.recv().await {
        if session
          .update(cx, |session, cx| {
            if let Err(error) = session.apply_loro_undo_redirect(redirect, cx) {
              session.detach(DetachReason::Fatal(format!("collaboration undo failed: {error:#}")), cx);
            }
          })
          .is_err()
        {
          break;
        }
      }
    })
    .detach();
  }
}
