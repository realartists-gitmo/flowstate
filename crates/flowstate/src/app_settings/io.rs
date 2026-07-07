use std::{
  collections::HashMap,
  sync::{Mutex, OnceLock},
};

use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};

type CommandKeyCache = HashMap<crate::commands::CommandId, Vec<String>>;

#[derive(Clone)]
struct CachedAppSettings {
  path: PathBuf,
  settings: AppSettings,
  effective_keymap: crate::commands::Keymap,
  keys_by_command: CommandKeyCache,
}

static APP_SETTINGS_CACHE: OnceLock<Mutex<Option<CachedAppSettings>>> = OnceLock::new();
static APP_SETTINGS_WATCHER: OnceLock<Mutex<Option<RecommendedWatcher>>> = OnceLock::new();

fn app_settings_cache() -> &'static Mutex<Option<CachedAppSettings>> {
  APP_SETTINGS_CACHE.get_or_init(|| Mutex::new(None))
}

fn invalidate_app_settings_cache() {
  if let Ok(mut cache) = app_settings_cache().lock() {
    *cache = None;
  }
}

fn start_app_settings_watcher(path: &std::path::Path) {
  let Some(parent) = path.parent().map(PathBuf::from) else {
    return;
  };
  if let Err(error) = fs::create_dir_all(&parent) {
    tracing::warn!(error = %error, path = %parent.display(), "failed to create app settings directory for watcher");
    return;
  }
  let watcher_slot = APP_SETTINGS_WATCHER.get_or_init(|| Mutex::new(None));
  let Ok(mut watcher_slot) = watcher_slot.lock() else {
    return;
  };
  if watcher_slot.is_some() {
    return;
  }

  let watcher = RecommendedWatcher::new(
    move |event: notify::Result<notify::Event>| {
      if event.is_ok() {
        invalidate_app_settings_cache();
      }
    },
    NotifyConfig::default(),
  );
  let Ok(mut watcher) = watcher else {
    tracing::warn!(path = %parent.display(), "failed to create app settings watcher");
    return;
  };
  if let Err(error) = watcher.watch(&parent, RecursiveMode::NonRecursive) {
    tracing::warn!(error = %error, path = %parent.display(), "failed to watch app settings directory");
    return;
  }
  *watcher_slot = Some(watcher);
}

fn effective_keymap_for(settings: &AppSettings) -> crate::commands::Keymap {
  if settings.keymap.is_empty() {
    crate::commands::Keymap::defaults()
  } else {
    crate::commands::Keymap {
      entries: settings.keymap.clone(),
    }
  }
}

fn keys_by_command_for(keymap: &crate::commands::Keymap) -> CommandKeyCache {
  let mut keys = CommandKeyCache::new();
  for entry in &keymap.entries {
    keys
      .entry(entry.command)
      .or_default()
      .push(entry.key.clone());
  }
  keys
}

fn cached_settings_from(settings: AppSettings, path: PathBuf) -> CachedAppSettings {
  let effective_keymap = effective_keymap_for(&settings);
  let keys_by_command = keys_by_command_for(&effective_keymap);
  CachedAppSettings {
    path,
    settings,
    effective_keymap,
    keys_by_command,
  }
}

#[hotpath::measure]
pub fn load_app_settings() -> AppSettings {
  load_cached_app_settings().settings
}

fn load_cached_app_settings() -> CachedAppSettings {
  let path = settings_path();
  start_app_settings_watcher(&path);

  if let Ok(cache) = app_settings_cache().lock()
    && let Some(cached) = cache.as_ref().filter(|cached| cached.path == path)
  {
    return cached.clone();
  }

  let settings = load_app_settings_from_path(path.clone()).unwrap_or_default();
  let cached = cached_settings_from(settings, path);
  if let Ok(mut cache) = app_settings_cache().lock() {
    *cache = Some(cached.clone());
  }
  cached
}

fn load_app_settings_from_path(path: PathBuf) -> io::Result<AppSettings> {
  match fs::read_to_string(&path) {
    Ok(text) => Ok(parse_app_settings(&text).unwrap_or_default()),
    Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(AppSettings::default()),
    Err(err) => Err(err),
  }
}

fn parse_app_settings(text: &str) -> Option<AppSettings> {
  toml::from_str(text).ok()
}

#[hotpath::measure]
pub fn load_document_theme() -> DocumentTheme {
  load_app_settings()
    .document_theme
    .map(DocumentTheme::from)
    .unwrap_or_else(flowstate_document_theme)
}

