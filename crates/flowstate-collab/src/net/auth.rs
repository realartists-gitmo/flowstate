//! Symmetric bearer admission shared by every editor in a live session.

use std::{
  collections::{HashMap, HashSet},
  sync::{Arc, OnceLock, RwLock},
};

use anyhow::{Result, anyhow, ensure};

use crate::{
  admission::SessionAdmission,
  ids::{PeerId, SessionId},
};

#[derive(Debug)]
struct SessionAuthState {
  admission: SessionAdmission,
  peers: HashSet<PeerId>,
}

#[derive(Clone, Debug, Default)]
pub struct SessionAuthRegistry {
  inner: Arc<RwLock<HashMap<SessionId, SessionAuthState>>>,
}

impl SessionAuthRegistry {
  pub fn configure_session(&self, session: SessionId, admission: SessionAdmission) {
    let mut sessions = self.write();
    match sessions.get_mut(&session) {
      Some(state) if state.admission == admission => {},
      Some(state) => {
        tracing::warn!(%session, "replacing collaboration admission secret; clearing authenticated peers");
        state.admission = admission;
        state.peers.clear();
      },
      None => {
        sessions.insert(
          session,
          SessionAuthState {
            admission,
            peers: HashSet::new(),
          },
        );
      },
    }
  }

  pub fn remove_session(&self, session: SessionId) {
    self.write().remove(&session);
  }

  pub fn authenticate_peer(&self, session: SessionId, peer: PeerId, admission: &SessionAdmission) -> Result<()> {
    let mut sessions = self.write();
    let state = sessions
      .get_mut(&session)
      .ok_or_else(|| anyhow!("collaboration session is not configured for admission"))?;
    ensure!(state.admission == *admission, "collaboration admission secret is invalid");
    state.peers.insert(peer);
    Ok(())
  }

  #[must_use]
  pub fn authorize_direct(&self, session: SessionId, peer: PeerId) -> bool {
    self
      .read()
      .get(&session)
      .is_some_and(|state| state.peers.contains(&peer))
  }

  #[must_use]
  pub fn admission(&self, session: SessionId) -> Option<SessionAdmission> {
    self
      .read()
      .get(&session)
      .map(|state| state.admission.clone())
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

static LOCAL_ADMISSIONS: OnceLock<RwLock<HashMap<SessionId, SessionAdmission>>> = OnceLock::new();

fn local_admissions() -> &'static RwLock<HashMap<SessionId, SessionAdmission>> {
  LOCAL_ADMISSIONS.get_or_init(|| RwLock::new(HashMap::new()))
}

pub fn install_local_admission(session: SessionId, admission: SessionAdmission) {
  local_admissions()
    .write()
    .expect("local collaboration admission lock poisoned")
    .insert(session, admission);
}

pub fn clear_local_admission(session: SessionId) {
  local_admissions()
    .write()
    .expect("local collaboration admission lock poisoned")
    .remove(&session);
}

#[must_use]
pub fn local_admission(session: SessionId) -> Option<SessionAdmission> {
  local_admissions()
    .read()
    .expect("local collaboration admission lock poisoned")
    .get(&session)
    .cloned()
}

#[cfg(test)]
mod tests {
  use iroh::SecretKey;

  use super::SessionAuthRegistry;
  use crate::{SessionAdmission, SessionId};

  #[test]
  fn every_peer_authenticates_with_the_same_session_secret() {
    let registry = SessionAuthRegistry::default();
    let session = SessionId::from_bytes([1; 32]);
    let admission = SessionAdmission::from_bytes([2; 32]);
    let first = SecretKey::from_bytes(&[3; 32]).public();
    let second = SecretKey::from_bytes(&[4; 32]).public();
    registry.configure_session(session, admission.clone());
    registry
      .authenticate_peer(session, first, &admission)
      .expect("first editor");
    registry
      .authenticate_peer(session, second, &admission)
      .expect("second editor");
    assert!(registry.authorize_direct(session, first));
    assert!(registry.authorize_direct(session, second));
  }

  #[test]
  fn wrong_secret_is_rejected() {
    let registry = SessionAuthRegistry::default();
    let session = SessionId::from_bytes([5; 32]);
    registry.configure_session(session, SessionAdmission::from_bytes([6; 32]));
    let peer = SecretKey::from_bytes(&[7; 32]).public();
    assert!(
      registry
        .authenticate_peer(session, peer, &SessionAdmission::from_bytes([8; 32]))
        .is_err()
    );
    assert!(!registry.authorize_direct(session, peer));
  }
}
