use std::{
  collections::{VecDeque, hash_map::DefaultHasher},
  fs,
  hash::{Hash, Hasher},
  io,
  ops::Range,
  path::{Path, PathBuf},
  rc::Rc,
  sync::{Arc, Mutex, OnceLock},
  time::{Duration, Instant},
};

use crop::Rope;
use gpui::{
  App, Bounds, ClipboardEntry, ClipboardItem, Context, CursorStyle, DragMoveEvent, Entity, ExternalPaths, FocusHandle, Focusable, Image,
  ImageFormat, InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions,
  Pixels, Point, Render, SharedString, Size, Subscription, Task, Timer, Window, actions, div, img, point, prelude::*, px, relative, rgb, size,
};
use gpui_component::ActiveTheme as _;
use gpui_component::scroll::{Scrollbar, ScrollbarHandle, ScrollbarShow};
use gpui_component::{VirtualListScrollHandle, v_virtual_list};
use rustc_hash::FxHashMap;

use super::*;

const DISABLE_SCROLL_LIMITING_FUNCTIONS: bool = true; // cfg!(target_os = "linux");---scroll limit is now obsolete on all OSs
const SCROLL_FOREGROUND_OVERSCAN_PX: f32 = 384.0;
const SCROLL_FOREGROUND_MATERIALIZE_BUDGET_MS: u64 = 8;
const SCROLL_FOREGROUND_MAX_CHUNK_LINES: usize = 96;
const TYPING_PREFETCH_SUPPRESSION_WINDOW: Duration = Duration::from_millis(150);
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorSelection {
  pub anchor: DocumentOffset,
  pub head: DocumentOffset,
}

#[hotpath::measure_all]
impl EditorSelection {
  fn caret() -> Self {
    let zero = DocumentOffset::default();
    Self { anchor: zero, head: zero }
  }

  pub(super) fn normalized(&self) -> Range<DocumentOffset> {
    self.anchor.min(self.head)..self.anchor.max(self.head)
  }

