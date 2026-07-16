use std::{
  collections::{VecDeque, hash_map::DefaultHasher},
  fs,
  future::Future,
  hash::{Hash, Hasher},
  io,
  ops::Range,
  path::{Path, PathBuf},
  pin::Pin,
  rc::Rc,
  sync::{Arc, Mutex, OnceLock},
  time::{Duration, Instant},
};

use crop::Rope;
use gpui::{
  App, Bounds, ClipboardEntry, ClipboardItem, Context, CursorStyle, DragMoveEvent, Entity, EntityInputHandler, ExternalPaths, FocusHandle,
  Focusable, Image, ImageFormat, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
  PathPromptOptions, Pixels, Point, Render, SharedString, Size, Subscription, Task, Timer, UTF16Selection, Window, actions, div, img, point,
  prelude::*, px, rgb, size,
};
use gpui_component::ActiveTheme as _;
use gpui_component::scroll::{Scrollbar, ScrollbarHandle, ScrollbarShow};
use gpui_component::{VirtualListScrollHandle, v_virtual_list};
use rustc_hash::{FxHashMap, FxHashSet};

use super::*;

const DISABLE_SCROLL_LIMITING_FUNCTIONS: bool = true; // cfg!(target_os = "linux");---scroll limit is now obsolete on all OSs
const SCROLL_FOREGROUND_OVERSCAN_PX: f32 = 384.0;
const SCROLL_FOREGROUND_MATERIALIZE_BUDGET_MS: u64 = 8;
const SCROLL_FOREGROUND_MAX_CHUNK_LINES: usize = 96;
const TYPING_PREFETCH_SUPPRESSION_WINDOW: Duration = Duration::from_millis(500);
const RECOVERY_WRITE_DEBOUNCE: Duration = Duration::from_millis(750);
const RECOVERY_TYPING_IDLE_WINDOW: Duration = Duration::from_secs(2);
const OFFSCREEN_LAYOUT_CACHE_OVERSCAN_PARAGRAPHS: usize = 24;
const OFFSCREEN_PREP_CACHE_OVERSCAN_PARAGRAPHS: usize = 160;

actions!(
  rich_text_editor,
  [
    MoveLeft,
    MoveRight,
    MoveUp,
    MoveDown,
    MoveLineStart,
    MoveLineEnd,
    SelectLeft,
    SelectRight,
    SelectUp,
    SelectDown,
    SelectLineStart,
    SelectLineEnd,
    SelectAll,
    MoveWordLeft,
    MoveWordRight,
    SelectWordLeft,
    SelectWordRight,
    DeleteWordBackward,
    DeleteWordForward,
    PageUp,
    PageDown,
    SelectPageUp,
    SelectPageDown,
    MoveDocumentStart,
    MoveDocumentEnd,
    SelectDocumentStart,
    SelectDocumentEnd,
    Copy,
    Cut,
    Paste,
    Save,
    Undo,
    Redo,
    SetParagraphStyle0,
    SetParagraphStyle1,
    SetParagraphStyle2,
    SetParagraphStyle3,
    SetParagraphStyle4,
    SetParagraphStyle5,
    SetParagraphStyle6,
    ToggleUnderline,
    ToggleStrikethrough,
    ToggleSemanticStyle1,
    ToggleSemanticStyle2,
    ToggleSemanticStyle3,
    ToggleSemanticStyle4,
    ToggleSemanticStyle5,
    SetHighlightStyle1,
    SetHighlightStyle2,
    SetHighlightStyle3,
    ApplyHighlightToSelection,
    ClearFormatting,
    ClearHighlight,
    InsertImage,
    InsertTable,
    InsertEquation,
    ZoomIn,
    ZoomOut,
    Backspace,
    Delete,
    InsertNewline,
    InsertSoftLineBreak,
  ]
);

// If you add a user-triggerable editor action here, also add it to
// `RichTextEditorCommand`. Host applications should map their own command
// catalogs and default keybindings onto that library action surface so command
// palettes, menus, and shortcut UI stay aligned with editor behavior.

// Direction enums used internally by the movement helpers.
#[derive(Clone, Copy)]
enum HDir {
  Left,
  Right,
}

#[derive(Clone, Copy)]
enum VDir {
  Up,
  Down,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum SelectionGranularity {
  Character,
  Word,
  Paragraph,
}

#[derive(Clone, Debug)]
pub struct ToolkitTextDrag {
  pub title: String,
  pub text: String,
  pub paragraphs: Vec<InputParagraph>,
  pub cursor_offset: Point<Pixels>,
}

impl Render for ToolkitTextDrag {
  fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
    div()
      .id("toolkit-text-drag-root")
      .pl(self.cursor_offset.x + px(8.0))
      .pt(self.cursor_offset.y + px(10.0))
      .child(
        div()
          .id("toolkit-text-drag")
          .w(px(220.0))
          .max_w(px(260.0))
          .rounded(px(6.0))
          .border_1()
          .border_color(rgb(0x94a3b8))
          .bg(rgb(0xffffff))
          .p_2()
          .text_xs()
          .text_color(rgb(0x0f172a))
          .child(self.title.clone()),
      )
  }
}

