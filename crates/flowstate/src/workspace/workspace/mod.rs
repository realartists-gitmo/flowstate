use std::{
  cell::{Cell, RefCell},
  collections::{HashMap, HashSet},
  fs,
  path::{Path, PathBuf},
  rc::Rc,
  sync::Arc,
  time::Duration,
};

use gpui::{
  Anchor as Corner, AnyElement, AnyWindowHandle, App, Context, DismissEvent, DummyKeyboardMapper, Entity, Focusable, Hsla, InteractiveElement,
  IntoElement, KeyBinding, Keystroke, MouseButton, NoAction, PathPromptOptions, Pixels, Point, PromptButton, PromptLevel, Render, ScrollHandle,
  SharedString, Subscription, WeakEntity, Window, WindowBounds, WindowDecorations, WindowOptions, anchored, black, deferred, div, prelude::*,
  px,
};
#[cfg(target_os = "windows")]
use gpui::{Bounds, size};
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants};
use gpui_component::checkbox::Checkbox;
use gpui_component::color_picker::{ColorPicker, ColorPickerState};
use gpui_component::input::{Input, InputEvent, InputState, NumberInput, NumberInputEvent, StepAction};
use gpui_component::list::ListItem;
use gpui_component::menu::{DropdownMenu as _, PopupMenu, PopupMenuItem};
use gpui_component::resizable::{ResizableState, h_resizable, resizable_panel, v_resizable};
use gpui_component::scroll::ScrollableElement;
use gpui_component::select::{SearchableVec, Select, SelectEvent, SelectState};
use gpui_component::setting::{SettingField, SettingGroup, SettingItem, SettingPage, Settings};
use gpui_component::slider::{Slider, SliderEvent, SliderState, SliderValue};
use gpui_component::tab::{Tab, TabBar};
use gpui_component::tree::{TreeItem, TreeState, tree};
use gpui_component::{
  ActiveTheme as _, Colorize as _, Disableable, Icon, IconName, Root, Selectable, Sizable, Theme, ThemeRegistry, TitleBar,
  VirtualListScrollHandle, WindowExt as _, h_flex, v_flex,
};
use rustc_hash::{FxHashMap, FxHashSet};
use uuid::Uuid;

use crate::app_settings::{
  load_autosave, load_document_theme, load_recent_documents, load_send_custom_directory,
  load_send_to_document_directory, load_smart_word_selection, load_tub_root, save_autosave, save_document_theme, save_recent_documents,
  save_send_custom_directory, save_send_to_document_directory, save_smart_word_selection, save_theme_name,
};
#[cfg(not(target_family = "wasm"))]
use crate::app_settings::load_local_user_identity;
use crate::commands::CommandId;
#[cfg(not(target_family = "wasm"))]
use crate::docx_conversion::{convert_docx_to_document, import_docx_to_loro};
#[cfg(not(target_family = "wasm"))]
use crate::flow::{FlowEditor, FlowPanel};
use crate::rich_text_element::{
  ArmedInlineTool, CustomParagraphBorder, DocumentProjection, DocumentTheme, InputParagraph, InputRun, ParagraphStyle, RichTextDocumentElement,
  RichTextEditor, Save, SectionKind, ThemeUnderline, ZoomIn, ZoomOut, document_from_input, document_text_slice, flowstate_document_theme,
  paragraph_byte_range, paragraph_index_for_id,
};
use crate::workspace::document_panel::DocumentPanel;
#[cfg(not(target_family = "wasm"))]
use crate::workspace::file_management::{
  UNTITLED_DOCUMENT_NAME, UNTITLED_FLOW_NAME, default_save_directory, new_blank_document, normalize_db8_path, normalize_fl0_path,
};
#[cfg(not(target_family = "wasm"))]
use crate::workspace::file_search_overlay::FileSearchOverlay;
use crate::workspace::icons::{AppIcon, icon_button};
#[cfg(not(target_family = "wasm"))]
use flowstate_tub::{SearchHit, SearchUnitKind, TubFile, TubIndex, TubTreeNode};

