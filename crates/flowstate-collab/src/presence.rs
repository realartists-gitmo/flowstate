//! Presence state and roster derivation.

use anyhow::{Context as _, Result};
use loro::{
  LoroValue, Subscription,
  awareness::{EphemeralStore, EphemeralStoreEvent},
};
use serde::{Deserialize, Serialize};

use crate::ids::{PALETTE, PeerId, SessionId};

pub const PRESENCE_TIMEOUT_MS: i64 = 30_000;
pub const PRESENCE_KEEPALIVE_SECS: u64 = 10;

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PresenceState {
  pub name: String,
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
    Self {
      store: EphemeralStore::new(PRESENCE_TIMEOUT_MS),
      self_key: peer_key(self_peer),
      self_color: color_for_peer(self_peer),
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
    self
      .store
      .set(&self.self_key, LoroValue::Binary(encode_state(state)?.into()));
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

  pub fn apply(&self, bytes: &[u8]) -> Result<()> {
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
  let mut key = String::with_capacity(peer.as_bytes().len() * 2);
  for byte in peer.as_bytes() {
    use std::fmt::Write as _;
    let _ = write!(&mut key, "{byte:02x}");
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
  let color_rgb = color_for_peer_key(&key)?;
  Some(RosterEntry {
    color_rgb,
    key,
    name: state.name,
    selection: state.selection,
  })
}

fn color_for_peer_key(key: &str) -> Option<u32> {
  let mut bytes = Vec::with_capacity(key.len() / 2);
  let mut chunks = key.as_bytes().chunks_exact(2);
  if !chunks.remainder().is_empty() {
    return None;
  }
  for chunk in &mut chunks {
    let text = std::str::from_utf8(chunk).ok()?;
    bytes.push(u8::from_str_radix(text, 16).ok()?);
  }
  Some(PALETTE[SessionId::color_index_for_peer_bytes(&bytes)])
}