/// Editor selection (§16).
///
/// `anchor`/`head` remain plain [`DocumentOffset`]s so all existing
/// position/range math keeps working unchanged. Each endpoint additionally
/// carries an explicit [`SelectionAffinity`] and [`VisualGravity`] describing
/// the user's intent. These are a genuine, stored part of the selection — the
/// collaboration runtime reads them directly instead of re-deriving a side from
/// selection direction.
///
/// Construct selections through the helpers below ([`EditorSelection::collapsed`],
/// [`EditorSelection::range`], [`EditorSelection::moved`]) rather than struct
/// literals so the affinity/gravity fields are always populated; all default to
/// neutral.
#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct EditorSelection {
  pub anchor: DocumentOffset,
  pub head: DocumentOffset,
  /// Affinity of the fixed (anchor) endpoint.
  pub anchor_affinity: SelectionAffinity,
  /// Affinity of the moving (head) endpoint. This is the side the caret sits on
  /// for a collapsed selection.
  pub head_affinity: SelectionAffinity,
  /// Visual gravity of the fixed (anchor) endpoint at a soft-wrap seam.
  pub anchor_gravity: VisualGravity,
  /// Visual gravity of the moving (head) endpoint at a soft-wrap seam. This is
  /// the gravity consumed when painting a collapsed caret.
  pub head_gravity: VisualGravity,
}

#[hotpath::measure_all]
impl EditorSelection {
  fn caret() -> Self {
    Self::default()
  }

  /// Collapsed caret at `offset` with neutral affinity/gravity.
  #[must_use]
  pub fn collapsed(offset: DocumentOffset) -> Self {
    Self {
      anchor: offset,
      head: offset,
      ..Self::default()
    }
  }

  /// Collapsed caret at `offset` carrying an explicit affinity/gravity. Used by
  /// boundary navigation (objects, line edges) where the side/gravity matters
  /// even though the selection is collapsed.
  #[must_use]
  pub fn collapsed_with(offset: DocumentOffset, affinity: SelectionAffinity, gravity: VisualGravity) -> Self {
    Self {
      anchor: offset,
      head: offset,
      anchor_affinity: affinity,
      head_affinity: affinity,
      anchor_gravity: gravity,
      head_gravity: gravity,
    }
  }

  /// Range selection from `anchor` to `head` with neutral affinity/gravity on
  /// both endpoints.
  #[must_use]
  pub fn range(anchor: DocumentOffset, head: DocumentOffset) -> Self {
    Self {
      anchor,
      head,
      ..Self::default()
    }
  }

  /// Apply a caret motion: move `head` to `new_head` carrying the supplied
  /// affinity/gravity. When `extend` is set the anchor (and its intent) is
  /// preserved, producing/continuing a range; otherwise the selection collapses
  /// onto the new head and the anchor mirrors the head's intent. This preserves
  /// the moving endpoint's affinity across selection extension (§16).
  #[must_use]
  pub(super) fn moved(&self, new_head: DocumentOffset, head_affinity: SelectionAffinity, head_gravity: VisualGravity, extend: bool) -> Self {
    if extend {
      Self {
        anchor: self.anchor,
        head: new_head,
        anchor_affinity: self.anchor_affinity,
        head_affinity,
        anchor_gravity: self.anchor_gravity,
        head_gravity,
      }
    } else {
      Self::collapsed_with(new_head, head_affinity, head_gravity)
    }
  }

  pub(super) fn normalized(&self) -> Range<DocumentOffset> {
    self.anchor.min(self.head)..self.anchor.max(self.head)
  }

  pub fn is_caret(&self) -> bool {
    self.anchor == self.head
  }

