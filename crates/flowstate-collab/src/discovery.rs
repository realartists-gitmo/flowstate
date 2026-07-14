//! Provider-neutral, signed collaboration rendezvous.

use std::{future::Future, pin::Pin, sync::Arc};

use anyhow::{Result, ensure};
use iroh::{EndpointAddr, PublicKey, Signature};
use serde::{Deserialize, Serialize};

use crate::{
  SessionId,
  identity::{PortableIdentitySecret, SignedProfile},
};

const ADVERTISEMENT_CONTEXT: &[u8] = b"flowstate/discovery-advertisement/v1";
const DOCUMENT_FINGERPRINT_CONTEXT: &[u8] = b"flowstate/document-discovery-fingerprint/v1";
const ADMISSION_REQUEST_CONTEXT: &[u8] = b"flowstate/discovery-admission-request/v1";

/// Stable, title-free rendezvous key derived from the canonical document ID.
/// Forking a document changes that ID, so unrelated lineages do not discover
/// one another merely because they share a path or Dropbox filename.
#[must_use]
pub fn document_fingerprint(document_id: u128) -> [u8; 32] {
  let mut hasher = blake3::Hasher::new();
  hasher.update(DOCUMENT_FINGERPRINT_CONTEXT);
  hasher.update(&document_id.to_le_bytes());
  *hasher.finalize().as_bytes()
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum DiscoveryTransport {
  Dropbox,
  Bluetooth,
}

/// Short-lived rendezvous data. It deliberately contains no title and no
/// session admission bearer; discovery identifies a reachable trusted person,
/// after which the normal authorization pipeline exchanges an invite.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryAdvertisement {
  pub identity: PublicKey,
  pub device_id: u128,
  pub document_fingerprint: [u8; 32],
  pub session: SessionId,
  pub endpoint: EndpointAddr,
  pub expires_at_unix: u64,
  pub profile: SignedProfile,
  pub signature: Signature,
}

/// Signed, short-lived proof sent over the encrypted direct channel when a
/// trusted peer asks for an invite. It is not an admission bearer and is safe
/// to reject without revealing document data.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct DiscoveryAdmissionRequest {
  pub identity: PublicKey,
  pub session: SessionId,
  pub document_fingerprint: [u8; 32],
  pub nonce: [u8; 32],
  pub expires_at_unix: u64,
  pub signature: Signature,
}

impl DiscoveryAdmissionRequest {
  #[must_use]
  pub fn issue(
    secret: &PortableIdentitySecret,
    session: SessionId,
    document_fingerprint: [u8; 32],
    nonce: [u8; 32],
    expires_at_unix: u64,
  ) -> Self {
    let identity = secret.public();
    let signature = secret.sign(&admission_request_payload(
      &identity,
      session,
      &document_fingerprint,
      &nonce,
      expires_at_unix,
    ));
    Self {
      identity,
      session,
      document_fingerprint,
      nonce,
      expires_at_unix,
      signature,
    }
  }

  pub fn verify(&self, now_unix: u64) -> Result<()> {
    ensure!(now_unix < self.expires_at_unix, "discovery admission request expired");
    self
      .identity
      .verify(
        &admission_request_payload(
          &self.identity,
          self.session,
          &self.document_fingerprint,
          &self.nonce,
          self.expires_at_unix,
        ),
        &self.signature,
      )
      .map_err(|_| anyhow::anyhow!("discovery admission request signature is invalid"))
  }
}

impl DiscoveryAdvertisement {
  #[must_use]
  pub fn issue(
    secret: &PortableIdentitySecret,
    device_id: u128,
    document_fingerprint: [u8; 32],
    session: SessionId,
    endpoint: EndpointAddr,
    expires_at_unix: u64,
    profile: SignedProfile,
  ) -> Self {
    let identity = secret.public();
    let payload = advertisement_payload(&identity, device_id, &document_fingerprint, session, &endpoint, expires_at_unix, &profile);
    let signature = secret.sign(&payload);
    Self {
      identity,
      device_id,
      document_fingerprint,
      session,
      endpoint,
      expires_at_unix,
      profile,
      signature,
    }
  }

  pub fn verify(&self, now_unix: u64) -> Result<()> {
    ensure!(now_unix < self.expires_at_unix, "discovery advertisement expired");
    ensure!(
      self.profile.identity == self.identity,
      "discovery profile identity does not match its signer"
    );
    ensure!(self.profile.verify(), "discovery profile signature is invalid");
    let payload = advertisement_payload(
      &self.identity,
      self.device_id,
      &self.document_fingerprint,
      self.session,
      &self.endpoint,
      self.expires_at_unix,
      &self.profile,
    );
    self
      .identity
      .verify(&payload, &self.signature)
      .map_err(|_| anyhow::anyhow!("discovery advertisement signature is invalid"))
  }
}

/// Dropbox sidecars and OS Bluetooth adapters implement this same interface.
/// Backends only move signed advertisements; they never decide document access.
pub trait RendezvousBackend: Send + Sync {
  fn transport(&self) -> DiscoveryTransport;
  fn publish(&self, advertisement: DiscoveryAdvertisement) -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
  fn scan(&self, document_fingerprint: [u8; 32]) -> Pin<Box<dyn Future<Output = Result<Vec<DiscoveryAdvertisement>>> + Send + '_>>;
  fn clear(&self, identity: PublicKey, device_id: u128, document_fingerprint: [u8; 32])
  -> Pin<Box<dyn Future<Output = Result<()>> + Send + '_>>;
}

