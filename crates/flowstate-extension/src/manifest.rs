use std::collections::HashSet;
use std::path::{Component, Path};

use serde::Deserialize;
use thiserror::Error;

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ExtensionManifest {
    pub manifest_version: u32,
    pub id: String,
    pub name: String,
    pub version: String,
    pub component: String,
    #[serde(default)]
    pub actions: Vec<ActionManifest>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct ActionManifest {
    pub id: String,
    pub label: String,
    #[serde(default)]
    pub requires_document: bool,
}

#[derive(Debug, Error, Eq, PartialEq)]
pub enum ManifestError {
    #[error("manifest TOML is invalid: {0}")]
    Toml(String),
    #[error("unsupported manifest version {0}")]
    UnsupportedVersion(u32),
    #[error("{field} must be a dot-separated identifier")]
    InvalidId { field: &'static str },
    #[error("{field} must not be empty")]
    Empty { field: &'static str },
    #[error("component must be a relative path without parent traversal")]
    InvalidComponentPath,
    #[error("duplicate action id: {0}")]
    DuplicateAction(String),
    #[error("at least one action is required")]
    NoActions,
}

impl ExtensionManifest {
    pub fn parse(source: &str) -> Result<Self, ManifestError> {
        let manifest: Self = toml::from_str(source).map_err(|error| ManifestError::Toml(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), ManifestError> {
        if self.manifest_version != 1 {
            return Err(ManifestError::UnsupportedVersion(self.manifest_version));
        }
        validate_id(&self.id, "id")?;
        require_text(&self.name, "name")?;
        require_text(&self.version, "version")?;
        let path = Path::new(&self.component);
        if path.is_absolute() || path.as_os_str().is_empty() || path.components().any(|part| matches!(part, Component::ParentDir | Component::RootDir | Component::Prefix(_))) {
            return Err(ManifestError::InvalidComponentPath);
        }
        if self.actions.is_empty() {
            return Err(ManifestError::NoActions);
        }
        let mut ids = HashSet::new();
        for action in &self.actions {
            validate_id(&action.id, "action id")?;
            require_text(&action.label, "action label")?;
            if !ids.insert(&action.id) {
                return Err(ManifestError::DuplicateAction(action.id.clone()));
            }
        }
        Ok(())
    }
}

fn require_text(value: &str, field: &'static str) -> Result<(), ManifestError> {
    if value.trim().is_empty() { Err(ManifestError::Empty { field }) } else { Ok(()) }
}

fn validate_id(value: &str, field: &'static str) -> Result<(), ManifestError> {
    let valid = !value.is_empty() && value.split('.').all(|part| !part.is_empty() && part.bytes().all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_')));
    if valid { Ok(()) } else { Err(ManifestError::InvalidId { field }) }
}
