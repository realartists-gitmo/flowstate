#[cfg(test)]
mod tests {
use std::fs;

use flowstate_extension::{ComponentDigest, ManifestError, TrustDecision, TrustStore, discover_extensions};

fn manifest(id: &str, component: &str) -> String {
    format!(r#"
manifest_version = 1
id = "{id}"
name = "Test"
version = "1.0.0"
component = "{component}"

[[actions]]
id = "run"
label = "Run"
"#)
}

#[test]
fn discovers_valid_extensions_and_reports_invalid_ones() {
    let directory = tempfile::tempdir().unwrap();
    let valid = directory.path().join("valid");
    fs::create_dir(&valid).unwrap();
    fs::write(valid.join("extension.toml"), manifest("com.example.valid", "extension.wasm")).unwrap();
    fs::write(valid.join("extension.wasm"), b"component").unwrap();
    let invalid = directory.path().join("invalid");
    fs::create_dir(&invalid).unwrap();
    fs::write(invalid.join("extension.toml"), manifest("bad id", "missing.wasm")).unwrap();

    let result = discover_extensions(directory.path());
    assert_eq!(result.extensions.len(), 1);
    assert_eq!(result.extensions[0].manifest.id, "com.example.valid");
    assert_eq!(result.issues.len(), 1);
}

#[test]
fn rejects_duplicate_actions_and_path_traversal() {
    let duplicate = format!("{}\n[[actions]]\nid = \"run\"\nlabel = \"Again\"", manifest("com.example.test", "extension.wasm"));
    assert_eq!(flowstate_extension::ExtensionManifest::parse(&duplicate), Err(ManifestError::DuplicateAction("run".to_owned())));
    let traversal = manifest("com.example.test", "../extension.wasm");
    assert_eq!(flowstate_extension::ExtensionManifest::parse(&traversal), Err(ManifestError::InvalidComponentPath));
}

#[test]
fn component_changes_require_fresh_approval() {
    let directory = tempfile::tempdir().unwrap();
    let component = directory.path().join("extension.wasm");
    fs::write(&component, b"first").unwrap();
    let first = ComponentDigest::from_file(&component).unwrap();
    let mut trust = TrustStore::default();
    assert_eq!(trust.decision("com.example.test", &first), TrustDecision::ApprovalRequired);
    trust.approve("com.example.test", first.clone());
    assert_eq!(trust.decision("com.example.test", &first), TrustDecision::Trusted);
    fs::write(&component, b"second").unwrap();
    let second = ComponentDigest::from_file(&component).unwrap();
    assert_eq!(trust.decision("com.example.test", &second), TrustDecision::ApprovalRequired);
}
}
