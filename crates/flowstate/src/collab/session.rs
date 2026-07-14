use std::{
  collections::HashSet,
  sync::Arc,
  time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use flowstate_collab::{
  DocumentKind, SessionAdmission, SessionId, SyncIoHandle,
  crdt_runtime::{CrdtRuntime, RuntimeEvent},
  doc_io::DocIoHandle,
  flow::{FlowDocHandle, FlowIoHandle, FlowPublishEvent, FlowRuntime},
  ids::PeerId,
  local_write::{LocalDocHandle, LocalWriteConfig},
  net::{
    NetCommand, PeerAddr, PublishPayload,
    anti_entropy::{AntiEntropyState, GapAction},
    direct::{DirectServeRequest, DirectSessionHandler},
    runtime::CommandSender,
  },
  presence::PresenceStore,
  proto_gossip::GossipMsg,
};
use flowstate_fidelity::{self as fidelity, FidelityClass};
use gpui::{Context, Entity, EventEmitter, Subscription, Timer};
use loro::{LoroDoc, Subscription as LoroSubscription};
use uuid::Uuid;

use crate::app_settings::{load_document_theme, load_local_user_identity, load_local_user_profile};
use crate::flow::{FlowEditor, FlowEditorEvent};
use crate::rich_text_element::{AssetId, AssetRecord, CollaborationRole, DocumentProjection, EditorEvent, RichTextEditor};

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
  pub payload: JoinedDocumentPayload,
}

/// The join snapshot's materialization, per document kind.
pub enum JoinedDocumentPayload {
  RichText(DocumentProjection),
  Flow(flowstate_flow::FlowBoardProjection),
}

/// The session's document-facing editor arm (spec Part C): ONE session type,
/// two-arm document half.
#[derive(Clone)]
pub enum CollabEditor {
  RichText(Entity<RichTextEditor>),
  Flow(Entity<FlowEditor>),
}

impl CollabEditor {
  pub fn as_rich_text(&self) -> Option<&Entity<RichTextEditor>> {
    match self {
      Self::RichText(editor) => Some(editor),
      Self::Flow(_) => None,
    }
  }

  pub fn as_flow(&self) -> Option<&Entity<FlowEditor>> {
    match self {
      Self::Flow(editor) => Some(editor),
      Self::RichText(_) => None,
    }
  }
}

/// The parked join-handoff write authority, per document kind.
pub enum JoinedAuthority {
  RichText(Arc<LocalDocHandle>),
  Flow(Arc<FlowDocHandle>),
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
  // Loro-first (spec §3, invariant 5): the session is TRANSPORT-ONLY. It holds
  // the document I/O service handle to import remote updates and drain the
  // publish queue — never a write path into the document.
  runtime: Option<SyncIoHandle>,
  // JOIN handoff slot (spec §3 join gate): the write authority constructed by
  // `finish_join_snapshot`, held ONLY until the workspace takes it via
  // `take_joined_document_services`. The session never calls into it.
  join_authority: Option<JoinedAuthority>,
  runtime_vv: Vec<u8>,
  kind: DocumentKind,
  editor: Option<CollabEditor>,
  panel_id: Option<Uuid>,
  // UI-only asset cache records fetched after Loro metadata arrives.
  pending_asset_records: Vec<(AssetId, AssetRecord)>,
  pending_remote_updates: Vec<Vec<u8>>,
  pending_remote_update_bytes: usize,
  pending_remote_updates_overflowed: bool,
  presence: Option<PresenceStore>,
  net_tx: CommandSender,
  direct_tx: async_channel::Sender<DirectServeRequest>,
  direct_rx: async_channel::Receiver<DirectServeRequest>,
  editor_subscriptions: Vec<Subscription>,
  loro_subscriptions: Vec<LoroSubscription>,
  neighbors: HashSet<flowstate_collab::ids::PeerId>,
  bootstrap_addrs: Vec<PeerAddr>,
  // Symmetric live-session admission, retained only in memory for reconnects
  // and participant-created invitations.
  admission: Option<SessionAdmission>,
  // The role this endpoint holds in the session; gates local writes/undo and is
  // pushed onto the editor at attach so `can_write_collaboration()` takes effect.
  collaboration_role: CollaborationRole,
  asset_pulls_in_flight: HashSet<AssetId>,
  anti_entropy: AntiEntropyState,
  direct_pump_started: bool,
  presence_refresh_pending: bool,
  presence_refresh_generation: u64,
  external_caret_refresh_pending: bool,
  external_caret_refresh_generation: u64,
  comment_annotation_refresh_pending: bool,
  comment_annotation_refresh_generation: u64,
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
  publish_pump_pending: bool,
}

const DIRECT_REQUEST_CHANNEL_CAPACITY: usize = 32;
const JOIN_FIRST_NEIGHBOR_TIMEOUT: Duration = Duration::from_secs(15);
const JOIN_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);
/// Debounce for draining the doc I/O publish queue after local commits (spec
/// §6 publication rule: an update is publishable once its intent released the
/// write gate; the session only batches the network broadcast).
const LOCAL_UPDATE_PUBLISH_DEBOUNCE_MS: u64 = 24;

