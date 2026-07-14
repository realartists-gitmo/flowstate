use std::{collections::HashMap, time::Duration};

use flowstate_collab::{
  crdt_runtime::RuntimePresenceCaretRequest,
  ids::PeerId,
  presence::{PresenceState, PresenceStore},
};
use flowstate_fidelity::{self as fidelity, FidelityClass};
use gpui::{Context, Timer};

use super::{CollabSession, SessionNotice};

const PRESENCE_REFRESH_DEBOUNCE: Duration = Duration::from_millis(50);
const EXTERNAL_CARET_REFRESH_DEBOUNCE: Duration = Duration::from_millis(16);

impl CollabSession {
  pub(super) fn apply_presence(&mut self, from: PeerId, bytes: &[u8], cx: &mut Context<Self>) {
    if let Some(presence) = &self.presence {
      tracing::trace!(session = %self.session, from = %from, bytes = bytes.len(), "applying remote collaboration presence update");
      let before = remote_roster(presence);
      let before_keys = if fidelity::enabled() {
        roster_key_set(presence)
      } else {
        std::collections::HashSet::new()
      };
      if let Err(error) = presence.apply_from(&from, bytes) {
        fidelity::event(FidelityClass::Presence, "presence-reject", || {
          format!("session={} from={from} bytes={} error={error:#}", self.session, bytes.len())
        });
        tracing::warn!(session = %self.session, bytes = bytes.len(), error = %format_args!("{error:#}"), "collaboration presence update failed");
        return;
      }
      if fidelity::enabled() {
        // Presence is a per-peer, self-authored signal: `apply_from` only accepts
        // a frame that touches the delivering peer's own key. Any other key that
        // changed across the apply means a frame bound to the wrong peer slipped
        // through — an impersonation/misroute the trust gate should have dropped.
        let expected_key = flowstate_collab::presence::peer_key(&from);
        let after_keys = roster_key_set(presence);
        let offending: Vec<String> = after_keys
          .symmetric_difference(&before_keys)
          .filter(|key| **key != expected_key)
          .cloned()
          .collect();
        fidelity::check(offending.is_empty(), FidelityClass::Presence, "presence-key-unbound", || {
          format!(
            "session={} from={from} expected_key={expected_key} offending_keys={offending:?}",
            self.session
          )
        });
        fidelity::event(FidelityClass::Presence, "presence-apply", || {
          format!(
            "session={} from={from} bytes={} roster_keys={}",
            self.session,
            bytes.len(),
            after_keys.len()
          )
        });
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
    let selection = self
      .rich_text_editor()
      .map(|editor| editor.read(cx).selection().clone());
    let runtime = self.runtime.clone().and_then(|io| io.as_rich_text().cloned());
    let session_id = self.session;
    self.presence_refresh_generation = self.presence_refresh_generation.wrapping_add(1);
    let generation = self.presence_refresh_generation;
    cx.spawn(async move |session, cx| {
      let selection = match (runtime, selection) {
        (Some(runtime), Some(selection)) => runtime.presence_selection(selection).await,
        _ => Ok(None),
      };
      let _ = session.update(cx, |session, cx| match selection {
        Ok(selection) => {
          if session.presence_refresh_generation != generation {
            tracing::trace!(session = %session_id, generation, current_generation = session.presence_refresh_generation, "skipping stale own collaboration presence refresh");
            return;
          }
          let state = PresenceState {
            name: default_presence_name(),
            color_rgb: crate::app_settings::load_local_user_profile().color_rgb,
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
    if self.external_caret_refresh_pending {
      return;
    }
    self.external_caret_refresh_pending = true;
    cx.spawn(async move |session, cx| {
      Timer::after(EXTERNAL_CARET_REFRESH_DEBOUNCE).await;
      let _ = session.update(cx, |session, cx| {
        session.external_caret_refresh_pending = false;
        session.refresh_external_carets_now(cx);
      });
    })
    .detach();
  }

  pub(super) fn refresh_comment_annotations(&mut self, cx: &mut Context<Self>) {
    if self.comment_annotation_refresh_pending {
      return;
    }
    let Some(runtime) = self.runtime.clone().and_then(|io| io.as_rich_text().cloned()) else {
      return;
    };
    let Some(editor) = self.rich_text_editor() else { return };
    self.comment_annotation_refresh_pending = true;
    self.comment_annotation_refresh_generation = self.comment_annotation_refresh_generation.wrapping_add(1);
    let generation = self.comment_annotation_refresh_generation;
    let session_id = self.session;
    cx.spawn(async move |session, cx| {
      Timer::after(EXTERNAL_CARET_REFRESH_DEBOUNCE).await;
      let result = runtime.comments().await;
      let _ = session.update(cx, |session, cx| {
        if session.comment_annotation_refresh_generation != generation {
          return;
        }
        session.comment_annotation_refresh_pending = false;
        match result {
          Ok(comments)
            if session
              .rich_text_editor()
              .is_some_and(|current| current == editor) =>
          {
            let selections = comments
              .into_iter()
              .filter(|thread| !thread.resolved)
              .filter_map(|thread| thread.anchor)
              .map(|(start, end)| crate::rich_text_element::ExternalSelection {
                selection: crate::rich_text_element::EditorSelection::range(start, end),
                color_rgb: 0xd99a20,
              })
              .collect();
            editor.update(cx, |editor, cx| editor.set_annotation_selections(selections, cx));
          },
          Ok(_) => {},
          Err(error) => tracing::warn!(session = %session_id, error = %format_args!("{error:#}"), "refreshing comment annotations failed"),
        }
      });
    })
    .detach();
  }

  fn refresh_external_carets_now(&mut self, cx: &mut Context<Self>) {
    let Some(presence) = &self.presence else {
      tracing::trace!(session = %self.session, "skipping external caret refresh because presence is missing");
      return;
    };
    let Some(runtime) = self.runtime.clone().and_then(|io| io.as_rich_text().cloned()) else {
      tracing::trace!(session = %self.session, "skipping external caret refresh because no rich-text runtime is attached");
      return;
    };
    let Some(editor) = self.rich_text_editor() else {
      tracing::trace!(session = %self.session, "skipping external caret refresh because no rich-text editor is attached");
      return;
    };
    // §perf: self_key is only used for equality; borrow it instead of allocating a String.
    let self_key = presence.self_key();
    let requests: Vec<RuntimePresenceCaretRequest> = presence
      .roster()
      .into_iter()
      .filter(|entry| entry.key != self_key)
      .filter_map(|entry| {
        entry
          .selection
          .map(|selection| RuntimePresenceCaretRequest {
            selection,
            color_rgb: entry.color_rgb,
          })
      })
      .collect();
    let session_id = self.session;
    self.external_caret_refresh_generation = self.external_caret_refresh_generation.wrapping_add(1);
    let generation = self.external_caret_refresh_generation;
    if requests.is_empty() {
      tracing::trace!(session = %self.session, "clearing collaboration external carets because no remote selections are present");
      editor.update(cx, |editor, cx| {
        editor.set_external_carets(Vec::new(), cx);
        editor.set_external_selections(Vec::new(), cx);
      });
      return;
    }
    cx.spawn(async move |session, cx| {
      let result = runtime.resolve_presence_carets(requests).await;
      let _ = session.update(cx, |session, cx| match result {
        Ok(resolved) => {
          if session.external_caret_refresh_generation != generation {
            tracing::trace!(session = %session_id, generation, current_generation = session.external_caret_refresh_generation, "skipping stale collaboration external caret refresh");
            return;
          }
          tracing::trace!(session = %session_id, carets = resolved.carets.len(), "refreshing collaboration external carets");
          if session
            .rich_text_editor()
            .is_some_and(|current| current == editor)
          {
            if fidelity::enabled() {
              // A resolved external caret must land inside the editor's current
              // projection; a paragraph index past the end means the resolution
              // targeted a document the local editor no longer reflects.
              let paragraph_count = editor.read(cx).document().paragraphs.len();
              for caret in &resolved.carets {
                fidelity::check(
                  caret.offset.paragraph < paragraph_count,
                  FidelityClass::Presence,
                  "external-caret-invalid-offset",
                  || {
                    format!(
                      "session={session_id} paragraph={} byte={} paragraphs={paragraph_count}",
                      caret.offset.paragraph, caret.offset.byte,
                    )
                  },
                );
              }
              fidelity::event(FidelityClass::Presence, "external-carets-resolved", || {
                format!("session={session_id} carets={} paragraphs={paragraph_count}", resolved.carets.len())
              });
            }
            editor.update(cx, |editor, cx| {
              editor.set_external_selections(resolved.selections, cx);
              editor.set_external_carets(resolved.carets, cx);
            });
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
      let bytes = presence.encode_self();
      tracing::debug!(session = %self.session, bytes = bytes.len(), "publishing own collaboration presence snapshot");
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

/// Full set of roster keys (including this endpoint's own). Used only by the
/// fidelity presence-binding invariant to detect a frame that mutated a key
/// other than the delivering peer's own.
fn roster_key_set(presence: &PresenceStore) -> std::collections::HashSet<String> {
  presence
    .roster()
    .into_iter()
    .map(|entry| entry.key)
    .collect()
}

fn default_presence_name() -> String {
  crate::app_settings::load_local_user_profile().display_name
}
