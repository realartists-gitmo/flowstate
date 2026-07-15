use std::{
  collections::HashMap,
  path::Path,
  sync::{Arc, Mutex, OnceLock},
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

// Arc'd cache entries: every settings/keybinding accessor previously DEEP
// CLONED the whole cached struct (keymap, per-command key vectors, recents,
// theme — ~13KB) per lookup; ribbon tooltips alone made 21k such lookups per
// session (274MB). An Arc bump replaces all of that.
static APP_SETTINGS_CACHE: OnceLock<Mutex<Option<Arc<CachedAppSettings>>>> = OnceLock::new();
static APP_SETTINGS_WATCHER: OnceLock<Mutex<Option<RecommendedWatcher>>> = OnceLock::new();

fn app_settings_cache() -> &'static Mutex<Option<Arc<CachedAppSettings>>> {
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
  load_cached_app_settings().settings.clone()
}

fn load_cached_app_settings() -> Arc<CachedAppSettings> {
  let path = settings_path();
  start_app_settings_watcher(&path);

  if let Ok(cache) = app_settings_cache().lock()
    && let Some(cached) = cache.as_ref().filter(|cached| cached.path == path)
  {
    return Arc::clone(cached);
  }

  let settings = load_app_settings_from_path(path.clone()).unwrap_or_default();
  let cached = Arc::new(cached_settings_from(settings, path));
  if let Ok(mut cache) = app_settings_cache().lock() {
    *cache = Some(Arc::clone(&cached));
  }
  cached
}

fn load_app_settings_from_path(path: PathBuf) -> io::Result<AppSettings> {
  match fs::read_to_string(&path) {
    Ok(text) => match parse_app_settings(&text) {
      Ok(settings) => Ok(settings),
      Err(error) => {
        // Law 2/7: a malformed settings file used to be silently discarded —
        // the next save then OVERWROTE the user's file with defaults. Now the
        // broken file is preserved beside the live one and the workspace
        // surfaces a persistent warning (see `take_settings_load_warning`).
        let backup = path.with_extension("toml.invalid");
        let backup_note = match fs::copy(&path, &backup) {
          Ok(_) => format!("the original was saved to {}", backup.display()),
          Err(copy_error) => format!("backing up the original FAILED: {copy_error}"),
        };
        record_settings_load_warning(format!(
          "Settings file could not be parsed ({error}); defaults are in effect and {backup_note}"
        ));
        Ok(AppSettings::default())
      },
    },
    Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(AppSettings::default()),
    Err(err) => Err(err),
  }
}

fn parse_app_settings(text: &str) -> Result<AppSettings, toml::de::Error> {
  toml::from_str(text)
}

fn settings_load_warning_slot() -> &'static Mutex<Option<String>> {
  static WARNING: OnceLock<Mutex<Option<String>>> = OnceLock::new();
  WARNING.get_or_init(|| Mutex::new(None))
}

fn record_settings_load_warning(message: String) {
  tracing::error!("{message}");
  if let Ok(mut slot) = settings_load_warning_slot().lock() {
    *slot = Some(message);
  }
}

/// One-shot: the workspace drains this at construction and surfaces it in
/// the status bar's activity zone as a persistent failure.
pub fn take_settings_load_warning() -> Option<String> {
  settings_load_warning_slot().lock().ok()?.take()
}

#[hotpath::measure]
pub fn load_document_theme() -> DocumentTheme {
  load_app_settings()
    .document_theme
    .map(DocumentTheme::from)
    .unwrap_or_else(flowstate_document_theme)
}

#[hotpath::measure]
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
  load_cached_app_settings().effective_keymap.clone()
}

pub fn load_dropbox_collaboration() -> Option<(flowstate_collab::dropbox::DropboxCredentials, String)> {
  let settings = load_app_settings().dropbox_collaboration;
  if !settings.enabled || settings.access_token.is_empty() {
    return None;
  }
  Some((
    flowstate_collab::dropbox::DropboxCredentials {
      app_key: settings.app_key,
      access_token: settings.access_token,
      refresh_token: settings.refresh_token,
      access_token_expires_at: settings.access_token_expires_at,
    },
    settings.root,
  ))
}

