use std::{collections::HashMap, time::Duration};

use flowstate_collab::{
  crdt_runtime::RuntimePresenceCaretRequest,
  presence::{PresenceState, PresenceStore},
};
use gpui::{Context, Timer};

use super::{CollabSession, SessionNotice};

const PRESENCE_REFRESH_DEBOUNCE: Duration = Duration::from_millis(50);

impl CollabSession {
  pub(super) fn apply_presence(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
    if let Some(presence) = &self.presence {
      tracing::trace!(session = %self.session, bytes = bytes.len(), "applying remote collaboration presence update");
      let before = remote_roster(presence);
      if let Err(error) = presence.apply(bytes) {
        tracing::warn!(session = %self.session, bytes = bytes.len(), error = %format_args!("{error:#}"), "collaboration presence update failed");
        return;
      }
      let roster_changed = self.emit_presence_roster_diff(before, cx);
      let peer_count_changed = self.refresh_peer_count();
      self.evaluate_connectivity(cx);
      self.refresh_external_carets(cx);
      if roster_changed || peer_count_changed {
        cx.notify();
      }
    } else {
      tracing::debug!(session = %self.session, bytes = bytes.len(), "ignored collaboration presence update before local presence was established");
    }
  }

  pub(super) fn refresh_own_presence(&mut self, cx: &mut Context<Self>) {
    if self.presence.is_none() {
      tracing::trace!(session = %self.session, "skipping own collaboration presence refresh because store is missing");
      return;
    }
    let selection = self.editor.as_ref().map(|editor| editor.read(cx).selection().clone());
    let runtime = self.runtime.clone();
    let session_id = self.session;
    cx.spawn(async move |session, cx| {
      let selection = match (runtime, selection) {
        (Some(runtime), Some(selection)) => runtime.presence_selection(selection).await,
        _ => Ok(None),
      };
      let _ = session.update(cx, |session, cx| match selection {
        Ok(selection) => {
          let state = PresenceState {
            name: default_presence_name(),
            selection,
          };
          tracing::trace!(session = %session_id, has_selection = state.selection.is_some(), "refreshing own collaboration presence");
          if let Some(presence) = &session.presence
            && let Err(error) = presence.set_self(&state)
          {
            tracing::warn!(session = %session_id, error = %format_args!("{error:#}"), "collaboration presence encode failed");
          }
          if session.refresh_peer_count() {
            cx.notify();
          }
        },
        Err(error) => {
          tracing::warn!(session = %session_id, error = %format_args!("{error:#}"), "resolving own collaboration selection failed");
        },
      });
    })
    .detach();
  }

  pub(super) fn schedule_own_presence_refresh(&mut self, cx: &mut Context<Self>) {
    if self.presence.is_none() || self.presence_refresh_pending {
      return;
    }
    self.presence_refresh_pending = true;
    cx.spawn(async move |session, cx| {
      Timer::after(PRESENCE_REFRESH_DEBOUNCE).await;
      let _ = session.update(cx, |session, cx| {
        session.presence_refresh_pending = false;
        session.refresh_own_presence(cx);
      });
    })
    .detach();
  }

  pub(super) fn remove_outdated_presence(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(presence) = &self.presence else {
      return false;
    };
    let before = remote_roster(presence);
    presence.remove_outdated();
    self.emit_presence_roster_diff(before, cx)
  }

