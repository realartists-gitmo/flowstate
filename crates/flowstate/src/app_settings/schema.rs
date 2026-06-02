use std::{fs, io, path::PathBuf};

use gpui::{Hsla, px};
use gpui_component::PixelsExt;
use serde::{Deserialize, Serialize};

use crate::ribbon::RibbonMode;
use crate::rich_text_element::{
  CustomParagraphBorder, CustomParagraphStyle, CustomSemanticStyle, DocumentTheme, ThemeUnderline, flowstate_document_theme,
};
use dirs::{config_dir, data_dir};

#[derive(Clone, Default, Deserialize, Serialize)]
#[serde(default)]
pub struct AppSettings {
  pub theme_name: Option<String>,
  pub document_theme: Option<DocumentThemeSettings>,
  pub editor: EditorSettings,
  pub toolkit: ToolkitSettings,
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(default)]
pub struct EditorSettings {
  pub ribbon_mode: RibbonMode,
  pub smart_word_selection: bool,
  pub autosave: bool,
  pub send_to_document_directory: bool,
  pub send_custom_directory: Option<PathBuf>,
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
      ribbon_mode: RibbonMode::default(),
      smart_word_selection: true,
      autosave: false,
      send_to_document_directory: true,
      send_custom_directory: None,
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
  pub hat_box_enabled: bool,
  pub hat_border_width: f32,
  pub block_box_enabled: bool,
  pub block_border_width: f32,
  pub tag_box_enabled: bool,
  pub tag_border_width: f32,
  pub analytic_box_enabled: bool,
  pub analytic_border_width: f32,
  pub undertag_box_enabled: bool,
  pub undertag_border_width: f32,
  pub cite_box_enabled: bool,
  pub cite_border_width: f32,
  pub emphasis_box_enabled: bool,
  pub emphasis_border_width: f32,
  pub underline_box_enabled: bool,
  pub underline_border_width: f32,
  pub condensed_box_enabled: bool,
  pub condensed_border_width: f32,
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
