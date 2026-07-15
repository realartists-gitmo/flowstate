use std::{fs, io, path::PathBuf};

use gpui::{Hsla, px};
use gpui_component::PixelsExt;
use serde::{Deserialize, Serialize};

use crate::rich_text_element::{
  CustomParagraphBorder, CustomParagraphStyle, CustomSemanticStyle, DocumentTheme, ThemeUnderline, flowstate_document_theme,
};
use dirs::{config_dir, data_dir};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AppSettings {
  pub theme_name: Option<String>,
  // §15/§31: a stable per-install durable author identity. Serialized as a
  // string because a v4 UUID rendered as `u128` overflows TOML's signed 64-bit
  // integer range. A value of `0` means "not generated yet";
  // `load_local_user_identity` mints one and persists it on first read. Kept
  // ahead of the table-valued fields below so TOML serialization stays valid.
  #[serde(with = "local_user_id_serde")]
  pub local_user_id: u128,
  pub local_user_display_name: Option<String>,
  /// Stable profile metadata shared by presence, revisions, and discovery.
  pub local_user_color_rgb: Option<u32>,
  pub local_user_avatar_path: Option<PathBuf>,
  #[serde(with = "local_user_id_serde")]
  pub local_device_id: u128,
  /// Plaintext-at-rest pending the platform keychain security review.
  pub local_identity_signing_secret: Option<String>,
  /// Monotonic signed-profile revision. Incremented whenever public profile
  /// metadata changes so peers can reject stale rename/avatar updates.
  pub local_profile_sequence: u64,
  pub collaboration_discovery_paused: bool,
  /// Nearby BLE rendezvous is opt-in because enabling it asks the operating
  /// system for radio/privacy permission.
  pub bluetooth_collaboration_discovery_enabled: bool,
  pub dropbox_collaboration: DropboxCollaborationSettings,
  pub dropbox_documents: Vec<DropboxDocumentBinding>,
  pub document_theme: Option<DocumentThemeSettings>,
  pub editor: EditorSettings,
  pub toolkit: ToolkitSettings,
  pub recent_documents: Vec<PathBuf>,
  pub trusted_collaborators: Vec<TrustedCollaborator>,
  pub collaboration_squads: Vec<CollaborationSquad>,
  pub keymap: Vec<crate::commands::KeymapEntry>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct DropboxCollaborationSettings {
  pub enabled: bool,
  /// Dropbox folder containing the document, or empty for the app root.
  pub root: String,
  pub app_key: String,
  /// Plaintext-at-rest pending the same platform-keychain review as the local
  /// identity secret. Never included in diagnostics exports.
  pub access_token: String,
  pub refresh_token: Option<String>,
  pub access_token_expires_at: Option<u64>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct DropboxDocumentBinding {
  pub local_path: PathBuf,
  pub remote_path: String,
  /// Last Dropbox revision successfully downloaded or uploaded. `None` makes
  /// the first upload create-only rather than overwriting unknown cloud data.
  pub revision: Option<String>,
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct TrustedCollaborator {
  pub identity_key: String,
  pub display_name: String,
  pub avatar_path: Option<PathBuf>,
  pub color_rgb: Option<u32>,
  pub verified: bool,
  pub scopes: Vec<CollaborationScope>,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "kind", content = "path", rename_all = "snake_case")]
pub enum CollaborationScope {
  Document(PathBuf),
  Folder(PathBuf),
  Global,
  Exclusion(PathBuf),
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct CollaborationSquad {
  pub id: String,
  pub name: String,
  pub member_identity_keys: Vec<String>,
  pub default_scopes: Vec<CollaborationScope>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LocalUserProfile {
  pub user_id: u128,
  pub device_id: u128,
  pub display_name: String,
  pub color_rgb: u32,
  pub avatar_path: Option<PathBuf>,
  pub identity_fingerprint: String,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum StandingAccess {
  Allowed,
  DiscoveryPaused,
  UnknownIdentity,
  VerificationRequired,
  OutOfScope,
}

/// Serde adapter that stores `local_user_id` as a string. TOML integers are
/// signed 64-bit, so a 128-bit identity cannot round-trip as a native integer.
mod local_user_id_serde {
  use serde::{Deserialize, Deserializer, Serializer};

  pub fn serialize<S>(value: &u128, serializer: S) -> Result<S::Ok, S::Error>
  where
    S: Serializer,
  {
    serializer.serialize_str(&value.to_string())
  }

  pub fn deserialize<'de, D>(deserializer: D) -> Result<u128, D::Error>
  where
    D: Deserializer<'de>,
  {
    let raw = String::deserialize(deserializer)?;
    raw.parse::<u128>().map_err(serde::de::Error::custom)
  }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct EditorSettings {
  pub smart_word_selection: bool,
  pub autosave: bool,
  pub send_to_document_directory: bool,
  pub send_custom_directory: Option<PathBuf>,
  /// D-S4: collapse all app-level animation to zero duration. Settings-UI
  /// row lands with the P5-S2 unified home; editable in settings.toml now.
  #[serde(default)]
  pub reduce_motion: bool,
}

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct ToolkitSettings {
  pub tub_root: Option<PathBuf>,
}

#[hotpath::measure_all]
impl Default for EditorSettings {
  fn default() -> Self {
    Self {
      smart_word_selection: true,
      autosave: false,
      send_to_document_directory: true,
      send_custom_directory: None,
      reduce_motion: false,
    }
  }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct DocumentThemeSettings {
  pub default_font_family: String,
  pub default_text_color: StoredHsla,
  pub document_background_color: StoredHsla,
  pub pageless_inset_x: f32,
  pub pageless_inset_top: f32,
  pub pageless_inset_bottom: f32,
  pub body_font_size: f32,
  pub cite_font_size: f32,
  pub condensed_font_size: f32,
  pub ultracondensed_font_size: f32,
  pub pocket_font_size: f32,
  pub hat_font_size: f32,
  pub block_font_size: f32,
  pub tag_font_size: f32,
  pub undertag_font_size: f32,
  pub line_spacing: f32,
  pub line_gap_fraction: f32,
  pub paragraph_after: f32,
  pub pocket_before: f32,
  pub hat_before: f32,
  pub block_before: f32,
  pub tag_before: f32,
  #[serde(default = "default_true")]
  pub pocket_box_enabled: bool,
  pub pocket_border_width: f32,
  pub pocket_border_space_x: f32,
  pub pocket_border_space_y: f32,
  #[serde(default = "default_true")]
  pub hat_box_enabled: bool,
  pub hat_border_width: f32,
  #[serde(default = "default_true")]
  pub block_box_enabled: bool,
  pub block_border_width: f32,
  #[serde(default = "default_true")]
  pub tag_box_enabled: bool,
  pub tag_border_width: f32,
  #[serde(default = "default_true")]
  pub analytic_box_enabled: bool,
  pub analytic_border_width: f32,
  #[serde(default = "default_true")]
  pub undertag_box_enabled: bool,
  pub undertag_border_width: f32,
  #[serde(default = "default_true")]
  pub cite_box_enabled: bool,
  pub cite_border_width: f32,
  #[serde(default = "default_true")]
  pub emphasis_box_enabled: bool,
  pub emphasis_border_width: f32,
  #[serde(default = "default_true")]
  pub underline_box_enabled: bool,
  pub underline_border_width: f32,
  #[serde(default = "default_true")]
  pub condensed_box_enabled: bool,
  pub condensed_border_width: f32,
  #[serde(default = "default_true")]
  pub ultracondensed_box_enabled: bool,
  pub ultracondensed_border_width: f32,
  pub emphasis_border_paint_width: f32,
  pub box_padding_left: f32,
  pub box_padding_right: f32,
  pub box_padding_top: f32,
  pub box_padding_bottom: f32,
  pub highlight_pad_x: f32,
  pub highlight_top_extra_fraction: f32,
  pub highlight_bottom_extra_fraction: f32,
  pub underline_fallback_top_from_baseline: f32,
  pub underline_rule_thickness: f32,
  pub snap_underline_rules_to_pixels: bool,
  pub double_underline_top_from_baseline: f32,
  pub double_underline_gap: f32,
  pub highlight_spoken: StoredHsla,
  pub highlight_insert: StoredHsla,
  pub highlight_alternative: StoredHsla,
  pub highlight_marked: StoredHsla,
  pub pocket_color: StoredHsla,
  pub hat_color: StoredHsla,
  pub block_color: StoredHsla,
  pub tag_color: StoredHsla,
  pub analytic_color: StoredHsla,
  pub undertag_color: StoredHsla,
  pub cite_color: StoredHsla,
  pub underline_color: StoredHsla,
  pub emphasis_color: StoredHsla,
  pub condensed_color: StoredHsla,
  pub ultracondensed_color: StoredHsla,
  pub normal_bold: bool,
  pub normal_italic: bool,
  pub normal_underline: ThemeUnderlineSetting,
  pub pocket_bold: bool,
  pub pocket_italic: bool,
  pub pocket_underline: ThemeUnderlineSetting,
  pub hat_bold: bool,
  pub hat_italic: bool,
  pub hat_underline: ThemeUnderlineSetting,
  pub block_bold: bool,
  pub block_italic: bool,
  pub block_underline: ThemeUnderlineSetting,
  pub tag_bold: bool,
  pub tag_italic: bool,
  pub tag_underline: ThemeUnderlineSetting,
  pub analytic_bold: bool,
  pub analytic_italic: bool,
  pub analytic_underline: ThemeUnderlineSetting,
  pub undertag_bold: bool,
  pub undertag_italic: bool,
  pub undertag_underline: ThemeUnderlineSetting,
  pub cite_bold: bool,
  pub cite_italic: bool,
  pub cite_underline: ThemeUnderlineSetting,
  pub underline_bold: bool,
  pub underline_italic: bool,
  pub underline_underline: ThemeUnderlineSetting,
  pub emphasis_bold: bool,
  pub emphasis_italic: bool,
  pub emphasis_underline: ThemeUnderlineSetting,
  pub condensed_bold: bool,
  pub condensed_italic: bool,
  pub condensed_underline: ThemeUnderlineSetting,
  pub ultracondensed_bold: bool,
  pub ultracondensed_italic: bool,
  pub ultracondensed_underline: ThemeUnderlineSetting,
}

fn default_true() -> bool {
  true
}

#[derive(Clone, Copy, Deserialize, Serialize)]
pub struct StoredHsla {
  h: f32,
  s: f32,
  l: f32,
  a: f32,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
pub enum ThemeUnderlineSetting {
  #[default]
  None,
  Single,
  Double,
}

#[hotpath::measure_all]
impl Default for DocumentThemeSettings {
  fn default() -> Self {
    Self::from(&flowstate_document_theme())
  }
}
