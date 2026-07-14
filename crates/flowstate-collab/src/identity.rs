//! Durable person identity and mutual trust primitives.

use anyhow::{Result, anyhow, ensure};
use iroh::{PublicKey, SecretKey, Signature};
use serde::{Deserialize, Serialize};

const PROFILE_CONTEXT: &[u8] = b"flowstate/person-profile/v1";
const TRUST_CONTEXT: &[u8] = b"flowstate/trust-attestation/v1";

/// Plaintext-at-rest signing seed. Callers decide how it is protected and
/// persisted; its `Debug` implementation never exposes key material.
#[derive(Clone)]
pub struct PortableIdentitySecret(SecretKey);

impl std::fmt::Debug for PortableIdentitySecret {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.write_str("PortableIdentitySecret([REDACTED])")
  }
}

impl PortableIdentitySecret {
  #[must_use]
  pub fn generate() -> Self {
    Self(SecretKey::generate())
  }

  pub fn from_hex(text: &str) -> Result<Self> {
    ensure!(
      text.len() == 64 && text.is_ascii(),
      "identity signing seed must contain 64 hexadecimal characters"
    );
    let mut bytes = [0u8; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
      *byte = u8::from_str_radix(&text[index * 2..index * 2 + 2], 16).map_err(|_| anyhow!("identity signing seed is not hexadecimal"))?;
    }
    Ok(Self(SecretKey::from_bytes(&bytes)))
  }

  #[must_use]
  pub fn to_hex(&self) -> String {
    use std::fmt::Write as _;
    self
      .0
      .to_bytes()
      .iter()
      .fold(String::with_capacity(64), |mut hex, byte| {
        let _ = write!(hex, "{byte:02x}");
        hex
      })
  }

  #[must_use]
  pub fn public(&self) -> PublicKey {
    self.0.public()
  }

  #[must_use]
  pub(crate) fn sign(&self, payload: &[u8]) -> Signature {
    self.0.sign(payload)
  }

  #[must_use]
  pub fn sign_profile(&self, sequence: u64, mut display_name: String, color_rgb: u32, avatar_digest: Option<[u8; 32]>) -> SignedProfile {
    // `SignedProfile::verify` rejects names over 64 bytes; clamp here so a
    // locally-entered long name can never mint a profile every peer discards.
    if display_name.len() > 64 {
      let mut cut = 64;
      while !display_name.is_char_boundary(cut) {
        cut -= 1;
      }
      display_name.truncate(cut);
    }
    let identity = self.public();
    let payload = profile_payload(&identity, sequence, &display_name, color_rgb, avatar_digest.as_ref());
    SignedProfile {
      identity,
      sequence,
      display_name,
      color_rgb: color_rgb & 0x00ff_ffff,
      avatar_digest,
      signature: self.0.sign(&payload),
    }
  }

  #[must_use]
  pub fn attest_trust(&self, subject: PublicKey, issued_at_unix: u64) -> TrustAttestation {
    let signer = self.public();
    let signature = self
      .0
      .sign(&trust_payload(&signer, &subject, issued_at_unix));
    TrustAttestation {
      signer,
      subject,
      issued_at_unix,
      signature,
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SignedProfile {
  pub identity: PublicKey,
  pub sequence: u64,
  pub display_name: String,
  pub color_rgb: u32,
  pub avatar_digest: Option<[u8; 32]>,
  pub signature: Signature,
}

impl SignedProfile {
  #[must_use]
  pub fn verify(&self) -> bool {
    self.color_rgb <= 0x00ff_ffff
      && self.display_name.len() <= 64
      && self
        .identity
        .verify(
          &profile_payload(
            &self.identity,
            self.sequence,
            &self.display_name,
            self.color_rgb,
            self.avatar_digest.as_ref(),
          ),
          &self.signature,
        )
        .is_ok()
  }
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct TrustAttestation {
  pub signer: PublicKey,
  pub subject: PublicKey,
  pub issued_at_unix: u64,
  pub signature: Signature,
}

impl TrustAttestation {
  #[must_use]
  pub fn verify(&self) -> bool {
    self
      .signer
      .verify(&trust_payload(&self.signer, &self.subject, self.issued_at_unix), &self.signature)
      .is_ok()
  }
}

/// Short code people compare out-of-band before exchanging mutual standing
/// access attestations. Sorting makes both screens show the same code.
#[must_use]
pub fn safety_code(left: &PublicKey, right: &PublicKey) -> String {
  let (first, second) = if left.as_bytes() <= right.as_bytes() {
    (left, right)
  } else {
    (right, left)
  };
  let mut input = Vec::with_capacity(64);
  input.extend_from_slice(first.as_bytes());
  input.extend_from_slice(second.as_bytes());
  let digest = blake3::hash(&input);
  let number = u64::from_le_bytes(
    digest.as_bytes()[..8]
      .try_into()
      .expect("eight digest bytes"),
  ) % 1_000_000_000_000;
  format!("{:04} {:04} {:04}", number / 100_000_000, (number / 10_000) % 10_000, number % 10_000)
}

fn profile_payload(identity: &PublicKey, sequence: u64, display_name: &str, color_rgb: u32, avatar_digest: Option<&[u8; 32]>) -> Vec<u8> {
  postcard::to_stdvec(&(PROFILE_CONTEXT, identity, sequence, display_name, color_rgb & 0x00ff_ffff, avatar_digest))
    .expect("portable profile signing payload should serialize")
}

fn trust_payload(signer: &PublicKey, subject: &PublicKey, issued_at_unix: u64) -> Vec<u8> {
  postcard::to_stdvec(&(TRUST_CONTEXT, signer, subject, issued_at_unix)).expect("trust signing payload should serialize")
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn profile_updates_and_attestations_are_signed() {
    let alex = PortableIdentitySecret::generate();
    let blair = PortableIdentitySecret::generate();
    let profile = alex.sign_profile(7, "Alex".into(), 0x123456, None);
    assert!(profile.verify());
    let attestation = alex.attest_trust(blair.public(), 42);
    assert!(attestation.verify());
    assert_eq!(safety_code(&alex.public(), &blair.public()), safety_code(&blair.public(), &alex.public()));
  }

  #[test]
  fn signing_seed_round_trips_without_debug_disclosure() {
    let secret = PortableIdentitySecret::generate();
    let restored = PortableIdentitySecret::from_hex(&secret.to_hex()).unwrap();
    assert_eq!(restored.public(), secret.public());
    assert!(!format!("{secret:?}").contains(&secret.to_hex()));
    // 64 BYTES of non-ASCII must error, not panic on a char boundary.
    assert!(PortableIdentitySecret::from_hex(&"é".repeat(32)).is_err());
  }

  #[test]
  fn oversized_display_names_are_clamped_to_verifiable_profiles() {
    let secret = PortableIdentitySecret::generate();
    let profile = secret.sign_profile(1, "é".repeat(64), 0x123456, None);
    assert!(profile.display_name.len() <= 64);
    assert!(profile.verify());
  }
}