pub fn load_dropbox_document_binding(path: &Path) -> Option<DropboxDocumentBinding> {
  let canonical = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
  load_app_settings()
    .dropbox_documents
    .into_iter()
    .find(|binding| {
      binding
        .local_path
        .canonicalize()
        .unwrap_or_else(|_| binding.local_path.clone())
        == canonical
    })
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
  let profile = load_local_user_profile();
  (profile.user_id, Some(profile.display_name))
}

pub fn load_local_identity_secret() -> Option<flowstate_collab::identity::PortableIdentitySecret> {
  // Ensure first-run identity generation has happened before reading the seed.
  let _ = load_local_user_profile();
  load_app_settings()
    .local_identity_signing_secret
    .as_deref()
    .and_then(|secret| flowstate_collab::identity::PortableIdentitySecret::from_hex(secret).ok())
}

pub fn load_local_signed_profile() -> Option<flowstate_collab::identity::SignedProfile> {
  let profile = load_local_user_profile();
  let settings = load_app_settings();
  let secret = settings
    .local_identity_signing_secret
    .as_deref()
    .and_then(|secret| flowstate_collab::identity::PortableIdentitySecret::from_hex(secret).ok())?;
  let avatar_digest = profile
    .avatar_path
    .as_deref()
    .and_then(|path| fs::read(path).ok())
    .map(|bytes| *blake3::hash(&bytes).as_bytes());
  Some(secret.sign_profile(
    settings.local_profile_sequence.max(1),
    profile.display_name,
    profile.color_rgb,
    avatar_digest,
  ))
}

