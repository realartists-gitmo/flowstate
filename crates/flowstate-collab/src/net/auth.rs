//! Per-session capability authentication shared by the direct and gossip
//! transports (FS-080).
//!
//! The registry tracks, per session:
//! - the session owner's public key (learned from the ticket on joiners, or
//!   from the local endpoint on the owner),
//! - the current capability revocation epoch, and
//! - which remote endpoints have presented a valid [`SessionCapability`] over
//!   the direct handshake, and with which role.
//!
//! Enforcement provided here:
//! - Direct requests (snapshot/updates/blob/asset) are only served to the
//!   session owner or to endpoints that authenticated with a valid,
//!   unexpired, current-epoch capability. This is owner-side enforcement: a
//!   malicious client cannot skip it.
//! - Gossip `Update`/`UpdateAvailable` frames delivered from an endpoint that
//!   authenticated as a viewer are dropped before import.
//!
//! Honest limitations (by design of the underlying transports):
//! - Document updates are authorless CRDT bytes and gossip only exposes the
//!   *delivering neighbor* (`delivered_from`), not the original author. A
//!   viewer whose frames are relayed through an editor neighbor, or one that
//!   never performed a direct handshake with this endpoint, is not caught by
//!   the gossip-layer role filter. The reliable enforcement point remains the
//!   direct-serve gate above (and honest peers dropping known-viewer frames).
//! - Capabilities are bearer grants: possession of the invite ticket grants
//!   the role to whichever endpoint presents it first-person.

use std::{
  collections::HashMap,
  sync::{Arc, OnceLock, RwLock},
};

use anyhow::{Result, anyhow, ensure};
use iroh::Signature;

use crate::{
  capability::{CapabilityRole, SessionCapability, verify_epoch_bump},
  ids::{PeerId, SessionId},
};

/// A remote endpoint that presented a valid capability over the direct handshake.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct AuthenticatedPeer {
  pub role: CapabilityRole,
  pub capability_epoch: u64,
  pub expires_at: u64,
}

#[derive(Debug)]
struct SessionAuthState {
  owner: PeerId,
  current_epoch: u64,
  peers: HashMap<PeerId, AuthenticatedPeer>,
}

/// Shared session authentication state. Cheap to clone; all clones observe the
/// same state.
#[derive(Clone, Debug, Default)]
pub struct SessionAuthRegistry {
  inner: Arc<RwLock<HashMap<SessionId, SessionAuthState>>>,
}

#[allow(
  clippy::significant_drop_tightening,
  reason = "each method holds the session lock across its whole small critical section; early-dropping adds noise without a real contention benefit"
)]
impl SessionAuthRegistry {
  /// Enable capability enforcement for `session` with the given owner key.
  ///
  /// Idempotent; re-configuring keeps the highest epoch seen so a signed bump
  /// cannot be rolled back by a later configure.
  pub fn configure_session(&self, session: SessionId, owner: PeerId, initial_epoch: u64) {
    let mut sessions = self.write();
    let state = sessions.entry(session).or_insert_with(|| SessionAuthState {
      owner,
      current_epoch: initial_epoch,
      peers: HashMap::new(),
    });
    state.owner = owner;
    state.current_epoch = state.current_epoch.max(initial_epoch);
    tracing::debug!(%session, owner = %owner, epoch = state.current_epoch, "configured collaboration session capability auth");
  }

  pub fn remove_session(&self, session: SessionId) {
    let removed = self.write().remove(&session).is_some();
    tracing::debug!(%session, removed, "removed collaboration session capability auth");
  }

  #[must_use]
  pub fn owner(&self, session: SessionId) -> Option<PeerId> {
    self.read().get(&session).map(|state| state.owner)
  }

  #[must_use]
  pub fn current_epoch(&self, session: SessionId) -> Option<u64> {
    self.read().get(&session).map(|state| state.current_epoch)
  }

  /// Verify a capability presented by `peer` over the direct handshake and, on
  /// success, record the endpoint as authenticated with the granted role.
  pub fn authenticate_peer(&self, session: SessionId, peer: PeerId, capability: &SessionCapability, now_unix: u64) -> Result<CapabilityRole> {
    let mut sessions = self.write();
    let state = sessions
      .get_mut(&session)
      .ok_or_else(|| anyhow!("collaboration session is not configured for capability auth"))?;
    ensure!(
      capability.owner == state.owner,
      "collaboration capability was signed by an unknown owner key"
    );
    capability.verify(session, now_unix, state.current_epoch)?;
    state.peers.insert(
      peer,
      AuthenticatedPeer {
        role: capability.role,
        capability_epoch: capability.capability_epoch,
        expires_at: capability.expires_at,
      },
    );
    tracing::info!(%session, peer = %peer, role = capability.role.label(), epoch = capability.capability_epoch, "collaboration peer authenticated with capability");
    Ok(capability.role)
  }