  /// Whether two selections occupy the same anchor/head offsets, ignoring
  /// affinity/gravity. Used by directional movement to detect a true positional
  /// no-op (preserving the historical offset-based early-return behavior).
  pub(super) fn same_positions(&self, other: &Self) -> bool {
    self.anchor == other.anchor && self.head == other.head
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CollaborationRole {
  Owner,
  Editor,
  Viewer,
}

impl CollaborationRole {
  #[must_use]
  pub const fn can_write(self) -> bool {
    matches!(self, Self::Owner | Self::Editor)
  }
}

// Loro-first (spec §10, invariant 11): the editor holds NO content history.
// Undo/redo executes through the write authority's Loro UndoManager.

#[derive(Clone, Debug)]
pub enum SaveStatus {
  Saved,
  Dirty,
  Saving,
  SaveFailed(String),
}

/// Describes whether a toolbar-visible style is consistently applied across
/// the current selection. `Mixed` lets UI controls show an indeterminate state
/// when the selection spans differently styled text or paragraphs.
#[derive(Clone, Debug, Eq, PartialEq)]
pub enum SelectionState<T> {
  None,
  Uniform(T),
  Mixed,
}

#[hotpath::measure_all]
impl<T> SelectionState<T> {
  pub fn is_mixed(&self) -> bool {
    matches!(self, Self::Mixed)
  }
}

#[derive(Clone, Debug)]
struct SelectionStateBuilder<T> {
  state: SelectionState<T>,
}

#[hotpath::measure_all]
impl<T> Default for SelectionStateBuilder<T> {
  fn default() -> Self {
    Self { state: SelectionState::None }
  }
}

#[hotpath::measure_all]
impl<T: core::marker::Copy + Eq> SelectionStateBuilder<T> {
  fn push(&mut self, value: T) {
    match self.state {
      SelectionState::None => self.state = SelectionState::Uniform(value),
      SelectionState::Uniform(current) if current != value => self.state = SelectionState::Mixed,
      SelectionState::Uniform(_) | SelectionState::Mixed => {},
    }
  }

  fn is_mixed(&self) -> bool {
    self.state.is_mixed()
  }

  fn finish(self) -> SelectionState<T> {
    self.state
  }
}

#[hotpath::measure]
fn offset_in_range(offset: DocumentOffset, range: Range<DocumentOffset>) -> bool {
  range.start <= offset && offset <= range.end
}

#[hotpath::measure]
fn point_distance_squared(a: Point<Pixels>, b: Point<Pixels>) -> f32 {
  let ax: f32 = a.x.into();
  let ay: f32 = a.y.into();
  let bx: f32 = b.x.into();
  let by: f32 = b.y.into();
  let dx = ax - bx;
  let dy = ay - by;
  dx * dx + dy * dy
}

#[hotpath::measure]
pub(super) fn adjust_drop_after_source_delete(drop: DocumentOffset, source: Range<DocumentOffset>) -> DocumentOffset {
  if drop <= source.start {
    return drop;
  }
  if source.start.paragraph == source.end.paragraph {
    if drop.paragraph == source.start.paragraph {
      return DocumentOffset {
        paragraph: drop.paragraph,
        byte: drop
          .byte
          .saturating_sub(source.end.byte - source.start.byte),
      };
    }
    return drop;
  }
  if drop.paragraph <= source.end.paragraph {
    return source.start;
  }
  DocumentOffset {
    paragraph: drop.paragraph - (source.end.paragraph - source.start.paragraph),
    byte: drop.byte,
  }
}

/// Formatting state for the current caret or selection.
///
/// This is intentionally a read-only snapshot. Toolbars can render buttons,
/// menus, or segmented controls from this, then call the existing mutation
/// methods on `RichTextEditor` when the user chooses a style.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RichTextEditorStyleState {
  pub paragraph_style: SelectionState<ParagraphStyle>,
  pub semantic: SelectionState<RunSemanticStyle>,
  pub underline: SelectionState<bool>,
  pub strikethrough: SelectionState<bool>,
  pub highlight: SelectionState<Option<HighlightStyle>>,
}

/// Runtime behavior preferences for the editor.
///
/// This is intentionally separate from document data. Future settings UI can
/// edit this object without changing saved document content.
#[derive(Clone, Debug, PartialEq)]
pub struct RichTextEditorConfig {
  pub smart_word_selection: bool,
  pub allow_paragraph_breaks: bool,
  pub flow_cell_surface: bool,
  pub show_section_collapse_controls: bool,
  pub caret_color: Option<gpui::Hsla>,
  pub show_own_collaboration_caret_color: bool,
}

#[hotpath::measure_all]
impl Default for RichTextEditorConfig {
  fn default() -> Self {
    Self {
      smart_word_selection: true,
      allow_paragraph_breaks: true,
      flow_cell_surface: false,
      show_section_collapse_controls: true,
      caret_color: None,
      show_own_collaboration_caret_color: true,
    }
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalCaret {
  pub offset: DocumentOffset,
  pub visual_gravity: VisualGravity,
  pub color_rgb: u32,
}

/// A remote peer's (non-collapsed) selection range, painted behind the text in
/// that peer's presence color. The head caret is carried separately as an
/// [`ExternalCaret`]; this is only the highlighted span.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalSelection {
  pub selection: EditorSelection,
  pub color_rgb: u32,
}

/// A transient "you are here" highlight painted over a range after
/// programmatic navigation (outline peek, comment jump, tub open). Decays on a
/// timer; `generation` guards a stale timer from erasing a newer flash.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct JumpFlash {
  pub selection: EditorSelection,
  pub color_rgb: u32,
  pub(super) generation: u64,
}

/// Fallback flash color for callers with no theme access (the annotation
/// amber). Callers with a theme pass their own via `peek_paragraph`/
/// `flash_range`; migrates to the derived visual engine in D-S2.
pub const DEFAULT_JUMP_FLASH_RGB: u32 = 0x00D9_9A20;

/// How long a navigation flash stays painted before the timer clears it.
pub const JUMP_FLASH_DURATION: Duration = Duration::from_millis(900);

// Loro-first: hooks carry NO pending edit batches (nothing is ever pending —
// intents commit synchronously) and return no replacement projection (the
// canonical doc is always current; there is nothing to replay).
/// H-S1: whether a save was user-initiated or autosave grain — the host's
/// save hook stamps its revision record accordingly.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum NativeSaveKind {
  Explicit,
  Auto,
}

pub type NativeSaveHook = Rc<dyn Fn(PathBuf, Vec<AssetRecord>, NativeSaveKind) -> Pin<Box<dyn Future<Output = io::Result<()>>>>>;

/// M2: what the pointer was over when the context menu was requested — the
/// host builds a menu whose top changes with the target.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum EditorContextTarget {
  Text {
    offset: DocumentOffset,
    has_selection: bool,
    /// Inside a durable annotation span (a comment mark) — the host offers
    /// "Open Thread".
    over_annotation: bool,
  },
  Image {
    block_ix: usize,
  },
  Table {
    block_ix: usize,
  },
  Equation {
    block_ix: usize,
  },
}

/// M2: the host's context-menu opener. The editor resolves WHAT was clicked
/// (and applies standard right-click selection semantics); the host owns the
/// menu itself, because its verbs reach workspace surfaces.
pub type ContextMenuHook = Rc<dyn Fn(Point<Pixels>, EditorContextTarget, &mut Window, &mut App)>;