/// Load the one active portable person profile and ensure its per-device and
/// signing identities exist. These writes are silent metadata persistence and
/// never mark an open document dirty.
pub fn load_local_user_profile() -> LocalUserProfile {
  let mut settings = load_app_settings();
  let mut changed = false;
  if settings.local_user_id == 0 {
    settings.local_user_id = uuid::Uuid::new_v4().as_u128();
    changed = true;
  }
  if settings.local_device_id == 0 {
    settings.local_device_id = uuid::Uuid::new_v4().as_u128();
    changed = true;
  }
  if settings
    .local_user_display_name
    .as_deref()
    .is_none_or(str::is_empty)
  {
    settings.local_user_display_name = os_username().or_else(|| Some("Flowstate user".into()));
    changed = true;
  }
  if settings.local_user_color_rgb.is_none() {
    let digest = blake3::hash(&settings.local_user_id.to_le_bytes());
    settings.local_user_color_rgb =
      Some(flowstate_collab::ids::PALETTE[usize::from(digest.as_bytes()[0]) % flowstate_collab::ids::PALETTE.len()]);
    changed = true;
  }
  if settings
    .local_identity_signing_secret
    .as_deref()
    .and_then(|secret| flowstate_collab::identity::PortableIdentitySecret::from_hex(secret).ok())
    .is_none()
  {
    settings.local_identity_signing_secret = Some(flowstate_collab::identity::PortableIdentitySecret::generate().to_hex());
    settings.local_profile_sequence = settings.local_profile_sequence.max(1);
    changed = true;
  }
  let secret = settings
    .local_identity_signing_secret
    .as_deref()
    .unwrap_or_default();
  let fingerprint = flowstate_collab::identity::PortableIdentitySecret::from_hex(secret)
    .map(|secret| secret.public().to_string())
    .unwrap_or_else(|_| blake3::hash(secret.as_bytes()).to_hex().to_string());
  let profile = LocalUserProfile {
    user_id: settings.local_user_id,
    device_id: settings.local_device_id,
    display_name: settings
      .local_user_display_name
      .clone()
      .unwrap_or_else(|| "Flowstate user".into()),
    color_rgb: settings.local_user_color_rgb.unwrap_or(0x5b8def) & 0x00ff_ffff,
    avatar_path: settings.local_user_avatar_path.clone(),
    identity_fingerprint: fingerprint,
  };
  if changed && let Err(error) = save_app_settings(settings) {
    tracing::warn!(error = %error, "persisting generated local collaboration profile failed");
  }
  profile
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

/// Evaluate standing access for ambient discovery. A TOFU contact is never
/// enough: the contact must be safety-code verified, discovery must be active,
/// and the document must fall inside an explicit person override or squad
/// default without matching an exclusion.
pub fn standing_access_for_path(identity_key: &str, document_path: &Path) -> StandingAccess {
  standing_access_in_settings(&load_app_settings(), identity_key, document_path)
}

pub fn trusted_identity_keys_for_path(document_path: &Path) -> Vec<iroh::PublicKey> {
  let settings = load_app_settings();
  settings
    .trusted_collaborators
    .iter()
    .filter(|contact| standing_access_in_settings(&settings, &contact.identity_key, document_path) == StandingAccess::Allowed)
    .filter_map(|contact| contact.identity_key.parse().ok())
    .collect()
}

fn standing_access_in_settings(settings: &AppSettings, identity_key: &str, document_path: &Path) -> StandingAccess {
  if settings.collaboration_discovery_paused {
    return StandingAccess::DiscoveryPaused;
  }
  let Some(contact) = settings
    .trusted_collaborators
    .iter()
    .find(|contact| contact.identity_key == identity_key)
  else {
    return StandingAccess::UnknownIdentity;
  };
  if !contact.verified {
    return StandingAccess::VerificationRequired;
  }

  let squad_scopes = settings
    .collaboration_squads
    .iter()
    .filter(|squad| {
      squad
        .member_identity_keys
        .iter()
        .any(|member| member == identity_key)
    })
    .flat_map(|squad| squad.default_scopes.iter());
  let all_scopes = contact
    .scopes
    .iter()
    .chain(squad_scopes)
    .collect::<Vec<_>>();
  if all_scopes
    .iter()
    .any(|scope| matches!(scope, CollaborationScope::Exclusion(path) if document_path.starts_with(path)))
  {
    return StandingAccess::OutOfScope;
  }
  let person_has_positive_override = contact
    .scopes
    .iter()
    .any(|scope| !matches!(scope, CollaborationScope::Exclusion(_)));
  let allowed = all_scopes.iter().any(|scope| match scope {
    CollaborationScope::Document(path) => path == document_path,
    CollaborationScope::Folder(path) => document_path.starts_with(path),
    CollaborationScope::Global => true,
    CollaborationScope::Exclusion(_) => false,
  });
  if person_has_positive_override {
    let person_allowed = contact.scopes.iter().any(|scope| match scope {
      CollaborationScope::Document(path) => path == document_path,
      CollaborationScope::Folder(path) => document_path.starts_with(path),
      CollaborationScope::Global => true,
      CollaborationScope::Exclusion(_) => false,
    });
    if person_allowed {
      StandingAccess::Allowed
    } else {
      StandingAccess::OutOfScope
    }
  } else if allowed {
    StandingAccess::Allowed
  } else {
    StandingAccess::OutOfScope
  }
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

pub fn save_dropbox_collaboration(credentials: flowstate_collab::dropbox::DropboxCredentials, root: String, enabled: bool) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.dropbox_collaboration = DropboxCollaborationSettings {
    enabled,
    root,
    app_key: credentials.app_key,
    access_token: credentials.access_token,
    refresh_token: credentials.refresh_token,
    access_token_expires_at: credentials.access_token_expires_at,
  };
  save_app_settings(settings)
}

pub fn save_collaboration_discovery_options(paused: bool, bluetooth_enabled: bool) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.collaboration_discovery_paused = paused;
  settings.bluetooth_collaboration_discovery_enabled = bluetooth_enabled;
  save_app_settings(settings)
}

pub fn save_dropbox_connection_draft(app_key: String, root: String) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.dropbox_collaboration.app_key = app_key;
  settings.dropbox_collaboration.root = root;
  save_app_settings(settings)
}