  pub(super) fn refresh_external_carets(&mut self, cx: &mut Context<Self>) {
    let Some(presence) = &self.presence else {
      tracing::trace!(session = %self.session, "skipping external caret refresh because presence is missing");
      return;
    };
    let Some(runtime) = self.runtime.clone() else {
      tracing::trace!(session = %self.session, "skipping external caret refresh because Loro doc is missing");
      return;
    };
    let Some(editor) = self.editor.clone() else {
      tracing::trace!(session = %self.session, "skipping external caret refresh because editor is missing");
      return;
    };
    let self_key = presence.self_key().to_string();
    let requests = presence
      .roster()
      .into_iter()
      .filter(|entry| entry.key != self_key)
      .filter_map(|entry| {
        entry.selection.map(|selection| RuntimePresenceCaretRequest {
          selection,
          color_rgb: entry.color_rgb,
        })
      })
      .collect();
    let session_id = self.session;
    cx.spawn(async move |session, cx| {
      let result = runtime.resolve_presence_carets(requests).await;
      let _ = session.update(cx, |session, cx| match result {
        Ok(resolved) => {
          tracing::trace!(session = %session_id, carets = resolved.carets.len(), "refreshing collaboration external carets");
          if session.editor.as_ref().is_some_and(|current| current == &editor) {
            editor.update(cx, |editor, cx| editor.set_external_carets(resolved.carets, cx));
          }
        },
        Err(error) => {
          tracing::warn!(session = %session_id, error = %format_args!("{error:#}"), "resolving collaboration external carets failed");
        },
      });
    })
    .detach();
  }

  pub(super) fn publish_presence_snapshot(&self) {
    if let Some(presence) = &self.presence {
      let bytes = presence.encode_all();
      tracing::debug!(session = %self.session, bytes = bytes.len(), "publishing collaboration presence snapshot");
      self.publish_presence_bytes(bytes);
    } else {
      tracing::trace!(session = %self.session, "skipping collaboration presence snapshot because store is missing");
    }
  }

  pub(super) fn publish_presence_bytes(&self, bytes: Vec<u8>) {
    if bytes.is_empty() {
      tracing::trace!(session = %self.session, "skipping empty collaboration presence publish");
      return;
    }
    let byte_len = bytes.len();
    if let Err(error) = self
      .net_tx
      .try_send(flowstate_collab::net::NetCommand::Publish {
        session: self.session,
        payload: flowstate_collab::net::PublishPayload::Presence(bytes),
      })
    {
      tracing::warn!(session = %self.session, bytes = byte_len, error = %error, "queueing collaboration presence publish failed");
    } else {
      tracing::trace!(session = %self.session, bytes = byte_len, "queued collaboration presence publish");
    }
  }

  pub(super) fn refresh_peer_count(&mut self) -> bool {
    let peers_present = self.peers_present();
    let session_id = self.session;
    if let super::SessionPhase::Attached(attachment) = &mut self.phase {
      let previous = attachment.peers_present;
      attachment.peers_present = peers_present;
      if previous != peers_present {
        tracing::info!(session = %session_id, previous, peers_present, "collaboration peer presence count changed");
        return true;
      }
    }
    false
  }

  pub(super) fn peers_present(&self) -> usize {
    self
      .presence
      .as_ref()
      .map_or(0, |presence| presence.roster().len().saturating_sub(1))
  }

  fn emit_presence_roster_diff(&mut self, before: HashMap<String, String>, cx: &mut Context<Self>) -> bool {
    let Some(presence) = &self.presence else {
      return false;
    };
    let after = remote_roster(presence);
    let changed = before != after;
    for (key, name) in &after {
      if !before.contains_key(key) {
        cx.emit(SessionNotice::PeerJoined(name.clone()));
      }
    }
    for (key, name) in before {
      if !after.contains_key(&key) {
        cx.emit(SessionNotice::PeerLeft(name));
      }
    }
    changed
  }
}

fn remote_roster(presence: &PresenceStore) -> HashMap<String, String> {
  let self_key = presence.self_key().to_string();
  presence
    .roster()
    .into_iter()
    .filter(|entry| entry.key != self_key)
    .map(|entry| (entry.key, entry.name))
    .collect()
}

fn default_presence_name() -> String {
  std::env::var("FLOWSTATE_COLLAB_NAME")
    .or_else(|_| std::env::var("USER"))
    .or_else(|_| std::env::var("USERNAME"))
    .unwrap_or_else(|_| "Flowstate user".to_string())
}
