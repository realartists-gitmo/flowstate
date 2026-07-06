use std::{
  collections::HashSet,
  rc::Rc,
  time::{Duration, Instant},
};

use anyhow::{Context as _, Result, anyhow, bail};
use flowstate_collab::{
  SessionCapability, SessionId,
  crdt_runtime::{CrdtRuntime, EditorCommitResult, RuntimeEvent},
  crdt_runtime_actor::CrdtRuntimeHandle,
  ids::PeerId,
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

use crate::app_settings::{load_document_theme, load_local_user_identity};
use crate::rich_text_element::{
  AssetId, AssetRecord, CollaborationRole, DocumentProjection, EditorEvent, RichTextEditor, SemanticEditCommand, UndoRedirect,
};

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
  pub document: DocumentProjection,
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
  runtime: Option<CrdtRuntimeHandle>,
  runtime_vv: Vec<u8>,
  editor: Option<Entity<RichTextEditor>>,
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
  undo_tx: async_channel::Sender<UndoRedirect>,
  undo_rx: async_channel::Receiver<UndoRedirect>,
  editor_subscriptions: Vec<Subscription>,
  loro_subscriptions: Vec<LoroSubscription>,
  neighbors: HashSet<flowstate_collab::ids::PeerId>,
  bootstrap_addrs: Vec<PeerAddr>,
  // FS-080: the owner-signed capability we present at the direct handshake, kept
  // so reconnects can re-supply it. `None` for the owner (this endpoint is the
  // session owner and needs no ticket).
  capability: Option<SessionCapability>,
  // The role this endpoint holds in the session; gates local writes/undo and is
  // pushed onto the editor at attach so `can_write_collaboration()` takes effect.
  collaboration_role: CollaborationRole,
  asset_pulls_in_flight: HashSet<AssetId>,
  anti_entropy: AntiEntropyState,
  direct_pump_started: bool,
  undo_pump_started: bool,
  presence_refresh_pending: bool,
  presence_refresh_generation: u64,
  external_caret_refresh_pending: bool,
  external_caret_refresh_generation: u64,
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
  last_self_check: Option<(Vec<u8>, u64)>,
  local_edit_flush_pending: bool,
}

const DIRECT_REQUEST_CHANNEL_CAPACITY: usize = 32;
const UNDO_REQUEST_CHANNEL_CAPACITY: usize = 128;
const JOIN_FIRST_NEIGHBOR_TIMEOUT: Duration = Duration::from_secs(15);
const JOIN_SNAPSHOT_TIMEOUT: Duration = Duration::from_secs(30);
const LOCAL_EDIT_FLUSH_DEBOUNCE_MS: u64 = 24;

fn coalesce_collaboration_commands(commands: impl IntoIterator<Item = SemanticEditCommand>) -> Vec<SemanticEditCommand> {
  let mut coalesced = Vec::new();
  for command in commands {
    if let Some(previous) = coalesced.last_mut()
      && merge_adjacent_insert_text(previous, &command)
    {
      continue;
    }
    coalesced.push(command);
  }
  coalesced
}

fn merge_adjacent_insert_text(previous: &mut SemanticEditCommand, next: &SemanticEditCommand) -> bool {
  let SemanticEditCommand::InsertText { at, text, styles } = previous else {
    return false;
  };
  let SemanticEditCommand::InsertText {
    at: next_at,
    text: next_text,
    styles: next_styles,
  } = next
  else {
    return false;
  };
  if *styles != *next_styles || at.paragraph != next_at.paragraph || at.byte + text.len() != next_at.byte {
    return false;
  }
  text.push_str(next_text);
  true
}

/// Stable short tag for a runtime event variant, used only by fidelity firehose
/// lines so an event stream reads which transition was applied.
fn runtime_event_kind(event: &RuntimeEvent) -> &'static str {
  match event {
    RuntimeEvent::LocalUpdate { .. } => "local-update",
    RuntimeEvent::RemoteUpdateApplied { .. } => "remote-update-applied",
    RuntimeEvent::RevisionOpened { .. } => "revision-opened",
    RuntimeEvent::RevisionForked { .. } => "revision-forked",
    RuntimeEvent::SelectionRestored { .. } => "selection-restored",
    RuntimeEvent::ProjectionUpdated { .. } => "projection-updated",
    RuntimeEvent::ProjectionPatched { .. } => "projection-patched",
  }
}

