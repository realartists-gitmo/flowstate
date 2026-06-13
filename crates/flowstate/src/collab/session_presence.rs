use flowstate_collab::presence::{PresenceSelection, PresenceState};
use gpui::Context;

use super::{CollabSession, presence_view};

impl CollabSession {
  pub(super) fn apply_presence(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
    if let Some(presence) = &self.presence {
      if let Err(error) = presence.apply(bytes) {
        eprintln!("flowstate collab presence update failed: {error:#}");
      }
      self.refresh_peer_count();
      self.evaluate_connectivity(cx);
      self.refresh_external_carets(cx);
      cx.notify();
    }
  }

  pub(super) fn refresh_own_presence(&mut self, cx: &mut Context<Self>) {
    let Some(presence) = &self.presence else {
      return;
    };
    let state = PresenceState {
      name: default_presence_name(),
      selection: self.own_presence_selection(cx),
    };
    if let Err(error) = presence.set_self(&state) {
      eprintln!("flowstate collab presence encode failed: {error:#}");
    }
    self.refresh_peer_count();
  }

  pub(super) fn refresh_external_carets(&mut self, cx: &mut Context<Self>) {
    let Some(presence) = &self.presence else {
      return;
    };
    let Some(doc) = &self.doc else {
      return;
    };
    let Some(binding) = &self.binding else {
      return;
    };
    let Some(editor) = self.editor.clone() else {
      return;
    };
    let carets = presence_view::external_carets(doc, binding, presence);
    editor.update(cx, |editor, cx| editor.set_external_carets(carets, cx));
  }

  pub(super) fn publish_presence_snapshot(&self) {
    if let Some(presence) = &self.presence {
      self.publish_presence_bytes(presence.encode_all());
    }
  }

  pub(super) fn publish_presence_bytes(&self, bytes: Vec<u8>) {
    if bytes.is_empty() {
      return;
    }
    let _ = self.net_tx.try_send(flowstate_collab::net::NetCommand::Publish {
      session: self.session,
      payload: flowstate_collab::net::PublishPayload::Presence(bytes),
    });
  }

  pub(super) fn refresh_peer_count(&mut self) {
    let peers_present = self.peers_present();
    if let super::SessionPhase::Attached(attachment) = &mut self.phase {
      attachment.peers_present = peers_present;
    }
  }

  pub(super) fn peers_present(&self) -> usize {
    self
      .presence
      .as_ref()
      .map_or(0, |presence| presence.roster().len().saturating_sub(1))
  }

  fn own_presence_selection(&self, cx: &mut Context<Self>) -> Option<PresenceSelection> {
    let editor = self.editor.as_ref()?.read(cx);
    let binding = self.binding.as_ref()?;
    presence_view::selection_for_editor(editor, binding)
  }
}

fn default_presence_name() -> String {
  std::env::var("FLOWSTATE_COLLAB_NAME")
    .or_else(|_| std::env::var("USER"))
    .or_else(|_| std::env::var("USERNAME"))
    .unwrap_or_else(|_| "Flowstate user".to_string())
}