/// R1-B: the host's recognized-style HTML paste provider. Called at paste
/// time (after the internal rich-fragment path, before the plain-text
/// flatten); returns paragraphs only when the platform `text/html` slot
/// carries recognized style names — `None` keeps the paste plain.
pub type HtmlPasteInterpreter = Rc<dyn Fn() -> Option<Vec<crate::InputParagraph>>>;
pub type NativeExportHook = Rc<dyn Fn(PathBuf, DocumentExportFormat, Vec<AssetRecord>) -> Pin<Box<dyn Future<Output = io::Result<()>>>>>;
pub type NativeRecoveryHook = Rc<dyn Fn(PathBuf) -> Pin<Box<dyn Future<Output = io::Result<()>>>>>;

#[derive(Clone)]
pub(super) struct ParagraphChunkLayoutCacheEntry {
  key: ParagraphCacheKey,
  // §act-nine A9.3: the chunk cache is POSITIONAL (indexed by paragraph_ix),
  // and the (style, version) `key` alone can collide across DIFFERENT
  // paragraphs after a row shift. The stable id pins the slot to the paragraph
  // the chunks were built for — replacing the old global `edit_generation`
  // check, which nuked every paragraph's chunks on any edit.
  paragraph_id: ParagraphId,
  width: Pixels,
  invisibility_mode: bool,
  layout_generation: u64,
  prep: Arc<ParagraphPrep>,
  chunks: Vec<ParagraphChunkLayout>,
  complete: bool,
  exact_height: Pixels,
}

#[derive(Clone)]
struct ParagraphChunkLayout {
  start_byte: usize,
  end_byte: usize,
  height: Pixels,
  layout: Rc<LayoutState>,
}

#[derive(Clone, Default)]
struct ParagraphPrepSlot {
  normal: Option<Arc<ParagraphPrep>>,
  invisible: Option<Arc<ParagraphPrep>>,
}

#[hotpath::measure_all]
impl ParagraphPrepSlot {
  fn get(&self, invisibility_mode: bool) -> Option<&Arc<ParagraphPrep>> {
    if invisibility_mode {
      self.invisible.as_ref()
    } else {
      self.normal.as_ref()
    }
  }

  fn set(&mut self, prep: Arc<ParagraphPrep>) {
    if prep.key.invisibility_mode {
      self.invisible = Some(prep);
    } else {
      self.normal = Some(prep);
    }
  }
}

struct ParagraphShapingCacheEntry {
  key: ParagraphLayoutWorkKey,
  fragment_shapes: FragmentShapeCache,
}

#[derive(Clone)]
struct ParagraphCacheRetainRanges {
  visible: Range<usize>,
  active: Range<usize>,
}

impl Default for ParagraphCacheRetainRanges {
  fn default() -> Self {
    Self { visible: 0..0, active: 0..0 }
  }
}

impl ParagraphCacheRetainRanges {
  fn contains(&self, paragraph_ix: usize) -> bool {
    self.visible.contains(&paragraph_ix) || self.active.contains(&paragraph_ix)
  }

  fn covers(&self, required: &Self) -> bool {
    self.contains_range(&required.visible) && self.contains_range(&required.active)
  }

  fn contains_range(&self, range: &Range<usize>) -> bool {
    range.is_empty() || range_within(&self.visible, range) || range_within(&self.active, range)
  }
}

fn range_within(outer: &Range<usize>, inner: &Range<usize>) -> bool {
  outer.start <= inner.start && outer.end >= inner.end
}

#[derive(Clone, Copy, PartialEq)]
struct ParagraphEstimateHeightCacheEntry {
  key: ParagraphCacheKey,
  // §perf-heaven T5: the STABLE paragraph identity for this slot. The estimate
  // cache is indexed by paragraph_ix, and `key` hashes only (style, version) —
  // so after an insert/delete a slot can be reused by a different paragraph
  // that coincidentally shares (style, version). Storing the paragraph id makes
  // slot reuse a cache MISS instead of a stale-height hit, which lets validity
  // drop the global `edit_generation` (that over-invalidated EVERY paragraph's
  // estimate on any edit — the O(document) cold estimate cost). `layout_generation`
  // still catches theme/zoom changes the per-paragraph key cannot see.
  paragraph_id: ParagraphId,
  width: Pixels,
  invisibility_mode: bool,
  layout_generation: u64,
  height: Pixels,
  source_len: usize,
}

#[derive(Clone)]
struct LayoutPrepRequest {
  width: Pixels,
  invisibility_mode: bool,
  paragraphs: Vec<usize>,
}

#[derive(Clone, Copy, Default)]
struct LayoutPrepMetrics {
  requested: usize,
  completed: usize,
  installed: usize,
  stale: usize,
  batches: usize,
  text_bytes: usize,
}

#[derive(Clone, Copy, Default)]
struct LayoutRuntimeMetrics {
  ui_chunk_builds: usize,
  ui_chunk_build_time: Duration,
  prefetch_budget_overruns: usize,
  scroll_budget_overruns: usize,
}

