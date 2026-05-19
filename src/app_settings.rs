use std::{env, fs, io, path::PathBuf};

use serde::{Deserialize, Serialize};

#[derive(Default, Deserialize, Serialize)]
pub struct AppSettings {
  pub theme_name: Option<String>,
}

pub fn load_app_settings() -> AppSettings {
  let Ok(text) = fs::read_to_string(settings_path()) else {
    return AppSettings::default();
  };
  serde_json::from_str(&text).unwrap_or_default()
}

pub fn save_theme_name(theme_name: &str) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.theme_name = Some(theme_name.to_string());
  let path = settings_path();
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent)?;
  }
  let text = serde_json::to_string_pretty(&settings)?;
  fs::write(path, text)
}

fn settings_path() -> PathBuf {
  if cfg!(target_os = "windows") {
    if let Some(appdata) = env::var_os("APPDATA") {
      return PathBuf::from(appdata).join("Odrenrir").join("settings.json");
    }
  }

  if let Some(config_home) = env::var_os("XDG_CONFIG_HOME") {
    return PathBuf::from(config_home).join("odrenrir").join("settings.json");
  }

  if let Some(home) = env::var_os("HOME") {
    return PathBuf::from(home).join(".config").join("odrenrir").join("settings.json");
  }

  PathBuf::from("odrenrir-settings.json")
}
