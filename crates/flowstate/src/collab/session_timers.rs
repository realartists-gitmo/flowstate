use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use flowstate_collab::{
  binding::DocBinding,
  net::NetCommand,
  presence::PRESENCE_KEEPALIVE_SECS,
  projection, self_check,
};
use gpui::{Context, Timer};

use crate::{app_settings::load_document_theme, rich_text_element::Document};

use super::{Attachment, CollabSession, Connectivity, SessionPhase};

const ZERO_NEIGHBOR_OFFLINE_GRACE: Duration = Duration::from_secs(5);
const QUIET_DIGEST_ROUNDS_OFFLINE: u8 = 2;
const SELF_CHECK_INTERVAL: Duration = Duration::from_secs(30);
const SELF_CHECK_IDLE: Duration = Duration::from_secs(2);
const RECOVERY_MAX_BACKOFF: Duration = Duration::from_secs(30);

impl CollabSession {
  pub(super) fn attach_timers(&mut self, cx: &mut Context<Self>) {
    if self.timers_started {
      return;
    }
    self.timers_started = true;
    self.spawn_digest_timer(cx);
    self.spawn_presence_timer(cx);
    self.spawn_self_check_timer(cx);
  }

  pub(super) fn note_inbound_traffic(&mut self, cx: &mut Context<Self>) {
    self.inbound_since_last_digest = true;
    self.quiet_digest_rounds = 0;
    if self.mark_online(cx) {
      self.anti_entropy.on_reconnect();
      self.publish_digest();
    }
  }

  pub(super) fn mark_online(&mut self, cx: &mut Context<Self>) -> bool {
    let mut changed = false;
    if let SessionPhase::Attached(attachment) = &mut self.phase
      && matches!(attachment.connectivity, Connectivity::Offline { .. })
    {
      attachment.connectivity = Connectivity::Online;
      self.next_recovery_at = None;
      changed = true;
    }
    if changed {
      cx.notify();
    }
    changed
  }

  pub(super) fn evaluate_connectivity(&mut self, cx: &mut Context<Self>) {
    if !matches!(self.phase, SessionPhase::Attached(_)) {
      return;
    }

    self.refresh_peer_count();
    if self.peers_present() == 0 || !self.neighbors.is_empty() {
      self.mark_online(cx);
      return;
    }

    let now = Instant::now();
    let zero_since = self.zero_neighbors_since.get_or_insert(now);
    let quiet_enough = self.quiet_digest_rounds >= QUIET_DIGEST_ROUNDS_OFFLINE || !self.endpoint_online;
    if quiet_enough && now.saturating_duration_since(*zero_since) >= ZERO_NEIGHBOR_OFFLINE_GRACE {
      self.mark_offline(now, cx);
    }
  }

  fn spawn_digest_timer(&mut self, cx: &mut Context<Self>) {
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
    })
    .detach();
  }

  fn spawn_presence_timer(&mut self, cx: &mut Context<Self>) {
    cx.spawn(async move |session, cx| {
      loop {
        Timer::after(Duration::from_secs(PRESENCE_KEEPALIVE_SECS)).await;
        match session.update(cx, |session, cx| session.run_presence_tick(cx)) {
          Ok(true) => {},
          Ok(false) | Err(_) => break,
        }
      }
    })
    .detach();
  }

  fn spawn_self_check_timer(&mut self, cx: &mut Context<Self>) {
    cx.spawn(async move |session, cx| {
      loop {
        Timer::after(SELF_CHECK_INTERVAL).await;
        match session.update(cx, |session, cx| session.run_self_check_tick(cx)) {
          Ok(true) => {},
          Ok(false) | Err(_) => break,
        }
      }
    })
    .detach();
  }

  fn next_digest_delay(&self) -> Option<Duration> {
    self.timer_live().then(|| self.anti_entropy.duration_until_digest(Instant::now()))
  }

  fn run_digest_tick(&mut self, cx: &mut Context<Self>) -> bool {
    if !self.timer_live() {
      return false;
    }

    let now = Instant::now();
    if self.anti_entropy.digest_due(now) {
      if matches!(self.phase, SessionPhase::Attached(_)) {
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
      if let Some(presence) = &self.presence {
        presence.remove_outdated();
      }
      self.refresh_own_presence(cx);
      self.refresh_peer_count();
      self.evaluate_connectivity(cx);
      self.refresh_external_carets(cx);
      cx.notify();
    }
    true
  }

  fn run_self_check_tick(&mut self, cx: &mut Context<Self>) -> bool {
    if !self.timer_live() {
      return false;
    }
    if !matches!(self.phase, SessionPhase::Attached(_)) || self.last_document_activity.elapsed() < SELF_CHECK_IDLE {
      return true;
    }
    if let Err(error) = self.run_self_check(cx) {
      eprintln!("flowstate collab self-check failed: {error:#}");
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
      if self.bootstrap_addrs.is_empty() {
        let _ = self.net_tx.try_send(NetCommand::EnsureUp);
      } else {
        let _ = self.net_tx.try_send(NetCommand::JoinSession {
          session: self.session,
          bootstrap: self.bootstrap_addrs.clone(),
        });
      }
      self.publish_digest();
      cx.notify();
    }
  }

  fn run_self_check(&mut self, cx: &mut Context<Self>) -> Result<()> {
    let Some(doc) = &self.doc else {
      return Ok(());
    };
    let Some(editor) = self.editor.clone() else {
      return Ok(());
    };

    let live_document = editor.read(cx).document().clone();
    let live_hash = self_check::projection_hash(&live_document);
    let mut projected = projection::document_from_loro(doc, load_document_theme())?;
    projected.assets = live_document.assets.clone();
    let projected_hash = self_check::projection_hash(&projected);
    if live_hash == projected_hash {
      return Ok(());
    }

    eprintln!(
      "flowstate collab projection drift detected: live_hash={live_hash:016x} projected_hash={projected_hash:016x} vv_bytes={}",
      doc.oplog_vv().encode().len()
    );
    self.rebuild_from_projection(projected, cx)
  }

  fn rebuild_from_projection(&mut self, projected: Document, cx: &mut Context<Self>) -> Result<()> {
    let Some(doc) = &self.doc else {
      return Ok(());
    };
    let Some(editor) = self.editor.clone() else {
      return Ok(());
    };
    editor.update(cx, |editor, cx| editor.replace_document_from_collaboration(projected, cx));
    let document = editor.read(cx).document().clone();
    self.binding = Some(DocBinding::build(doc, &document).context("rebuilding collaboration binding after self-check failed")?);
    self.last_document_activity = Instant::now();
    self.refresh_external_carets(cx);
    Ok(())
  }

  fn mark_offline(&mut self, now: Instant, cx: &mut Context<Self>) {
    if let SessionPhase::Attached(attachment) = &mut self.phase
      && matches!(attachment.connectivity, Connectivity::Online)
    {
      attachment.connectivity = Connectivity::Offline { since: now, retries: 0 };
      self.next_recovery_at = Some(now);
      cx.notify();
    }
  }

  fn timer_live(&self) -> bool {
    !matches!(self.phase, SessionPhase::Detached(_))
  }
}

fn recovery_delay(retries: u32) -> Duration {
  let shift = retries.min(5);
  let secs = 1_u64.checked_shl(shift).unwrap_or(RECOVERY_MAX_BACKOFF.as_secs());
  Duration::from_secs(secs).min(RECOVERY_MAX_BACKOFF)
}