impl CollabSession {
  pub fn from_local_runtime(
    session: SessionId,
    panel_id: Uuid,
    editor: Entity<RichTextEditor>,
    title: String,
    io: DocIoHandle,
    net_tx: CommandSender,
  ) -> Self {
    Self::from_local_parts(
      session,
      panel_id,
      CollabEditor::RichText(editor),
      title,
      SyncIoHandle::RichText(io),
      DocumentKind::RichText,
      net_tx,
    )
  }

  /// Start a session on an open FLOW tab: the identical wiring shape (the
  /// editor already holds its write authority; the session only takes the
  /// transport-side flow I/O handle).
  pub fn from_local_flow_runtime(
    session: SessionId,
    panel_id: Uuid,
    editor: Entity<FlowEditor>,
    title: String,
    io: FlowIoHandle,
    net_tx: CommandSender,
  ) -> Self {
    Self::from_local_parts(
      session,
      panel_id,
      CollabEditor::Flow(editor),
      title,
      SyncIoHandle::Flow(io),
      DocumentKind::Flow,
      net_tx,
    )
  }

  fn from_local_parts(
    session: SessionId,
    panel_id: Uuid,
    editor: CollabEditor,
    title: String,
    io: SyncIoHandle,
    kind: DocumentKind,
    net_tx: CommandSender,
  ) -> Self {
    // Direct serving is network-bound and should backpressure rather than grow without limit.
    let (direct_tx, direct_rx) = async_channel::bounded(DIRECT_REQUEST_CHANNEL_CAPACITY);
    let now = Instant::now();
    tracing::info!(
      %session,
      %panel_id,
      title = %title,
      "attached local collaboration session to document I/O service",
    );

    Self {
      session,
      title,
      phase: SessionPhase::Creating,
      runtime: Some(io),
      join_authority: None,
      runtime_vv: Vec::new(),
      kind,
      editor: Some(editor),
      panel_id: Some(panel_id),
      pending_asset_records: Vec::new(),
      pending_remote_updates: Vec::new(),
      pending_remote_update_bytes: 0,
      pending_remote_updates_overflowed: false,
      presence: None,
      net_tx,
      direct_tx,
      direct_rx,
      editor_subscriptions: Vec::new(),
      loro_subscriptions: Vec::new(),
      neighbors: HashSet::new(),
      bootstrap_addrs: Vec::new(),
      admission: None,
      collaboration_role: CollaborationRole::Editor,
      asset_pulls_in_flight: HashSet::new(),
      anti_entropy: AntiEntropyState::new(session, now),
      direct_pump_started: false,
      presence_refresh_pending: false,
      presence_refresh_generation: 0,
      external_caret_refresh_pending: false,
      external_caret_refresh_generation: 0,
      comment_annotation_refresh_pending: false,
      comment_annotation_refresh_generation: 0,
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
      publish_pump_pending: false,
    }
  }

