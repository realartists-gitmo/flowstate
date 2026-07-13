#[cfg(test)]
mod tests {
  use super::*;
  use tempfile::tempdir;

  fn test_settings_path(root: &std::path::Path) -> PathBuf {
    root.join("flowstate/settings.toml")
  }

  #[test]
  fn saves_app_settings_as_toml() {
    let dir = tempdir().unwrap();
    let path = test_settings_path(dir.path());
    let settings = AppSettings {
      theme_name: Some("midnight".into()),
      ..AppSettings::default()
    };

    save_app_settings_to_path(&settings, path.clone()).unwrap();

    let text = fs::read_to_string(path).unwrap();
    assert!(text.contains("theme_name = \"midnight\""));
    assert!(!text.contains('{'));
  }

  #[test]
  fn loads_toml_settings() {
    let dir = tempdir().unwrap();
    let path = test_settings_path(dir.path());
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(
      &path,
      r#"
theme_name = "sunset"

[editor]
smart_word_selection = false
"#,
    )
    .unwrap();

    let settings = load_app_settings_from_path(path).unwrap();
    assert_eq!(settings.theme_name.as_deref(), Some("sunset"));
    assert!(!settings.editor.smart_word_selection);
  }

  #[test]
  fn dropbox_document_bindings_round_trip() {
    let binding = DropboxDocumentBinding {
      local_path: PathBuf::from("/briefs/aff.db8"),
      remote_path: "/Team/aff.db8".into(),
      revision: Some("015abc".into()),
    };
    let mut settings = AppSettings::default();
    settings.dropbox_documents.push(binding.clone());
    let text = toml::to_string(&settings).unwrap();
    let decoded: AppSettings = toml::from_str(&text).unwrap();
    assert_eq!(decoded.dropbox_documents.len(), 1);
    assert_eq!(decoded.dropbox_documents[0].local_path, binding.local_path);
    assert_eq!(decoded.dropbox_documents[0].remote_path, binding.remote_path);
    assert_eq!(decoded.dropbox_documents[0].revision, binding.revision);
  }

  #[test]
  fn standing_access_requires_verification_scope_and_honors_exclusions() {
    let mut settings = AppSettings::default();
    settings.trusted_collaborators.push(TrustedCollaborator {
      identity_key: "alex".into(),
      display_name: "Alex".into(),
      verified: true,
      ..TrustedCollaborator::default()
    });
    settings.collaboration_squads.push(CollaborationSquad {
      id: "debate".into(),
      name: "Debate squad".into(),
      member_identity_keys: vec!["alex".into()],
      default_scopes: vec![
        CollaborationScope::Folder(PathBuf::from("/team")),
        CollaborationScope::Exclusion(PathBuf::from("/team/private")),
      ],
    });
    assert_eq!(
      standing_access_in_settings(&settings, "alex", std::path::Path::new("/team/case.db8")),
      StandingAccess::Allowed
    );
    assert_eq!(
      standing_access_in_settings(&settings, "alex", std::path::Path::new("/team/private/notes.db8")),
      StandingAccess::OutOfScope
    );
    settings.trusted_collaborators[0].verified = false;
    assert_eq!(
      standing_access_in_settings(&settings, "alex", std::path::Path::new("/team/case.db8")),
      StandingAccess::VerificationRequired
    );
  }
}
