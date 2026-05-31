use std::{
  cell::Cell,
  collections::{HashMap, HashSet},
  path::{Path, PathBuf},
  rc::Rc,
  sync::Arc,
};

use gpui::{
  AnyElement, AnyWindowHandle, App, Bounds, Context, Corner, DummyKeyboardMapper, Entity, Focusable, Hsla, InteractiveElement, IntoElement,
  KeyBinding, Keystroke, MouseButton, NoAction, PathPromptOptions, Pixels, PromptButton, PromptLevel, Render, ScrollHandle, SharedString, Subscription,
  WeakEntity, Window, WindowBounds, WindowDecorations, WindowOptions, black, div, prelude::*, px, size,
};
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::color_picker::{ColorPicker, ColorPickerState};
use gpui_component::input::{Input, InputEvent, InputState, NumberInput, NumberInputEvent, StepAction};
use gpui_component::list::ListItem;
use gpui_component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_component::resizable::{ResizableState, h_resizable, resizable_panel, v_resizable};
use gpui_component::scroll::ScrollableElement;
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_component::setting::{SettingGroup, SettingItem, SettingPage, Settings};
use gpui_component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::tree::{TreeItem, TreeState, tree};
use gpui_component::{
  ActiveTheme as _, Colorize as _, Disableable, Icon, IconName, PixelsExt, Root, Selectable, Sizable, Theme, ThemeRegistry, TitleBar, h_flex,
  v_flex,
};
use uuid::Uuid;

use crate::app_settings::{
  load_autosave, load_document_theme, load_send_custom_directory, load_send_to_document_directory, load_smart_word_selection, load_tub_root,
  save_autosave, save_document_theme, save_send_custom_directory, save_send_to_document_directory, save_smart_word_selection, save_theme_name,
};
use crate::commands::{COMMAND_SPECS, CommandId};
use crate::docx_conversion::convert_docx_to_document;
use crate::flow::{FlowEditor, FlowPanel};
use crate::rich_text_element::{
  Document, DocumentTheme, ParagraphStyle, RichTextEditor, Save, ThemeUnderline, ZoomIn, ZoomOut, load_or_create_document, paragraph_byte_range,
};
use crate::workspace::document_panel::DocumentPanel;
use crate::workspace::file_management::{
  UNTITLED_DOCUMENT_NAME, UNTITLED_FLOW_NAME, default_save_directory, new_blank_document, normalize_db8_path, normalize_fl0_path,
};
use crate::workspace::file_search_overlay::FileSearchOverlay;
use crate::workspace::icons::{AppIcon, icon_button};
use flowstate_tub::{SearchHit, SearchUnitKind, TubFile, TubIndex, TubTreeNode};

pub(super) const APP_CHROME_BORDER_WIDTH: Pixels = px(1.0);
const SIDE_PANEL_COLLAPSED_WIDTH: Pixels = px(30.0);

#[path = "../toolkit_panel.rs"]
mod toolkit_panel;