  pub fn joining(
    session: SessionId,
    title: String,
    net_tx: CommandSender,
    bootstrap_addrs: Vec<PeerAddr>,
    admission: SessionAdmission,
    kind: DocumentKind,
  ) -> Self {
    let collaboration_role = CollaborationRole::Editor;
    let (direct_tx, direct_rx) = async_channel::bounded(DIRECT_REQUEST_CHANNEL_CAPACITY);
    let now = Instant::now();
    tracing::info!(%session, title = %title, bootstrap_count = bootstrap_addrs.len(), "created joining collaboration session state");
    Self {
      session,
      title,
      phase: SessionPhase::Joining(JoinStage::Resolving),
      runtime: None,
      join_authority: None,
      runtime_vv: Vec::new(),
      kind,
      editor: None,
      panel_id: None,
      pending_asset_records: Vec::new(),
      pending_remote_updates: Vec::new(),
      pending_remote_update_bytes: 0,
      pending_remote_updates_overflowed: false,
      presence: None,
      net_tx,
      direct_tx,
      direct_rx,
      editor_subscriptions: Vec::new(),
      loro_subscriptions: Vec::new(),
      neighbors: HashSet::new(),
      bootstrap_addrs,
      admission: Some(admission),
      collaboration_role,
      asset_pulls_in_flight: HashSet::new(),
      anti_entropy: AntiEntropyState::new(session, now),
      direct_pump_started: false,
      presence_refresh_pending: false,
      presence_refresh_generation: 0,
      external_caret_refresh_pending: false,
      external_caret_refresh_generation: 0,
      comment_annotation_refresh_pending: false,
      comment_annotation_refresh_generation: 0,
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
      publish_pump_pending: false,
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

  /// The session's document kind (rides the v4 invite ticket under its HMAC).
  pub fn document_kind(&self) -> DocumentKind {
    self.kind
  }

  /// The rich-text arm of the attached editor (presence/comments/assets are
  /// rich-text-only surfaces; flow arms no-op through this accessor).
  pub(super) fn rich_text_editor(&self) -> Option<Entity<RichTextEditor>> {
    self.editor.as_ref().and_then(CollabEditor::as_rich_text).cloned()
  }

  pub(super) fn flow_editor(&self) -> Option<Entity<FlowEditor>> {
    self.editor.as_ref().and_then(CollabEditor::as_flow).cloned()
  }

  pub(super) fn set_admission(&mut self, admission: SessionAdmission) {
    self.admission = Some(admission);
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
    // Hand the direct-serve path the document I/O service handle so
    // snapshot/update pulls are answered through the gate-disciplined I/O
    // thread (spec I-9a: raw ungated doc reads are outlawed — they can
    // force-commit mid-intent state). Absent a runtime yet, it falls back to
    // the session-served request channel.
    DirectSessionHandler::new(self.direct_tx.clone(), self.runtime.clone())
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

  pub fn attach_joined_editor(&mut self, panel_id: Uuid, editor: CollabEditor, cx: &mut Context<Self>) -> Result<()> {
    if self.runtime.is_none() {
      tracing::warn!(session = %self.session, %panel_id, "cannot attach joined editor before snapshot load finishes");
      bail!("collaboration snapshot has not finished loading");
    }

    tracing::info!(
      session = %self.session,
      %panel_id,
      pending_remote_updates = self.pending_remote_updates.len(),
      pending_asset_records = self.pending_asset_records.len(),
      "attaching joined collaboration editor",
    );
    self.panel_id = Some(panel_id);
    self.editor = Some(editor);
    self.attach(cx);

    let pending_updates = std::mem::take(&mut self.pending_remote_updates);
    self.pending_remote_update_bytes = 0;
    for update in pending_updates {
      // §perf: each queued update is owned; move it in to avoid re-copying.
      self.import_update_bytes_owned(update, cx)?;
    }
    if std::mem::take(&mut self.pending_remote_updates_overflowed)
      && let Some(from) = self.pull_candidates(None).first().copied()
    {
      tracing::info!(session = %self.session, from = %from, "starting collaboration resync pull after pre-attach queue overflow");
      // Also go through the dedup so this recovery pull registers the slot its
      // `finish_pull` will later clear (never clearing a digest pull's slot).
      if let GapAction::Pull { from, our_vv } = self
        .anti_entropy
        .begin_pull(from, self.runtime_vv.clone(), std::time::Instant::now())
      {
        self.start_update_pull(from, our_vv, cx);
      }
    }
    self.flush_pending_asset_records(cx);
    asset_transfer::schedule_missing_assets(self, None, cx);
    self.publish_digest();
    cx.notify();
    tracing::info!(session = %self.session, %panel_id, "joined collaboration editor attached");
    Ok(())
  }

  pub fn attach(&mut self, cx: &mut Context<Self>) {
    tracing::debug!(session = %self.session, phase = ?self.phase, "attaching collaboration session hooks");
    self.attach_editor_hooks(cx);
    self.attach_direct_request_pump(cx);
    self.attach_timers(cx);
    self.refresh_runtime_version_vector(cx);
    self.refresh_comment_annotations(cx);
    // Spec §6: pump once immediately so intents committed before the session
    // started (the publish queue is filled by gate-held local commits
    // regardless of any session existing) broadcast right away.
    self.pump_publish(cx);
  }

  /// JOIN handoff (spec §3): the workspace takes ownership of the document
  /// services built by `finish_join_snapshot` — the write authority moves out
  /// (the session must never hold a write path once attached) and the I/O
  /// handle is shared. Returns `None` before the snapshot import completes or
  /// after the services were already taken.
  pub fn take_joined_document_services(&mut self) -> Option<(JoinedAuthority, SyncIoHandle)> {
    let io = self.runtime.clone()?;
    let authority = self.join_authority.take()?;
    Some((authority, io))
  }

  /// Single fidelity-instrumented write point for the cached runtime version
  /// vector. Logs every update; when `enforce_monotonic` is set (op-application
  /// paths, where the vector can only advance) it also checks the new vector
  /// dominates or equals the previous one. Strictly additive: the vector is
  /// assigned unconditionally, so behavior is identical when tracing is off.
  fn update_runtime_vv(&mut self, new_vv: Vec<u8>, source: &'static str, enforce_monotonic: bool) {
    if fidelity::enabled() {
      fidelity::event(FidelityClass::Frontier, "runtime-vv-update", || {
        format!(
          "session={} source={source} old_bytes={} new_bytes={}",
          self.session,
          self.runtime_vv.len(),
          new_vv.len(),
        )
      });
      if enforce_monotonic
        && !self.runtime_vv.is_empty()
        && !new_vv.is_empty()
        && let (Ok(old), Ok(new)) = (loro::VersionVector::decode(&self.runtime_vv), loro::VersionVector::decode(&new_vv))
      {
        let relation = new.partial_cmp(&old);
        fidelity::check(
          matches!(relation, Some(std::cmp::Ordering::Greater | std::cmp::Ordering::Equal)),
          FidelityClass::Frontier,
          "runtime-vv-regression",
          || {
            format!(
              "session={} source={source} relation={relation:?} old_bytes={} new_bytes={}",
              self.session,
              self.runtime_vv.len(),
              new_vv.len(),
            )
          },
        );
      }
    }
    self.runtime_vv = new_vv;
  }

  fn refresh_runtime_version_vector(&mut self, cx: &mut Context<Self>) {
    let Some(runtime) = self.runtime.clone() else {
      return;
    };
    let session_id = self.session;
    cx.spawn(async move |session, cx| {
      let result = runtime.oplog_version_vector().await;
      let _ = session.update(cx, |session, _| match result {
        Ok(vv) => {
          // Authoritative re-read of the oplog vector: enforcing monotonicity
          // here would false-positive against benign async reordering with a
          // concurrent op-applying commit, so log the update without the check.
          session.update_runtime_vv(vv, "oplog-refresh", false);
          session.publish_digest();
        },
        Err(error) => {
          tracing::warn!(session = %session_id, error = %format_args!("{error:#}"), "reading collaboration runtime version vector failed");
        },
      });
    })
    .detach();
  }

  /// Re-synchronize anti-entropy state after a save/checkpoint that was applied
  /// to the shared document runtime *outside* this session.
  ///
  /// Saving a document runs the editor's native save hook, which calls
  /// `DocIoHandle::checkpoint_package`. Recording that named revision is a
  /// real Loro mutation: it advances the canonical frontier/version-vector. The
  /// hook returns the resulting `RuntimeEvent::LocalUpdate` bytes, but the save
  /// hook future runs without any GPUI context (see `NativeSaveHook`), so it
  /// cannot route them back into this session. As a result our cached
  /// `runtime_vv` would go stale and peers would not learn about the new
  /// revision op until the next periodic digest (~10s).
  ///
  /// Re-reading the authoritative oplog version vector and re-publishing a
  /// digest fixes the staleness and lets anti-entropy converge immediately:
  /// peers see we are ahead (`SenderHasMissingOps`) and pull the revision op on
  /// the spot. We intentionally do not re-publish update bytes or re-apply a
  /// projection here — the checkpoint changed no visible content, and the
  /// runtime remains the single Loro owner.
  pub(super) fn refresh_after_external_checkpoint(&mut self, cx: &mut Context<Self>) {
    if self.runtime.is_none() {
      tracing::trace!(session = %self.session, "ignoring external checkpoint refresh because the session has no runtime");
      return;
    }
    tracing::debug!(session = %self.session, "refreshing collaboration anti-entropy state after an external document checkpoint");
    self.refresh_runtime_version_vector(cx);
  }

  pub fn establish_local_peer(&mut self, peer: &flowstate_collab::ids::PeerId, cx: &mut Context<Self>) {
    if self.presence.is_none() {
      tracing::info!(session = %self.session, peer = %peer, "establishing local collaboration peer presence");
      let profile = load_local_user_profile();
      let presence = PresenceStore::new_with_color(peer, profile.color_rgb);
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
    if let (Some(editor), Some(presence)) = (self.rich_text_editor(), self.presence.as_ref()) {
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
    tracing::info!(session = %self.session, snapshot_bytes = snapshot.len(), kind = ?self.kind, "building collaboration document from join snapshot");
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

    if matches!(self.kind, DocumentKind::Flow) {
      return self.finish_join_flow_snapshot(snapshot);
    }

    let doc = LoroDoc::new();
    flowstate_document::loro_schema::configure_text_styles(&doc);
    doc
      .import_with(snapshot, "remote")
      .context("importing collaboration snapshot failed")?;
    let runtime = CrdtRuntime::from_doc(doc, None, None).context("creating joined collaboration CRDT runtime")?;
    let mut document = runtime
      .projection_snapshot()
      .context("projecting joined Loro-native document")?;
    // Loro-first services (spec §3): wrap the imported core in the write gate.
    // The session keeps only the transport-side I/O handle; the write
    // authority is parked for the workspace handoff (join gate: the editor
    // cannot receive it before this point, i.e. before the initial snapshot
    // import completed).
    let (authority, gate) = LocalDocHandle::new(runtime, LocalWriteConfig::default());
    let doc_io = DocIoHandle::spawn(gate).context("starting joined collaboration document I/O service")?;
    let io = doc_io.clone();
    // §15/§31: bind this joiner's durable author identity to the joined
    // document so their revisions record an author and `users_by_id` is
    // populated. The user-registration op converges to peers via
    // anti-entropy. Fire-and-forget and non-fatal: a failure must not break
    // the join. `create_document_panel` receives this document through the
    // `Attachment` source, which deliberately skips re-binding to avoid a
    // redundant second call.
    let identity_io = doc_io.clone();
    cx.spawn(async move |session, cx| {
      let (user_id, display_name) = cx
        .background_executor()
        .spawn(async { load_local_user_identity() })
        .await;
      match identity_io.set_author_identity(user_id, display_name).await {
        Ok(events) => {
          let applied = session.update(cx, |session, cx| session.apply_runtime_events(events, false, cx));
          if let Ok(Err(error)) = applied {
            tracing::warn!(error = %format_args!("{error:#}"), "publishing durable author identity update failed");
          }
        },
        Err(error) => {
          tracing::warn!(error = %format_args!("{error:#}"), "binding durable author identity to joined collaboration document failed");
        },
      }
    })
    .detach();
    document.theme = load_document_theme();
    tracing::info!(
      session = %self.session,
      paragraphs = document.paragraphs.len(),
      blocks = document.blocks.len(),
      assets = document.assets.assets.len(),
      "built collaboration document from join snapshot",
    );

    self.runtime = Some(SyncIoHandle::RichText(io));
    self.join_authority = Some(JoinedAuthority::RichText(Arc::new(authority)));
    fidelity::event(FidelityClass::Frontier, "runtime-vv-reset", || {
      format!("session={} source=join-snapshot prior_bytes={}", self.session, self.runtime_vv.len())
    });
    self.runtime_vv.clear();
    Ok(JoinedDocument {
      session: self.session,
      title: format!("{} (shared)", self.title),
      payload: JoinedDocumentPayload::RichText(document),
    })
  }

  /// The FLOW join arm (spec Part C): `FlowRuntime::from_snapshot` (schema
  /// validated — the wrong-kind defense) → `FlowDocHandle::new` →
  /// `FlowIoHandle::spawn` → park the authority for the workspace handoff.
  fn finish_join_flow_snapshot(&mut self, snapshot: &[u8]) -> Result<JoinedDocument> {
    let runtime = FlowRuntime::from_snapshot(snapshot).context("building joined flow runtime from snapshot")?;
    let board = runtime.board_ref().clone();
    let (authority, gate) = FlowDocHandle::new(runtime);
    let io = FlowIoHandle::spawn(gate).context("starting joined flow I/O service")?;
    tracing::info!(
      session = %self.session,
      sheets = board.sheets.len(),
      "built collaboration flow document from join snapshot",
    );
    self.runtime = Some(SyncIoHandle::Flow(io));
    self.join_authority = Some(JoinedAuthority::Flow(authority));
    fidelity::event(FidelityClass::Frontier, "runtime-vv-reset", || {
      format!("session={} source=join-snapshot prior_bytes={}", self.session, self.runtime_vv.len())
    });
    self.runtime_vv.clear();
    Ok(JoinedDocument {
      session: self.session,
      title: format!("{} (shared)", self.title),
      payload: JoinedDocumentPayload::Flow(board),
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
    self.flush_pending_asset_records(cx);

    // Loro-first (invariant 5): leaving a session stops transport only. The
    // document's write authority, gate, and I/O service are untouched — the
    // editor keeps editing through the identical path; nothing to reset on it
    // beyond collaboration presentation state.
    match self.editor.clone() {
      Some(CollabEditor::RichText(editor)) => {
        editor.update(cx, |editor, cx| {
          editor.set_recovery_path(None, cx);
          editor.set_collaboration_role(None, cx);
          editor.set_own_collaboration_caret_color(None, cx);
          editor.set_external_carets(Vec::new(), cx);
        });
      },
      Some(CollabEditor::Flow(editor)) => {
        editor.update(cx, |editor, cx| {
          editor.set_recovery_path(None, cx);
          editor.set_external_presences(Vec::new(), cx);
        });
      },
      None => {},
    }

    self.editor_subscriptions.clear();
    self.loro_subscriptions.clear();
    self.presence = None;
    self.runtime = None;
    self.join_authority = None;
    self.runtime_vv.clear();
    self.pending_asset_records.clear();
    self.pending_remote_updates.clear();
    self.pending_remote_update_bytes = 0;
    self.pending_remote_updates_overflowed = false;
    self.publish_pump_pending = false;
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
    self.presence_refresh_generation = self.presence_refresh_generation.wrapping_add(1);
    self.external_caret_refresh_pending = false;
    self.external_caret_refresh_generation = self.external_caret_refresh_generation.wrapping_add(1);
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

  /// Drain the doc I/O publish queue (committed local Loro updates) and
  /// broadcast them (spec §6 publication rule). The session never writes
  /// document content — the updates were committed by the editor's write
  /// authority under the gate; this is transport only (invariant 5).
  pub(super) fn pump_publish(&mut self, cx: &mut Context<Self>) {
    if matches!(self.phase, SessionPhase::Detached(_)) {
      tracing::trace!(session = %self.session, phase = ?self.phase, "skipping local update publish pump for detached session");
      return;
    }
    let Some(io) = self.runtime.clone() else {
      tracing::trace!(session = %self.session, "skipping local update publish pump because the document I/O service is missing");
      return;
    };
    let session_id = self.session;
    match io {
      SyncIoHandle::RichText(io) => {
        cx.spawn(async move |session, cx| {
          match io.pump_publish().await {
            Ok(events) => {
              if events.is_empty() {
                return;
              }
              let published = events.len();
              let _ = session.update(cx, |session, cx| {
                fidelity::event(FidelityClass::Reconcile, "publish-pump", || {
                  format!("session={session_id} events={published}")
                });
                if let Err(error) = session.apply_runtime_events(events, false, cx) {
                  tracing::error!(session = %session_id, error = %format_args!("{error:#}"), "publishing committed local collaboration updates failed");
                  return;
                }
                session.refresh_external_carets(cx);
              });
            },
            Err(error) => {
              tracing::warn!(session = %session_id, error = %format_args!("{error:#}"), "pumping collaboration local update publish queue failed");
            },
          }
        })
        .detach();
      },
      SyncIoHandle::Flow(io) => {
        cx.spawn(async move |session, cx| {
          match io.pump_publish().await {
            Ok(events) => {
              if events.is_empty() {
                return;
              }
              let published = events.len();
              let _ = session.update(cx, |session, cx| {
                fidelity::event(FidelityClass::Reconcile, "publish-pump", || {
                  format!("session={session_id} kind=flow events={published}")
                });
                session.apply_flow_events(events, cx);
              });
            },
            Err(error) => {
              tracing::warn!(session = %session_id, error = %format_args!("{error:#}"), "pumping flow publish queue failed");
            },
          }
        })
        .detach();
      },
    }
  }

  /// The flow arm of the publish/apply pump: `FlowPublishEvent`s ride the
  /// SAME vv-tracking + gossip publication the rich-text events use.
  pub(super) fn apply_flow_events(&mut self, events: Vec<FlowPublishEvent>, cx: &mut Context<Self>) {
    for event in events {
      match event {
        FlowPublishEvent::LocalUpdate { bytes, version_vector, .. } => {
          self.update_runtime_vv(version_vector, "local-update", true);
          self.publish_update_bytes(bytes);
        },
        FlowPublishEvent::RemoteUpdateApplied { version_vector, .. } => {
          self.update_runtime_vv(version_vector, "remote-update-applied", true);
        },
      }
    }
    self.last_document_activity = Instant::now();
    // The projection changes ride the ordered board/cell streams; pump the
    // flow editor to drain them.
    if let Some(editor) = self.flow_editor() {
      editor.update(cx, |editor, cx| {
        editor.sync_board_from_handle(cx);
        editor.sync_cell_editors_from_authority(cx);
      });
    }
    cx.notify();
  }

  pub(super) fn apply_runtime_events(&mut self, events: Vec<RuntimeEvent>, apply_projection: bool, cx: &mut Context<Self>) -> Result<()> {
    // §hang-watchdog: the main-thread handler for each incoming collab update.
    let apply_started = std::time::Instant::now();
    let event_count = events.len();
    for event in events {
      fidelity::event(FidelityClass::Projection, "apply-runtime-event", || {
        let kind = match &event {
          RuntimeEvent::LocalUpdate { .. } => "local-update",
          RuntimeEvent::RemoteUpdateApplied { .. } => "remote-update-applied",
          RuntimeEvent::RevisionOpened { .. } => "revision-opened",
          RuntimeEvent::RevisionForked { .. } => "revision-forked",
          RuntimeEvent::SelectionRestored { .. } => "selection-restored",
          RuntimeEvent::ProjectionUpdated { .. } => "projection-updated",
          RuntimeEvent::ProjectionPatched { .. } => "projection-patched",
          RuntimeEvent::HistoryRebaseRequired { .. } => "history-rebase-required",
        };
        format!("session={} kind={kind} apply_projection={apply_projection}", self.session)
      });
      match event {
        RuntimeEvent::LocalUpdate { bytes, version_vector, .. } => {
          self.update_runtime_vv(version_vector, "local-update", true);
          self.publish_update_bytes(bytes);
        },
        // §A12.1.3 slice 4: a below-root remote payload was merged into the
        // PACKAGE (durable, full history) but this shallow session's
        // in-memory doc cannot hold it — the document must be reopened to
        // show the merged state. Rare by construction; surfaced loudly.
        // Follow-up (design Q4): auto-reopen instead of warn-only.
        RuntimeEvent::HistoryRebaseRequired { merged_frontier } => {
          tracing::warn!(
            session = %self.session,
            merged_frontier_len = merged_frontier.len(),
            "offline collaborator history merged on disk; reopen the document to see it"
          );
        },
        RuntimeEvent::RemoteUpdateApplied { pending, version_vector, .. } => {
          self.update_runtime_vv(version_vector, "remote-update-applied", true);
          if let Some(pending) = pending {
            tracing::debug!(
              session = %self.session,
              pending_ranges = pending.iter().count(),
              "remote collaboration update has pending Loro dependencies; requesting anti-entropy pull immediately",
            );
            if let Some(from) = self.pull_candidates(None).first().copied() {
              // Route through `begin_pull` (the same dedup the digest path uses)
              // rather than calling `start_update_pull` directly: a burst of pending
              // imports (each carrying the SAME frozen `runtime_vv` while the import
              // stays buffered) must not fire a storm of identical pulls at one peer.
              // begin_pull admits at most one in-flight pull per peer (with a
              // deadline), and registers the slot the later `finish_pull` clears.
              match self
                .anti_entropy
                .begin_pull(from, self.runtime_vv.clone(), std::time::Instant::now())
              {
                GapAction::Pull { from, our_vv } => self.start_update_pull(from, our_vv, cx),
                _ => {
                  tracing::debug!(session = %self.session, from = %from, "pending-dependency pull skipped; one is already in flight for this peer");
                },
              }
            } else {
              tracing::warn!(session = %self.session, "cannot pull pending Loro dependencies because no collaboration peers are available");
            }
          }
        },
        RuntimeEvent::ProjectionUpdated {
          document, version_vector, ..
        } if apply_projection => {
          self.update_runtime_vv(version_vector, "projection-updated", true);
          self.apply_runtime_projection(*document, cx)?;
        },
        RuntimeEvent::ProjectionPatched { batch, version_vector, .. } if apply_projection => {
          self.update_runtime_vv(version_vector, "projection-patched", true);
          self.apply_runtime_patches(batch, cx)?;
        },
        RuntimeEvent::RevisionOpened { document, .. } if apply_projection => {
          self.apply_runtime_projection(*document, cx)?;
        },
        RuntimeEvent::SelectionRestored { selection } if apply_projection => {
          if let Some(editor) = self.rich_text_editor() {
            editor.update(cx, |editor, cx| editor.restore_runtime_selection(selection, cx));
          }
        },
        RuntimeEvent::RevisionForked { .. }
        | RuntimeEvent::ProjectionUpdated { .. }
        | RuntimeEvent::ProjectionPatched { .. }
        | RuntimeEvent::RevisionOpened { .. }
        | RuntimeEvent::SelectionRestored { .. } => {},
      }
    }
    if event_count > 0 {
      self.refresh_comment_annotations(cx);
    }
    let apply_ms = apply_started.elapsed().as_millis();
    if apply_ms > 150 {
      tracing::warn!("slow collab apply_runtime_events (hang watchdog): {apply_ms}ms, events={event_count}");
    }
    Ok(())
  }

  fn apply_runtime_patches(&mut self, batch: flowstate_document::ProjectionPatchBatch, cx: &mut Context<Self>) -> Result<()> {
    let Some(editor) = self.rich_text_editor() else {
      return Ok(());
    };
    fidelity::event(FidelityClass::Projection, "apply-projection-patches", || {
      format!(
        "session={} patches={} base_frontier_bytes={} new_frontier_bytes={}",
        self.session,
        batch.patches.len(),
        batch.base_frontier.len(),
        batch.new_frontier.len(),
      )
    });
    // Field fix 2026-07-07: remote batches reach the editor exclusively via
    // the core's ORDERED projection stream — this event is only the pump. The
    // payload in the event is intentionally unused (ordering lives in the
    // stream, not the delivery channel).
    let _ = &batch;
    editor.update(cx, |editor, cx| editor.sync_projection_from_authority(cx));
    self.last_document_activity = Instant::now();
    self.refresh_external_carets(cx);
    Ok(())
  }

  fn apply_runtime_projection(&mut self, mut document: DocumentProjection, cx: &mut Context<Self>) -> Result<()> {
    let Some(editor) = self.rich_text_editor() else {
      return Ok(());
    };
    fidelity::event(FidelityClass::Projection, "apply-projection", || {
      format!(
        "session={} paragraphs={} blocks={} frontier_bytes={}",
        self.session,
        document.paragraphs.len(),
        document.blocks.len(),
        document.frontier.len(),
      )
    });
    // Field fix 2026-07-07: full replaces also ride the ordered stream; this
    // event is only the pump (the sync preserves UI-cached asset bytes).
    let _ = &mut document;
    editor.update(cx, |editor, cx| editor.sync_projection_from_authority(cx));
    self.last_document_activity = Instant::now();
    self.refresh_external_carets(cx);
    Ok(())
  }

  fn publish_update_bytes(&self, bytes: Vec<u8>) {
    if bytes.is_empty() {
      tracing::trace!(session = %self.session, "skipping empty collaboration update publish");
      return;
    }
    let bytes_len = bytes.len();
    if let Err(error) = self.net_tx.try_send(NetCommand::Publish {
      session: self.session,
      payload: PublishPayload::Update(bytes),
    }) {
      tracing::warn!(session = %self.session, bytes = bytes_len, error = %error, "queueing collaboration update publish failed");
    } else {
      tracing::trace!(session = %self.session, bytes = bytes_len, "queued collaboration update publish from CRDT runtime event");
    }
  }

  pub fn handle_gossip(&mut self, from: flowstate_collab::ids::PeerId, msg: GossipMsg, cx: &mut Context<Self>) {
    let gossip_kind = msg.kind();
    let gossip_payload_bytes = msg.payload_len();
    tracing::trace!(session = %self.session, from = %from, gossip_kind, gossip_payload_bytes, "handling collaboration gossip message");
    self.known_peers.insert(from);
    self.note_inbound_traffic(cx);
    let result = match msg {
      // §perf: the gossip payload is owned; move it in to avoid a full-update memcpy.
      GossipMsg::Update(bytes) => self
        .import_update_bytes_owned(bytes, cx)
        .map(|()| asset_transfer::schedule_missing_assets(self, Some(from), cx)),
      GossipMsg::UpdateAvailable { blob, len } => {
        tracing::debug!(session = %self.session, from = %from, ?blob, bytes = len, "collaboration update available via direct blob pull");
        self.pull_blob(from, blob, cx);
        Ok(())
      },
      GossipMsg::Presence(bytes) => {
        self.apply_presence(from, &bytes, cx);
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
    self.publish_presence_snapshot();
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
    let vv = self.runtime_vv.clone();
    let action = self.anti_entropy.on_lagged(peer, vv, Instant::now());
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
    let editor = match editor {
      CollabEditor::RichText(editor) => editor,
      CollabEditor::Flow(editor) => {
        self.attach_flow_editor_hooks(&editor, cx);
        return;
      },
    };

    tracing::debug!(session = %self.session, "attaching collaboration editor hooks");
    editor.update(cx, |editor, cx| {
      if editor.document_path().is_none() {
        editor.set_recovery_path(Some(presence_view::collaboration_recovery_path(self.session, &self.title)), cx);
        tracing::debug!(session = %self.session, "set collaboration recovery path for untitled document");
      }
      // FS-080: publish the session role so the editor's `can_write_collaboration()`
      // gate blocks local edits/undo for a Viewer. Detach clears it.
      // Loro-first: no capture flags, no undo redirect — the editor's write
      // authority is the only write path and undo executes through it.
      editor.set_collaboration_role(Some(self.collaboration_role), cx);
    });

    // Spec §6 publication rule: every editor change means intents may have
    // committed under the gate; debounce, then drain the publish queue.
    self
      .editor_subscriptions
      .push(cx.observe(&editor, |session, _, cx| {
        session.schedule_publish_pump(cx);
        session.flush_pending_asset_records(cx);
      }));

    self
      .editor_subscriptions
      .push(cx.subscribe(&editor, |session, _, event: &EditorEvent, cx| {
        match event {
          EditorEvent::SelectionChanged { .. } => {
            tracing::trace!(session = %session.session, "collaboration local selection changed; refreshing presence");
            session.schedule_own_presence_refresh(cx);
          },
          EditorEvent::ReconciliationRecovery {
            dropped_batches,
            reason,
            total_recoveries,
          } => {
            // A recovery means the editor's optimistic state diverged from the
            // canonical projection and had to be rebuilt: a fidelity failure.
            fidelity::violation(FidelityClass::Reconcile, "reconciliation-recovery", || {
              format!(
                "session={} dropped_batches={dropped_batches} total_recoveries={total_recoveries} reason={reason}",
                session.session,
              )
            });
            tracing::warn!(
              session = %session.session,
              dropped_batches,
              total_recoveries,
              %reason,
              "editor reconciliation recovery; optimistic state diverged from the canonical projection",
            );
          },
          _ => {},
        }
      }));
    tracing::debug!(session = %self.session, subscriptions = self.editor_subscriptions.len(), "collaboration editor hooks attached");
  }

  /// FLOW editor hooks (spec Part C): observe → debounced publish pump;
  /// pathless joined tabs get a debounced `.fl0` recovery file.
  fn attach_flow_editor_hooks(&mut self, editor: &Entity<FlowEditor>, cx: &mut Context<Self>) {
    tracing::debug!(session = %self.session, "attaching collaboration flow editor hooks");
    editor.update(cx, |editor, cx| {
      if editor.document_path().is_none() {
        editor.set_recovery_path(Some(presence_view::collaboration_flow_recovery_path(self.session, &self.title)), cx);
      }
    });
    self
      .editor_subscriptions
      .push(cx.observe(editor, |session, _, cx| {
        session.schedule_publish_pump(cx);
      }));
    self
      .editor_subscriptions
      .push(cx.subscribe(editor, |session, _, event: &FlowEditorEvent, cx| {
        match event {
          FlowEditorEvent::Changed => session.schedule_publish_pump(cx),
          FlowEditorEvent::ActiveCellChanged(_) | FlowEditorEvent::ActiveSheetChanged(_) => {
            // Step 11 (presence): board focus rides the presence channel.
            session.schedule_own_presence_refresh(cx);
          },
        }
      }));
    tracing::debug!(session = %self.session, subscriptions = self.editor_subscriptions.len(), "collaboration flow editor hooks attached");
  }

  fn schedule_publish_pump(&mut self, cx: &mut Context<Self>) {
    if self.publish_pump_pending {
      return;
    }
    self.publish_pump_pending = true;
    cx.spawn(async move |session, cx| {
      Timer::after(Duration::from_millis(LOCAL_UPDATE_PUBLISH_DEBOUNCE_MS)).await;
      let _ = session.update(cx, |session, cx| {
        session.publish_pump_pending = false;
        session.pump_publish(cx);
        session.flush_pending_asset_records(cx);
      });
    })
    .detach();
  }
}
