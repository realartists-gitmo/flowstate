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
}