impl CollabSession {
  pub fn from_local_runtime(
    session: SessionId,
    panel_id: Uuid,
    editor: Entity<RichTextEditor>,
    title: String,
    runtime: CrdtRuntimeHandle,
    net_tx: CommandSender,
  ) -> Self {
    // Direct serving is network-bound and should backpressure rather than grow without limit.
    let (direct_tx, direct_rx) = async_channel::bounded(DIRECT_REQUEST_CHANNEL_CAPACITY);
    // Undo redirects can burst under key-repeat, but a stuck pump should not retain unbounded UI events.
    let (undo_tx, undo_rx) = async_channel::bounded(UNDO_REQUEST_CHANNEL_CAPACITY);
    let now = Instant::now();
    tracing::info!(
      %session,
      %panel_id,
      title = %title,
      "attached local collaboration session to document runtime",
    );

    Self {
      session,
      title,
      phase: SessionPhase::Creating,
      runtime: Some(runtime),
      runtime_vv: Vec::new(),
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
      undo_tx,
      undo_rx,
      editor_subscriptions: Vec::new(),
      loro_subscriptions: Vec::new(),
      neighbors: HashSet::new(),
      bootstrap_addrs: Vec::new(),
      // The endpoint that starts a session is its owner: full write access, no
      // bearer capability to present.
      capability: None,
      collaboration_role: CollaborationRole::Owner,
      asset_pulls_in_flight: HashSet::new(),
      anti_entropy: AntiEntropyState::new(session, now),
      direct_pump_started: false,
      undo_pump_started: false,
      presence_refresh_pending: false,
      presence_refresh_generation: 0,
      external_caret_refresh_pending: false,
      external_caret_refresh_generation: 0,
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
      last_self_check: None,
      local_edit_flush_pending: false,
    }
  }

