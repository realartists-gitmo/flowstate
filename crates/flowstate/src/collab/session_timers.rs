use std::time::{Duration, Instant};

use anyhow::Result;
use flowstate_collab::{net::NetCommand, presence::PRESENCE_KEEPALIVE_SECS, self_check};
use gpui::{Context, Timer};

use crate::rich_text_element::Document;

use super::{Attachment, CollabSession, Connectivity, DetachReason, SessionNotice, SessionPhase};

const ZERO_NEIGHBOR_OFFLINE_GRACE: Duration = Duration::from_secs(5);
const QUIET_DIGEST_ROUNDS_OFFLINE: u8 = 2;
const SELF_CHECK_INTERVAL: Duration = Duration::from_secs(30);
const SELF_CHECK_IDLE: Duration = Duration::from_secs(2);
const RECOVERY_MAX_BACKOFF: Duration = Duration::from_secs(30);
const RECOVERY_NEIGHBOR_WAIT: Duration = Duration::from_secs(10);

impl CollabSession {
  pub(super) fn attach_timers(&mut self, cx: &mut Context<Self>) {
    if self.timers_started {
      tracing::trace!(session = %self.session, "collaboration timers already started");
      return;
    }
    self.timers_started = true;
    tracing::debug!(session = %self.session, "starting collaboration timers");
    self.spawn_digest_timer(cx);
    self.spawn_presence_timer(cx);
    self.spawn_self_check_timer(cx);
  }

  pub(super) fn note_inbound_traffic(&mut self, cx: &mut Context<Self>) {
    self.inbound_since_last_digest = true;
    self.quiet_digest_rounds = 0;
    tracing::trace!(session = %self.session, "noted inbound collaboration network traffic");
    if self.mark_online(cx) {
      self.anti_entropy.on_reconnect();
      self.publish_digest();
    }
  }

  pub(super) fn mark_online(&mut self, cx: &mut Context<Self>) -> bool {
    if self
      .awaiting_recovery_neighbor_until
      .is_some_and(|deadline| self.peers_present() > 0 && self.neighbors.is_empty() && Instant::now() < deadline)
    {
      return false;
    }

    let mut changed = false;
    if let SessionPhase::Attached(attachment) = &mut self.phase
      && matches!(attachment.connectivity, Connectivity::Offline { .. })
    {
      attachment.connectivity = Connectivity::Online;
      self.next_recovery_at = None;
      self.awaiting_recovery_neighbor_until = None;
      self.probe_pending = false;
      self.last_probe_failed = false;
      changed = true;
    }
    if changed {
      tracing::info!(session = %self.session, "collaboration session marked online");
      cx.notify();
    }
    changed
  }

  pub(super) fn evaluate_connectivity(&mut self, cx: &mut Context<Self>) {
    if !matches!(self.phase, SessionPhase::Attached(_)) {
      tracing::trace!(session = %self.session, phase = ?self.phase, "skipping collaboration connectivity evaluation for inactive phase");
      return;
    }

    self.refresh_peer_count();
    if self.peers_present() == 0 || !self.neighbors.is_empty() {
      tracing::trace!(session = %self.session, peers_present = self.peers_present(), neighbors = self.neighbors.len(), "collaboration connectivity considered online");
      self.mark_online(cx);
      return;
    }

    let now = Instant::now();
    let zero_since = *self.zero_neighbors_since.get_or_insert(now);
    let zero_long_enough = now.saturating_duration_since(zero_since) >= ZERO_NEIGHBOR_OFFLINE_GRACE;
    let quiet_enough = self.quiet_digest_rounds >= QUIET_DIGEST_ROUNDS_OFFLINE;
    tracing::trace!(
      session = %self.session,
      peers_present = self.peers_present(),
      neighbors = self.neighbors.len(),
      quiet_rounds = self.quiet_digest_rounds,
      quiet_enough,
      zero_long_enough,
      endpoint_online = self.endpoint_online,
      last_probe_failed = self.last_probe_failed,
      "evaluating collaboration connectivity",
    );
    if zero_long_enough && quiet_enough && (!self.endpoint_online || self.last_probe_failed || !self.start_connectivity_probe(cx)) {
      self.mark_offline(now, cx);
    }
  }

  fn spawn_digest_timer(&mut self, cx: &mut Context<Self>) {
    let session_id = self.session;
    tracing::debug!(session = %session_id, "starting collaboration digest timer");
    cx.spawn(async move |session, cx| {
      loop {
        let delay = match session.update(cx, |session, _| session.next_digest_delay()) {
          Ok(Some(delay)) => delay,
          Ok(None) | Err(_) => break,
        };
        Timer::after(delay).await;
        match session.update(cx, |session, cx| session.run_digest_tick(cx)) {
          Ok(true) => {},
          Ok(false) | Err(_) => break,
        }
      }
      tracing::debug!(session = %session_id, "collaboration digest timer stopped");
    })
    .detach();
  }