  /// Whether a direct data request from `peer` may be served.
  ///
  /// Allows: sessions without configured auth (bare-transport tests), the
  /// session owner, and peers whose recorded capability is still unexpired and
  /// at the current epoch.
  #[must_use]
  pub fn authorize_direct(&self, session: SessionId, peer: PeerId, now_unix: u64) -> bool {
    let sessions = self.read();
    let Some(state) = sessions.get(&session) else {
      return true;
    };
    if peer == state.owner {
      return true;
    }
    state
      .peers
      .get(&peer)
      .is_some_and(|entry| now_unix < entry.expires_at && entry.capability_epoch >= state.current_epoch)
  }

  /// Role recorded for `peer` at the direct handshake, if still valid.
  #[must_use]
  pub fn role_of(&self, session: SessionId, peer: PeerId, now_unix: u64) -> Option<CapabilityRole> {
    let sessions = self.read();
    let state = sessions.get(&session)?;
    let entry = state.peers.get(&peer)?;
    (now_unix < entry.expires_at && entry.capability_epoch >= state.current_epoch).then_some(entry.role)
  }

  /// Whether a gossip document-update frame delivered from `peer` should be
  /// dropped because the peer authenticated as a viewer. See the module docs
  /// for what this does and does not catch.
  #[must_use]
  pub fn should_drop_update_from(&self, session: SessionId, peer: PeerId, now_unix: u64) -> bool {
    self
      .role_of(session, peer, now_unix)
      .is_some_and(|role| !role.can_write())
  }

  /// Raise the session capability epoch (owner-local revocation), dropping
  /// authenticated peers whose capability predates the new epoch. Returns the
  /// effective epoch.
  pub fn bump_epoch(&self, session: SessionId, new_epoch: u64) -> Result<u64> {
    let mut sessions = self.write();
    let state = sessions
      .get_mut(&session)
      .ok_or_else(|| anyhow!("collaboration session is not configured for capability auth"))?;
    state.current_epoch = state.current_epoch.max(new_epoch);
    let before = state.peers.len();
    let current = state.current_epoch;
    state
      .peers
      .retain(|_, entry| entry.capability_epoch >= current);
    tracing::info!(%session, epoch = current, dropped_peers = before - state.peers.len(), "collaboration capability epoch bumped");
    Ok(current)
  }

  /// Apply a gossiped, owner-signed epoch bump control frame. Returns whether
  /// the bump was accepted.
  pub fn apply_epoch_bump(&self, session: SessionId, epoch: u64, signature: &Signature) -> bool {
    let owner = {
      let sessions = self.read();
      let Some(state) = sessions.get(&session) else {
        tracing::debug!(%session, epoch, "ignored collaboration epoch bump for unconfigured session");
        return false;
      };
      state.owner
    };
    if !verify_epoch_bump(&owner, session, epoch, signature) {
      tracing::warn!(%session, epoch, "rejected collaboration epoch bump with invalid owner signature");
      return false;
    }
    self.bump_epoch(session, epoch).is_ok()
  }

  fn read(&self) -> std::sync::RwLockReadGuard<'_, HashMap<SessionId, SessionAuthState>> {
    self
      .inner
      .read()
      .expect("collaboration auth registry lock poisoned")
  }

  fn write(&self) -> std::sync::RwLockWriteGuard<'_, HashMap<SessionId, SessionAuthState>> {
    self
      .inner
      .write()
      .expect("collaboration auth registry lock poisoned")
  }
}

static LOCAL_CAPABILITIES: OnceLock<RwLock<HashMap<SessionId, SessionCapability>>> = OnceLock::new();

fn local_capabilities() -> &'static RwLock<HashMap<SessionId, SessionCapability>> {
  LOCAL_CAPABILITIES.get_or_init(|| RwLock::new(HashMap::new()))
}

/// Store the capability this endpoint should present when pulling from peers
/// of `session` (installed at join, cleared at leave).
pub fn install_local_capability(session: SessionId, capability: SessionCapability) {
  if let Ok(mut capabilities) = local_capabilities().write() {
    capabilities.insert(session, capability);
    tracing::debug!(%session, "installed local collaboration capability");
  }
}

pub fn clear_local_capability(session: SessionId) {
  if let Ok(mut capabilities) = local_capabilities().write() {
    let removed = capabilities.remove(&session).is_some();
    tracing::debug!(%session, removed, "cleared local collaboration capability");
  }
}

#[must_use]
pub fn local_capability(session: SessionId) -> Option<SessionCapability> {
  local_capabilities()
    .read()
    .ok()
    .and_then(|capabilities| capabilities.get(&session).cloned())
}

#[cfg(test)]
mod tests {
  use iroh::SecretKey;

  use super::SessionAuthRegistry;
  use crate::{
    capability::{CapabilityRole, SessionCapability, sign_epoch_bump},
    ids::SessionId,
  };

  fn secret(seed: u8) -> SecretKey {
    SecretKey::from_bytes(&[seed; 32])
  }

  #[test]
  fn unconfigured_sessions_allow_direct_requests() {
    let registry = SessionAuthRegistry::default();
    let session = SessionId::from_bytes([1; 32]);
    assert!(registry.authorize_direct(session, secret(1).public(), 10));
  }

