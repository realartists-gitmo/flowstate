#[hotpath::measure]
pub fn load_app_settings() -> AppSettings {
  let Ok(text) = fs::read_to_string(settings_path()) else {
    return AppSettings::default();
  };
  serde_json::from_str(&text).unwrap_or_default()
}

#[hotpath::measure]
pub fn load_document_theme() -> DocumentTheme {
  load_app_settings()
    .document_theme
    .map(DocumentTheme::from)
    .unwrap_or_default()
}

#[hotpath::measure]
pub fn load_ribbon_mode() -> RibbonMode {
  load_app_settings().editor.ribbon_mode
}

#[hotpath::measure]
pub fn load_smart_word_selection() -> bool {
  load_app_settings().editor.smart_word_selection
}

// Document style appearance is intentionally user-side. The DB8 file keeps
// semantic assignments only; this app setting decides how those semantics look.
#[hotpath::measure]
pub fn save_theme_name(theme_name: &str) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.theme_name = Some(theme_name.to_string());
  save_app_settings(settings)
}

#[hotpath::measure]
pub fn save_document_theme(theme: &DocumentTheme) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.document_theme = Some(DocumentThemeSettings::from(theme));
  save_app_settings(settings)
}

#[hotpath::measure]
pub fn save_ribbon_mode(ribbon_mode: RibbonMode) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.editor.ribbon_mode = ribbon_mode;
  save_app_settings(settings)
}

#[hotpath::measure]
pub fn save_smart_word_selection(enabled: bool) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.editor.smart_word_selection = enabled;
  save_app_settings(settings)
}

#[hotpath::measure]
fn save_app_settings(settings: AppSettings) -> io::Result<()> {
  let path = settings_path();
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent)?;
  }
  let text = serde_json::to_string_pretty(&settings)?;
  fs::write(path, text)
}