  fn spawn_presence_timer(&mut self, cx: &mut Context<Self>) {
    let session_id = self.session;
    tracing::debug!(session = %session_id, "starting collaboration presence timer");
    cx.spawn(async move |session, cx| {
      loop {
        Timer::after(Duration::from_secs(PRESENCE_KEEPALIVE_SECS)).await;
        match session.update(cx, |session, cx| session.run_presence_tick(cx)) {
          Ok(true) => {},
          Ok(false) | Err(_) => break,
        }
      }
      tracing::debug!(session = %session_id, "collaboration presence timer stopped");
    })
    .detach();
  }

  fn spawn_self_check_timer(&mut self, cx: &mut Context<Self>) {
    let session_id = self.session;
    tracing::debug!(session = %session_id, "starting collaboration self-check timer");
    cx.spawn(async move |session, cx| {
      loop {
        Timer::after(SELF_CHECK_INTERVAL).await;
        match session.update(cx, |session, cx| session.run_self_check_tick(cx)) {
          Ok(true) => {},
          Ok(false) | Err(_) => break,
        }
      }
      tracing::debug!(session = %session_id, "collaboration self-check timer stopped");
    })
    .detach();
  }

  fn next_digest_delay(&self) -> Option<Duration> {
    self
      .timer_live()
      .then(|| self.anti_entropy.duration_until_digest(Instant::now()))
  }

  fn run_digest_tick(&mut self, cx: &mut Context<Self>) -> bool {
    if !self.timer_live() {
      return false;
    }

    let now = Instant::now();
    if self.anti_entropy.digest_due(now) {
      if matches!(self.phase, SessionPhase::Attached(_)) {
        tracing::trace!(session = %self.session, quiet_rounds = self.quiet_digest_rounds, inbound_since_last_digest = self.inbound_since_last_digest, "collaboration digest timer fired");
        self.publish_digest();
        self.finish_digest_round(now, cx);
      }
      self.anti_entropy.mark_digest_sent(now);
    }
    true
  }

  fn run_presence_tick(&mut self, cx: &mut Context<Self>) -> bool {
    if !self.timer_live() {
      return false;
    }
    if self.presence.is_some() {
      tracing::trace!(session = %self.session, "collaboration presence timer fired");
      let roster_changed = self.remove_outdated_presence(cx);
      self.refresh_own_presence(cx);
      let peer_count_changed = self.refresh_peer_count();
      self.evaluate_connectivity(cx);
      self.refresh_external_carets(cx);
      if roster_changed || peer_count_changed {
        cx.notify();
      }
    }
    true
  }

  fn run_self_check_tick(&mut self, cx: &mut Context<Self>) -> bool {
    if !self.timer_live() {
      return false;
    }
    if !matches!(self.phase, SessionPhase::Attached(_)) || self.last_document_activity.elapsed() < SELF_CHECK_IDLE {
      tracing::trace!(session = %self.session, phase = ?self.phase, idle_for = ?self.last_document_activity.elapsed(), "skipping collaboration self-check tick");
      return true;
    }
    tracing::trace!(session = %self.session, idle_for = ?self.last_document_activity.elapsed(), "running collaboration self-check tick");
    if let Err(error) = self.run_self_check(cx) {
      tracing::error!(session = %self.session, error = %format_args!("{error:#}"), "collaboration self-check failed");
    }
    true
  }

  fn finish_digest_round(&mut self, now: Instant, cx: &mut Context<Self>) {
    if self.inbound_since_last_digest {
      self.quiet_digest_rounds = 0;
    } else {
      self.quiet_digest_rounds = self.quiet_digest_rounds.saturating_add(1);
    }
    self.inbound_since_last_digest = false;
    tracing::trace!(session = %self.session, quiet_rounds = self.quiet_digest_rounds, "finished collaboration digest round");
    self.evaluate_connectivity(cx);
    self.run_recovery_if_due(now, cx);
  }

  fn run_recovery_if_due(&mut self, now: Instant, cx: &mut Context<Self>) {
    let Some(next_recovery_at) = self.next_recovery_at else {
      return;
    };
    if now < next_recovery_at {
      return;
    }

    let mut attempted = false;
    if let SessionPhase::Attached(Attachment {
      connectivity: Connectivity::Offline { retries, .. },
      ..
    }) = &mut self.phase
    {
      let delay = recovery_delay(*retries);
      *retries = retries.saturating_add(1);
      self.next_recovery_at = Some(now + delay);
      attempted = true;
    }

    if attempted {
      tracing::warn!(session = %self.session, bootstrap_count = self.bootstrap_addrs.len(), "attempting collaboration connectivity recovery");
      self.awaiting_recovery_neighbor_until = Some(now + RECOVERY_NEIGHBOR_WAIT);
      if self.bootstrap_addrs.is_empty() {
        if let Err(error) = self.net_tx.try_send(NetCommand::EnsureUp) {
          tracing::warn!(session = %self.session, error = %error, "queueing collaboration ensure-up recovery failed");
        }
      } else {
        if let Err(error) = self.net_tx.try_send(NetCommand::JoinSession {
          session: self.session,
          bootstrap: self.bootstrap_addrs.clone(),
        }) {
          tracing::warn!(session = %self.session, error = %error, "queueing collaboration join-session recovery failed");
        }
      }
      self.publish_digest();
      cx.notify();
    }
  }

