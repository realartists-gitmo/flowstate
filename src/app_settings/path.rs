#[hotpath::measure]
fn settings_path() -> PathBuf {
  config_dir()
    .unwrap_or("./".into())
    .join::<PathBuf>("flowstate/settings.json".into())
}

pub fn flowstate_data_dir() -> PathBuf {
  data_dir()
    .unwrap_or("./".into())
    .join::<PathBuf>("flowstate".into())
}
