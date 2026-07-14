/// Sandbox override for every on-disk settings artifact. Headless tests set
/// this (and `FLOWSTATE_DATA_DIR`) to a temp dir so constructing a Workspace
/// never touches — or mints — the real user profile in `~/.config/flowstate`.
fn config_dir_override() -> Option<PathBuf> {
  std::env::var_os("FLOWSTATE_CONFIG_DIR").map(PathBuf::from)
}

#[hotpath::measure]
fn settings_path() -> PathBuf {
  config_dir_override()
    .unwrap_or_else(|| config_dir().unwrap_or("./".into()))
    .join::<PathBuf>("flowstate/settings.toml".into())
}

pub fn flowstate_data_dir() -> PathBuf {
  std::env::var_os("FLOWSTATE_DATA_DIR")
    .map(PathBuf::from)
    .unwrap_or_else(|| data_dir().unwrap_or("./".into()))
    .join::<PathBuf>("flowstate".into())
}
