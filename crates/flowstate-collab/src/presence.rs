//! Presence state and roster derivation.

use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result};
use loro::{
  LoroValue, Subscription,
  awareness::{EphemeralStore, EphemeralStoreEvent},
};
use serde::{Deserialize, Serialize};

use crate::ids::{PALETTE, PeerId, SessionId};

pub const PRESENCE_TIMEOUT_MS: i64 = 30_000;
pub const PRESENCE_KEEPALIVE_SECS: u64 = 10;

/// Part B (FS-080) presence hardening caps. Display names and selection cursor
/// encodings are attacker-controlled once they arrive over gossip, so they are
/// clamped on encode and rejected past these bounds on apply.
pub const MAX_PRESENCE_NAME_BYTES: usize = 64;
pub const MAX_PRESENCE_CURSOR_BYTES: usize = 256;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PresenceState {
  pub name: String,
  pub color_rgb: u32,
  pub selection: Option<PresenceSelection>,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PresenceSelection {
  pub anchor: SelectionEndpoint,
  pub head: SelectionEndpoint,
  pub direction: SelectionDirection,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SelectionEndpoint {
  pub cursor: Vec<u8>,
  pub affinity: SelectionAffinity,
  pub visual_gravity: VisualGravity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SelectionAffinity {
  Before,
  After,
  Neutral,
}

/// §16: the presence wire enum mirrors `gpui_flowtext::SelectionAffinity`
/// variant-for-variant, so the editor's stored affinity converts directly
/// without re-deriving a side from selection direction. Conversion is
/// centralized here rather than scattered across the runtime.
impl From<gpui_flowtext::SelectionAffinity> for SelectionAffinity {
  fn from(value: gpui_flowtext::SelectionAffinity) -> Self {
    match value {
      gpui_flowtext::SelectionAffinity::Before => Self::Before,
      gpui_flowtext::SelectionAffinity::After => Self::After,
      gpui_flowtext::SelectionAffinity::Neutral => Self::Neutral,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum VisualGravity {
  Upstream,
  Downstream,
  Neutral,
}

/// §16: the presence wire enum mirrors `gpui_flowtext::VisualGravity`
/// variant-for-variant, so the editor's stored gravity converts directly.
impl From<gpui_flowtext::VisualGravity> for VisualGravity {
  fn from(value: gpui_flowtext::VisualGravity) -> Self {
    match value {
      gpui_flowtext::VisualGravity::Upstream => Self::Upstream,
      gpui_flowtext::VisualGravity::Downstream => Self::Downstream,
      gpui_flowtext::VisualGravity::Neutral => Self::Neutral,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SelectionDirection {
  Forward,
  Backward,
  None,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RosterEntry {
  pub key: String,
  pub name: String,
  pub color_rgb: u32,
  pub selection: Option<PresenceSelection>,
}

#[derive(Clone, Debug)]
pub struct PresenceStore {
  store: EphemeralStore,
  self_key: String,
  self_color: u32,
}

impl PresenceStore {
  #[must_use]
  pub fn new(self_peer: &PeerId) -> Self {
    Self::new_with_color(self_peer, color_for_peer(self_peer))
  }

  #[must_use]
  pub fn new_with_color(self_peer: &PeerId, color_rgb: u32) -> Self {
    Self {
      store: EphemeralStore::new(PRESENCE_TIMEOUT_MS),
      self_key: peer_key(self_peer),
      self_color: color_rgb & 0x00ff_ffff,
    }
  }

  #[must_use]
  pub fn self_key(&self) -> &str {
    &self.self_key
  }

  #[must_use]
  pub const fn self_color(&self) -> u32 {
    self.self_color
  }

  pub fn set_self(&self, state: &PresenceState) -> Result<()> {
    let sanitized = sanitize_presence_state(state);
    self
      .store
      .set(&self.self_key, LoroValue::Binary(encode_state(&sanitized)?.into()));
    Ok(())
  }

  pub fn delete_self(&self) {
    self.store.delete(&self.self_key);
  }

  #[must_use]
  pub fn encode_self(&self) -> Vec<u8> {
    self.store.encode(&self.self_key)
  }

  #[must_use]
  pub fn encode_all(&self) -> Vec<u8> {
    self.store.encode_all()
  }

  /// Apply a presence frame delivered by `from`, enforcing Part B trust rules:
  /// the frame may only add, overwrite, or remove `from`'s own entry (no
  /// impersonation or griefing of other peers), and its display name / selection
  /// cursors must be within [`MAX_PRESENCE_NAME_BYTES`] / [`MAX_PRESENCE_CURSOR_BYTES`].
  /// A violating frame is dropped (returns `Ok(())`); only a genuinely
  /// undecodable store payload is an error.
  pub fn apply_from(&self, from: &PeerId, bytes: &[u8]) -> Result<()> {
    let expected_key = peer_key(from);

    // Replay into a scratch store seeded with our current state to learn which
    // keys this frame would add, overwrite, or remove. Presence is a per-peer,
    // self-authored signal: any frame touching a key other than the delivering
    // peer's own is impersonation (or a relayed frame) and is dropped whole.
    let scratch = EphemeralStore::new(PRESENCE_TIMEOUT_MS);
    if let Err(error) = scratch.apply(&self.store.encode_all()) {
      tracing::warn!(error = %error, "seeding collaboration presence guard store failed");
    }
    let touched = Arc::new(Mutex::new(Vec::<String>::new()));
    let recorder = Arc::clone(&touched);
    let subscription = scratch.subscribe(Box::new(move |event: &EphemeralStoreEvent| {
      if let Ok(mut keys) = recorder.lock() {
        keys.extend(event.added.iter().cloned());
        keys.extend(event.updated.iter().cloned());
        keys.extend(event.removed.iter().cloned());
      }
      true
    }));
    scratch
      .apply(bytes)
      .map_err(|error| anyhow::anyhow!(error.to_string()))
      .context("decoding collaboration presence update failed")?;
    drop(subscription);

    let touched = touched
      .lock()
      .map_err(|_| anyhow::anyhow!("collaboration presence guard lock poisoned"))?;
    for key in touched.iter() {
      if key != &expected_key {
        tracing::warn!(from = %from, key = %key, "dropped collaboration presence frame that mutates another peer's entry");
        return Ok(());
      }
    }
    drop(touched);

    // Reject an oversized/malformed value for the delivering peer's own key
    // rather than importing it; sanitize-on-encode only binds honest peers.
    if let Some(LoroValue::Binary(value)) = scratch.get_all_states().get(&expected_key).cloned() {
      match decode_state(value.as_ref()) {
        Ok(state) if presence_state_within_caps(&state) => {},
        Ok(_) => {
          tracing::warn!(from = %from, "dropped collaboration presence frame exceeding display-name/selection caps");
          return Ok(());
        },
        Err(error) => {
          tracing::warn!(from = %from, error = %format_args!("{error:#}"), "dropped undecodable collaboration presence frame");
          return Ok(());
        },
      }
    }

    self
      .store
      .apply(bytes)
      .map_err(|error| anyhow::anyhow!(error.to_string()))
      .context("applying collaboration presence update failed")
  }

  pub fn remove_outdated(&self) {
    self.store.remove_outdated();
  }

  #[must_use]
  pub fn roster(&self) -> Vec<RosterEntry> {
    let mut entries = self
      .store
      .get_all_states()
      .into_iter()
      .filter_map(|(key, value)| roster_entry_from_value(key, value))
      .collect::<Vec<_>>();
    entries.sort_by(|left, right| {
      left
        .name
        .cmp(&right.name)
        .then_with(|| left.key.cmp(&right.key))
    });
    entries
  }

  pub fn subscribe_local_updates(&self, callback: impl Fn(&Vec<u8>) -> bool + Send + Sync + 'static) -> Subscription {
    self.store.subscribe_local_updates(Box::new(callback))
  }

  pub fn subscribe(&self, callback: impl Fn(&EphemeralStoreEvent) -> bool + Send + Sync + 'static) -> Subscription {
    self.store.subscribe(Box::new(callback))
  }
}

pub fn encode_state(state: &PresenceState) -> Result<Vec<u8>> {
  postcard::to_stdvec(state).context("encoding collaboration presence state failed")
}

pub fn decode_state(bytes: &[u8]) -> Result<PresenceState> {
  postcard::from_bytes(bytes).context("decoding collaboration presence state failed")
}

#[must_use]
pub fn peer_key(peer: &PeerId) -> String {
  // §perf: manual nibble→hex lookup avoids invoking the fmt machinery twice per byte.
  const HEX: &[u8; 16] = b"0123456789abcdef";
  let bytes = peer.as_bytes();
  let mut key = String::with_capacity(bytes.len() * 2);
  for &byte in bytes {
    key.push(HEX[(byte >> 4) as usize] as char);
    key.push(HEX[(byte & 0x0f) as usize] as char);
  }
  key
}

#[must_use]
pub fn color_for_peer(peer: &PeerId) -> u32 {
  PALETTE[SessionId::color_index_for_peer(peer)]
}

fn roster_entry_from_value(key: String, value: LoroValue) -> Option<RosterEntry> {
  let LoroValue::Binary(bytes) = value else {
    return None;
  };
  let state = decode_state(bytes.as_ref()).ok()?;
  Some(RosterEntry {
    color_rgb: state.color_rgb & 0x00ff_ffff,
    key,
    name: state.name,
    selection: state.selection,
  })
}

/// Clamp a presence state to the Part B caps before it is broadcast: strip
/// control characters, truncate the display name, and drop a selection whose
/// cursor encodings exceed the cap (a corrupt/oversized cursor is unusable).
fn sanitize_presence_state(state: &PresenceState) -> PresenceState {
  let selection = match &state.selection {
    Some(selection) if selection_within_caps(selection) => Some(selection.clone()),
    _ => None,
  };
  PresenceState {
    name: sanitize_display_name(&state.name),
    color_rgb: state.color_rgb & 0x00ff_ffff,
    selection,
  }
}

fn sanitize_display_name(name: &str) -> String {
  let cleaned: String = name.chars().filter(|c| !c.is_control()).collect();
  truncate_to_bytes(&cleaned, MAX_PRESENCE_NAME_BYTES)
}

fn truncate_to_bytes(text: &str, max_bytes: usize) -> String {
  if text.len() <= max_bytes {
    return text.to_string();
  }
  let mut end = max_bytes;
  while end > 0 && !text.is_char_boundary(end) {
    end -= 1;
  }
  text[..end].to_string()
}

/// Whether a decoded remote presence state satisfies the Part B caps. Applied
/// as a rejection gate on `apply_from` (we cannot rewrite raw store bytes in
/// place without re-broadcasting, so an out-of-cap frame is dropped instead).
fn presence_state_within_caps(state: &PresenceState) -> bool {
  state.name.len() <= MAX_PRESENCE_NAME_BYTES
    && !state.name.chars().any(|c| c.is_control())
    && state.selection.as_ref().is_none_or(selection_within_caps)
}

fn selection_within_caps(selection: &PresenceSelection) -> bool {
  selection.anchor.cursor.len() <= MAX_PRESENCE_CURSOR_BYTES && selection.head.cursor.len() <= MAX_PRESENCE_CURSOR_BYTES
}

#[cfg(test)]
mod tests {
  use iroh::SecretKey;

  use super::*;

  fn test_peer(seed: u8) -> PeerId {
    SecretKey::from_bytes(&[seed; 32]).public()
  }

  fn named_state(name: &str) -> PresenceState {
    PresenceState {
      name: name.to_string(),
      color_rgb: 0x3b82f6,
      selection: None,
    }
  }

  #[test]
  fn presence_frame_is_bound_to_the_delivering_peer() {
    let peer_a = test_peer(1);
    let peer_b = test_peer(2);
    let peer_c = test_peer(3);

    let store_a = PresenceStore::new(&peer_a);
    store_a
      .set_self(&named_state("Ada"))
      .expect("set self presence");
    let frame = store_a.encode_self();

    // Delivered by its author, A's frame is accepted into B's roster.
    let honest = PresenceStore::new(&peer_b);
    honest
      .apply_from(&peer_a, &frame)
      .expect("honest presence applies");
    assert!(
      honest
        .roster()
        .iter()
        .any(|entry| entry.key == peer_key(&peer_a)),
      "author-delivered presence must populate the roster",
    );

    // The identical frame delivered by an impersonating peer C is rejected: it
    // would mutate A's entry, which C is not authorized to touch.
    let guarded = PresenceStore::new(&peer_b);
    guarded
      .apply_from(&peer_c, &frame)
      .expect("impersonated presence is dropped, not fatal");
    assert!(
      guarded
        .roster()
        .iter()
        .all(|entry| entry.key != peer_key(&peer_a)),
      "presence delivered by a peer other than its author must be rejected",
    );
  }

  #[test]
  fn impersonating_peer_cannot_delete_another_peers_entry() {
    let peer_a = test_peer(4);
    let peer_b = test_peer(5);
    let peer_c = test_peer(6);

    // B has learned A's presence.
    let store_a = PresenceStore::new(&peer_a);
    store_a
      .set_self(&named_state("Ada"))
      .expect("set self presence");
    let victim = PresenceStore::new(&peer_b);
    victim
      .apply_from(&peer_a, &store_a.encode_self())
      .expect("learn A");
    assert!(
      victim
        .roster()
        .iter()
        .any(|entry| entry.key == peer_key(&peer_a))
    );

    // C crafts a frame that deletes A's entry and delivers it first-person.
    store_a.delete_self();
    let delete_frame = store_a.encode_self();
    victim
      .apply_from(&peer_c, &delete_frame)
      .expect("griefing delete is dropped, not fatal");
    assert!(
      victim
        .roster()
        .iter()
        .any(|entry| entry.key == peer_key(&peer_a)),
      "a peer must not be able to delete another peer's presence entry",
    );
  }

  #[test]
  fn display_name_is_sanitized_on_encode() {
    let long_tail = "z".repeat(200);
    let dirty = format!("Ada\u{7}\nLovelace{long_tail}");
    let sanitized = sanitize_display_name(&dirty);
    assert!(
      sanitized.len() <= MAX_PRESENCE_NAME_BYTES,
      "name must be capped to {MAX_PRESENCE_NAME_BYTES} bytes"
    );
    assert!(!sanitized.chars().any(|c| c.is_control()), "control characters must be stripped");
    assert!(sanitized.starts_with("AdaLovelace"), "leading printable characters are preserved");
  }

  #[test]
  fn multibyte_display_name_truncates_on_a_char_boundary() {
    // 'é' is two bytes in UTF-8; truncation must never split a code point.
    let name = "é".repeat(MAX_PRESENCE_NAME_BYTES);
    let sanitized = sanitize_display_name(&name);
    assert!(sanitized.len() <= MAX_PRESENCE_NAME_BYTES);
    assert!(std::str::from_utf8(sanitized.as_bytes()).is_ok());
  }

  #[test]
  fn oversized_presence_value_is_rejected_on_apply() {
    let peer_a = test_peer(7);
    let peer_b = test_peer(8);

    // Craft a frame that bypasses sanitize-on-encode by writing the raw state
    // straight into an ephemeral store under A's key.
    let raw = EphemeralStore::new(PRESENCE_TIMEOUT_MS);
    let oversized = named_state(&"q".repeat(MAX_PRESENCE_NAME_BYTES * 4));
    raw.set(&peer_key(&peer_a), LoroValue::Binary(encode_state(&oversized).expect("encode").into()));
    let frame = raw.encode(&peer_key(&peer_a));

    let guarded = PresenceStore::new(&peer_b);
    guarded
      .apply_from(&peer_a, &frame)
      .expect("oversized presence is dropped, not fatal");
    assert!(
      guarded.roster().is_empty(),
      "a presence value exceeding the display-name cap must be rejected",
    );
  }
}