#[derive(Clone, Debug)]
pub struct ItemSizeBenchmarkResult {
  pub elapsed: Duration,
  pub cache_hit: bool,
  pub item_count: usize,
  pub exact_height_count: usize,
  pub total_height: f32,
  pub prep_requested: usize,
  pub prep_completed: usize,
  pub prep_installed: usize,
  pub prep_stale: usize,
  pub prep_batches: usize,
  pub prep_text_bytes: usize,
  pub ui_chunk_builds: usize,
  pub ui_chunk_build_time: Duration,
  pub prefetch_budget_overruns: usize,
  pub scroll_budget_overruns: usize,
}

/// §perf-heaven T5 net: the layout-fidelity oracle for the per-paragraph height
/// ESTIMATE — the heuristic scroll uses for not-yet-laid-out paragraphs, and the
/// value a persisted read model would serialize/reload. Layout heights are NOT
/// covered by the CRDT convergence fuzz or the corpus sweep, so this IS the net:
/// for every paragraph that has a COMPLETE exact layout, it compares the estimate
/// to the exact height. A test asserts the estimate never wildly UNDER-shoots
/// (the dangerous case — scroll jumps up as the real height lands) and stays in a
/// bounded band, so any change to the estimate (or a persisted estimate that
/// diverges from a fresh one) trips loudly.
#[derive(Clone, Debug, Default)]
pub struct EstimateAccuracy {
  /// Paragraphs with a complete exact layout that were compared.
  pub compared: usize,
  /// Worst estimate/exact - 1 over the compared set (estimate too TALL).
  pub max_over_ratio: f32,
  /// Worst 1 - estimate/exact over the compared set (estimate too SHORT).
  pub max_under_ratio: f32,
}

#[derive(Clone)]
enum PasteCache {
  Rich { metadata: String, fragment: RichClipboardFragment },
  Plain { text: String },
}

#[derive(Clone)]
struct PendingTextDrag {
  start_position: Point<Pixels>,
  source_selection: EditorSelection,
}

#[derive(Clone)]
struct ActiveTextDrag {
  source_range: Range<DocumentOffset>,
  fragment: RichClipboardFragment,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum ImageResizeHandle {
  Left,
  Right,
  TopLeft,
  TopRight,
  BottomLeft,
  BottomRight,
}

#[hotpath::measure_all]
impl ImageResizeHandle {
  fn horizontal_sign(self) -> f32 {
    match self {
      Self::Left | Self::TopLeft | Self::BottomLeft => -1.0,
      Self::Right | Self::TopRight | Self::BottomRight => 1.0,
    }
  }
}

#[derive(Clone)]
struct ImageResizeDrag {
  block_ix: usize,
  start_position: Point<Pixels>,
  start_width: Pixels,
  handle: ImageResizeHandle,
  before: ImageBlock,
}

#[derive(Clone)]
struct TableColumnResizeDrag {
  block_ix: usize,
  column_ix: usize,
  start_position: Point<Pixels>,
  start_widths: Vec<u32>,
  before: TableBlock,
}

/// B-S7: an in-flight row/column reorder drag, grabbed on the table's edge
/// bands (left band = rows, top band = columns). `target` is the insertion
/// SLOT (0..=len) the drop indicator paints; the drop emits one
/// identity-addressed `MoveRow`/`MoveColumn`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TableMoveDrag {
  pub block_ix: usize,
  pub axis: TableMoveAxis,
  pub target: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum TableMoveAxis {
  Row { row_ix: usize, row_id: RowId },
  Column { column_ix: usize, column_id: ColumnId },
}

/// B-S7: a rectangular CELL range on one table — anchor..head inclusive,
/// the spreadsheet idiom. Never canonical state; range verbs iterate it into
/// per-cell intents.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct CellRangeSelection {
  pub block_ix: usize,
  pub anchor: (usize, usize),
  pub head: (usize, usize),
}

impl CellRangeSelection {
  pub fn rows(&self) -> std::ops::RangeInclusive<usize> {
    self.anchor.0.min(self.head.0)..=self.anchor.0.max(self.head.0)
  }

  pub fn cells(&self) -> std::ops::RangeInclusive<usize> {
    self.anchor.1.min(self.head.1)..=self.anchor.1.max(self.head.1)
  }

  pub fn contains(&self, block_ix: usize, row_ix: usize, cell_ix: usize) -> bool {
    block_ix == self.block_ix && self.rows().contains(&row_ix) && self.cells().contains(&cell_ix)
  }