pub fn disconnect_dropbox_collaboration() -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.dropbox_collaboration.enabled = false;
  settings.dropbox_collaboration.access_token.clear();
  settings.dropbox_collaboration.refresh_token = None;
  settings.dropbox_collaboration.access_token_expires_at = None;
  save_app_settings(settings)
}

pub fn save_local_collaboration_profile(display_name: String, color_rgb: u32, avatar_path: Option<PathBuf>) -> io::Result<()> {
  let mut settings = load_app_settings();
  settings.local_user_display_name = Some(display_name);
  settings.local_user_color_rgb = Some(color_rgb & 0x00ff_ffff);
  settings.local_user_avatar_path = avatar_path;
  settings.local_profile_sequence = settings.local_profile_sequence.max(1).saturating_add(1);
  save_app_settings(settings)
}

pub fn save_trusted_collaborator(collaborator: TrustedCollaborator) -> io::Result<()> {
  let mut settings = load_app_settings();
  if let Some(existing) = settings
    .trusted_collaborators
    .iter_mut()
    .find(|existing| existing.identity_key == collaborator.identity_key)
  {
    *existing = collaborator;
  } else {
    settings.trusted_collaborators.push(collaborator);
  }
  save_app_settings(settings)
}

pub fn remove_trusted_collaborator(identity_key: &str) -> io::Result<bool> {
  let mut settings = load_app_settings();
  let before = settings.trusted_collaborators.len();
  settings
    .trusted_collaborators
    .retain(|collaborator| collaborator.identity_key != identity_key);
  for squad in &mut settings.collaboration_squads {
    squad
      .member_identity_keys
      .retain(|member| member != identity_key);
  }
  let removed = before != settings.trusted_collaborators.len();
  if removed {
    save_app_settings(settings)?;
  }
  Ok(removed)
}

pub fn save_collaboration_squad(squad: CollaborationSquad) -> io::Result<()> {
  let mut settings = load_app_settings();
  if let Some(existing) = settings
    .collaboration_squads
    .iter_mut()
    .find(|existing| existing.id == squad.id)
  {
    *existing = squad;
  } else {
    settings.collaboration_squads.push(squad);
  }
  save_app_settings(settings)
}

pub fn remove_collaboration_squad(id: &str) -> io::Result<bool> {
  let mut settings = load_app_settings();
  let before = settings.collaboration_squads.len();
  settings.collaboration_squads.retain(|squad| squad.id != id);
  let removed = before != settings.collaboration_squads.len();
  if removed {
    save_app_settings(settings)?;
  }
  Ok(removed)
}

pub fn save_dropbox_document_binding(binding: DropboxDocumentBinding) -> io::Result<()> {
  let mut settings = load_app_settings();
  if let Some(existing) = settings
    .dropbox_documents
    .iter_mut()
    .find(|existing| existing.local_path == binding.local_path)
  {
    *existing = binding;
  } else {
    settings.dropbox_documents.push(binding);
  }
  save_app_settings(settings)
}

pub fn remove_dropbox_document_binding(path: &Path) -> io::Result<bool> {
  let mut settings = load_app_settings();
  let before = settings.dropbox_documents.len();
  settings
    .dropbox_documents
    .retain(|binding| binding.local_path != path);
  let removed = settings.dropbox_documents.len() != before;
  if removed {
    save_app_settings(settings)?;
  }
  Ok(removed)
}

#[hotpath::measure]
pub fn save_app_settings(settings: AppSettings) -> io::Result<()> {
  let path = settings_path();
  start_app_settings_watcher(&path);
  save_app_settings_to_path(&settings, path.clone())?;
  let cached = Arc::new(cached_settings_from(settings, path));
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
  fs::write(&path, text)?;
  restrict_to_owner(&path)
}

/// The settings file carries the identity signing seed and Dropbox tokens,
/// so it must never be group/world readable.
#[cfg(unix)]
fn restrict_to_owner(path: &Path) -> io::Result<()> {
  use std::os::unix::fs::PermissionsExt as _;
  fs::set_permissions(path, fs::Permissions::from_mode(0o600))
}

#[cfg(not(unix))]
fn restrict_to_owner(_path: &Path) -> io::Result<()> {
  Ok(())
}