  #[test]
  fn configured_sessions_only_serve_owner_and_authenticated_peers() {
    let registry = SessionAuthRegistry::default();
    let session = SessionId::from_bytes([2; 32]);
    let owner = secret(1);
    let editor = secret(2).public();
    let stranger = secret(3).public();
    registry.configure_session(session, owner.public(), 0);

    assert!(registry.authorize_direct(session, owner.public(), 10));
    assert!(!registry.authorize_direct(session, editor, 10));
    assert!(!registry.authorize_direct(session, stranger, 10));

    let capability = SessionCapability::issue(&owner, session, CapabilityRole::Editor, 1_000, 0);
    let role = registry
      .authenticate_peer(session, editor, &capability, 10)
      .expect("valid capability should authenticate");
    assert_eq!(role, CapabilityRole::Editor);
    assert!(registry.authorize_direct(session, editor, 10));
    assert!(!registry.authorize_direct(session, editor, 1_000), "expired capability must stop authorizing");
    assert!(!registry.authorize_direct(session, stranger, 10));
  }

  #[test]
  fn stale_epoch_and_foreign_owner_capabilities_are_rejected() {
    let registry = SessionAuthRegistry::default();
    let session = SessionId::from_bytes([3; 32]);
    let owner = secret(4);
    let peer = secret(5).public();
    registry.configure_session(session, owner.public(), 2);

    let stale = SessionCapability::issue(&owner, session, CapabilityRole::Editor, 1_000, 1);
    assert!(registry.authenticate_peer(session, peer, &stale, 10).is_err());

    let forged = SessionCapability::issue(&secret(6), session, CapabilityRole::Editor, 1_000, 2);
    assert!(registry.authenticate_peer(session, peer, &forged, 10).is_err());

    let current = SessionCapability::issue(&owner, session, CapabilityRole::Editor, 1_000, 2);
    assert!(registry.authenticate_peer(session, peer, &current, 10).is_ok());
  }

  #[test]
  fn epoch_bump_revokes_previously_authenticated_peers() {
    let registry = SessionAuthRegistry::default();
    let session = SessionId::from_bytes([4; 32]);
    let owner = secret(7);
    let peer = secret(8).public();
    registry.configure_session(session, owner.public(), 0);

    let capability = SessionCapability::issue(&owner, session, CapabilityRole::Editor, 1_000, 0);
    registry
      .authenticate_peer(session, peer, &capability, 10)
      .expect("capability should authenticate");
    assert!(registry.authorize_direct(session, peer, 10));

    let epoch = registry.bump_epoch(session, 1).expect("bump should succeed");
    assert_eq!(epoch, 1);
    assert!(!registry.authorize_direct(session, peer, 10), "epoch bump must revoke stale peers");
    assert!(registry.authenticate_peer(session, peer, &capability, 10).is_err());

    let reissued = SessionCapability::issue(&owner, session, CapabilityRole::Editor, 1_000, 1);
    assert!(registry.authenticate_peer(session, peer, &reissued, 10).is_ok());
  }

  #[test]
  fn gossiped_epoch_bump_requires_a_valid_owner_signature() {
    let registry = SessionAuthRegistry::default();
    let session = SessionId::from_bytes([5; 32]);
    let owner = secret(9);
    registry.configure_session(session, owner.public(), 0);

    let forged = sign_epoch_bump(&secret(10), session, 3);
    assert!(!registry.apply_epoch_bump(session, 3, &forged));
    assert_eq!(registry.current_epoch(session), Some(0));

    let genuine = sign_epoch_bump(&owner, session, 3);
    assert!(registry.apply_epoch_bump(session, 3, &genuine));
    assert_eq!(registry.current_epoch(session), Some(3));
  }

  #[test]
  fn viewer_updates_are_flagged_for_drop_and_editor_updates_are_not() {
    let registry = SessionAuthRegistry::default();
    let session = SessionId::from_bytes([6; 32]);
    let owner = secret(11);
    let viewer = secret(12).public();
    let editor = secret(13).public();
    let stranger = secret(14).public();
    registry.configure_session(session, owner.public(), 0);

    let viewer_capability = SessionCapability::issue(&owner, session, CapabilityRole::Viewer, 1_000, 0);
    let editor_capability = SessionCapability::issue(&owner, session, CapabilityRole::Editor, 1_000, 0);
    registry
      .authenticate_peer(session, viewer, &viewer_capability, 10)
      .expect("viewer capability should authenticate");
    registry
      .authenticate_peer(session, editor, &editor_capability, 10)
      .expect("editor capability should authenticate");

    assert!(registry.should_drop_update_from(session, viewer, 10));
    assert!(!registry.should_drop_update_from(session, editor, 10));
    // Unknown senders are not dropped: gossip relays legitimately deliver
    // frames from peers that never authenticated with us directly.
    assert!(!registry.should_drop_update_from(session, stranger, 10));
  }
}
