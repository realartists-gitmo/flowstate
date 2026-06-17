use std::{collections::HashSet, rc::Rc, time::{Duration, Instant}};

use anyhow::{Context as _, Result, anyhow, bail};
use flowstate_collab::{
  SessionId,
  binding::DocBinding,
  ids::PeerId,
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
use gpui::{Context, Entity, EventEmitter, Subscription, Timer};
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

#[derive(Clone, Debug)]
pub enum SessionNotice {
  PeerJoined(String),
  PeerLeft(String),
  LeftSession,
  Disconnected(String),
  ViewRebuilt,
  IncompatibleVersion(String),
}

impl EventEmitter<SessionNotice> for CollabSession {}

pub(super) enum JoinNeighborSignal {
  NeighborUp,
  TimedOut,
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
  presence_refresh_pending: bool,
  local_update_publish_attached: bool,
  timers_started: bool,
  endpoint_online: bool,
  zero_neighbors_since: Option<Instant>,
  inbound_since_last_digest: bool,
  quiet_digest_rounds: u8,
  next_recovery_at: Option<Instant>,
  awaiting_recovery_neighbor_until: Option<Instant>,
  known_peers: HashSet<PeerId>,
  probe_pending: bool,
  last_probe_failed: bool,
  join_neighbor_tx: Option<async_channel::Sender<JoinNeighborSignal>>,
  incompatible_version_peers: HashSet<PeerId>,
  last_document_activity: Instant,
}

const DIRECT_REQUEST_CHANNEL_CAPACITY: usize = 32;
const UNDO_REQUEST_CHANNEL_CAPACITY: usize = 128;
const UNDO_MERGE_INTERVAL_MS: i64 = 500;
const UNDO_MAX_STEPS: usize = 300;
const JOIN_FIRST_NEIGHBOR_TIMEOUT: Duration = Duration::from_secs(15);
const JOIN_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);

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
    // Direct serving is network-bound and should backpressure rather than grow without limit.
    let (direct_tx, direct_rx) = async_channel::bounded(DIRECT_REQUEST_CHANNEL_CAPACITY);
    // Undo redirects can burst under key-repeat, but a stuck pump should not retain unbounded UI events.
    let (undo_tx, undo_rx) = async_channel::bounded(UNDO_REQUEST_CHANNEL_CAPACITY);
    let now = Instant::now();
    tracing::info!(
      %session,
      %panel_id,
      title = %title,
      paragraphs = document.paragraphs.len(),
      blocks = document.blocks.len(),
      assets = document.assets.assets.len(),
      "built local collaboration document projection",
    );

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
      presence_refresh_pending: false,
      local_update_publish_attached: false,
      timers_started: false,
      endpoint_online: true,
      zero_neighbors_since: Some(now),
      inbound_since_last_digest: false,
      quiet_digest_rounds: 0,
      next_recovery_at: None,
      awaiting_recovery_neighbor_until: None,
      known_peers: HashSet::new(),
      probe_pending: false,
      last_probe_failed: false,
      join_neighbor_tx: None,
      incompatible_version_peers: HashSet::new(),
      last_document_activity: now,
    })
  }

  pub fn joining(session: SessionId, title: String, net_tx: CommandSender, bootstrap_addrs: Vec<PeerAddr>) -> Self {
    let (direct_tx, direct_rx) = async_channel::bounded(DIRECT_REQUEST_CHANNEL_CAPACITY);
    let (undo_tx, undo_rx) = async_channel::bounded(UNDO_REQUEST_CHANNEL_CAPACITY);
    let now = Instant::now();
    tracing::info!(%session, title = %title, bootstrap_count = bootstrap_addrs.len(), "created joining collaboration session state");
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
      presence_refresh_pending: false,
      local_update_publish_attached: false,
      timers_started: false,
      endpoint_online: true,
      zero_neighbors_since: Some(now),
      inbound_since_last_digest: false,
      quiet_digest_rounds: 0,
      next_recovery_at: None,
      awaiting_recovery_neighbor_until: None,
      known_peers: HashSet::new(),
      probe_pending: false,
      last_probe_failed: false,
      join_neighbor_tx: None,
      incompatible_version_peers: HashSet::new(),
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
    tracing::debug!(session = %self.session, ?stage, "collaboration join stage changed");
    self.phase = SessionPhase::Joining(stage);
    cx.notify();
  }

  pub(super) fn prepare_join_neighbor_wait(&mut self, cx: &mut Context<Self>) -> async_channel::Receiver<JoinNeighborSignal> {
    let (neighbor_tx, neighbor_rx) = async_channel::bounded(1);
    self.join_neighbor_tx = Some(neighbor_tx.clone());
    self.phase = SessionPhase::Joining(JoinStage::Subscribing);
    cx.spawn(async move |_, _| {
      Timer::after(JOIN_FIRST_NEIGHBOR_TIMEOUT).await;
      let _ = neighbor_tx.try_send(JoinNeighborSignal::TimedOut);
    })
    .detach();
    cx.notify();
    neighbor_rx
  }

  pub(super) fn begin_join_bootstrap(
    &mut self,
    inviter: PeerId,
    neighbor_rx: async_channel::Receiver<JoinNeighborSignal>,
    cx: &mut Context<Self>,
  ) -> async_channel::Receiver<Result<JoinedDocument>> {
    let (result_tx, result_rx) = async_channel::bounded(1);
    let session_id = self.session;

    cx.spawn(async move |session, cx| {
      match neighbor_rx.recv().await {
        Ok(JoinNeighborSignal::NeighborUp) => {},
        Ok(JoinNeighborSignal::TimedOut) | Err(_) => {
          let detail = "Couldn't reach anyone in this session. Make sure the inviter is online and the invite is current, then try again.".to_string();
          let _ = session.update(cx, |session, cx| {
            session.detach(DetachReason::JoinFailed(detail.clone()), cx);
          });
          let _ = result_tx.try_send(Err(anyhow!(detail)));
          return;
        },
      }

      let reply_rx = match session.update(cx, |session, cx| session.start_join_snapshot_pull(inviter, result_tx.clone(), cx)) {
        Ok(Ok(reply_rx)) => reply_rx,
        Ok(Err(_)) => return,
        Err(error) => {
          let detail = format!("collaboration join session disappeared: {error}");
          let _ = result_tx.try_send(Err(anyhow!(detail)));
          return;
        },
      };

      let joined = match reply_rx.recv().await {
        Ok(Ok(bytes)) => {
          tracing::info!(session = %session_id, snapshot_bytes = bytes.len(), "collaboration join snapshot pulled");
          session
            .update(cx, |session, cx| match session.finish_join_snapshot(&bytes, cx) {
              Ok(joined) => Ok(joined),
              Err(error) => {
                tracing::error!(session = %session.session, error = %format_args!("{error:#}"), "building collaboration document from snapshot failed");
                let detail = format!("building collaboration document failed: {error:#}");
                session.detach(DetachReason::JoinFailed(detail), cx);
                Err(error.context("building collaboration document failed"))
              },
            })
            .unwrap_or_else(|error| {
              tracing::error!(error = %error, "collaboration join session disappeared while building snapshot");
              Err(anyhow!("collaboration join session disappeared: {error}"))
            })
        },
        Ok(Err(error)) => {
          tracing::error!(session = %session_id, error = %format_args!("{error:#}"), "pulling collaboration join snapshot failed");
          let detail = format!("pulling collaboration snapshot failed: {error:#}");
          let _ = session.update(cx, |session, cx| {
            session.detach(DetachReason::JoinFailed(detail.clone()), cx);
          });
          Err(anyhow!(detail))
        },
        Err(error) => {
          tracing::error!(session = %session_id, error = %error, "collaboration join snapshot reply channel closed");
          let detail = format!("collaboration snapshot reply channel closed: {error}");
          let _ = session.update(cx, |session, cx| {
            session.detach(DetachReason::JoinFailed(detail.clone()), cx);
          });
          Err(anyhow!(detail))
        },
      };
      let _ = result_tx.try_send(joined);
    })
    .detach();

    result_rx
  }

  fn start_join_snapshot_pull(
    &mut self,
    inviter: PeerId,
    result_tx: async_channel::Sender<Result<JoinedDocument>>,
    cx: &mut Context<Self>,
  ) -> Result<async_channel::Receiver<Result<Vec<u8>>>> {
    self.join_neighbor_tx = None;
    self.phase = SessionPhase::Joining(JoinStage::FetchingSnapshot { got: 0, total: None });
    cx.notify();

    let (reply_tx, reply_rx) = async_channel::bounded(1);
    let (progress_tx, progress_rx) = async_channel::bounded(8);
    let candidates = self.pull_candidates(Some(inviter));
    tracing::info!(session = %self.session, inviter = %inviter, candidate_count = candidates.len(), "requesting collaboration join snapshot");
    if let Err(error) = self.net_tx.try_send(NetCommand::PullSnapshot {
      session: self.session,
      candidates,
      progress: Some(progress_tx),
      reply: reply_tx,
    }) {
      tracing::error!(session = %self.session, inviter = %inviter, error = %error, "queueing collaboration join snapshot pull failed");
      let detail = format!("requesting collaboration snapshot failed: {error}");
      self.detach(DetachReason::JoinFailed(detail.clone()), cx);
      let _ = result_tx.try_send(Err(anyhow!(detail.clone())));
      bail!(detail);
    }

    cx.spawn(async move |session, cx| {
      while let Ok(progress) = progress_rx.recv().await {
        if session
          .update(cx, |session, cx| {
            if matches!(session.phase, SessionPhase::Joining(JoinStage::FetchingSnapshot { .. })) {
              session.phase = SessionPhase::Joining(JoinStage::FetchingSnapshot {
                got: progress.got,
                total: Some(progress.total),
              });
              cx.notify();
            }
          })
          .is_err()
        {
          break;
        }
      }
    })
    .detach();

    cx.spawn(async move |session, cx| {
      Timer::after(JOIN_SNAPSHOT_TIMEOUT).await;
      let detail = "Joining this session timed out while fetching the document snapshot. Check your connection and try again.".to_string();
      let timed_out = session
        .update(cx, |session, cx| {
          if matches!(session.phase, SessionPhase::Joining(JoinStage::FetchingSnapshot { .. })) {
            session.detach(DetachReason::JoinFailed(detail.clone()), cx);
            true
          } else {
            false
          }
        })
        .unwrap_or(false);
      if timed_out {
        let _ = result_tx.try_send(Err(anyhow!(detail)));
      }
    })
    .detach();

    Ok(reply_rx)
  }

  pub fn attach_joined_editor(&mut self, panel_id: Uuid, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) -> Result<()> {
    if self.doc.is_none() || self.binding.is_none() {
      tracing::warn!(session = %self.session, %panel_id, "cannot attach joined editor before snapshot load finishes");
      bail!("collaboration snapshot has not finished loading");
    }

    tracing::info!(
      session = %self.session,
      %panel_id,
      pending_remote_updates = self.pending_remote_updates.len(),
      pending_remote_patches = self.pending_remote_patches.len(),
      "attaching joined collaboration editor",
    );
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
    tracing::info!(session = %self.session, %panel_id, "joined collaboration editor attached");
    Ok(())
  }

  pub fn attach(&mut self, cx: &mut Context<Self>) {
    tracing::debug!(session = %self.session, phase = ?self.phase, "attaching collaboration session hooks");
    self.attach_undo_manager();
    self.attach_editor_hooks(cx);
    self.attach_loro_publish_hook();
    self.attach_direct_request_pump(cx);
    self.attach_undo_request_pump(cx);
    self.attach_timers(cx);
  }

  pub fn establish_local_peer(&mut self, peer: &flowstate_collab::ids::PeerId, cx: &mut Context<Self>) {
    if self.presence.is_none() {
      tracing::info!(session = %self.session, peer = %peer, "establishing local collaboration peer presence");
      let presence = PresenceStore::new(peer);
      let session = self.session;
      let net_tx = self.net_tx.clone();
      self
        .loro_subscriptions
        .push(presence.subscribe_local_updates(move |bytes| {
          let bytes_len = bytes.len();
          if let Err(error) = net_tx.try_send(NetCommand::Publish {
            session,
            payload: PublishPayload::Presence(bytes.clone()),
          }) {
            tracing::warn!(%session, bytes = bytes_len, error = %error, "queueing collaboration presence publish failed");
          } else {
            tracing::trace!(%session, bytes = bytes_len, "queued collaboration presence publish from local update");
          }
          true
        }));
      self.presence = Some(presence);
    } else {
      tracing::debug!(session = %self.session, peer = %peer, "local collaboration peer presence already established");
    }
    if let (Some(editor), Some(presence)) = (self.editor.clone(), self.presence.as_ref()) {
      editor.update(cx, |editor, cx| editor.set_own_collaboration_caret_color(Some(presence.self_color()), cx));
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
    tracing::info!(session = %self.session, peer = %peer, peers_present = self.peers_present(), "local collaboration peer established");
  }

  fn finish_join_snapshot(&mut self, snapshot: &[u8], cx: &mut Context<Self>) -> Result<JoinedDocument> {
    tracing::info!(session = %self.session, snapshot_bytes = snapshot.len(), "building collaboration document from join snapshot");
    if matches!(self.phase, SessionPhase::Detached(_)) {
      bail!("collaboration join is no longer active");
    }
    let total = snapshot.len() as u64;
    self.phase = SessionPhase::Joining(JoinStage::FetchingSnapshot {
      got: total,
      total: Some(total),
    });
    cx.notify();
    self.phase = SessionPhase::Joining(JoinStage::Building);
    cx.notify();

    let doc = schema::new_configured_doc();
    doc
      .import_with(snapshot, "remote")
      .context("importing collaboration snapshot failed")?;
    projection::verify_lineage(&doc, self.session)?;
    let document = projection::document_from_loro(&doc, load_document_theme())?;
    let binding = DocBinding::build(&doc, &document)?;
    tracing::info!(
      session = %self.session,
      paragraphs = document.paragraphs.len(),
      blocks = document.blocks.len(),
      assets = document.assets.assets.len(),
      "built collaboration document from join snapshot",
    );

    self.doc = Some(doc);
    self.binding = Some(binding);
    Ok(JoinedDocument {
      session: self.session,
      title: format!("{} (shared)", self.title),
      document,
    })
  }

  pub fn detach(&mut self, reason: DetachReason, cx: &mut Context<Self>) -> bool {
    if matches!(self.phase, SessionPhase::Detached(_)) {
      tracing::debug!(session = %self.session, ?reason, "collaboration session already detached");
      return false;
    }

    tracing::warn!(session = %self.session, ?reason, phase = ?self.phase, "detaching collaboration session");
    let user_left = matches!(reason, DetachReason::UserLeft);
    let fatal_detail = match &reason {
      DetachReason::Fatal(detail) => Some(detail.clone()),
      DetachReason::UserLeft | DetachReason::JoinFailed(_) => None,
    };
    if let Some(presence) = &self.presence {
      presence.delete_self();
      self.publish_presence_bytes(presence.encode_self());
    }
    if let Err(error) = self
      .net_tx
      .try_send(NetCommand::LeaveSession { session: self.session })
    {
      tracing::warn!(session = %self.session, error = %error, "queueing collaboration leave-session command failed during detach");
    }
    self.flush_pending_remote_patches(cx);

    if let Some(editor) = self.editor.clone() {
      editor.update(cx, |editor, cx| {
        editor.set_recovery_path(None, cx);
        editor.set_collaboration_role(None, cx);
        editor.set_collab_undo_redirect(None);
        editor.set_collab_capture(false);
        editor.set_own_collaboration_caret_color(None, cx);
        editor.clear_undo_redo_stacks();
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
    self.awaiting_recovery_neighbor_until = None;
    self.probe_pending = false;
    self.last_probe_failed = false;
    self.join_neighbor_tx = None;
    self.presence_refresh_pending = false;
    self.local_update_publish_attached = false;
    self.phase = SessionPhase::Detached(reason);
    if user_left {
      cx.emit(SessionNotice::LeftSession);
    } else if let Some(detail) = fatal_detail {
      cx.emit(SessionNotice::Disconnected(detail));
    }
    cx.notify();
    tracing::info!(session = %self.session, "collaboration session detached and cleaned up");
    true
  }

  pub fn flush_local_edits(&mut self, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) -> Result<()> {
    if matches!(self.phase, SessionPhase::Detached(_) | SessionPhase::Joining(_)) {
      tracing::trace!(session = %self.session, phase = ?self.phase, "skipping local collaboration edit flush for inactive phase");
      return Ok(());
    }

    let edits = editor.update(cx, |editor, _| {
      let edits = editor.take_pending_collab_edits();
      editor.clear_undo_redo_stacks();
      edits
    });
    let edit_count = edits.len();
    let operation_count = edits.iter().map(|edit| edit.operations.len()).sum::<usize>();
    if edit_count == 0 || operation_count == 0 {
      tracing::trace!(session = %self.session, edit_count, operation_count, "no local collaboration edits to flush");
      return Ok(());
    }
    tracing::debug!(session = %self.session, edit_count, operation_count, "flushing local collaboration edits into Loro");
    let document = editor.read(cx).document().clone();
    let Some(doc) = &self.doc else {
      tracing::warn!(session = %self.session, edit_count, operation_count, "cannot flush local collaboration edits because Loro doc is missing");
      return Ok(());
    };
    let Some(binding) = &mut self.binding else {
      tracing::warn!(session = %self.session, edit_count, operation_count, "cannot flush local collaboration edits because binding is missing");
      return Ok(());
    };

    let mut applied = false;
    for edit in edits {
      if edit.operations.is_empty() {
        continue;
      }
      let operation_count = edit.operations.len();
      if let Err(error) = (LocalApplier { doc, binding }).apply(&document, &edit.operations) {
        tracing::error!(session = %self.session, operation_count, error = %format_args!("{error:#}"), "applying local collaboration edit failed");
        return Err(error);
      }
      tracing::trace!(session = %self.session, operation_count, "applied local collaboration edit to Loro");
      applied = true;
    }
    if applied {
      self.last_document_activity = Instant::now();
      tracing::debug!(session = %self.session, "local collaboration edits flushed");
    }
    Ok(())
  }

  pub fn handle_gossip(&mut self, from: flowstate_collab::ids::PeerId, msg: GossipMsg, cx: &mut Context<Self>) {
    let gossip_kind = msg.kind();
    let gossip_payload_bytes = msg.payload_len();
    tracing::trace!(session = %self.session, from = %from, gossip_kind, gossip_payload_bytes, "handling collaboration gossip message");
    self.known_peers.insert(from);
    self.note_inbound_traffic(cx);
    let result = match msg {
      GossipMsg::Update(bytes) => self
        .import_update_bytes(&bytes, cx)
        .map(|()| asset_transfer::schedule_missing_assets(self, Some(from), cx)),
      GossipMsg::UpdateAvailable { blob, len } => {
        tracing::debug!(session = %self.session, from = %from, ?blob, bytes = len, "collaboration update available via direct blob pull");
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
      tracing::error!(session = %self.session, from = %from, gossip_kind, error = %format_args!("{error:#}"), "collaboration gossip handling failed");
      self.detach(DetachReason::Fatal(format!("collaboration update failed: {error:#}")), cx);
    }
  }

  pub fn neighbor_up(&mut self, peer: flowstate_collab::ids::PeerId, cx: &mut Context<Self>) {
    let inserted = self.neighbors.insert(peer);
    self.known_peers.insert(peer);
    self.zero_neighbors_since = None;
    self.last_probe_failed = false;
    if let Some(join_neighbor_tx) = self.join_neighbor_tx.take() {
      let _ = join_neighbor_tx.try_send(JoinNeighborSignal::NeighborUp);
    }
    self.anti_entropy.on_neighbor_up();
    self.mark_online(cx);
    self.publish_digest();
    asset_transfer::schedule_missing_assets(self, Some(peer), cx);
    cx.notify();
    tracing::info!(session = %self.session, peer = %peer, inserted, neighbors = self.neighbors.len(), "collaboration neighbor up");
  }

  pub fn neighbor_down(&mut self, peer: flowstate_collab::ids::PeerId, cx: &mut Context<Self>) {
    let removed = self.neighbors.remove(&peer);
    if self.neighbors.is_empty() && self.zero_neighbors_since.is_none() {
      self.zero_neighbors_since = Some(Instant::now());
    }
    self.evaluate_connectivity(cx);
    cx.notify();
    tracing::info!(session = %self.session, peer = %peer, removed, neighbors = self.neighbors.len(), "collaboration neighbor down");
  }

  pub fn handle_gossip_lagged(&mut self, cx: &mut Context<Self>) {
    tracing::warn!(session = %self.session, neighbors = self.neighbors.len(), "collaboration gossip lagged; scheduling recovery");
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
    tracing::info!(session = %self.session, online, previous_online = self.endpoint_online, "collaboration endpoint online state applied to session");
    self.endpoint_online = online;
    if online {
      self.last_probe_failed = false;
    }
    if online {
      if self.peers_present() == 0 || !self.neighbors.is_empty() {
        self.mark_online(cx);
      }
    } else {
      self.evaluate_connectivity(cx);
    }
    cx.notify();
  }

  pub fn handle_incompatible_version(&mut self, peer: PeerId, cx: &mut Context<Self>) {
    if self.incompatible_version_peers.insert(peer) {
      cx.emit(SessionNotice::IncompatibleVersion(peer.to_string()));
    }
  }

  fn attach_editor_hooks(&mut self, cx: &mut Context<Self>) {
    if !self.editor_subscriptions.is_empty() {
      tracing::trace!(session = %self.session, "collaboration editor hooks already attached");
      return;
    }
    let Some(editor) = self.editor.clone() else {
      tracing::warn!(session = %self.session, "cannot attach collaboration editor hooks because editor is missing");
      return;
    };

    tracing::debug!(session = %self.session, "attaching collaboration editor hooks");
    editor.update(cx, |editor, cx| {
      if editor.document_path().is_none() {
        editor.set_recovery_path(Some(presence_view::collaboration_recovery_path(self.session, &self.title)), cx);
        tracing::debug!(session = %self.session, "set collaboration recovery path for untitled document");
      }
      editor.clear_undo_redo_stacks();
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
          tracing::error!(session = %session.session, error = %format_args!("{error:#}"), "capturing local collaboration edit failed");
          session.detach(DetachReason::Fatal(format!("capturing local collaboration edit failed: {error:#}")), cx);
          return;
        }
        session.flush_pending_remote_patches(cx);
      }));

    self
      .editor_subscriptions
      .push(cx.subscribe(&editor, |session, _, event: &EditorEvent, cx| {
        if matches!(event, EditorEvent::SelectionChanged { .. }) {
          tracing::trace!(session = %session.session, "collaboration local selection changed; refreshing presence");
          session.schedule_own_presence_refresh(cx);
        }
      }));
    tracing::debug!(session = %self.session, subscriptions = self.editor_subscriptions.len(), "collaboration editor hooks attached");
  }

  fn attach_loro_publish_hook(&mut self) {
    if self.doc.is_none() || self.local_update_publish_attached {
      tracing::trace!(session = %self.session, has_doc = self.doc.is_some(), attached = self.local_update_publish_attached, "skipping collaboration Loro publish hook attach");
      return;
    }
    let Some(doc) = &self.doc else {
      return;
    };
    let session = self.session;
    let net_tx = self.net_tx.clone();
    tracing::debug!(%session, "attaching collaboration Loro local-update publish hook");
    self
      .loro_subscriptions
      .push(doc.subscribe_local_update(Box::new(move |bytes| {
        let bytes_len = bytes.len();
        if let Err(error) = net_tx.try_send(NetCommand::Publish {
          session,
          payload: PublishPayload::Update(bytes.clone()),
        }) {
          tracing::warn!(%session, bytes = bytes_len, error = %error, "queueing collaboration update publish failed");
        } else {
          tracing::trace!(%session, bytes = bytes_len, "queued collaboration update publish from local Loro update");
        }
        true
      })));
    self.local_update_publish_attached = true;
  }

  fn attach_undo_manager(&mut self) {
    if self.undo_manager.is_some() {
      tracing::trace!(session = %self.session, "collaboration undo manager already attached");
      return;
    }
    let Some(doc) = &self.doc else {
      tracing::warn!(session = %self.session, "cannot attach collaboration undo manager because Loro doc is missing");
      return;
    };
    let mut undo_manager = UndoManager::new(doc);
    undo_manager.set_merge_interval(UNDO_MERGE_INTERVAL_MS);
    undo_manager.set_max_undo_steps(UNDO_MAX_STEPS);
    self.undo_manager = Some(undo_manager);
    tracing::debug!(session = %self.session, merge_interval_ms = 500, max_undo_steps = 300, "collaboration undo manager attached");
  }

  fn attach_undo_request_pump(&mut self, cx: &mut Context<Self>) {
    if self.undo_pump_started {
      tracing::trace!(session = %self.session, "collaboration undo request pump already started");
      return;
    }
    self.undo_pump_started = true;
    let requests = self.undo_rx.clone();
    let session_id = self.session;
    tracing::debug!(session = %session_id, "starting collaboration undo request pump");
    cx.spawn(async move |session, cx| {
      while let Ok(redirect) = requests.recv().await {
        tracing::debug!(session = %session_id, ?redirect, "received collaboration undo redirect");
        if session
          .update(cx, |session, cx| {
            if let Err(error) = session.apply_loro_undo_redirect(redirect, cx) {
              tracing::error!(session = %session.session, error = %format_args!("{error:#}"), "collaboration undo redirect failed");
              session.detach(DetachReason::Fatal(format!("collaboration undo failed: {error:#}")), cx);
            }
          })
          .is_err()
        {
          tracing::debug!(session = %session_id, "collaboration undo request pump session disappeared");
          break;
        }
      }
      tracing::debug!(session = %session_id, "collaboration undo request pump stopped");
    })
    .detach();
  }
}