pub struct Workspace {
  document_panels: Vec<Entity<DocumentPanel>>,
  flow_panels: Vec<Entity<FlowPanel>>,
  active_document_id: Option<Uuid>,
  active_editor: Option<Entity<RichTextEditor>>,
  active_flow: Option<Entity<FlowEditor>>,
  ribbon_collapsed: bool,
  outline_collapsed: bool,
  toolkit_collapsed: bool,
  left_nav_mode: LeftNavMode,
  tab_bar_scroll_handle: ScrollHandle,
  body_resizable_state: Entity<ResizableState>,
  content_resizable_state: Entity<ResizableState>,
  ribbon_resizable_state: Entity<ResizableState>,
  committed_ribbon_height: Pixels,
  outline_tree: Entity<TreeState>,
  outline_cache: Option<OutlineCache>,
  collapsed_outline_items: HashSet<usize>,
  outline_revision: u64,
  outline_viewport_paragraph: Option<usize>,
  outline_scrolled_paragraph: Option<usize>,
  editor_subscriptions: Vec<(Uuid, Subscription)>,
  settings_overlay: Option<WorkspaceSettingsOverlay>,
  document_style_section: DocumentStyleSection,
  settings_section: WorkspaceSettingsSection,
  autosave_enabled: bool,
  autosave_document_generations: HashMap<Uuid, u64>,
  autosave_flow_in_flight: HashSet<Uuid>,
  file_search_overlay: Option<Entity<FileSearchOverlay>>,
  tub_root: Option<PathBuf>,
  tub_index: Option<Arc<TubIndex>>,
  tub_files: Vec<TubFile>,
  tub_tree: Entity<TreeState>,
  tub_tree_items: Vec<TreeItem>,
  tub_tree_entries: Vec<TubTreeNode>,
  tub_expanded_dirs: HashSet<PathBuf>,
  tub_file_search_input: Entity<InputState>,
  tub_file_search_generation: u64,
  tub_status: SharedString,
  tub_watcher: Option<flowstate_tub::TubWatcher>,
  tub_watch_polling: bool,
  tub_scan_in_flight: bool,
  tub_scan_pending: bool,
  active_tub_path: Option<PathBuf>,
  toolkit_search_input: Entity<InputState>,
  toolkit_search_filter: ToolkitSearchFilter,
  toolkit_hits: Vec<SearchHit>,
  toolkit_status: SharedString,
  toolkit_search_generation: u64,
  _tub_file_search_subscription: Subscription,
  _toolkit_search_subscription: Subscription,
  zoom_slider: Entity<SliderState>,
  _zoom_slider_subscription: Subscription,
  _keybinding_interceptor: Subscription,
}

#[derive(Clone)]
struct DocumentTab {
  id: Uuid,
  label: SharedString,
  active: bool,
}

type FontFamilySelectDelegate = SearchableVec<SharedString>;

struct FontFamilySelectState {
  select: Entity<SelectState<FontFamilySelectDelegate>>,
  _subscription: Subscription,
}

struct StyleNumberInputState {
  input: Entity<InputState>,
  initial_value: f64,
  _subscriptions: Vec<Subscription>,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkspaceSettingsOverlay {
  Styles,
  Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkspaceSettingsSection {
  General,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LeftNavMode {
  Outline,
  Tub,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolkitSearchFilter {
  All,
  Blocks,
  Tags,
  Analytics,
}

impl ToolkitSearchFilter {
  fn label(self) -> &'static str {
    match self {
      Self::All => "All",
      Self::Blocks => "Blocks",
      Self::Tags => "Tags",
      Self::Analytics => "Analytics",
    }
  }

  fn kinds(self) -> &'static [SearchUnitKind] {
    match self {
      Self::All => &[SearchUnitKind::BlockSection, SearchUnitKind::TagSection, SearchUnitKind::Analytic],
      Self::Blocks => &[SearchUnitKind::BlockSection],
      Self::Tags => &[SearchUnitKind::TagSection],
      Self::Analytics => &[SearchUnitKind::Analytic],
    }
  }
}

impl WorkspaceSettingsSection {
  fn title(self) -> &'static str {
    match self {
      Self::General => "General",
    }
  }

  fn index(self) -> usize {
    match self {
      Self::General => 0,
    }
  }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DocumentStyleSection {
  Text,
  Style,
  Colors,
  Size,
  Background,
}

impl DocumentStyleSection {
  fn title(self) -> &'static str {
    match self {
      Self::Text => "Text",
      Self::Style => "Style",
      Self::Colors => "Colors",
      Self::Size => "Size",
      Self::Background => "Background",
    }
  }

  fn index(self) -> usize {
    match self {
      Self::Text => 0,
      Self::Style => 1,
      Self::Colors => 2,
      Self::Size => 3,
      Self::Background => 4,
    }
  }
}

include!("documents.rs");
include!("workspace_state.rs");
include!("load.rs");
include!("traits.rs");
include!("render_settings.rs");
include!("render_top_bar.rs");
include!("render_body.rs");
include!("render_outline.rs");
include!("render_documents.rs");
include!("render_status.rs");
include!("zoom_status.rs");
include!("keybindings.rs");
include!("window.rs");
include!("outline.rs");
include!("top_bar.rs");
include!("style_settings.rs");
include!("theme.rs");
include!("tests.rs");