  pub fn joining(
    session: SessionId,
    title: String,
    net_tx: CommandSender,
    bootstrap_addrs: Vec<PeerAddr>,
    capability: SessionCapability,
  ) -> Self {
    let collaboration_role = CollaborationRole::from(capability.role);
    let (direct_tx, direct_rx) = async_channel::bounded(DIRECT_REQUEST_CHANNEL_CAPACITY);
    let (undo_tx, undo_rx) = async_channel::bounded(UNDO_REQUEST_CHANNEL_CAPACITY);
    let now = Instant::now();
    tracing::info!(%session, title = %title, bootstrap_count = bootstrap_addrs.len(), "created joining collaboration session state");
    Self {
      session,
      title,
      phase: SessionPhase::Joining(JoinStage::Resolving),
      runtime: None,
      runtime_vv: Vec::new(),
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
      undo_tx,
      undo_rx,
      editor_subscriptions: Vec::new(),
      loro_subscriptions: Vec::new(),
      neighbors: HashSet::new(),
      bootstrap_addrs,
      capability: Some(capability),
      collaboration_role,
      asset_pulls_in_flight: HashSet::new(),
      anti_entropy: AntiEntropyState::new(session, now),
      direct_pump_started: false,
      undo_pump_started: false,
      presence_refresh_pending: false,
      presence_refresh_generation: 0,
      external_caret_refresh_pending: false,
      external_caret_refresh_generation: 0,
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
      last_self_check: None,
      local_edit_flush_pending: false,
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
    // Hand the direct-serve path a shared read handle to the runtime's Loro doc so
    // snapshot/update pulls are answered off the mutation actor's queue (they can't
    // be starved behind local edits). Absent a runtime yet, it falls back to the
    // actor-served path.
    DirectSessionHandler::new(self.direct_tx.clone(), self.runtime.as_ref().map(CrdtRuntimeHandle::read_doc))
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
      self.import_update_bytes(&update, cx)?;
    }
    if std::mem::take(&mut self.pending_remote_updates_overflowed)
      && let Some(from) = self.pull_candidates(None).first().copied()
    {
      tracing::info!(session = %self.session, from = %from, "starting collaboration resync pull after pre-attach queue overflow");
      // Also go through the dedup so this recovery pull registers the slot its
      // `finish_pull` will later clear (never clearing a digest pull's slot).
      if let GapAction::Pull { from, our_vv } = self.anti_entropy.begin_pull(from, self.runtime_vv.clone(), std::time::Instant::now()) {
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
    self.attach_undo_request_pump(cx);
    self.attach_timers(cx);
    self.refresh_runtime_version_vector(cx);
  }

  pub fn runtime_handle(&self) -> Option<CrdtRuntimeHandle> {
    self.runtime.clone()
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
        && let (Ok(old), Ok(new)) = (
          loro::VersionVector::decode(&self.runtime_vv),
          loro::VersionVector::decode(&new_vv),
        )
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
  /// `CrdtRuntimeHandle::checkpoint_package`. Recording that named revision is a
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

    let doc = LoroDoc::new();
    flowstate_document::loro_schema::configure_text_styles(&doc);
    doc
      .import_with(snapshot, "remote")
      .context("importing collaboration snapshot failed")?;
    let runtime = CrdtRuntime::from_doc(doc, None, None).context("creating joined collaboration CRDT runtime")?;
    let mut document = runtime
      .projection_snapshot()
      .context("projecting joined Loro-native document")?;
    let runtime = CrdtRuntimeHandle::spawn(runtime).context("starting joined collaboration CRDT runtime actor")?;
    // §15/§31: bind this joiner's durable author identity to the joined runtime
    // so their revisions record an author and `users_by_id` is populated. The
    // user-registration op converges to peers via anti-entropy. Fire-and-forget
    // and non-fatal: a failure must not break the join. This runtime later
    // reaches `create_document_panel` through the `Handle` source, which
    // deliberately skips re-binding to avoid a redundant second call.
    let identity_runtime = runtime.clone();
    cx.spawn(async move |session, cx| {
      let (user_id, display_name) = cx
        .background_executor()
        .spawn(async { load_local_user_identity() })
        .await;
      match identity_runtime
        .set_author_identity(user_id, display_name)
        .await
      {
        Ok(events) => {
          let applied = session.update(cx, |session, cx| session.apply_runtime_events(events, false, cx));
          if let Ok(Err(error)) = applied {
            tracing::warn!(error = %format_args!("{error:#}"), "publishing durable author identity update failed");
          }
        },
        Err(error) => {
          tracing::warn!(error = %format_args!("{error:#}"), "binding durable author identity to joined collaboration runtime failed");
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

    self.runtime = Some(runtime);
    fidelity::event(FidelityClass::Frontier, "runtime-vv-reset", || {
      format!("session={} source=join-snapshot prior_bytes={}", self.session, self.runtime_vv.len())
    });
    self.runtime_vv.clear();
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
    self.flush_pending_asset_records(cx);
    if let Some(editor) = self.editor.clone() {
      self.flush_local_edits(editor, cx);
    }

    if let Some(editor) = self.editor.clone() {
      editor.update(cx, |editor, cx| {
        editor.set_recovery_path(None, cx);
        editor.set_collaboration_role(None, cx);
        editor.set_session_undo_redirect(None);
        editor.set_session_capture(false);
        editor.set_runtime_capture(true);
        editor.set_own_collaboration_caret_color(None, cx);
        editor.clear_undo_redo_stacks();
        editor.set_external_carets(Vec::new(), cx);
      });
    }

    self.editor_subscriptions.clear();
    self.loro_subscriptions.clear();
    self.presence = None;
    self.runtime = None;
    self.runtime_vv.clear();
    self.pending_asset_records.clear();
    self.pending_remote_updates.clear();
    self.pending_remote_update_bytes = 0;
    self.pending_remote_updates_overflowed = false;
    self.local_edit_flush_pending = false;
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

  pub fn flush_local_edits(&mut self, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) {
    if matches!(self.phase, SessionPhase::Detached(_) | SessionPhase::Joining(_)) {
      tracing::trace!(session = %self.session, phase = ?self.phase, "skipping local collaboration edit flush for inactive phase");
      return;
    }

    if !self.collaboration_role.can_write() {
      // FS-080: a viewer must never mutate the shared document. The editor
      // already gates write commands via `can_write_collaboration()`; discard
      // any edits that were captured defensively so they can never reach Loro.
      let dropped = editor.update(cx, |editor, _| editor.take_pending_session_edits().len());
      if dropped > 0 {
        tracing::warn!(session = %self.session, dropped, "discarded local edits captured in a view-only collaboration session");
      }
      return;
    }

    if editor.read(cx).runtime_transaction_in_flight() {
      tracing::trace!(session = %self.session, "deferring local collaboration edit flush until the current runtime transaction is acknowledged");
      return;
    }

    let edits = editor.update(cx, |editor, _| editor.take_pending_session_edits());
    let edit_count = edits.len();
    let retry_edits = edits.clone();
    let transaction_id = edits
      .iter()
      .find(|edit| edit.transaction_id != 0)
      .map(|edit| edit.transaction_id)
      .unwrap_or_else(|| Uuid::new_v4().as_u128());
    let base_frontier = edits
      .iter()
      .find(|edit| !edit.semantic_commands.is_empty())
      .map(|edit| edit.base_frontier.clone())
      .unwrap_or_default();
    debug_assert!(
      edits
        .iter()
        .filter(|edit| !edit.semantic_commands.is_empty())
        .all(|edit| edit.base_frontier == base_frontier),
      "queued collaboration commands must share one projection frontier",
    );
    let selection_after = edits
      .iter()
      .rev()
      .find_map(|edit| edit.selection_after.clone());
    let stable_selection_after = edits
      .iter()
      .rev()
      .find_map(|edit| edit.stable_selection_after.clone());
    let commands = coalesce_collaboration_commands(edits.into_iter().flat_map(|edit| edit.semantic_commands));
    let operation_count = commands.len();
    if edit_count == 0 || operation_count == 0 {
      tracing::trace!(session = %self.session, edit_count, operation_count, "no local collaboration edits to flush");
      return;
    }
    tracing::debug!(session = %self.session, edit_count, operation_count, "flushing local collaboration edits into Loro");
    let Some(runtime) = self.runtime.clone() else {
      tracing::warn!(session = %self.session, edit_count, operation_count, "cannot flush local collaboration edits because Loro doc is missing");
      editor.update(cx, |editor, _| editor.prepend_pending_semantic_edits(retry_edits));
      return;
    };
    if fidelity::enabled() {
      let in_flight = editor.read(cx).runtime_transaction_in_flight();
      fidelity::check(
        !in_flight,
        FidelityClass::Reconcile,
        "concurrent-transaction",
        || format!("session={} txn={transaction_id:x} began a local flush while a runtime transaction was already in flight", self.session),
      );
    }
    fidelity::event(FidelityClass::Reconcile, "local-flush-begin", || {
      format!(
        "session={} txn={transaction_id:x} base_frontier_bytes={} edit_count={edit_count} operation_count={operation_count}",
        self.session,
        base_frontier.len(),
      )
    });
    editor.update(cx, |editor, _| editor.begin_runtime_transaction(transaction_id));
    let assets = editor
      .read(cx)
      .document()
      .assets
      .assets
      .values()
      .cloned()
      .collect();
    let session_id = self.session;
    cx.spawn(async move |session, cx| {
      let result = runtime
        .apply_editor_commands(transaction_id, base_frontier, commands, assets, selection_after.clone())
        .await;
      match result {
        Ok(commit) => {
          let applied = session.update(cx, |session, cx| {
            let result = session.apply_local_runtime_events(commit, stable_selection_after.clone(), cx);
            if result.is_ok() {
              session.last_document_activity = Instant::now();
            }
            result
          });
          let materialization_error = match applied {
            Ok(Ok(())) => None,
            Ok(Err(error)) => Some(format!("{error:#}")),
            Err(error) => {
              tracing::debug!(session = %session_id, %error, "collaboration session disappeared while applying local commit");
              return;
            },
          };
          if let Some(detail) = materialization_error {
            tracing::warn!(session = %session_id, error = %detail, "local committed projection failed; repairing from canonical runtime snapshot");
            match runtime.projection_snapshot().await {
              Ok(document) => {
                let _ = session.update(cx, |session, cx| {
                  fidelity::event(FidelityClass::Reconcile, "local-commit-abort", || {
                    format!("session={session_id} txn={transaction_id:x} reason=materialization-repair")
                  });
                  if let Some(editor) = session.editor.clone() {
                    editor.update(cx, |editor, cx| {
                      editor.replace_document_projection_replaying_pending(document, Vec::new(), selection_after, cx);
                      editor.abort_runtime_transaction(cx);
                    });
                  }
                  session.last_document_activity = Instant::now();
                  session.last_self_check = None;
                  session.refresh_external_carets(cx);
                });
              },
              Err(error) => {
                let _ = session.update(cx, |session, cx| {
                  fidelity::event(FidelityClass::Reconcile, "local-commit-abort", || {
                    format!("session={session_id} txn={transaction_id:x} reason=materialization-repair-failed")
                  });
                  if let Some(editor) = session.editor.clone() {
                    editor.update(cx, |editor, cx| editor.abort_runtime_transaction(cx));
                  }
                  session.detach(
                    DetachReason::Fatal(format!("canonical projection repair failed after local commit: {error:#}")),
                    cx,
                  );
                });
              },
            }
          }
        },
        Err(error) => {
          let stale = error
            .downcast_ref::<flowstate_collab::crdt_runtime::StaleProjectionError>()
            .is_some();
          if stale {
            match runtime.projection_snapshot().await {
              Ok(document) => {
                tracing::debug!(session = %session_id, "rebasing stale optimistic collaboration transaction on canonical projection");
                let _ = session.update(cx, |session, cx| {
                  fidelity::event(FidelityClass::Reconcile, "local-commit-abort", || {
                    format!("session={session_id} txn={transaction_id:x} reason=stale-rebase")
                  });
                  if let Some(editor) = session.editor.clone() {
                    editor.update(cx, |editor, cx| {
                      editor.replace_document_projection_replaying_pending(document, retry_edits, None, cx);
                      editor.abort_runtime_transaction(cx);
                    });
                  }
                });
              },
              Err(snapshot_error) => {
                let _ = session.update(cx, |session, cx| {
                  fidelity::event(FidelityClass::Reconcile, "local-commit-abort", || {
                    format!("session={session_id} txn={transaction_id:x} reason=stale-repair-failed")
                  });
                  if let Some(editor) = session.editor.clone() {
                    editor.update(cx, |editor, cx| editor.abort_runtime_transaction(cx));
                  }
                  session.detach(
                    DetachReason::Fatal(format!("canonical projection repair failed after stale local transaction: {snapshot_error:#}")),
                    cx,
                  );
                });
              },
            }
          } else {
            tracing::error!(session = %session_id, error = %format_args!("{error:#}"), "local collaboration transaction was rejected; restoring canonical projection");
            match runtime.projection_snapshot().await {
              Ok(document) => {
                let _ = session.update(cx, |session, cx| {
                  fidelity::event(FidelityClass::Reconcile, "local-commit-abort", || {
                    format!("session={session_id} txn={transaction_id:x} reason=rejected")
                  });
                  if let Some(editor) = session.editor.clone() {
                    editor.update(cx, |editor, cx| {
                      editor.replace_document_projection_replaying_pending(document, Vec::new(), None, cx);
                      editor.abort_runtime_transaction(cx);
                    });
                  }
                  session.last_self_check = None;
                  session.refresh_external_carets(cx);
                });
              },
              Err(snapshot_error) => {
                let _ = session.update(cx, |session, cx| {
                  fidelity::event(FidelityClass::Reconcile, "local-commit-abort", || {
                    format!("session={session_id} txn={transaction_id:x} reason=rejected-repair-failed")
                  });
                  if let Some(editor) = session.editor.clone() {
                    editor.update(cx, |editor, cx| editor.abort_runtime_transaction(cx));
                  }
                  session.detach(
                    DetachReason::Fatal(format!(
                      "canonical projection repair failed after rejected local transaction: {snapshot_error:#}; original error: {error:#}"
                    )),
                    cx,
                  );
                });
              },
            }
          }
        },
      }
    })
    .detach();
  }

  pub(super) fn apply_runtime_events(&mut self, events: Vec<RuntimeEvent>, apply_projection: bool, cx: &mut Context<Self>) -> Result<()> {
    // §hang-watchdog: the main-thread handler for each incoming collab update.
    let apply_started = std::time::Instant::now();
    let event_count = events.len();
    for event in events {
      fidelity::event(FidelityClass::Projection, "apply-runtime-event", || {
        format!("session={} kind={} apply_projection={apply_projection}", self.session, runtime_event_kind(&event))
      });
      match event {
        RuntimeEvent::LocalUpdate { bytes, version_vector, .. } => {
          self.update_runtime_vv(version_vector, "local-update", true);
          self.publish_update_bytes(bytes);
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
              match self.anti_entropy.begin_pull(from, self.runtime_vv.clone(), std::time::Instant::now()) {
                GapAction::Pull { from, our_vv } => self.start_update_pull(from, our_vv, cx),
                _ => tracing::debug!(session = %self.session, from = %from, "pending-dependency pull skipped; one is already in flight for this peer"),
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
          if let Some(editor) = self.editor.clone() {
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
    let apply_ms = apply_started.elapsed().as_millis();
    if apply_ms > 150 {
      tracing::warn!("slow collab apply_runtime_events (hang watchdog): {apply_ms}ms, events={event_count}");
    }
    Ok(())
  }

  fn apply_local_runtime_events(
    &mut self,
    commit: EditorCommitResult,
    selection_after: Option<crate::rich_text_element::StableEditorSelection>,
    cx: &mut Context<Self>,
  ) -> Result<()> {
    let transaction_id = commit.transaction_id;
    fidelity::event(FidelityClass::Reconcile, "local-commit-complete", || {
      format!(
        "session={} txn={transaction_id:x} base_frontier_bytes={} new_frontier_bytes={} events={} projection_events={}",
        self.session,
        commit.base_frontier.len(),
        commit.new_frontier.len(),
        commit.events.len(),
        commit.projection_event_count(),
      )
    });
    if commit.projection_event_count() > 1 {
      bail!(
        "local editor transaction {} returned multiple projection transitions",
        commit.transaction_id
      );
    }
    let frontier = commit.new_frontier;
    // The editor is acknowledged (`complete_runtime_transaction`) at `frontier`;
    // the single projection transition carried by this commit must land on that
    // same frontier, or the editor would close its optimistic gate against a
    // projection it never materialized.
    if fidelity::enabled() {
      for event in &commit.events {
        let event_frontier = match event {
          RuntimeEvent::ProjectionPatched { batch, .. } => Some(batch.new_frontier.as_slice()),
          RuntimeEvent::ProjectionUpdated { frontier, .. } => Some(frontier.as_slice()),
          _ => None,
        };
        if let Some(event_frontier) = event_frontier {
          fidelity::check(
            event_frontier == frontier.as_slice(),
            FidelityClass::Frontier,
            "ack-frontier-mismatch",
            || {
              format!(
                "session={} txn={transaction_id:x} ack_frontier_bytes={} projection_event_frontier_bytes={}",
                self.session,
                frontier.len(),
                event_frontier.len(),
              )
            },
          );
        }
      }
    }
    self.apply_runtime_events(commit.events, true, cx)?;
    if let Some(editor) = self.editor.clone() {
      editor
        .update(cx, |editor, cx| {
          editor.complete_runtime_transaction(commit.transaction_id, frontier, selection_after, cx)
        })
        .map_err(anyhow::Error::new)?;
    }
    self.last_self_check = None;
    self.refresh_external_carets(cx);
    Ok(())
  }

  fn apply_runtime_patches(&mut self, batch: flowstate_document::ProjectionPatchBatch, cx: &mut Context<Self>) -> Result<()> {
    let Some(editor) = self.editor.clone() else {
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
    editor
      .update(cx, |editor, cx| editor.apply_projection_patch_batch(&batch, cx))
      .map_err(anyhow::Error::new)?;
    self.last_document_activity = Instant::now();
    self.last_self_check = None;
    self.refresh_external_carets(cx);
    Ok(())
  }

  fn apply_runtime_projection(&mut self, mut document: DocumentProjection, cx: &mut Context<Self>) -> Result<()> {
    let Some(editor) = self.editor.clone() else {
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
    let current = editor.read(cx).document().clone();
    document.assets = current.assets;
    document.theme = current.theme;
    editor.update(cx, |editor, cx| {
      editor.replace_document_projection_replaying_pending(document, Vec::new(), None, cx);
    });
    self.last_document_activity = Instant::now();
    self.last_self_check = None;
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
      GossipMsg::Update(bytes) => self
        .import_update_bytes(&bytes, cx)
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
      // FS-080: signed revocation control frames are consumed at the transport
      // layer (see `net::swarm::handle_event`) and never reach a session; this
      // arm keeps the match exhaustive defensively.
      GossipMsg::CapabilityEpoch { .. } => Ok(()),
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

    tracing::debug!(session = %self.session, "attaching collaboration editor hooks");
    editor.update(cx, |editor, cx| {
      if editor.document_path().is_none() {
        editor.set_recovery_path(Some(presence_view::collaboration_recovery_path(self.session, &self.title)), cx);
        tracing::debug!(session = %self.session, "set collaboration recovery path for untitled document");
      }
      // FS-080: publish the session role so the editor's `can_write_collaboration()`
      // gate blocks local edits/undo for a Viewer. Detach clears it.
      editor.set_collaboration_role(Some(self.collaboration_role), cx);
      editor.clear_undo_redo_stacks();
      editor.set_runtime_capture(false);
      editor.set_session_capture(true);
      editor.set_native_undo_hook(None);
      let undo_tx = self.undo_tx.clone();
      editor.set_session_undo_redirect(Some(Rc::new(move |redirect: UndoRedirect| {
        if let Err(error) = undo_tx.try_send(redirect) {
          tracing::warn!(%error, "collaboration undo redirect dropped because the undo queue is saturated");
        }
      })));
    });

    self
      .editor_subscriptions
      .push(cx.observe(&editor, |session, editor, cx| {
        session.schedule_local_edit_flush(editor.clone(), cx);
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

  fn schedule_local_edit_flush(&mut self, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) {
    if self.local_edit_flush_pending {
      return;
    }
    self.local_edit_flush_pending = true;
    cx.spawn(async move |session, cx| {
      Timer::after(Duration::from_millis(LOCAL_EDIT_FLUSH_DEBOUNCE_MS)).await;
      let _ = session.update(cx, |session, cx| {
        session.local_edit_flush_pending = false;
        session.flush_local_edits(editor, cx);
        session.flush_pending_asset_records(cx);
      });
    })
    .detach();
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

#[cfg(test)]
mod tests {
  use super::*;
  use crate::rich_text_element::{DocumentOffset, RunStyles};

  #[test]
  fn coalesces_adjacent_insert_text_commands() {
    let styles = RunStyles::default();
    let commands = vec![
      SemanticEditCommand::InsertText {
        at: DocumentOffset { paragraph: 0, byte: 0 },
        text: "h".to_string(),
        styles,
      },
      SemanticEditCommand::InsertText {
        at: DocumentOffset { paragraph: 0, byte: 1 },
        text: "i".to_string(),
        styles,
      },
      SemanticEditCommand::InsertText {
        at: DocumentOffset { paragraph: 1, byte: 0 },
        text: "!".to_string(),
        styles,
      },
    ];

    let coalesced = coalesce_collaboration_commands(commands);

    assert_eq!(coalesced.len(), 2);
    let SemanticEditCommand::InsertText { at, text, .. } = &coalesced[0] else {
      panic!("expected merged insert");
    };
    assert_eq!(*at, DocumentOffset { paragraph: 0, byte: 0 });
    assert_eq!(text, "hi");
  }
}
