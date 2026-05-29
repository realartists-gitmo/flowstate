use gpui::{
  AnyElement, App, Context, Entity, Hsla, IntoElement, Keystroke, ParentElement as _, Render, Styled as _, Window, div, prelude::*, px, relative,
};
use gpui_component::Size;
use gpui_component::button::DropdownButton;
use gpui_component::button::{Button, ButtonGroup, ButtonVariants as _, Toggle, ToggleVariants as _};
use gpui_component::kbd::Kbd;
use gpui_component::menu::PopupMenuItem;
use gpui_component::{ActiveTheme as _, Disableable as _, Icon, IconName, PixelsExt as _, Selectable as _, Sizable as _, StyledExt as _};
use serde::{Deserialize, Serialize};

use crate::commands::{CommandId, default_keys_for};
use crate::ribbon::style_catalog::{HIGHLIGHT_STYLE_SPECS, PARAGRAPH_STYLE_SPECS, SEMANTIC_STYLE_SPECS};
use crate::rich_text_element::{
  ApplyHighlightToSelection, ArmedInlineTool, DocumentTheme, HighlightStyle, ParagraphStyle, RichTextEditor, RichTextEditorStyleState,
  RunSemanticStyle, SelectionState,
};

/// User-selectable ribbon renderer.
///
/// This enum is intentionally serializable so a future settings panel can save
/// `editor.ribbon_mode` without touching the render implementations.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum RibbonMode {
  Legacy,
  #[default]
  Modern,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum RibbonDensity {
  Full,
  Compact,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ShortcutVisibility {
  Always,
  HideInCompact,
  HoverOnly,
  Hidden,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct ModernRibbonOptions {
  pub density: RibbonDensity,
  pub shortcut_visibility: ShortcutVisibility,
}

#[hotpath::measure_all]
impl Default for ModernRibbonOptions {
  fn default() -> Self {
    Self {
      density: RibbonDensity::Full,
      shortcut_visibility: ShortcutVisibility::Always,
    }
  }
}

/// Switching layer for the editor styles ribbon.
///
/// `LegacyStylesRibbon` and `ModernStylesRibbon` stay separate so the old
/// ribbon can be restored by changing only this mode value.
pub struct EditorRibbon {
  editor: Entity<RichTextEditor>,
  mode: RibbonMode,
  modern_options: ModernRibbonOptions,
  height: gpui::Pixels,
}

/// Compatibility name for code that wants to talk in settings terms.
pub type StylesRibbon = EditorRibbon;

#[hotpath::measure_all]
impl EditorRibbon {
  pub fn new(editor: Entity<RichTextEditor>) -> Self {
    Self::new_with_mode(editor, RibbonMode::default())
  }

  pub fn new_with_mode(editor: Entity<RichTextEditor>, mode: RibbonMode) -> Self {
    Self {
      editor,
      mode,
      modern_options: ModernRibbonOptions::default(),
      height: default_ribbon_height(),
    }
  }

  pub fn mode(&self) -> RibbonMode {
    self.mode
  }

  /// Future settings panels can call this after updating
  /// `settings.editor.ribbon_mode`.
  pub fn set_mode(&mut self, mode: RibbonMode, cx: &mut Context<Self>) {
    if self.mode != mode {
      self.mode = mode;
      cx.notify();
    }
  }

  pub fn set_modern_options(&mut self, modern_options: ModernRibbonOptions, cx: &mut Context<Self>) {
    if self.modern_options != modern_options {
      self.modern_options = modern_options;
      cx.notify();
    }
  }

  pub fn set_height(&mut self, height: gpui::Pixels, cx: &mut Context<Self>) {
    if self.height != height {
      self.height = height;
      cx.notify();
    }
  }

  fn paragraph_selected(state: &RichTextEditorStyleState, style: ParagraphStyle) -> bool {
    matches!(state.paragraph_style, SelectionState::Uniform(current) if current == style)
  }

  fn semantic_selected(state: &RichTextEditorStyleState, armed_tool: Option<ArmedInlineTool>, style: RunSemanticStyle) -> bool {
    matches!(armed_tool, Some(ArmedInlineTool::Semantic(current)) if current == style)
      || matches!(state.semantic, SelectionState::Uniform(current) if current == style)
  }

  fn underline_selected(state: &RichTextEditorStyleState, armed_tool: Option<ArmedInlineTool>) -> bool {
    matches!(armed_tool, Some(ArmedInlineTool::Underline)) || matches!(state.underline, SelectionState::Uniform(true))
  }

  fn strikethrough_selected(state: &RichTextEditorStyleState, armed_tool: Option<ArmedInlineTool>) -> bool {
    matches!(armed_tool, Some(ArmedInlineTool::Strikethrough)) || matches!(state.strikethrough, SelectionState::Uniform(true))
  }

  fn highlight_selected(state: &RichTextEditorStyleState, armed_tool: Option<ArmedInlineTool>, style: HighlightStyle) -> bool {
    matches!(armed_tool, Some(ArmedInlineTool::Highlight(current)) if current == style)
      || matches!(state.highlight, SelectionState::Uniform(Some(current)) if current == style)
  }
}