  pub(super) fn is_caret(&self) -> bool {
    self.anchor == self.head
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

struct EditRecord {
  before_selection: EditorSelection,
  before_generation: u64,
  after_selection: EditorSelection,
  after_generation: u64,
  operations: Vec<EditOperation>,
  canonical_operations: Vec<CanonicalOperation>,
}

#[derive(Clone, Debug)]
pub(super) enum EditOperation {
  InsertText {
    paragraph: usize,
    byte: usize,
    text: String,
    styles: RunStyles,
  },
  ReplaceParagraphSpan {
    before: DocumentSpan,
    after: DocumentSpan,
  },
  InsertRichFragment {
    offset: DocumentOffset,
    inserted_end: DocumentOffset,
    fragment: RichClipboardFragment,
  },
  DeleteBlock {
    block_ix: usize,
    block: Block,
  },
  #[allow(dead_code, reason = "IME state accessor is retained for platform input diagnostics.")]
  InsertBlocks {
    block_ix: usize,
    blocks: Vec<Block>,
  },
  ReplaceBlock {
    block_ix: usize,
    before: Block,
    after: Block,
  },
  ReplaceDocument {
    before: Box<Document>,
    after: Box<Document>,
  },
  MoveRichText {
    source_range: Range<DocumentOffset>,
    adjusted_drop: DocumentOffset,
    inserted_range: Range<DocumentOffset>,
    fragment: RichClipboardFragment,
  },
}

#[hotpath::measure_all]
impl EditOperation {
  pub(super) fn undo(&self, document: &mut Document) {
    match self {
      Self::InsertText { paragraph, byte, text, .. } => {
        delete_range_in_paragraph(document, *paragraph, *byte..*byte + text.len());
      },
      Self::ReplaceParagraphSpan { before, after } => {
        let _ = apply_document_span_replacement(document, after, before);
      },
      Self::InsertRichFragment { offset, inserted_end, .. } => {
        let _ = delete_cross_paragraph_range(document, *offset..*inserted_end);
      },
      Self::DeleteBlock { block_ix, block } => {
        let insert_ix = (*block_ix).min(document.blocks.len());
        Arc::make_mut(&mut document.blocks).insert(insert_ix, block.clone());
      },
      Self::InsertBlocks { block_ix, blocks } => {
        let end = (*block_ix + blocks.len()).min(document.blocks.len());
        Arc::make_mut(&mut document.blocks).drain(*block_ix..end);
      },
      Self::ReplaceBlock { block_ix, before, .. } => {
        if let Some(block) = Arc::make_mut(&mut document.blocks).get_mut(*block_ix) {
          *block = before.clone();
        }
      },
      Self::ReplaceDocument { before, .. } => {
        *document = before.as_ref().clone();
      },
      Self::MoveRichText {
        source_range,
        inserted_range,
        fragment,
        ..
      } => {
        delete_cross_paragraph_range(document, inserted_range.clone());
        insert_rich_fragment_at(document, source_range.start, fragment);
      },
    }
  }

  pub(super) fn redo(&self, document: &mut Document) {
    match self {
      Self::InsertText {
        paragraph,
        byte,
        text,
        styles,
      } => {
        insert_text_at(document, *paragraph, *byte, text, *styles);
      },
      Self::ReplaceParagraphSpan { before, after } => {
        let _ = apply_document_span_replacement(document, before, after);
      },
      Self::InsertRichFragment { offset, fragment, .. } => {
        insert_rich_fragment_at(document, *offset, fragment);
      },
      Self::DeleteBlock { block_ix, .. } => {
        if !matches!(document.blocks.get(*block_ix), Some(Block::Paragraph(_))) {
          Arc::make_mut(&mut document.blocks).remove(*block_ix);
        }
      },
      Self::InsertBlocks { block_ix, blocks } => {
        let insert_ix = (*block_ix).min(document.blocks.len());
        Arc::make_mut(&mut document.blocks).splice(insert_ix..insert_ix, blocks.clone());
      },
      Self::ReplaceBlock { block_ix, after, .. } => {
        if let Some(block) = Arc::make_mut(&mut document.blocks).get_mut(*block_ix) {
          *block = after.clone();
        }
      },
      Self::ReplaceDocument { after, .. } => {
        *document = after.as_ref().clone();
      },
      Self::MoveRichText {
        source_range,
        adjusted_drop,
        fragment,
        ..
      } => {
        delete_cross_paragraph_range(document, source_range.clone());
        insert_rich_fragment_at(document, *adjusted_drop, fragment);
      },
    }
  }
}

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
fn is_plain_text_insert(text: &str) -> bool {
  !text.is_empty() && !text.contains('\n') && !text.contains(SOFT_LINE_BREAK)
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
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RichTextEditorConfig {
  pub smart_word_selection: bool,
}

#[hotpath::measure_all]
impl Default for RichTextEditorConfig {
  fn default() -> Self {
    Self { smart_word_selection: true }
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalCaret {
  pub offset: DocumentOffset,
  pub color_rgb: u32,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ExternalSelection {
  pub selection: EditorSelection,
  pub color_rgb: u32,
}
#[derive(Clone)]
struct ParagraphChunkLayoutCacheEntry {
  key: ParagraphCacheKey,
  width: Pixels,
  invisibility_mode: bool,
  edit_generation: u64,
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

  fn clear(&mut self) {
    self.normal = None;
    self.invisible = None;
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
  width: Pixels,
  invisibility_mode: bool,
  edit_generation: u64,
  layout_generation: u64,
  height: Pixels,
  source_len: usize,
}

#[derive(Clone)]
struct LayoutPrepRequest {
  width: Pixels,
  edit_generation: u64,
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
pub(crate) struct ItemSizeBenchmarkResult {
  pub(crate) elapsed: Duration,
  pub(crate) cache_hit: bool,
  pub(crate) item_count: usize,
  pub(crate) exact_height_count: usize,
  pub(crate) total_height: f32,
  pub(crate) prep_requested: usize,
  pub(crate) prep_completed: usize,
  pub(crate) prep_installed: usize,
  pub(crate) prep_stale: usize,
  pub(crate) prep_batches: usize,
  pub(crate) prep_text_bytes: usize,
  pub(crate) ui_chunk_builds: usize,
  pub(crate) ui_chunk_build_time: Duration,
  pub(crate) prefetch_budget_overruns: usize,
  pub(crate) scroll_budget_overruns: usize,
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

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum BlockSelection {
  Image(usize),
  Equation(usize),
  Table(usize),
  TableCell { block_ix: usize, row_ix: usize, cell_ix: usize },
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) struct TableCellCaret {
  pub(super) block_ix: usize,
  pub(super) row_ix: usize,
  pub(super) cell_ix: usize,
  pub(super) paragraph_block_ix: usize,
  pub(super) anchor: usize,
  pub(super) byte: usize,
  pub(super) caret_visible: bool,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct EquationSourceSelection {
  anchor: usize,
  caret: usize,
  caret_visible: bool,
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
  item_sizes: Rc<Vec<Size<Pixels>>>,
  items: RenderVirtualItems,
  hide_initial_layout: bool,
}

#[derive(Clone)]
enum RenderVirtualItems {
  Document(Rc<Vec<VirtualItem>>),
  WithDropPreview(Rc<Vec<RenderVirtualItem>>),
}

#[derive(Clone)]
enum RenderVirtualItem {
  Document(VirtualItem),
  DropPreview,
}

impl RenderVirtualItems {
  fn get(&self, item_ix: usize) -> Option<RenderVirtualItem> {
    match self {
      Self::Document(items) => items.get(item_ix).cloned().map(RenderVirtualItem::Document),
      Self::WithDropPreview(items) => items.get(item_ix).cloned(),
    }
  }
}

enum RecoveryWriteDecision {
  Write { generation: u64, document: Box<Document> },
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

pub struct RichTextEditor {
  pub(super) focus_handle: FocusHandle,
  focus_subscriptions: Vec<Subscription>,
  scroll_handle: VirtualListScrollHandle,
  disposed: bool,
  document_path: Option<PathBuf>,
  document_display_name: Option<SharedString>,
  recovery_path: Option<PathBuf>,
  pub(super) document: Document,
  pub(super) selection: EditorSelection,
  config: RichTextEditorConfig,
  edit_generation: u64,
  saved_generation: u64,
  next_edit_generation: u64,
  last_send_document_generation: Option<u64>,
  last_format_export_generation: Option<u64>,
  zoom_percent: f32,
  save_status: SaveStatus,
  undo_stack: Vec<EditRecord>,
  redo_stack: Vec<EditRecord>,
  identity_map: DocumentIdentityMap,
  last_collaboration_edit: Option<CollaborationEdit>,
  collaboration_role: Option<CollaborationRole>,
  pub(super) local_caret_color_rgb: Option<u32>,
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
  table_cell_block_ix: usize,
  table_cell_anchor: usize,
  table_cell_caret: usize,
  equation_source_anchor: usize,
  equation_source_caret: usize,
  autoscroll_active: bool,
  pub(super) caret_visible: bool,
  caret_blink_active: bool,
  last_text_input_at: Option<Instant>,
  external_carets: Vec<ExternalCaret>,
  external_selections: Vec<ExternalSelection>,
  pending_typing_prefetch_resume: bool,
  resume_chunk_prefetch_after_typing: bool,
  paragraph_chunk_layout_cache: Vec<Option<ParagraphChunkLayoutCacheEntry>>,
  paragraph_prep_cache: Vec<ParagraphPrepSlot>,
  paragraph_shaping_cache: Vec<Option<ParagraphShapingCacheEntry>>,
  paragraph_estimate_height_cache: Vec<Option<ParagraphEstimateHeightCacheEntry>>,
  pending_layout_prep_task: Option<Task<()>>,
  pending_layout_prep_request: Option<LayoutPrepRequest>,
  layout_generation: u64,
  layout_prep_metrics: LayoutPrepMetrics,
  layout_runtime_metrics: LayoutRuntimeMetrics,
  pending_chunk_prefetch: bool,
  chunk_prefetch_queue: VecDeque<usize>,
  paragraph_height_cache: Vec<Option<ParagraphHeightCacheEntry>>,
  paragraph_height_cache_revision: u64,
  item_sizes_cache: Option<ItemSizesCache>,
  pending_item_sizes_patch_range: Option<Range<usize>>,
  layout_invalidation_hint: Option<Range<usize>>,
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
  // Remembered horizontal pixel position for vertical caret motion. When the
  // user presses Up/Down repeatedly we want the caret to track a consistent
  // x even on lines whose contents are shorter than the previous one. The
  // field is set when entering vertical motion and cleared by any other
  // action that changes x (typing, horizontal motion, Home/End, mouse).
  goal_x: Option<Pixels>,
}

include!("lifecycle.rs");
include!("object_selection.rs");
include!("style_state.rs");
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
include!("block_helpers.rs");
include!("render_blocks.rs");
include!("equation_renderer.rs");
include!("object_assets.rs");
include!("clipboard_helpers.rs");
include!("serialization.rs");