pub(super) const APP_CHROME_BORDER_WIDTH: Pixels = px(1.0);
const SIDE_PANEL_COLLAPSED_WIDTH: Pixels = px(30.0);

#[path = "../toolkit_panel.rs"]
#[cfg(not(target_family = "wasm"))]
mod toolkit_panel;

#[cfg(test)]
mod headless_tests;

pub struct Workspace {
  document_panels: Vec<Entity<DocumentPanel>>,
  #[cfg(not(target_family = "wasm"))]
  // §perf: Uuid keys are locally generated and trusted; use FxHash to avoid SipHash overhead.
  document_runtimes: FxHashMap<Uuid, flowstate_collab::doc_io::DocIoHandle>,
  #[cfg(not(target_family = "wasm"))]
  document_runtime_flush_pending: FxHashSet<Uuid>,
  /// §act-three C (background open): panels painted read-only from a phase-V
  /// cached projection whose authority runtime has not yet attached (phase G).
  /// Editing is inert until attach; session-persist + autosave skip them.
  #[cfg(not(target_family = "wasm"))]
  pending_authority_panels: FxHashSet<Uuid>,
  #[cfg(not(target_family = "wasm"))]
  flow_panels: Vec<Entity<FlowPanel>>,
  active_document_id: Option<Uuid>,
  active_editor: Option<Entity<RichTextEditor>>,
  #[cfg(not(target_family = "wasm"))]
  active_flow: Option<Entity<FlowEditor>>,
  ribbon_collapsed: bool,
  outline_collapsed: bool,
  toolkit_collapsed: bool,
  active_toolkit_tool: Option<ToolkitTool>,
  recent_documents: Vec<PathBuf>,
  recent_document_previews: HashMap<PathBuf, DocumentProjection>,
  recent_document_preview_generation: u64,
  #[cfg(not(target_family = "wasm"))]
  temporary_workspace_session_pending: Option<TemporaryWorkspaceSession>,
  #[cfg(not(target_family = "wasm"))]
  temporary_workspace_session_persist_scheduled: bool,
  left_nav_mode: LeftNavMode,
  tab_bar_scroll_handle: ScrollHandle,
  pinned_document_ids: Vec<Uuid>,
  speech_document_id: Option<Uuid>,
  // §perf: Uuid keys are locally generated and trusted; use FxHash to avoid SipHash overhead.
  speech_word_count_cache: FxHashMap<Uuid, (u64, usize)>,
  speech_word_count_pending: FxHashSet<Uuid>,
  body_resizable_state: Entity<ResizableState>,
  content_resizable_state: Entity<ResizableState>,
  ribbon_resizable_state: Entity<ResizableState>,
  committed_ribbon_height: Pixels,
  outline_tree: Entity<TreeState>,
  outline_cache: Option<OutlineCache>,
  collapsed_outline_items: HashSet<usize>,
  outline_revision: u64,
  outline_context_menu: Option<OutlineContextMenu>,
  outline_viewport_paragraph: Option<usize>,
  outline_active_paragraph: Option<usize>,
  outline_scrolled_paragraph: Option<usize>,
  editor_subscriptions: Vec<(Uuid, Subscription)>,
  settings_overlay: Option<WorkspaceSettingsOverlay>,
  document_style_picker_revision: u64,
  document_style_section: DocumentStyleSection,
  settings_section: WorkspaceSettingsSection,
  autosave_enabled: bool,
  // §perf: Uuid keys are locally generated and trusted; use FxHash to avoid SipHash overhead.
  autosave_document_generations: FxHashMap<Uuid, u64>,
  /// §act-five P9-throttle: the latest edit generation a debounced autosave is
  /// SCHEDULED for. A newer edit overwrites it, so the trailing timer coalesces a
  /// burst into ONE checkpoint instead of a full checkpoint per keystroke.
  autosave_pending_generation: FxHashMap<Uuid, u64>,
  #[cfg(not(target_family = "wasm"))]
  autosave_flow_in_flight: FxHashSet<Uuid>,
  #[cfg(not(target_family = "wasm"))]
  collaboration_dialog: Option<Entity<crate::collab::share_dialog::CollabShareDialog>>,
  #[cfg(not(target_family = "wasm"))]
  revision_dialog: Option<Entity<crate::workspace::revision_dialog::RevisionDialog>>,
  #[cfg(not(target_family = "wasm"))]
  comment_dialog: Option<Entity<crate::workspace::comment_dialog::CommentDialog>>,
  // §perf: SessionId keys are locally generated and trusted; use FxHash to avoid SipHash overhead.
  #[cfg(not(target_family = "wasm"))]
  collab_notice_subscriptions: FxHashMap<flowstate_collab::SessionId, Subscription>,
  #[cfg(not(target_family = "wasm"))]
  collab_incompatible_version_notices: HashSet<String>,
  #[cfg(not(target_family = "wasm"))]
  file_search_overlay: Option<Entity<FileSearchOverlay>>,
  #[cfg(not(target_family = "wasm"))]
  tub_root: Option<PathBuf>,
  #[cfg(not(target_family = "wasm"))]
  tub_index: Option<Arc<TubIndex>>,
  #[cfg(not(target_family = "wasm"))]
  tub_files: Vec<TubFile>,
  tub_tree: Entity<TreeState>,
  tub_tree_items: Vec<TreeItem>,
  #[cfg(not(target_family = "wasm"))]
  tub_tree_entries: Vec<TubTreeNode>,
  #[cfg(not(target_family = "wasm"))]
  tub_expanded_dirs: HashSet<PathBuf>,
  tub_file_search_input: Entity<InputState>,
  tub_file_search_generation: u64,
  tub_status: SharedString,
  #[cfg(not(target_family = "wasm"))]
  tub_watcher: Option<flowstate_tub::TubWatcher>,
  tub_watch_polling: bool,
  tub_scan_in_flight: bool,
  tub_scan_pending: bool,
  active_tub_path: Option<PathBuf>,
  toolkit_search_input: Entity<InputState>,
  toolkit_search_filter: ToolkitSearchFilter,
  #[cfg(not(target_family = "wasm"))]
  toolkit_hits: Vec<SearchHit>,
  expanded_toolkit_hits: HashSet<String>,
  toolkit_results_scroll_handle: VirtualListScrollHandle,
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
  pinned: bool,
  pin_index: Option<usize>,
  speech: bool,
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

struct KeymapInputState {
  input: Entity<InputState>,
  initial_value: String,
  _subscription: Subscription,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkspaceSettingsOverlay {
  Styles,
  Settings,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum WorkspaceSettingsSection {
  General,
  Collaboration,
  Keymap,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum LeftNavMode {
  Outline,
  Tub,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ToolkitTool {
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

  #[cfg(not(target_family = "wasm"))]
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
      Self::Collaboration => "Collaboration",
      Self::Keymap => "Keymap",
    }
  }

  fn index(self) -> usize {
    match self {
      Self::General => 0,
      Self::Collaboration => 1,
      Self::Keymap => 2,
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

#[cfg(not(target_family = "wasm"))]
include!("documents.rs");
#[cfg(target_family = "wasm")]
include!("web_documents.rs");
#[cfg(target_family = "wasm")]
mod web_file_picker;
#[cfg(not(target_family = "wasm"))]
include!("collab_prompts.rs");
#[cfg(not(target_family = "wasm"))]
include!("collab.rs");
include!("workspace_state.rs");
#[cfg(not(target_family = "wasm"))]
include!("load.rs");
include!("traits.rs");
#[cfg(not(target_family = "wasm"))]
include!("render_settings.rs");
include!("render_top_bar.rs");
include!("render_body.rs");
include!("render_outline.rs");
include!("render_documents.rs");
include!("render_status.rs");
include!("zoom_status.rs");
#[cfg(not(target_family = "wasm"))]
include!("keybindings.rs");
#[cfg(not(target_family = "wasm"))]
include!("window.rs");
include!("outline.rs");
include!("top_bar.rs");
include!("style_settings.rs");
#[cfg(not(target_family = "wasm"))]
include!("collaboration_settings.rs");
include!("keymap_settings.rs");
include!("theme.rs");
#[cfg(target_family = "wasm")]
include!("web_workspace_actions.rs");
include!("tests.rs");
