use std::collections::HashMap;
use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ComponentDigest(String);

impl ComponentDigest {
    pub fn from_file(path: &Path) -> std::io::Result<Self> {
        Ok(Self(hex::encode(Sha256::digest(fs::read(path)?))))
    }

    pub fn as_str(&self) -> &str { &self.0 }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TrustDecision { Trusted, ApprovalRequired }

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
#[serde(transparent)]
pub struct TrustStore(HashMap<String, ComponentDigest>);

impl TrustStore {
    pub fn decision(&self, extension_id: &str, digest: &ComponentDigest) -> TrustDecision {
        if self.0.get(extension_id) == Some(digest) { TrustDecision::Trusted } else { TrustDecision::ApprovalRequired }
    }

    pub fn approve(&mut self, extension_id: impl Into<String>, digest: ComponentDigest) {
        self.0.insert(extension_id.into(), digest);
    }

    pub fn revoke(&mut self, extension_id: &str) { self.0.remove(extension_id); }
}