  fn run_self_check(&mut self, cx: &mut Context<Self>) -> Result<()> {
    let Some(runtime) = self.runtime.as_ref() else {
      return Ok(());
    };
    let Some(editor) = self.editor.clone() else {
      return Ok(());
    };

    let live_document = editor.read(cx).document().clone();
    let live_hash = self_check::projection_hash(&live_document);
    let current_vv = runtime.doc().oplog_vv().encode();
    // Cheap path: if neither the Loro state (version vector) nor the live
    // projection hash changed since the last verified check, there can be no new
    // drift, so skip the full reprojection.
    if self
      .last_self_check
      .as_ref()
      .is_some_and(|(vv, hash)| *vv == current_vv && *hash == live_hash)
    {
      tracing::trace!(session = %self.session, "collaboration projection self-check skipped (unchanged)");
      return Ok(());
    }

    let mut projected = runtime.projection_snapshot()?;
    projected.assets = live_document.assets.clone();
    let projected_hash = self_check::projection_hash(&projected);
    if live_hash == projected_hash {
      self.last_self_check = Some((current_vv, live_hash));
      tracing::trace!(session = %self.session, live_hash = %format_args!("{live_hash:016x}"), "collaboration projection self-check passed");
      return Ok(());
    }

    tracing::error!(
      session = %self.session,
      live_hash = %format_args!("{live_hash:016x}"),
      projected_hash = %format_args!("{projected_hash:016x}"),
      vv_bytes = current_vv.len(),
      "collaboration projection drift detected",
    );
    self.rebuild_from_projection(projected, cx)
  }

  fn rebuild_from_projection(&mut self, projected: Document, cx: &mut Context<Self>) -> Result<()> {
    let Some(editor) = self.editor.clone() else {
      return Ok(());
    };
    tracing::warn!(session = %self.session, paragraphs = projected.paragraphs.len(), blocks = projected.blocks.len(), "rebuilding editor document from collaboration projection");
    editor.update(cx, |editor, cx| editor.replace_document_from_collaboration(projected, cx));
    self.last_document_activity = Instant::now();
    self.refresh_external_carets(cx);
    tracing::info!(session = %self.session, "rebuilt editor document from collaboration projection");
    cx.emit(SessionNotice::ViewRebuilt);
    Ok(())
  }

  fn start_connectivity_probe(&mut self, cx: &mut Context<Self>) -> bool {
    if self.probe_pending {
      return true;
    }
    let Some(runtime) = &self.runtime else {
      return false;
    };
    let candidates = self.known_peers.iter().copied().collect::<Vec<_>>();
    if candidates.is_empty() {
      return false;
    }
    let our_vv = runtime.doc().oplog_vv().encode();
    let (reply_tx, reply_rx) = async_channel::bounded(1);
    if self
      .net_tx
      .try_send(NetCommand::PullUpdates {
        session: self.session,
        candidates,
        our_vv,
        reply: reply_tx,
      })
      .is_err()
    {
      return false;
    }

    self.probe_pending = true;
    cx.spawn(async move |session, cx| {
      let result = reply_rx.recv().await;
      let _ = session.update(cx, |session, cx| {
        session.probe_pending = false;
        match result {
          Ok(Ok(bytes)) => {
            session.last_probe_failed = false;
            if !bytes.is_empty()
              && let Err(error) = session.import_update_bytes(&bytes, cx)
            {
              session.detach(
                DetachReason::Fatal(format!("probing collaboration peer failed to apply updates: {error:#}")),
                cx,
              );
              return;
            }
            session.note_inbound_traffic(cx);
          },
          Ok(Err(_)) | Err(_) => {
            session.last_probe_failed = true;
            session.evaluate_connectivity(cx);
          },
        }
      });
    })
    .detach();
    true
  }

  fn mark_offline(&mut self, now: Instant, cx: &mut Context<Self>) {
    let session_id = self.session;
    let quiet_rounds = self.quiet_digest_rounds;
    let neighbors = self.neighbors.len();
    if let SessionPhase::Attached(attachment) = &mut self.phase
      && matches!(attachment.connectivity, Connectivity::Online)
    {
      let peers_present = attachment.peers_present;
      attachment.connectivity = Connectivity::Offline { since: now, retries: 0 };
      self.next_recovery_at = Some(now);
      tracing::warn!(session = %session_id, quiet_rounds, neighbors, peers_present, "collaboration session marked offline");
      cx.notify();
    }
  }

  fn timer_live(&self) -> bool {
    !matches!(self.phase, SessionPhase::Detached(_))
  }
}

fn recovery_delay(retries: u32) -> Duration {
  let shift = retries.min(5);
  let secs = 1_u64
    .checked_shl(shift)
    .unwrap_or(RECOVERY_MAX_BACKOFF.as_secs());
  Duration::from_secs(secs).min(RECOVERY_MAX_BACKOFF)
}