/// Runs all configured transports through one lifecycle. A failed optional
/// transport is reported but does not discard peers found through another.
#[derive(Default)]
pub struct RendezvousSet {
  backends: Vec<Arc<dyn RendezvousBackend>>,
}

impl RendezvousSet {
  #[must_use]
  pub fn new(backends: Vec<Arc<dyn RendezvousBackend>>) -> Self {
    Self { backends }
  }

  pub async fn publish(&self, advertisement: DiscoveryAdvertisement) -> Vec<(DiscoveryTransport, anyhow::Error)> {
    let mut failures = Vec::new();
    for backend in &self.backends {
      if let Err(error) = backend.publish(advertisement.clone()).await {
        failures.push((backend.transport(), error));
      }
    }
    failures
  }

  pub async fn scan(&self, document_fingerprint: [u8; 32]) -> (Vec<DiscoveryAdvertisement>, Vec<(DiscoveryTransport, anyhow::Error)>) {
    let mut advertisements = Vec::new();
    let mut failures = Vec::new();
    for backend in &self.backends {
      match backend.scan(document_fingerprint).await {
        Ok(mut found) => advertisements.append(&mut found),
        Err(error) => failures.push((backend.transport(), error)),
      }
    }
    (advertisements, failures)
  }

  pub async fn clear(&self, identity: PublicKey, device_id: u128, document_fingerprint: [u8; 32]) -> Vec<(DiscoveryTransport, anyhow::Error)> {
    let mut failures = Vec::new();
    for backend in &self.backends {
      if let Err(error) = backend
        .clear(identity, device_id, document_fingerprint)
        .await
      {
        failures.push((backend.transport(), error));
      }
    }
    failures
  }
}

/// Verify, scope, and deduplicate untrusted backend results before UI code sees
/// them. `is_authorized` is the shared safety-code/squad/document policy gate.
pub fn eligible_advertisements(
  advertisements: impl IntoIterator<Item = DiscoveryAdvertisement>,
  document_fingerprint: [u8; 32],
  now_unix: u64,
  mut is_authorized: impl FnMut(&PublicKey) -> bool,
) -> Vec<DiscoveryAdvertisement> {
  let mut eligible: Vec<DiscoveryAdvertisement> = Vec::new();
  for advertisement in advertisements {
    if advertisement.document_fingerprint != document_fingerprint
      || advertisement.verify(now_unix).is_err()
      || !is_authorized(&advertisement.identity)
    {
      continue;
    }
    if let Some(index) = eligible
      .iter()
      .position(|existing| existing.identity == advertisement.identity && existing.device_id == advertisement.device_id)
    {
      if eligible[index].expires_at_unix < advertisement.expires_at_unix {
        eligible[index] = advertisement;
      }
    } else {
      eligible.push(advertisement);
    }
  }
  eligible
}

fn advertisement_payload(
  identity: &PublicKey,
  device_id: u128,
  document_fingerprint: &[u8; 32],
  session: SessionId,
  endpoint: &EndpointAddr,
  expires_at_unix: u64,
  profile: &SignedProfile,
) -> Vec<u8> {
  postcard::to_stdvec(&(
    ADVERTISEMENT_CONTEXT,
    identity,
    device_id,
    document_fingerprint,
    session,
    endpoint,
    expires_at_unix,
    profile,
  ))
  .expect("discovery advertisement signing payload should serialize")
}

fn admission_request_payload(
  identity: &PublicKey,
  session: SessionId,
  document_fingerprint: &[u8; 32],
  nonce: &[u8; 32],
  expires_at_unix: u64,
) -> Vec<u8> {
  postcard::to_stdvec(&(ADMISSION_REQUEST_CONTEXT, identity, session, document_fingerprint, nonce, expires_at_unix))
    .expect("discovery admission request signing payload should serialize")
}

#[cfg(test)]
mod tests {
  use super::*;
  use iroh::SecretKey;

  fn advertisement(secret: &PortableIdentitySecret, device_id: u128, expires_at_unix: u64) -> DiscoveryAdvertisement {
    let profile = secret.sign_profile(1, "Alex".into(), 0x123456, None);
    DiscoveryAdvertisement::issue(
      secret,
      device_id,
      [7; 32],
      SessionId::from_bytes([8; 32]),
      EndpointAddr::new(SecretKey::from_bytes(&[9; 32]).public()),
      expires_at_unix,
      profile,
    )
  }

  #[test]
  fn advertisements_are_signed_scoped_expiring_and_deduplicated() {
    let secret = PortableIdentitySecret::generate();
    let stale = advertisement(&secret, 3, 20);
    let fresh = advertisement(&secret, 3, 30);
    let result = eligible_advertisements([stale, fresh.clone()], [7; 32], 10, |identity| *identity == secret.public());
    assert_eq!(result, vec![fresh]);
    assert!(eligible_advertisements([advertisement(&secret, 4, 10)], [7; 32], 10, |_| true).is_empty());
    assert!(eligible_advertisements([advertisement(&secret, 5, 30)], [6; 32], 10, |_| true).is_empty());
  }
}