  /// More than one cell — a single-cell "range" is just the selection.
  pub fn is_multi(&self) -> bool {
    self.anchor != self.head
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BlockSelection {
  Image(usize),
  Equation(usize),
  Table(usize),
  /// A selected table cell (§P2b). `row_ix`/`cell_ix` stay for positional access
  /// by the layout/paint/caret readers, while `row_id`/`column_id` carry the
  /// durable identity resolved from the id-bearing model at selection time so
  /// structural emission and replay address the cell by id, not by a stale index.
  TableCell {
    block_ix: usize,
    row_ix: usize,
    cell_ix: usize,
    row_id: RowId,
    column_id: ColumnId,
  },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TableCellCaret {
  pub(super) block_ix: usize,
  pub(super) row_ix: usize,
  pub(super) cell_ix: usize,
  pub(super) row_id: RowId,
  pub(super) column_id: ColumnId,
  pub(super) paragraph_block_ix: usize,
  pub(super) anchor: usize,
  pub(super) byte: usize,
  pub(super) caret_visible: bool,
}

#[derive(Default)]
struct HeightPrefixIndex {
  heights: Vec<Pixels>,
  origins: Vec<Pixels>,
  total: Pixels,
}

#[derive(Clone)]
enum ScrollAnchorSnapshot {
  Item { item: VirtualItem, delta: Pixels },
  ParagraphRemainder { paragraph_ix: usize, start_byte: usize, delta: Pixels },
}

#[derive(Clone)]
struct VisibleChunkAnchor {
  paragraph_ix: usize,
  chunk_ix: usize,
  bounds: Bounds<Pixels>,
  scroll_y: Pixels,
}

#[derive(Clone)]
struct ScrollAnchorLock {
  anchor: ScrollAnchorSnapshot,
  offset_y: Pixels,
}

struct RenderLayoutSnapshot {
  width: Pixels,
  item_sizes: Rc<Vec<Size<Pixels>>>,
  items: RenderVirtualItems,
  hide_initial_layout: bool,
}

#[derive(Clone)]
enum RenderVirtualItems {
  DocumentProjection(Rc<Vec<VirtualItem>>),
  WithDropPreview(Rc<Vec<RenderVirtualItem>>),
}

#[derive(Clone)]
enum RenderVirtualItem {
  DocumentProjection(VirtualItem),
  DropPreview,
}

impl RenderVirtualItems {
  fn get(&self, item_ix: usize) -> Option<RenderVirtualItem> {
    match self {
      Self::DocumentProjection(items) => items
        .get(item_ix)
        .cloned()
        .map(RenderVirtualItem::DocumentProjection),
      Self::WithDropPreview(items) => items.get(item_ix).cloned(),
    }
  }
}

enum RecoveryWriteDecision {
  Write { generation: u64, document: Box<DocumentProjection> },
  WriteRuntime { generation: u64, hook: NativeRecoveryHook },
  Rescheduled,
  Idle,
}

#[hotpath::measure_all]
impl ScrollAnchorSnapshot {
  fn delta(&self) -> Pixels {
    match self {
      Self::Item { delta, .. } | Self::ParagraphRemainder { delta, .. } => *delta,
    }
  }

  fn paragraph_ix(&self) -> Option<usize> {
    match self {
      Self::Item {
        item: VirtualItem::ParagraphChunk { paragraph_ix, .. } | VirtualItem::ParagraphRemainder { paragraph_ix, .. },
        ..
      }
      | Self::ParagraphRemainder { paragraph_ix, .. } => Some(*paragraph_ix),
      Self::Item {
        item: VirtualItem::HiddenBlock { .. } | VirtualItem::StructuralBlock { .. },
        ..
      } => None,
    }
  }
}

#[hotpath::measure_all]
impl HeightPrefixIndex {
  fn rebuild(&mut self, sizes: &[Size<Pixels>]) {
    self.heights.clear();
    self.heights.reserve(sizes.len());
    self.origins.clear();
    self.origins.reserve(sizes.len());
    let mut cumulative = px(0.0);
    for size in sizes {
      self.origins.push(cumulative);
      cumulative += size.height;
      self.heights.push(size.height);
    }
    self.total = cumulative;
  }

  fn replace_range(&mut self, range: Range<usize>, sizes: &[Size<Pixels>]) -> bool {
    if range.start > range.end || range.end > self.heights.len() || self.origins.len() != self.heights.len() {
      return false;
    }

    let removed = range.end - range.start;
    self
      .heights
      .splice(range.clone(), sizes.iter().map(|size| size.height));
    self
      .origins
      .splice(range.clone(), std::iter::repeat_n(px(0.0), sizes.len()));
    if self.origins.len() != self.heights.len() || self.heights.len() + removed < sizes.len() {
      return false;
    }

    let mut cumulative = if range.start == 0 {
      px(0.0)
    } else {
      self.origins[range.start - 1] + self.heights[range.start - 1]
    };
    for ix in range.start..self.heights.len() {
      self.origins[ix] = cumulative;
      cumulative += self.heights[ix];
    }
    self.total = cumulative;
    true
  }

  fn len(&self) -> usize {
    self.heights.len()
  }

  fn total_height(&self) -> f32 {
    self.total.into()
  }

  fn item_top(&self, ix: usize) -> Pixels {
    self.origins.get(ix).copied().unwrap_or(self.total)
  }

  fn lower_bound(&self, target: Pixels) -> usize {
    if self.heights.is_empty() {
      return 0;
    }
    let target = target.max(px(0.0)).min(self.total);
    let mut low = 0usize;
    let mut high = self.heights.len().min(self.origins.len());
    while low < high {
      let mid = low + (high - low) / 2;
      if self.origins[mid] + self.heights[mid] > target {
        high = mid;
      } else {
        low = mid + 1;
      }
    }
    low.min(self.heights.len().saturating_sub(1))
  }
}

use crate::local_intents::{
  DeleteRangeIntent, FragmentBlock, InsertObjectIntent, InsertRichFragmentIntent, InsertTextIntent, JoinParagraphsIntent, LocalCommit,
  LocalIntent, LocalWriteAuthority, LocalWriteOutcome, ProjectionStreamItem, ReplaceMatch, ReplaceMatchesIntent, SetMarksIntent,
  SetParagraphStyleIntent, SetParagraphStylesIntent, SplitParagraphIntent, TextAnchor, UndoOutcome as LocalUndoOutcome, WriteRejected,
};

/// §caret-anchor: the local caret encoded as CRDT cursors, captured while the
/// editor and canonical core are in sync. `selection` is the caret it was
/// captured for — the fast path is used only while the live caret still equals it.
struct CaretAnchor {
  selection: EditorSelection,
  head_cursor: Vec<u8>,
  anchor_cursor: Vec<u8>,
}

pub struct RichTextEditor {
  pub(super) focus_handle: FocusHandle,
  focus_subscriptions: Vec<Subscription>,
  scroll_handle: VirtualListScrollHandle,
  disposed: bool,
  document_path: Option<PathBuf>,
  document_display_name: Option<SharedString>,
  recovery_path: Option<PathBuf>,
  pub(super) document: DocumentProjection,
  /// Loro-first: the injected write authority — the ONE local write path
  /// (spec invariant 5). `None` = read-only display surface.
  write_authority: Option<std::sync::Arc<dyn LocalWriteAuthority>>,
  pub(super) selection: EditorSelection,
  /// §caret-anchor FAST path: the selection's CRDT cursors, captured at synced
  /// moments. When a remote patch arrives while `selection` still equals
  /// `anchor.selection`, the caret is repositioned by resolving these cursors
  /// (O(log n)) instead of the O(doc) `fork_at` rebase. `None`/stale ⇒ fallback.
  caret_anchor: Option<CaretAnchor>,
  selection_movement_epoch: u64,
  config: RichTextEditorConfig,
  edit_generation: u64,
  saved_generation: u64,
  next_edit_generation: u64,
  last_send_document_generation: Option<u64>,
  last_format_export_generation: Option<u64>,
  zoom_percent: f32,
  zoom_anchor: Option<ZoomAnchorSnapshot>,
  zoom_anchor_apply_pending: bool,
  save_status: SaveStatus,
  identity_map: DocumentIdentityMap,
  reconciliation_recoveries: u64,
  native_save_hook: Option<NativeSaveHook>,
  context_menu_hook: Option<ContextMenuHook>,
  html_paste_interpreter: Option<HtmlPasteInterpreter>,
  native_export_hook: Option<NativeExportHook>,
  native_recovery_hook: Option<NativeRecoveryHook>,
  collaboration_role: Option<CollaborationRole>,
  own_collaboration_caret_color_rgb: Option<u32>,
  recovery_write_in_progress: bool,
  recovery_write_pending: bool,
  last_recovery_generation: u64,
  paste_cache: Option<PasteCache>,
  pub(super) pending_styles: Option<RunStyles>,
  pub(super) armed_inline_tool: Option<ArmedInlineTool>,
  pub(super) current_highlight_style: HighlightStyle,
  pub(super) current_highlight_choice: Option<HighlightStyle>,
  selecting: bool,
  drag_granularity: SelectionGranularity,
  drag_anchor: Option<DocumentOffset>,
  smart_selection_left_anchor_word: bool,
  smart_selection_exact_override: bool,
  last_drag_position: Option<Point<Pixels>>,
  pending_text_drag: Option<PendingTextDrag>,
  active_text_drag: Option<ActiveTextDrag>,
  drop_preview: Option<DropPreview>,
  image_resize_drag: Option<ImageResizeDrag>,
  table_column_resize_drag: Option<TableColumnResizeDrag>,
  pub(super) selected_block: Option<BlockSelection>,
  /// B-S7: the rectangular cell range (anchor = where extension started).
  pub(super) cell_range: Option<CellRangeSelection>,
  /// B-S7: the in-flight row/column reorder drag.
  pub(super) table_move_drag: Option<TableMoveDrag>,
  table_cell_block_ix: usize,
  table_cell_anchor: usize,
  table_cell_caret: usize,
  autoscroll_active: bool,
  pub(super) caret_visible: bool,
  caret_blink_active: bool,
  last_text_input_at: Option<Instant>,
  ime_marked_range: Option<Range<usize>>,
  external_carets: Vec<ExternalCaret>,
  external_selections: Vec<ExternalSelection>,
  /// Durable document annotations (currently unresolved comment anchors).
  /// Kept separate from peer presence so either layer can refresh without
  /// erasing the other.
  annotation_selections: Vec<ExternalSelection>,
  /// Index into `annotation_selections` under the pointer, painted with the
  /// stronger hover underline (C-S4). Reset whenever the annotation set moves.
  hovered_annotation: Option<usize>,
  /// Transient navigation flash (see [`JumpFlash`]); separate from
  /// annotations so comment refreshes never erase an in-flight flash.
  pub(super) jump_flash: Option<JumpFlash>,
  pub(super) jump_flash_generation: u64,
  pub(super) search_highlights: Vec<Range<DocumentOffset>>,
  pub(super) active_search_highlight: Option<usize>,
  pending_typing_prefetch_resume: bool,
  resume_chunk_prefetch_after_typing: bool,
  paragraph_chunk_layout_cache: Vec<Option<ParagraphChunkLayoutCacheEntry>>,
  // §perf-heaven T8.12: keyed by STABLE `ParagraphId`, not by `paragraph_ix`.
  // A positional `Vec` had to `resize_with` on every structural edit and left
  // trailing slots misaligned with the shifted paragraphs; the id-keyed map lets
  // an unchanged paragraph keep its slot regardless of position. Validity is
  // gated by the content key (style, version) + id + position inside the slot
  // (§act-nine A9.3 — no global `edit_generation`), so this is
  // correctness-neutral. Bounded in `resize_layout_aux_caches` against leaking
  // entries for deleted paragraphs (same policy as the estimate cache below).
  paragraph_prep_cache: FxHashMap<ParagraphId, ParagraphPrepSlot>,
  paragraph_shaping_cache: FxHashMap<ParagraphId, ParagraphShapingCacheEntry>,
  /// CT-S1: per-paragraph invisibility byte remaps (doc ⇄ projected/display),
  /// cached by paragraph version. `None` in a slot = the paragraph lays out
  /// verbatim under the mode (style-visible). `RefCell` because paint/hit-test
  /// read paths hold `&self`.
  invisibility_remap_cache: std::cell::RefCell<InvisibilityRemapCache>,
  // §perf-heaven T7.14: keyed by STABLE `ParagraphId`, not by `paragraph_ix`.
  // A positional `Vec` shifted on any mid-document insert/delete, turning every
  // trailing paragraph's estimate into a stale-slot MISS → an O(document) tail
  // recompute on the total-height pass. An id-keyed map lets a shifted paragraph
  // keep its cached estimate, so a structural edit recomputes only the touched
  // paragraphs. Bounded against unbounded growth from deletions in
  // `resize_layout_aux_caches`.
  paragraph_estimate_height_cache: FxHashMap<ParagraphId, ParagraphEstimateHeightCacheEntry>,
  pending_layout_prep_task: Option<Task<()>>,
  pending_layout_prep_request: Option<LayoutPrepRequest>,
  layout_generation: u64,
  layout_prep_metrics: LayoutPrepMetrics,
  layout_runtime_metrics: LayoutRuntimeMetrics,
  pending_chunk_prefetch: bool,
  chunk_prefetch_queue: VecDeque<usize>,
  paragraph_height_cache: Vec<Option<ParagraphHeightCacheEntry>>,
  paragraph_height_cache_revision: u64,
  // Convergence backstop for the scroll-materialization render loop. Records the
  // (rounded scroll-y, edit_generation) of the last render-path materialization
  // pass and how many consecutive passes ran at that unchanged signature. A large
  // document whose item-size cache cannot be incrementally patched re-runs a full
  // O(doc) rebuild + `cx.notify()` every render; if materialization never
  // "sticks" it loops forever and freezes the window. Any real scroll or edit
  // changes the signature and resets the count, so this caps ONLY the
  // pathological non-converging loop.
  scroll_materialize_signature: Option<(i32, u64)>,
  scroll_materialize_stall_frames: u32,
  item_sizes_cache: Option<ItemSizesCache>,
  pending_item_sizes_patch_range: Option<Range<usize>>,
  suppress_mutation_notify: usize,
  last_scroll_anchor: Option<ScrollAnchorSnapshot>,
  scroll_anchor_lock: Option<ScrollAnchorLock>,
  height_prefix_index: HeightPrefixIndex,
  measured_item_width: Option<Pixels>,
  pending_viewport_size_refresh: bool,
  initial_layout_hidden: bool,
  pending_snap_to_paragraph: Option<(usize, u8)>,
  pending_scroll_head_after_layout: bool,
  visible_layout_generation: u64,
  visible_layout_range: Range<usize>,
  visible_chunk_anchors: Vec<VisibleChunkAnchor>,
  layout_cache_retain_ranges: ParagraphCacheRetainRanges,
  prep_cache_retain_ranges: ParagraphCacheRetainRanges,
  invisibility_mode: bool,
  collapsed_section_ids: FxHashSet<SectionId>,
  hovered_collapse_paragraph: Option<usize>,
  // Remembered horizontal pixel position for vertical caret motion. When the
  // user presses Up/Down repeatedly we want the caret to track a consistent
  // x even on lines whose contents are shorter than the previous one. The
  // field is set when entering vertical motion and cleared by any other
  // action that changes x (typing, horizontal motion, Home/End, mouse).
  goal_x: Option<Pixels>,
}

impl gpui::EventEmitter<EditorEvent> for RichTextEditor {}

include!("lifecycle.rs");
include!("local_write_path.rs");
include!("projection_apply.rs");
include!("object_selection.rs");
include!("style_state.rs");
include!("search_highlights.rs");
include!("send_export.rs");
include!("zoom.rs");
include!("commands.rs");
include!("paste.rs");
include!("tables.rs");
include!("media.rs");
include!("table_equation_editing.rs");
include!("formatting.rs");
include!("action_handlers.rs");
include!("edit_pipeline.rs");
include!("scroll_anchor.rs");
include!("item_sizes.rs");
include!("layout_prep.rs");
include!("chunk_layout.rs");
include!("chunk_materialization.rs");
include!("chunk_navigation.rs");
include!("chunk_prefetch.rs");
include!("layout_access.rs");
include!("recovery.rs");
include!("movement_core.rs");
include!("block_insertion.rs");
include!("style_mutation.rs");
include!("caret_movement.rs");
include!("hit_testing.rs");
include!("mouse.rs");
include!("drop_preview.rs");
include!("traits.rs");
include!("platform.rs");
include!("virtual_helpers.rs");
include!("table_helpers.rs");
include!("render_blocks.rs");
include!("equation_renderer.rs");
include!("object_assets.rs");
include!("clipboard_helpers.rs");
include!("serialization.rs");
