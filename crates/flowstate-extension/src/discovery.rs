use std::collections::HashSet;
use std::fs;
use std::path::{Path, PathBuf};

use crate::ExtensionManifest;

#[derive(Clone, Debug)]
pub struct InstalledExtension {
    pub manifest: ExtensionManifest,
    pub root: PathBuf,
    pub component_path: PathBuf,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DiscoveryIssue {
    pub path: PathBuf,
    pub message: String,
}

#[derive(Clone, Debug, Default)]
pub struct DiscoveryResult {
    pub extensions: Vec<InstalledExtension>,
    pub issues: Vec<DiscoveryIssue>,
}

pub fn discover_extensions(root: &Path) -> DiscoveryResult {
    let mut result = DiscoveryResult::default();
    let Ok(entries) = fs::read_dir(root) else { return result };
    let mut manifests: Vec<_> = entries.flatten().map(|entry| entry.path().join("extension.toml")).filter(|path| path.is_file()).collect();
    manifests.sort();
    let mut ids = HashSet::new();
    for path in manifests {
        match load_extension(&path) {
            Ok(extension) if ids.insert(extension.manifest.id.clone()) => result.extensions.push(extension),
            Ok(extension) => result.issues.push(DiscoveryIssue { path, message: format!("duplicate extension id: {}", extension.manifest.id) }),
            Err(message) => result.issues.push(DiscoveryIssue { path, message }),
        }
    }
    result
}

fn load_extension(path: &Path) -> Result<InstalledExtension, String> {
    let source = fs::read_to_string(path).map_err(|error| error.to_string())?;
    let manifest = ExtensionManifest::parse(&source).map_err(|error| error.to_string())?;
    let root = path.parent().ok_or("manifest has no parent directory")?.to_path_buf();
    let component_path = root.join(&manifest.component);
    if !component_path.is_file() {
        return Err(format!("component does not exist: {}", component_path.display()));
    }
    let canonical_root = root.canonicalize().map_err(|error| error.to_string())?;
    let canonical_component = component_path.canonicalize().map_err(|error| error.to_string())?;
    if !canonical_component.starts_with(&canonical_root) {
        return Err("component resolves outside the extension directory".to_owned());
    }
    Ok(InstalledExtension { manifest, root, component_path: canonical_component })
}