#[hotpath::measure]
pub fn load_ribbon_mode() -> RibbonMode {
  load_app_settings().editor.ribbon_mode
}

#[hotpath::measure]
pub fn load_smart_word_selection() -> bool {
  load_app_settings().editor.smart_word_selection
}

pub fn load_autosave() -> bool {
  load_app_settings().editor.autosave
}

pub fn load_tub_root() -> Option<PathBuf> {
  load_app_settings().toolkit.tub_root
}

pub fn load_send_to_document_directory() -> bool {
  load_app_settings().editor.send_to_document_directory
}

pub fn load_send_custom_directory() -> Option<PathBuf> {
  load_app_settings().editor.send_custom_directory
}

pub fn load_recent_documents() -> Vec<PathBuf> {
  load_app_settings().recent_documents
}

pub fn load_keymap_entries() -> Vec<crate::commands::KeymapEntry> {
  load_app_settings().keymap
}

pub fn load_keymap() -> crate::commands::Keymap {
  load_cached_app_settings().effective_keymap
}

#[hotpath::measure]
pub fn load_keys_for_command(command: crate::commands::CommandId) -> Vec<String> {
  load_cached_app_settings()
    .keys_by_command
    .get(&command)
    .cloned()
    .unwrap_or_default()
}

#[hotpath::measure]
pub fn load_first_key_for_command(command: crate::commands::CommandId) -> Option<String> {
  load_cached_app_settings()
    .keys_by_command
    .get(&command)
    .and_then(|keys| keys.first().cloned())
}

/// §15/§31: load the stable per-install durable author identity, minting and
/// persisting a fresh one the first time it is requested.
///
/// The returned `(user_id, display_name)` is bound to a live document runtime
/// via `DocIoHandle::set_author_identity` so revisions record their
/// author and `users_by_id` is populated. Persisting is best-effort: a write
/// failure is logged but never fatal (the id regenerates on the next launch).
pub fn load_local_user_identity() -> (u128, Option<String>) {
  let mut settings = load_app_settings();
  if settings.local_user_id != 0 {
    return (settings.local_user_id, settings.local_user_display_name);
  }

  let user_id = uuid::Uuid::new_v4().as_u128();
  let display_name = os_username();
  settings.local_user_id = user_id;
  settings.local_user_display_name = display_name.clone();
  if let Err(error) = save_app_settings(settings) {
    tracing::warn!(error = %error, "persisting generated local user identity failed");
  }
  (user_id, display_name)
}

/// Best-effort OS account name used as the default author display name. Uses
/// only the standard environment so no extra dependency is introduced.
fn os_username() -> Option<String> {
  std::env::var("USER")
    .or_else(|_| std::env::var("USERNAME"))
    .ok()
    .map(|name| name.trim().to_owned())
    .filter(|name| !name.is_empty())
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
pub fn save_autosave(enabled: bool) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.editor.autosave = enabled;
  save_app_settings(settings)
}

pub fn save_tub_root(path: Option<PathBuf>) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.toolkit.tub_root = path;
  save_app_settings(settings)
}

pub fn save_send_to_document_directory(enabled: bool) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.editor.send_to_document_directory = enabled;
  save_app_settings(settings)
}

pub fn save_send_custom_directory(path: Option<PathBuf>) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.editor.send_custom_directory = path;
  save_app_settings(settings)
}

pub fn save_recent_documents(recent_documents: Vec<PathBuf>) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.recent_documents = recent_documents;
  save_app_settings(settings)
}

pub fn save_keymap_entries(keymap: Vec<crate::commands::KeymapEntry>) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.keymap = keymap;
  save_app_settings(settings)
}

#[hotpath::measure]
pub fn save_app_settings(settings: AppSettings) -> io::Result<()> {
  let path = settings_path();
  start_app_settings_watcher(&path);
  save_app_settings_to_path(&settings, path.clone())?;
  let cached = cached_settings_from(settings, path);
  if let Ok(mut cache) = app_settings_cache().lock() {
    *cache = Some(cached);
  }
  Ok(())
}

fn save_app_settings_to_path(settings: &AppSettings, path: PathBuf) -> io::Result<()> {
  if let Some(parent) = path.parent() {
    fs::create_dir_all(parent)?;
  }
  let text = toml::to_string_pretty(settings).map_err(io::Error::other)?;
  fs::write(path, text)
}
