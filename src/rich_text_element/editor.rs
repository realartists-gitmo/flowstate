use std::{
  collections::{HashMap, hash_map::DefaultHasher},
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
  App, Bounds, ClipboardEntry, ClipboardItem, Context, CursorStyle, Entity, ExternalPaths, FocusHandle, Focusable, Image, ImageFormat,
  InteractiveElement, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, PathPromptOptions, Pixels, Point,
  Render, ScrollStrategy, SharedString, Size, Subscription, Timer, Window, actions, div, img, point, prelude::*, px, rgb, size,
};
use gpui_component::scroll::{Scrollbar, ScrollbarHandle, ScrollbarShow};
use gpui_component::{VirtualListScrollHandle, v_virtual_list};

use super::*;

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
    SetParagraphPocket,
    SetParagraphHat,
    SetParagraphBlock,
    SetParagraphTag,
    SetParagraphAnalytic,
    ToggleCite,
    ToggleUnderline,
    ToggleEmphasis,
    SetHighlightSpoken,
    ClearFormatting,
    ClearHighlight,
    InsertImage,
    InsertTable,
    InsertEquation,
    Backspace,
    Delete,
    InsertNewline,
    InsertSoftLineBreak,
  ]
);

// If you add a user-triggerable editor action here, also add it to
// `crate::commands::CommandId`, `COMMAND_SPECS`, and
// `register_default_keybindings` when it has a default shortcut. This keeps
// keyboard rebinding, command-palette/menu labels, and "show shortcut" UI from
// drifting away from the editor's action surface.

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

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct EditorSelection {
  pub anchor: DocumentOffset,
  pub head: DocumentOffset,
}

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

#[derive(Clone, Debug)]
struct EditRecord {
  before_selection: EditorSelection,
  before_generation: u64,
  after_selection: EditorSelection,
  after_generation: u64,
  operations: Vec<EditOperation>,
}

#[derive(Clone, Debug)]
pub(super) enum EditOperation {
  ReplaceParagraphSpan {
    before: DocumentSpan,
    after: DocumentSpan,
  },
  DeleteBlock {
    block_ix: usize,
    block: Block,
  },
  #[allow(dead_code)]
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
    before: Document,
    after: Document,
  },
  MoveRichText {
    source_range: Range<DocumentOffset>,
    adjusted_drop: DocumentOffset,
    inserted_range: Range<DocumentOffset>,
    fragment: RichClipboardFragment,
  },
}

impl EditOperation {
  pub(super) fn undo(&self, document: &mut Document) {
    match self {
      Self::ReplaceParagraphSpan { before, after } => apply_document_span_replacement(document, after, before),
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
        *document = before.clone();
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
      Self::ReplaceParagraphSpan { before, after } => apply_document_span_replacement(document, before, after),
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
        *document = after.clone();
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

#[derive(Clone, Debug, Eq, PartialEq)]
pub(super) struct DocumentSpan {
  pub(super) start_paragraph: usize,
  pub(super) paragraphs: Vec<Paragraph>,
  pub(super) text: String,
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

impl<T> SelectionState<T> {
  pub fn is_mixed(&self) -> bool {
    matches!(self, Self::Mixed)
  }
}

fn selection_state_from_values<T: Eq>(values: impl IntoIterator<Item = T>) -> SelectionState<T> {
  let mut values = values.into_iter();
  let Some(first) = values.next() else {
    return SelectionState::None;
  };
  if values.any(|value| value != first) {
    SelectionState::Mixed
  } else {
    SelectionState::Uniform(first)
  }
}

fn offset_in_range(offset: DocumentOffset, range: Range<DocumentOffset>) -> bool {
  range.start <= offset && offset <= range.end
}

fn point_distance_squared(a: Point<Pixels>, b: Point<Pixels>) -> f32 {
  let ax: f32 = a.x.into();
  let ay: f32 = a.y.into();
  let bx: f32 = b.x.into();
  let by: f32 = b.y.into();
  let dx = ax - bx;
  let dy = ay - by;
  dx * dx + dy * dy
}

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
  pub highlight: SelectionState<Option<HighlightStyle>>,
}

/// Runtime behavior preferences for the editor.
///
/// This is intentionally separate from document data. Future settings UI can
/// edit this object without changing saved DB8 content.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RichTextEditorConfig {
  pub smart_word_selection: bool,
}

impl Default for RichTextEditorConfig {
  fn default() -> Self {
    Self { smart_word_selection: true }
  }
}

struct ItemSizesCache {
  width: Pixels,
  item_count: usize,
  height_revision: u64,
  sizes: Rc<Vec<Size<Pixels>>>,
}

#[derive(Clone)]
struct ParagraphLayoutCacheEntry {
  key: ParagraphCacheKey,
  width: Pixels,
  layout: Rc<LayoutState>,
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
  heights: Vec<f32>,
  tree: Vec<f32>,
}

impl HeightPrefixIndex {
  fn rebuild(&mut self, sizes: &[Size<Pixels>]) {
    self.heights.clear();
    self.heights.reserve(sizes.len());
    self.tree.clear();
    self.tree.resize(sizes.len() + 1, 0.0);
    for (ix, size) in sizes.iter().enumerate() {
      let height: f32 = size.height.into();
      self.heights.push(height);
      self.add(ix, height);
    }
  }

  fn len(&self) -> usize {
    self.heights.len()
  }

  fn add(&mut self, ix: usize, delta: f32) {
    let mut tree_ix = ix + 1;
    while tree_ix < self.tree.len() {
      self.tree[tree_ix] += delta;
      tree_ix += tree_ix & (!tree_ix + 1);
    }
  }

  fn total_height(&self) -> f32 {
    self.prefix_sum(self.heights.len())
  }

  fn prefix_sum(&self, count: usize) -> f32 {
    let mut tree_ix = count.min(self.heights.len());
    let mut sum = 0.0;
    while tree_ix > 0 {
      sum += self.tree[tree_ix];
      tree_ix &= tree_ix - 1;
    }
    sum
  }

  fn item_top(&self, ix: usize) -> Pixels {
    px(self.prefix_sum(ix))
  }

  fn lower_bound(&self, target: Pixels) -> usize {
    if self.heights.is_empty() {
      return 0;
    }
    let target: f32 = target.into();
    let target = target.clamp(0.0, self.total_height());
    let mut idx = 0usize;
    let mut bit = 1usize;
    while bit < self.tree.len() {
      bit <<= 1;
    }
    let mut sum = 0.0;
    while bit > 0 {
      let next = idx + bit;
      if next < self.tree.len() && sum + self.tree[next] <= target {
        idx = next;
        sum += self.tree[next];
      }
      bit >>= 1;
    }
    idx.min(self.heights.len().saturating_sub(1))
  }
}

pub struct RichTextEditor {
  pub(super) focus_handle: FocusHandle,
  focus_subscriptions: Vec<Subscription>,
  scroll_handle: VirtualListScrollHandle,
  document_path: Option<PathBuf>,
  recovery_path: Option<PathBuf>,
  pub(super) document: Document,
  pub(super) selection: EditorSelection,
  config: RichTextEditorConfig,
  edit_generation: u64,
  saved_generation: u64,
  next_edit_generation: u64,
  save_status: SaveStatus,
  undo_stack: Vec<EditRecord>,
  redo_stack: Vec<EditRecord>,
  recovery_write_in_progress: bool,
  recovery_write_pending: bool,
  last_recovery_generation: u64,
  paste_cache: Option<PasteCache>,
  pub(super) pending_styles: Option<RunStyles>,
  pub(super) armed_inline_tool: Option<ArmedInlineTool>,
  selecting: bool,
  drag_granularity: SelectionGranularity,
  drag_anchor: Option<DocumentOffset>,
  smart_selection_left_anchor_word: bool,
  smart_selection_exact_override: bool,
  last_drag_position: Option<Point<Pixels>>,
  pending_text_drag: Option<PendingTextDrag>,
  active_text_drag: Option<ActiveTextDrag>,
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
  last_layout: Option<Rc<LayoutState>>,
  paragraph_layout_cache: Vec<Option<ParagraphLayoutCacheEntry>>,
  paragraph_height_cache: Vec<Option<ParagraphHeightCacheEntry>>,
  paragraph_height_cache_revision: u64,
  item_sizes_cache: Option<ItemSizesCache>,
  height_prefix_index: HeightPrefixIndex,
  measured_item_width: Option<Pixels>,
  pending_viewport_size_refresh: bool,
  initial_layout_hidden: bool,
  pending_snap_to_paragraph: Option<(usize, u8)>,
  visible_layout_generation: u64,
  visible_layout_range: Range<usize>,
  visible_layout_parts: Vec<Option<LaidOutParagraph>>,
  // Remembered horizontal pixel position for vertical caret motion. When the
  // user presses Up/Down repeatedly we want the caret to track a consistent
  // x even on lines whose contents are shorter than the previous one. The
  // field is set when entering vertical motion and cleared by any other
  // action that changes x (typing, horizontal motion, Home/End, mouse).
  goal_x: Option<Pixels>,
}

impl RichTextEditor {
  pub fn new_with_path(document: Document, document_path: Option<PathBuf>, cx: &mut Context<Self>) -> Self {
    let paragraph_count = document.paragraphs.len();
    Self {
      focus_handle: cx.focus_handle(),
      focus_subscriptions: Vec::new(),
      scroll_handle: VirtualListScrollHandle::new(),
      recovery_path: document_path.as_ref().map(recovery_path_for_document),
      document_path,
      document,
      selection: EditorSelection::caret(),
      config: RichTextEditorConfig::default(),
      edit_generation: 0,
      saved_generation: 0,
      next_edit_generation: 1,
      save_status: SaveStatus::Saved,
      undo_stack: Vec::new(),
      redo_stack: Vec::new(),
      recovery_write_in_progress: false,
      recovery_write_pending: false,
      last_recovery_generation: 0,
      paste_cache: None,
      pending_styles: None,
      armed_inline_tool: None,
      selecting: false,
      drag_granularity: SelectionGranularity::Character,
      drag_anchor: None,
      smart_selection_left_anchor_word: false,
      smart_selection_exact_override: false,
      last_drag_position: None,
      pending_text_drag: None,
      active_text_drag: None,
      image_resize_drag: None,
      table_column_resize_drag: None,
      selected_block: None,
      table_cell_block_ix: 0,
      table_cell_anchor: 0,
      table_cell_caret: 0,
      equation_source_anchor: 0,
      equation_source_caret: 0,
      autoscroll_active: false,
      caret_visible: true,
      caret_blink_active: false,
      last_layout: None,
      paragraph_layout_cache: vec![None; paragraph_count],
      paragraph_height_cache: vec![None; paragraph_count],
      paragraph_height_cache_revision: 0,
      item_sizes_cache: None,
      height_prefix_index: HeightPrefixIndex::default(),
      measured_item_width: None,
      pending_viewport_size_refresh: false,
      initial_layout_hidden: true,
      pending_snap_to_paragraph: None,
      visible_layout_generation: 0,
      visible_layout_range: 0..0,
      visible_layout_parts: Vec::new(),
      goal_x: None,
    }
  }

  pub fn document(&self) -> &Document {
    &self.document
  }

  pub fn config(&self) -> &RichTextEditorConfig {
    &self.config
  }

  pub fn update_config(&mut self, update: impl FnOnce(&mut RichTextEditorConfig), cx: &mut Context<Self>) {
    update(&mut self.config);
    cx.notify();
  }

  pub fn save_status(&self) -> &SaveStatus {
    &self.save_status
  }

  pub fn selection(&self) -> &EditorSelection {
    &self.selection
  }

  fn select_block(&mut self, selection: BlockSelection, cx: &mut Context<Self>) {
    let block_ix = match selection {
      BlockSelection::Image(block_ix)
      | BlockSelection::Equation(block_ix)
      | BlockSelection::Table(block_ix)
      | BlockSelection::TableCell { block_ix, .. } => block_ix,
    };
    self.selected_block = Some(selection);
    self.table_cell_block_ix = 0;
    self.table_cell_caret = self
      .selected_table_cell_text()
      .map(|text| text.len())
      .unwrap_or(0);
    self.table_cell_anchor = self.table_cell_caret;
    let equation_source_len = self
      .selected_equation_source()
      .map(|source| source.len())
      .unwrap_or(0);
    self.equation_source_caret = equation_source_len;
    self.equation_source_anchor = equation_source_len;
    self.selecting = false;
    self.pending_text_drag = None;
    self.active_text_drag = None;
    self.scroll_block_into_view(block_ix);
    cx.notify();
  }

  fn select_block_from_click(
    &mut self,
    block_ix: usize,
    fallback: BlockSelection,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    window.focus(&self.focus_handle);
    if let Some((selection, paragraph_block_ix, byte)) = self.table_cell_selection_at(block_ix, position, window, cx) {
      self.selected_block = Some(selection);
      self.table_cell_block_ix = paragraph_block_ix;
      self.table_cell_anchor = byte;
      self.table_cell_caret = byte;
      self.selecting = false;
      self.drag_anchor = None;
      self.pending_text_drag = None;
      self.active_text_drag = None;
      self.goal_x = None;
      self.reset_caret_blink(cx);
      cx.notify();
    } else {
      self.select_block(fallback, cx);
    }
  }

  fn table_cell_selection_at(
    &mut self,
    block_ix: usize,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<(BlockSelection, usize, usize)> {
    let Block::Table(_) = self.document.blocks.get(block_ix)? else {
      return None;
    };
    let width = self.current_layout_width();
    let block_top = self.block_top_for_index(block_ix)?;
    let layout = layout_structural_block_at(&self.document, block_ix, width, block_top, window, cx)?;
    let LaidOutBlock::Table(table) = layout else {
      return None;
    };
    let viewport = self.scroll_handle.bounds();
    let document_point = point(position.x - viewport.left(), position.y - viewport.top() - self.scroll_handle.offset().y);
    for (row_ix, row) in table.rows.iter().enumerate() {
      for (cell_ix, cell) in row.cells.iter().enumerate() {
        if cell.bounds.contains(&document_point) {
          let selection = BlockSelection::TableCell { block_ix, row_ix, cell_ix };
          let mut fallback = (selection, 0, 0);
          for block in &cell.blocks {
            if let LaidOutBlock::Paragraph(paragraph) = block {
              fallback = (selection, paragraph.index, paragraph.len);
              if document_point.y <= paragraph.bottom {
                let offset = paragraph.hit_test(document_point);
                return Some((selection, paragraph.index, offset.byte));
              }
            }
          }
          return Some(fallback);
        }
      }
    }
    None
  }

  fn start_table_column_resize_if_hit(&mut self, block_ix: usize, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> bool {
    let Some((column_ix, widths, before)) = self.table_column_resize_hit_at(block_ix, position, window, cx) else {
      return false;
    };
    window.focus(&self.focus_handle);
    self.selected_block = Some(BlockSelection::Table(block_ix));
    self.table_column_resize_drag = Some(TableColumnResizeDrag {
      block_ix,
      column_ix,
      start_position: position,
      start_widths: widths,
      before,
    });
    self.selecting = false;
    self.pending_text_drag = None;
    self.active_text_drag = None;
    self.goal_x = None;
    cx.notify();
    true
  }

  fn table_column_resize_hit_at(
    &mut self,
    block_ix: usize,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Option<(usize, Vec<u32>, TableBlock)> {
    let Block::Table(table) = self.document.blocks.get(block_ix)?.clone() else {
      return None;
    };
    let width = self.current_layout_width();
    let block_top = self.block_top_for_index(block_ix)?;
    let layout = layout_structural_block_at(&self.document, block_ix, width, block_top, window, cx)?;
    let LaidOutBlock::Table(laid_out) = layout else {
      return None;
    };
    let viewport = self.scroll_handle.bounds();
    let document_point = point(position.x - viewport.left(), position.y - viewport.top() - self.scroll_handle.offset().y);
    if !laid_out.bounds.contains(&document_point) {
      return None;
    }

    let tolerance = 5.0;
    let first_row = laid_out.rows.first()?;
    let data_row = table.rows.first()?;
    let mut logical_column_ix = 0usize;
    for (cell_ix, cell_layout) in first_row.cells.iter().enumerate() {
      let span = data_row
        .cells
        .get(cell_ix)
        .map(|cell| cell.col_span.max(1) as usize)
        .unwrap_or(1);
      let border_column_ix = logical_column_ix.saturating_add(span).saturating_sub(1);
      let delta: f32 = (document_point.x - cell_layout.bounds.right()).into();
      if delta.abs() <= tolerance && border_column_ix < table_column_count(&table) {
        return Some((border_column_ix, fixed_table_column_widths_from_layout(&table, &laid_out), table));
      }
      logical_column_ix = logical_column_ix.saturating_add(span);
    }
    None
  }

  fn update_table_column_resize_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
    let Some(drag) = self.table_column_resize_drag.clone() else {
      return false;
    };
    let Some(Block::Table(table)) = Arc::make_mut(&mut self.document.blocks).get_mut(drag.block_ix) else {
      self.table_column_resize_drag = None;
      return true;
    };
    let delta: f32 = (position.x - drag.start_position.x).into();
    let column_count = drag
      .start_widths
      .len()
      .max(table_column_count(table))
      .max(1);
    while table.column_widths.len() < column_count {
      table.column_widths.push(TableColumnWidth::FixedPx(120));
    }
    for (ix, width) in drag.start_widths.iter().copied().enumerate() {
      if ix < table.column_widths.len() {
        table.column_widths[ix] = TableColumnWidth::FixedPx(width);
      }
    }
    let Some(start_width) = drag.start_widths.get(drag.column_ix).copied() else {
      self.table_column_resize_drag = None;
      return true;
    };
    table.column_widths[drag.column_ix] = TableColumnWidth::FixedPx((start_width as f32 + delta).clamp(32.0, 1600.0).round() as u32);
    table.version = drag.before.version.wrapping_add(1);
    self.invalidate_document_layout_caches();
    cx.notify();
    true
  }

  fn finish_table_column_resize_drag(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(drag) = self.table_column_resize_drag.take() else {
      return false;
    };
    let Some(Block::Table(after)) = self.document.blocks.get(drag.block_ix).cloned() else {
      cx.notify();
      return true;
    };
    if after == drag.before {
      cx.notify();
      return true;
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock {
        block_ix: drag.block_ix,
        before: Block::Table(drag.before),
        after: Block::Table(after),
      }],
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed(after_generation, cx);
    true
  }

  fn block_top_for_index(&self, block_ix: usize) -> Option<Pixels> {
    if self.height_prefix_index.len() == self.document.blocks.len() {
      return Some(self.height_prefix_index.item_top(block_ix));
    }
    let sizes = self.item_sizes_cache.as_ref()?;
    Some(
      sizes
        .sizes
        .iter()
        .take(block_ix)
      .fold(px(0.0), |top, size| top + size.height),
    )
  }

  fn scroll_block_into_view(&self, block_ix: usize) {
    let Some(sizes) = &self.item_sizes_cache else {
      return;
    };
    let Some(row_size) = sizes.sizes.get(block_ix).copied() else {
      return;
    };
    let Some(top) = self.block_top_for_index(block_ix) else {
      return;
    };
    let viewport = self.scroll_handle.bounds();
    let rect = Bounds::new(
      point(viewport.left(), viewport.top() + self.scroll_handle.offset().y + top),
      size(viewport.size.width, row_size.height),
    );
    scroll_rect_into_view(&self.scroll_handle, rect, px(8.0));
  }

  fn clear_block_selection(&mut self) {
    self.selected_block = None;
    self.table_cell_block_ix = 0;
    self.table_cell_anchor = 0;
    self.table_cell_caret = 0;
    self.equation_source_anchor = 0;
    self.equation_source_caret = 0;
  }

  fn selected_block_fragment(&self) -> Option<RichClipboardFragment> {
    let selection = self.selected_block?;
    if matches!(selection, BlockSelection::TableCell { .. }) {
      return None;
    }
    let block_ix = match selection {
      BlockSelection::Image(block_ix)
      | BlockSelection::Equation(block_ix)
      | BlockSelection::Table(block_ix)
      | BlockSelection::TableCell { block_ix, .. } => block_ix,
    };
    let block = self.document.blocks.get(block_ix)?;
    let mut assets = Vec::new();
    collect_block_assets(block, &self.document.assets, &mut assets);
    Some(RichClipboardFragment {
      format: "debateprocessor.rich-text-fragment.v1".to_string(),
      paragraphs: Vec::new(),
      blocks: vec![input_block_from_block(block)],
      assets,
    })
  }

  fn selected_ordered_fragment(&self, range: Range<DocumentOffset>) -> Option<RichClipboardFragment> {
    let start_block = self.block_ix_for_paragraph(range.start.paragraph)?;
    let end_block = self.block_ix_for_paragraph(range.end.paragraph)?;
    let has_object = self.document.blocks[start_block.min(end_block)..=start_block.max(end_block)]
      .iter()
      .any(|block| !matches!(block, Block::Paragraph(_)));
    if !has_object {
      return None;
    }
    let mut blocks = Vec::new();
    let mut assets = Vec::new();
    for block_ix in start_block..=end_block {
      match self.document.blocks.get(block_ix)? {
        Block::Paragraph(_) => {
          let Some(paragraph_ix) = self.paragraph_ix_for_block(block_ix) else {
            continue;
          };
          if paragraph_ix < range.start.paragraph || paragraph_ix > range.end.paragraph {
            continue;
          }
          let start = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
          let end = if paragraph_ix == range.end.paragraph {
            range.end.byte
          } else {
            paragraph_text_len(&self.document.paragraphs[paragraph_ix])
          };
          if start < end || (paragraph_ix > range.start.paragraph && paragraph_ix < range.end.paragraph) {
            blocks.push(InputBlock::Paragraph(input_paragraph_from_document_range(
              &self.document,
              paragraph_ix,
              start..end,
            )));
          }
        },
        block @ (Block::Image(_) | Block::Equation(_) | Block::Table(_)) => {
          if block_ix > start_block && block_ix < end_block {
            collect_block_assets(block, &self.document.assets, &mut assets);
            blocks.push(input_block_from_block(block));
          }
        },
      }
    }
    (!blocks.is_empty()).then_some(RichClipboardFragment {
      format: "debateprocessor.rich-text-fragment.v1".to_string(),
      paragraphs: Vec::new(),
      blocks,
      assets,
    })
  }

  fn selection_crosses_object_blocks(&self, range: Range<DocumentOffset>) -> bool {
    let Some(start_block) = self.block_ix_for_paragraph(range.start.paragraph) else {
      return false;
    };
    let Some(end_block) = self.block_ix_for_paragraph(range.end.paragraph) else {
      return false;
    };
    self.document.blocks[start_block.min(end_block)..=start_block.max(end_block)]
      .iter()
      .any(|block| !matches!(block, Block::Paragraph(_)))
  }

  pub(super) fn block_is_inside_text_selection(&self, block_ix: usize) -> bool {
    if self.selected_block.is_some() || self.selection.is_caret() {
      return false;
    }
    let range = self.selection.normalized();
    let Some(start_block) = self.block_ix_for_paragraph(range.start.paragraph) else {
      return false;
    };
    let Some(end_block) = self.block_ix_for_paragraph(range.end.paragraph) else {
      return false;
    };
    block_ix > start_block.min(end_block) && block_ix < start_block.max(end_block)
  }

  fn object_block_indices_in_text_range(&self, range: Range<DocumentOffset>) -> Vec<usize> {
    let Some(start_block) = self.block_ix_for_paragraph(range.start.paragraph) else {
      return Vec::new();
    };
    let Some(end_block) = self.block_ix_for_paragraph(range.end.paragraph) else {
      return Vec::new();
    };
    ((start_block + 1)..end_block)
      .filter(|block_ix| {
        self
          .document
          .blocks
          .get(*block_ix)
          .is_some_and(|block| !matches!(block, Block::Paragraph(_)))
      })
      .collect()
  }

  fn delete_selection_with_document_snapshot(&mut self, cx: &mut Context<Self>) -> bool {
    if self.selection.is_caret() {
      return false;
    }
    let range = self.selection.normalized();
    let object_indices = self.object_block_indices_in_text_range(range.clone());
    if object_indices.is_empty() {
      return false;
    }
    let before_document = self.document.clone();
    let before_selection = self.selection.clone();
    {
      let blocks = Arc::make_mut(&mut self.document.blocks);
      for block_ix in object_indices.into_iter().rev() {
        if block_ix < blocks.len() {
          blocks.remove(block_ix);
        }
      }
    }
    self.delete_selection_internal();
    let after_document = self.document.clone();
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.undo_stack.push(EditRecord {
      before_selection,
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceDocument {
        before: before_document,
        after: after_document,
      }],
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed(after_generation, cx);
    true
  }

  fn delete_selected_block(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(selection) = self.selected_block.take() else {
      return false;
    };
    if matches!(selection, BlockSelection::TableCell { .. }) {
      self.selected_block = Some(selection);
      return false;
    }
    let block_ix = match selection {
      BlockSelection::Image(block_ix)
      | BlockSelection::Equation(block_ix)
      | BlockSelection::Table(block_ix)
      | BlockSelection::TableCell { block_ix, .. } => block_ix,
    };
    if block_ix >= self.document.blocks.len() {
      return false;
    }
    let blocks = Arc::make_mut(&mut self.document.blocks);
    if matches!(blocks.get(block_ix), Some(Block::Paragraph(_))) {
      return false;
    }
    let block = blocks.remove(block_ix);
    let before_selection = self.selection.clone();
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.undo_stack.push(EditRecord {
      before_selection: before_selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::DeleteBlock { block_ix, block }],
    });
    self.redo_stack.clear();
    self.item_sizes_cache = None;
    self.last_layout = None;
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
    self.mark_document_changed(after_generation, cx);
    true
  }

  pub fn caret_paragraph(&self) -> usize {
    self.selection.head.paragraph
  }

  pub(super) fn drag_source_selection(&self) -> Option<EditorSelection> {
    self.active_text_drag.as_ref().map(|drag| EditorSelection {
      anchor: drag.source_range.start,
      head: drag.source_range.end,
    })
  }

  pub(super) fn caret_paint_width(&self) -> Pixels {
    if self.active_text_drag.is_some() { px(2.0) } else { px(1.0) }
  }

  pub(super) fn table_cell_caret_for_paint(&self, window: &Window) -> Option<TableCellCaret> {
    if !self.focus_handle.is_focused(window) {
      return None;
    }
    let BlockSelection::TableCell { block_ix, row_ix, cell_ix } = self.selected_block? else {
      return None;
    };
    Some(TableCellCaret {
      block_ix,
      row_ix,
      cell_ix,
      paragraph_block_ix: self.table_cell_block_ix,
      anchor: self.table_cell_anchor,
      byte: self.table_cell_caret,
      caret_visible: self.caret_visible,
    })
  }

  pub fn find_text(&self, query: &str) -> Vec<Range<DocumentOffset>> {
    find_text_ranges(&self.document, query)
  }

  pub fn style_state(&self) -> RichTextEditorStyleState {
    if let Some(paragraph) = self.selected_table_cell_paragraph() {
      let run_styles = if paragraph.paragraph.runs.is_empty() {
        vec![RunStyles::default()]
      } else {
        paragraph
          .paragraph
          .runs
          .iter()
          .map(|run| run.styles)
          .collect()
      };
      return RichTextEditorStyleState {
        paragraph_style: SelectionState::Uniform(paragraph.paragraph.style),
        semantic: selection_state_from_values(run_styles.iter().map(|styles| styles.semantic)),
        underline: selection_state_from_values(
          run_styles
            .iter()
            .map(|styles| styles.direct_underline || styles.semantic == RunSemanticStyle::Underline),
        ),
        highlight: selection_state_from_values(run_styles.iter().map(|styles| styles.highlight)),
      };
    }
    let range = self.selection.normalized();
    let paragraph_style = selection_state_from_values((range.start.paragraph..=range.end.paragraph).filter_map(|paragraph_ix| {
      self
        .document
        .paragraphs
        .get(paragraph_ix)
        .map(|paragraph| paragraph.style)
    }));

    let run_styles = if self.selection.is_caret() {
      vec![self.styles_at_caret()]
    } else {
      selection_run_styles(&self.document, range)
    };

    RichTextEditorStyleState {
      paragraph_style,
      semantic: selection_state_from_values(run_styles.iter().map(|styles| styles.semantic)),
      underline: selection_state_from_values(
        run_styles
          .iter()
          .map(|styles| styles.direct_underline || styles.semantic == RunSemanticStyle::Underline),
      ),
      highlight: selection_state_from_values(run_styles.iter().map(|styles| styles.highlight)),
    }
  }

  pub fn document_theme(&self) -> DocumentTheme {
    self.document.theme.clone()
  }

  pub fn has_unsaved_changes(&self) -> bool {
    self.edit_generation != self.saved_generation
  }

  pub fn edit_generation(&self) -> u64 {
    self.edit_generation
  }

  pub fn update_document_theme(&mut self, update: impl FnOnce(&mut DocumentTheme), cx: &mut Context<Self>) {
    update(&mut self.document.theme);
    self.invalidate_document_theme_layout(cx);
  }

  fn invalidate_document_theme_layout(&mut self, cx: &mut Context<Self>) {
    self.invalidate_document_layout_caches();
    cx.notify();
  }

  fn invalidate_document_layout_caches(&mut self) {
    self.last_layout = None;
    self.paragraph_layout_cache.clear();
    self.paragraph_height_cache = vec![None; self.document.paragraphs.len()];
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
    self.item_sizes_cache = None;
    self.height_prefix_index = HeightPrefixIndex::default();
  }

  pub fn save(&mut self, cx: &mut Context<Self>) -> io::Result<()> {
    let Some(path) = self.document_path.clone() else {
      return Ok(());
    };
    self.save_status = SaveStatus::Saving;
    cx.notify();
    let result = write_db8(&path, &self.document);
    match result {
      Ok(()) => {
        self.saved_generation = self.edit_generation;
        self.save_status = SaveStatus::Saved;
        if let Some(path) = &self.recovery_path {
          let _ = fs::remove_file(path);
        }
        cx.notify();
        Ok(())
      },
      Err(error) => {
        self.save_status = SaveStatus::SaveFailed(error.to_string());
        cx.notify();
        Err(error)
      },
    }
  }

  pub fn discard_recovery_file(&mut self) {
    if let Some(path) = &self.recovery_path {
      let _ = fs::remove_file(path);
    }
  }

  pub fn scroll_to_paragraph(&mut self, paragraph_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    if paragraph_ix < self.document.paragraphs.len() {
      // Outline navigation should place the insertion caret at the start of
      // the target paragraph, matching what the user just selected in the nav.
      self.selection = EditorSelection {
        anchor: DocumentOffset {
          paragraph: paragraph_ix,
          byte: 0,
        },
        head: DocumentOffset {
          paragraph: paragraph_ix,
          byte: 0,
        },
      };
      self.goal_x = None;
      self.reset_caret_blink(cx);

      let width = self.current_layout_width();
      let end = (paragraph_ix + 40).min(self.document.paragraphs.len());
      // Snapping by offset needs an exact cumulative height before the target.
      // If earlier rows still use estimates, the first snap can land slightly
      // above or below the requested paragraph and then shift after rendering.
      for ix in 0..end {
        self.ensure_exact_paragraph_height(ix, width, window, cx);
      }
      self.item_sizes_cache = None;
      let _ = self.paragraph_item_sizes(window, cx);
      self.pending_snap_to_paragraph = Some((paragraph_ix, 4));
      self.apply_pending_paragraph_snap(cx);
      cx.notify();
    }
  }

  pub fn undo(&mut self, cx: &mut Context<Self>) {
    let Some(record) = self.undo_stack.pop() else {
      return;
    };
    let restored_generation = record.before_generation;
    for operation in record.operations.iter().rev() {
      operation.undo(&mut self.document);
    }
    self.selection = record.before_selection.clone();
    self.edit_generation = restored_generation;
    self.redo_stack.push(record);
    self.after_history_restore(cx);
  }

  pub fn redo(&mut self, cx: &mut Context<Self>) {
    let Some(record) = self.redo_stack.pop() else {
      return;
    };
    let restored_generation = record.after_generation;
    for operation in &record.operations {
      operation.redo(&mut self.document);
    }
    self.selection = record.after_selection.clone();
    self.edit_generation = restored_generation;
    self.undo_stack.push(record);
    self.after_history_restore(cx);
  }

  pub fn move_left(&mut self, cx: &mut Context<Self>) {
    self.move_horizontal(HDir::Left, false, cx);
  }

  pub fn move_right(&mut self, cx: &mut Context<Self>) {
    self.move_horizontal(HDir::Right, false, cx);
  }

  pub fn move_up(&mut self, cx: &mut Context<Self>) {
    self.move_vertical(VDir::Up, false, cx);
  }

  pub fn move_down(&mut self, cx: &mut Context<Self>) {
    self.move_vertical(VDir::Down, false, cx);
  }

  pub fn move_line_start(&mut self, cx: &mut Context<Self>) {
    self.move_line_edge(true, false, cx);
  }

  pub fn move_line_end(&mut self, cx: &mut Context<Self>) {
    self.move_line_edge(false, false, cx);
  }

  pub fn select_left(&mut self, cx: &mut Context<Self>) {
    self.move_horizontal(HDir::Left, true, cx);
  }

  pub fn select_right(&mut self, cx: &mut Context<Self>) {
    self.move_horizontal(HDir::Right, true, cx);
  }

  pub fn select_up(&mut self, cx: &mut Context<Self>) {
    self.move_vertical(VDir::Up, true, cx);
  }

  pub fn select_down(&mut self, cx: &mut Context<Self>) {
    self.move_vertical(VDir::Down, true, cx);
  }

  pub fn select_line_start(&mut self, cx: &mut Context<Self>) {
    self.move_line_edge(true, true, cx);
  }

  pub fn select_line_end(&mut self, cx: &mut Context<Self>) {
    self.move_line_edge(false, true, cx);
  }

  pub fn select_all(&mut self, cx: &mut Context<Self>) {
    if self.document.paragraphs.is_empty() {
      return;
    }
    let last = self.document.paragraphs.len() - 1;
    let last_len = paragraph_text_len(&self.document.paragraphs[last]);
    let selection = EditorSelection {
      anchor: DocumentOffset { paragraph: 0, byte: 0 },
      head: DocumentOffset {
        paragraph: last,
        byte: last_len,
      },
    };
    if self.selection == selection {
      self.goal_x = None;
      return;
    }
    self.selection = selection;
    self.goal_x = None;
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    cx.notify();
  }

  pub fn move_word_left(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(self.word_left(self.selection.head), false, cx);
  }

  pub fn move_word_right(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(self.word_right(self.selection.head), false, cx);
  }

  pub fn select_word_left(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(self.word_left(self.selection.head), true, cx);
  }

  pub fn select_word_right(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(self.word_right(self.selection.head), true, cx);
  }

  pub fn page_up(&mut self, cx: &mut Context<Self>) {
    self.page_move(VDir::Up, false, cx);
  }

  pub fn page_down(&mut self, cx: &mut Context<Self>) {
    self.page_move(VDir::Down, false, cx);
  }

  pub fn select_page_up(&mut self, cx: &mut Context<Self>) {
    self.page_move(VDir::Up, true, cx);
  }

  pub fn select_page_down(&mut self, cx: &mut Context<Self>) {
    self.page_move(VDir::Down, true, cx);
  }

  pub fn move_document_start(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(DocumentOffset::default(), false, cx);
  }

  pub fn move_document_end(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(document_end(&self.document), false, cx);
  }

  pub fn select_document_start(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(DocumentOffset::default(), true, cx);
  }

  pub fn select_document_end(&mut self, cx: &mut Context<Self>) {
    self.move_to_offset(document_end(&self.document), true, cx);
  }

  pub fn insert_text_command(&mut self, text: &str, cx: &mut Context<Self>) {
    self.apply_document_edit(cx, |editor, cx| editor.insert_text(text, cx));
  }

  pub fn backspace_command(&mut self, cx: &mut Context<Self>) {
    if !self.selection.is_caret() && self.selection_crosses_object_blocks(self.selection.normalized()) {
      let _ = self.delete_selection_with_document_snapshot(cx);
      return;
    }
    self.apply_document_edit(cx, |editor, cx| editor.backspace(cx));
  }

  pub fn delete_forward_command(&mut self, cx: &mut Context<Self>) {
    if !self.selection.is_caret() && self.selection_crosses_object_blocks(self.selection.normalized()) {
      let _ = self.delete_selection_with_document_snapshot(cx);
      return;
    }
    self.apply_document_edit(cx, |editor, cx| editor.delete_forward(cx));
  }

  pub fn insert_paragraph_break_command(&mut self, cx: &mut Context<Self>) {
    self.apply_document_edit(cx, |editor, cx| editor.insert_paragraph_break(cx));
  }

  pub fn delete_word_backward_command(&mut self, cx: &mut Context<Self>) {
    self.apply_document_edit(cx, |editor, cx| {
      if editor.selection.is_caret() {
        let head = editor.selection.head;
        let anchor = editor.word_left(head);
        editor.selection = EditorSelection { anchor, head };
      }
      editor.delete_selection_internal();
      editor.after_text_mutation(cx);
    });
  }

  pub fn delete_word_forward_command(&mut self, cx: &mut Context<Self>) {
    self.apply_document_edit(cx, |editor, cx| {
      if editor.selection.is_caret() {
        let anchor = editor.selection.head;
        let head = editor.word_right(anchor);
        editor.selection = EditorSelection { anchor, head };
      }
      editor.delete_selection_internal();
      editor.after_text_mutation(cx);
    });
  }

  pub fn copy(&mut self, cx: &mut Context<Self>) {
    if let Some(text) = self.selected_equation_source_text() {
      cx.write_to_clipboard(ClipboardItem::new_string(text));
      self.paste_cache = None;
      return;
    }
    if let Some(fragment) = self.selected_table_cell_fragment() {
      let text = block_fragment_plain_text(&fragment);
      cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(text, fragment));
      self.paste_cache = None;
      return;
    }
    if let Some(fragment) = self.selected_block_fragment() {
      let text = block_fragment_plain_text(&fragment);
      cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(text, fragment));
      self.paste_cache = None;
      return;
    }
    if self.selection.is_caret() {
      return;
    }
    if let Some(fragment) = self.selected_ordered_fragment(self.selection.normalized()) {
      let text = block_fragment_plain_text(&fragment);
      cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(text, fragment));
      self.paste_cache = None;
      return;
    }
    let text = selected_plain_text(&self.document, self.selection.normalized());
    let fragment = selected_rich_fragment(&self.document, self.selection.normalized());
    cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(text, fragment));
    self.paste_cache = None;
  }

  pub fn cut(&mut self, cx: &mut Context<Self>) {
    self.copy(cx);
    if self.clear_selected_table_cell(cx) {
      return;
    }
    if self.selected_block.is_some() {
      self.apply_document_edit(cx, |editor, cx| {
        let _ = editor.delete_selected_block(cx);
      });
      return;
    }
    if self.delete_selection_with_document_snapshot(cx) {
      return;
    }
    self.apply_document_edit(cx, |editor, cx| {
      editor.delete_selection_internal();
      editor.after_text_mutation(cx);
    });
  }

  pub fn paste(&mut self, cx: &mut Context<Self>) {
    let Some(item) = cx.read_from_clipboard() else {
      return;
    };
    if let Some(image) = item.entries().iter().find_map(|entry| match entry {
      ClipboardEntry::Image(image) => Some(image.clone()),
      ClipboardEntry::String(_) => None,
    }) {
      self.insert_clipboard_image(image, cx);
      return;
    }
    if let Some(metadata) = item.metadata() {
      if let Some(PasteCache::Rich {
        metadata: cached_metadata,
        fragment,
      }) = &self.paste_cache
        && cached_metadata == metadata
      {
        let fragment = fragment.clone();
        if self.insert_rich_fragment_into_selected_table_cell(&fragment, cx) {
          return;
        }
        if fragment.blocks.is_empty() {
          self.apply_document_edit(cx, |editor, cx| editor.insert_rich_fragment(fragment, cx));
        } else {
          self.insert_rich_fragment(fragment, cx);
        }
        return;
      }
      if let Some(fragment) = serde_json::from_str::<RichClipboardFragment>(metadata)
        .ok()
        .filter(|fragment| fragment.format == "debateprocessor.rich-text-fragment.v1")
      {
        self.paste_cache = Some(PasteCache::Rich {
          metadata: metadata.to_string(),
          fragment: fragment.clone(),
        });
        if self.insert_rich_fragment_into_selected_table_cell(&fragment, cx) {
          return;
        }
        if fragment.blocks.is_empty() {
          self.apply_document_edit(cx, |editor, cx| editor.insert_rich_fragment(fragment, cx));
        } else {
          self.insert_rich_fragment(fragment, cx);
        }
        return;
      }
    }
    if let Some(text) = item.text() {
      if let Some(PasteCache::Plain { text: cached_text }) = &self.paste_cache
        && cached_text == &text
      {
        let text = cached_text.clone();
        if self.insert_plain_text_into_selected_table_cell(&text, cx) {
          return;
        }
        self.apply_document_edit(cx, |editor, cx| editor.insert_plain_text_fragment(&text, cx));
        return;
      }
      self.paste_cache = Some(PasteCache::Plain { text: text.clone() });
      if self.insert_plain_text_into_selected_table_cell(&text, cx) {
        return;
      }
      self.apply_document_edit(cx, |editor, cx| editor.insert_plain_text_fragment(&text, cx));
    }
  }

  fn selected_table_cell_fragment(&self) -> Option<RichClipboardFragment> {
    let cell = self.selected_table_cell()?;
    if let (Some(range), Some(paragraph)) = (self.table_cell_selection_range(), self.selected_table_cell_paragraph()) {
      return Some(RichClipboardFragment {
        format: "debateprocessor.rich-text-fragment.v1".to_string(),
        paragraphs: vec![input_paragraph_from_table_cell_range(paragraph, range)],
        blocks: Vec::new(),
        assets: Vec::new(),
      });
    }
    let paragraphs = cell
      .blocks
      .iter()
      .filter_map(|block| match block {
        TableCellBlock::Paragraph(paragraph) => Some(input_paragraph_from_table_cell_paragraph(paragraph)),
        TableCellBlock::Table(_) => None,
      })
      .collect::<Vec<_>>();
    (!paragraphs.is_empty()).then_some(RichClipboardFragment {
      format: "debateprocessor.rich-text-fragment.v1".to_string(),
      paragraphs,
      blocks: Vec::new(),
      assets: Vec::new(),
    })
  }

  fn insert_plain_text_into_selected_table_cell(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.is_empty() {
      return matches!(self.selected_block, Some(BlockSelection::TableCell { .. }));
    }
    let styles = self
      .selected_table_cell_paragraph()
      .map(|paragraph| table_cell_styles_at(paragraph, self.table_cell_caret))
      .unwrap_or_default();
    let paragraphs = normalized
      .split('\n')
      .map(|line| InputParagraph {
        style: ParagraphStyle::Normal,
        runs: if line.is_empty() {
          Vec::new()
        } else {
          vec![InputRun {
            text: line.to_string(),
            styles,
          }]
        },
      })
      .collect::<Vec<_>>();
    self.insert_paragraphs_into_selected_table_cell(&paragraphs, cx)
  }

  fn insert_rich_fragment_into_selected_table_cell(&mut self, fragment: &RichClipboardFragment, cx: &mut Context<Self>) -> bool {
    if fragment
      .blocks
      .iter()
      .any(|block| !matches!(block, InputBlock::Paragraph(_)))
    {
      return false;
    }
    if !fragment.blocks.is_empty() {
      let paragraphs = fragment
        .blocks
        .iter()
        .filter_map(|block| match block {
          InputBlock::Paragraph(paragraph) => Some(paragraph.clone()),
          InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_) => None,
        })
        .collect::<Vec<_>>();
      return self.insert_paragraphs_into_selected_table_cell(&paragraphs, cx);
    }
    self.insert_paragraphs_into_selected_table_cell(&fragment.paragraphs, cx)
  }

  fn insert_paragraphs_into_selected_table_cell(&mut self, paragraphs: &[InputParagraph], cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell {
      block_ix: _,
      row_ix,
      cell_ix,
    }) = self.selected_block
    else {
      return false;
    };
    if paragraphs.is_empty() {
      return true;
    }
    let current_paragraph_ix = self.table_cell_block_ix;
    let caret = self.table_cell_caret;
    let mut new_caret = None;
    self.edit_selected_table(cx, |table| {
      let Some(cell) = table
        .rows
        .get_mut(row_ix)
        .and_then(|row| row.cells.get_mut(cell_ix))
      else {
        return;
      };
      new_caret = insert_table_cell_paragraphs_at(cell, current_paragraph_ix, caret, paragraphs);
    });
    if let Some((paragraph_ix, byte)) = new_caret {
      self.table_cell_block_ix = paragraph_ix;
      self.table_cell_caret = byte;
      cx.notify();
    }
    true
  }

  fn selected_table_cell_text(&self) -> Option<String> {
    self
      .selected_table_cell_paragraph()
      .map(|paragraph| paragraph.text.clone())
  }

  fn selected_equation_source(&self) -> Option<String> {
    let BlockSelection::Equation(block_ix) = self.selected_block? else {
      return None;
    };
    let Block::Equation(equation) = self.document.blocks.get(block_ix)? else {
      return None;
    };
    Some(equation.source.to_string())
  }

  fn equation_source_selection_range(&self) -> Option<Range<usize>> {
    if self.equation_source_anchor == self.equation_source_caret {
      return None;
    }
    Some(self.equation_source_anchor.min(self.equation_source_caret)..self.equation_source_anchor.max(self.equation_source_caret))
  }

  fn selected_equation_source_text(&self) -> Option<String> {
    let source = self.selected_equation_source()?;
    let range = self.equation_source_selection_range()?;
    Some(source.get(range).unwrap_or("").to_string())
  }

  fn equation_source_selection_for_render(&self, block_ix: usize) -> Option<EquationSourceSelection> {
    if self.selected_block != Some(BlockSelection::Equation(block_ix)) {
      return None;
    }
    Some(EquationSourceSelection {
      anchor: self.equation_source_anchor,
      caret: self.equation_source_caret,
      caret_visible: self.caret_visible,
    })
  }

  pub(super) fn table_cell_selection_range(&self) -> Option<Range<usize>> {
    if self.table_cell_anchor == self.table_cell_caret {
      return None;
    }
    Some(self.table_cell_anchor.min(self.table_cell_caret)..self.table_cell_anchor.max(self.table_cell_caret))
  }

  fn selected_table_cell_paragraph(&self) -> Option<&TableCellParagraph> {
    let cell = self.selected_table_cell()?;
    let paragraph_ix = table_cell_paragraph_block_ix(cell, self.table_cell_block_ix)?;
    let TableCellBlock::Paragraph(paragraph) = cell.blocks.get(paragraph_ix)? else {
      return None;
    };
    Some(paragraph)
  }

  fn selected_table_cell(&self) -> Option<&TableCell> {
    let BlockSelection::TableCell { block_ix, row_ix, cell_ix } = self.selected_block? else {
      return None;
    };
    let Block::Table(table) = self.document.blocks.get(block_ix)? else {
      return None;
    };
    table.rows.get(row_ix)?.cells.get(cell_ix)
  }

  fn adjacent_selected_table_cell_paragraph(&self, forward: bool) -> Option<(usize, usize)> {
    let cell = self.selected_table_cell()?;
    let current_ix = table_cell_paragraph_block_ix(cell, self.table_cell_block_ix)?;
    let paragraph_ix = if forward {
      next_table_cell_paragraph_block_ix(cell, current_ix)?
    } else {
      previous_table_cell_paragraph_block_ix(cell, current_ix)?
    };
    let TableCellBlock::Paragraph(paragraph) = cell.blocks.get(paragraph_ix)? else {
      return None;
    };
    Some((paragraph_ix, paragraph.text.len()))
  }

  fn clear_selected_table_cell(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block else {
      return false;
    };
    self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
      paragraph.text.clear();
      paragraph.paragraph.byte_range = 0..0;
      paragraph.paragraph.runs.clear();
      paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
    });
    true
  }

  fn move_selected_table_cell(&mut self, forward: bool, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block else {
      return false;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix) else {
      return false;
    };
    let mut positions = Vec::new();
    for (row, table_row) in table.rows.iter().enumerate() {
      for cell in 0..table_row.cells.len() {
        positions.push((row, cell));
      }
    }
    let Some(current) = positions
      .iter()
      .position(|&(row, cell)| row == row_ix && cell == cell_ix)
    else {
      return false;
    };
    let next = if forward { current + 1 } else { current.saturating_sub(1) };
    let Some(&(row_ix, cell_ix)) = positions.get(next) else {
      return false;
    };
    self.selected_block = Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix });
    self.table_cell_block_ix = 0;
    self.table_cell_caret = self
      .selected_table_cell_text()
      .map(|text| text.len())
      .unwrap_or(0);
    cx.notify();
    true
  }

  pub fn insert_default_table(&mut self, rows: usize, columns: usize, cx: &mut Context<Self>) {
    let rows = rows.clamp(1, 20);
    let columns = columns.clamp(1, 12);
    let table = TableBlock {
      rows: (0..rows)
        .map(|_| TableRow {
          cells: (0..columns)
            .map(|_| TableCell {
              blocks: vec![TableCellBlock::Paragraph(TableCellParagraph {
                paragraph: Paragraph {
                  style: ParagraphStyle::Normal,
                  byte_range: 0..0,
                  runs: Vec::new(),
                  version: 0,
                },
                text: String::new(),
              })],
              row_span: 1,
              col_span: 1,
            })
            .collect(),
        })
        .collect(),
      column_widths: (0..columns)
        .map(|_| TableColumnWidth::Fraction(1))
        .collect(),
      style: TableStyle { header_row: false },
      version: 0,
    };
    self.insert_blocks_after_caret(vec![Block::Table(table)], cx);
  }

  pub fn insert_row_after_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_row = match self.selected_block {
      Some(BlockSelection::TableCell { row_ix, .. }) => Some(row_ix),
      _ => None,
    };
    self.edit_selected_table(cx, |table| {
      let columns = table
        .rows
        .iter()
        .map(|row| row.cells.len())
        .max()
        .unwrap_or(1)
        .max(table.column_widths.len())
        .max(1);
      let insert_ix = target_row
        .map(|row| row + 1)
        .unwrap_or(table.rows.len())
        .min(table.rows.len());
      table.rows.insert(insert_ix, default_table_row(columns));
    });
  }

  pub fn delete_last_row_from_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_row = match self.selected_block {
      Some(BlockSelection::TableCell { row_ix, .. }) => Some(row_ix),
      _ => None,
    };
    self.edit_selected_table(cx, |table| {
      if table.rows.len() > 1 {
        let row_ix = target_row
          .unwrap_or(table.rows.len() - 1)
          .min(table.rows.len() - 1);
        table.rows.remove(row_ix);
      }
    });
  }

  pub fn insert_column_after_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_column = match self.selected_block {
      Some(BlockSelection::TableCell { cell_ix, .. }) => Some(cell_ix),
      _ => None,
    };
    self.edit_selected_table(cx, |table| {
      let insert_ix = target_column
        .map(|column| column + 1)
        .unwrap_or(table.column_widths.len())
        .min(table.column_widths.len());
      table
        .column_widths
        .insert(insert_ix, TableColumnWidth::Fraction(1));
      for row in &mut table.rows {
        let cell_ix = insert_ix.min(row.cells.len());
        row.cells.insert(cell_ix, default_table_cell());
      }
    });
  }

  pub fn delete_last_column_from_selected_table(&mut self, cx: &mut Context<Self>) {
    let target_column = match self.selected_block {
      Some(BlockSelection::TableCell { cell_ix, .. }) => Some(cell_ix),
      _ => None,
    };
    self.edit_selected_table(cx, |table| {
      if table.column_widths.len() > 1 {
        let column_ix = target_column
          .unwrap_or(table.column_widths.len() - 1)
          .min(table.column_widths.len() - 1);
        table.column_widths.remove(column_ix);
        for row in &mut table.rows {
          if row.cells.len() > 1 {
            let cell_ix = column_ix.min(row.cells.len() - 1);
            row.cells.remove(cell_ix);
          }
        }
      } else {
        for row in &mut table.rows {
          if row.cells.len() > 1 {
            let cell_ix = target_column
              .unwrap_or(row.cells.len() - 1)
              .min(row.cells.len() - 1);
            row.cells.remove(cell_ix);
          }
        }
      }
    });
  }

  pub fn widen_selected_table_column(&mut self, cx: &mut Context<Self>) {
    self.adjust_selected_table_column_width(24, cx);
  }

  pub fn narrow_selected_table_column(&mut self, cx: &mut Context<Self>) {
    self.adjust_selected_table_column_width(-24, cx);
  }

  fn adjust_selected_table_column_width(&mut self, delta_px: i32, cx: &mut Context<Self>) {
    let target_column = match self.selected_block {
      Some(BlockSelection::TableCell { cell_ix, .. }) => cell_ix,
      _ => return,
    };
    self.edit_selected_table(cx, |table| {
      if target_column >= table.column_widths.len() {
        return;
      }
      let current = match table.column_widths[target_column] {
        TableColumnWidth::FixedPx(width) => width as i32,
        TableColumnWidth::Fraction(_) | TableColumnWidth::Auto => 120,
      };
      table.column_widths[target_column] = TableColumnWidth::FixedPx((current + delta_px).clamp(32, 1600) as u32);
    });
  }

  fn edit_selected_table(&mut self, cx: &mut Context<Self>, update: impl FnOnce(&mut TableBlock)) {
    let Some(block_ix) = self.selected_table_block_ix() else {
      return;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    let mut updated = table.clone();
    update(&mut updated);
    if updated == table {
      return;
    }
    updated.version = updated.version.wrapping_add(1);
    let before = Block::Table(table);
    let after = Block::Table(updated.clone());
    if let Some(block) = Arc::make_mut(&mut self.document.blocks).get_mut(block_ix) {
      *block = after.clone();
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock { block_ix, before, after }],
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed(after_generation, cx);
  }

  fn selected_table_block_ix(&self) -> Option<usize> {
    match self.selected_block {
      Some(BlockSelection::Table(block_ix) | BlockSelection::TableCell { block_ix, .. }) => Some(block_ix),
      _ => None,
    }
  }

  pub fn selected_block_kind(&self) -> Option<&'static str> {
    match self.selected_block {
      Some(BlockSelection::Image(_)) => Some("image"),
      Some(BlockSelection::Equation(_)) => Some("equation"),
      Some(BlockSelection::Table(_)) => Some("table"),
      Some(BlockSelection::TableCell { .. }) => Some("table-cell"),
      None => None,
    }
  }

  pub fn set_selected_image_alignment(&mut self, alignment: BlockAlignment, cx: &mut Context<Self>) {
    let Some(BlockSelection::Image(block_ix)) = self.selected_block else {
      return;
    };
    self.edit_selected_image(block_ix, cx, |image| {
      image.alignment = alignment;
      image.version = image.version.wrapping_add(1);
    });
  }

  pub fn set_selected_image_fit_width(&mut self, cx: &mut Context<Self>) {
    let Some(BlockSelection::Image(block_ix)) = self.selected_block else {
      return;
    };
    self.edit_selected_image(block_ix, cx, |image| {
      image.sizing = ImageSizing::FitWidth;
      image.version = image.version.wrapping_add(1);
    });
  }

  pub fn set_selected_image_intrinsic_size(&mut self, cx: &mut Context<Self>) {
    let Some(BlockSelection::Image(block_ix)) = self.selected_block else {
      return;
    };
    self.edit_selected_image(block_ix, cx, |image| {
      image.sizing = ImageSizing::Intrinsic;
      image.version = image.version.wrapping_add(1);
    });
  }

  pub fn widen_selected_image(&mut self, cx: &mut Context<Self>) {
    self.adjust_selected_image_width(48, cx);
  }

  pub fn narrow_selected_image(&mut self, cx: &mut Context<Self>) {
    self.adjust_selected_image_width(-48, cx);
  }

  fn adjust_selected_image_width(&mut self, delta_px: i32, cx: &mut Context<Self>) {
    let Some(BlockSelection::Image(block_ix)) = self.selected_block else {
      return;
    };
    let current_width = self
      .document
      .blocks
      .get(block_ix)
      .and_then(|block| match block {
        Block::Image(image) => Some(match image.sizing {
          ImageSizing::Fixed { width_px, .. } => width_px as i32,
          ImageSizing::Intrinsic => self
            .document
            .assets
            .assets
            .get(&image.asset_id)
            .and_then(image_asset_intrinsic_size)
            .map(|(width, _)| {
              let width: f32 = width.into();
              width as i32
            })
            .unwrap_or(320),
          ImageSizing::FitWidth => {
            let available_width = (self.current_layout_width() - self.document.theme.pageless_inset_x * 2.0).max(px(1.0));
            let available_width: f32 = available_width.into();
            available_width as i32
          },
        }),
        _ => None,
      })
      .unwrap_or(320);
    self.edit_selected_image(block_ix, cx, |image| {
      image.sizing = ImageSizing::Fixed {
        width_px: (current_width + delta_px).clamp(32, 2400) as u32,
        height_px: None,
      };
      image.version = image.version.wrapping_add(1);
    });
  }

  fn start_image_resize_drag(
    &mut self,
    block_ix: usize,
    handle: ImageResizeHandle,
    position: Point<Pixels>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    let Some(Block::Image(image)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    window.focus(&self.focus_handle);
    self.selected_block = Some(BlockSelection::Image(block_ix));
    self.table_cell_block_ix = 0;
    self.table_cell_caret = 0;
    self.image_resize_drag = Some(ImageResizeDrag {
      block_ix,
      start_position: position,
      start_width: self.image_rendered_width(&image),
      handle,
      before: image,
    });
    self.selecting = false;
    self.pending_text_drag = None;
    self.active_text_drag = None;
    self.goal_x = None;
    window.prevent_default();
    cx.stop_propagation();
    cx.notify();
  }

  fn update_image_resize_drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) -> bool {
    let Some(drag) = self.image_resize_drag.clone() else {
      return false;
    };
    let delta: f32 = (position.x - drag.start_position.x).into();
    let delta = delta * drag.handle.horizontal_sign();
    let start_width: f32 = drag.start_width.into();
    let max_width: f32 = (self.current_layout_width() - self.document.theme.pageless_inset_x * 2.0)
      .max(px(32.0))
      .into();
    let width_px = (start_width + delta)
      .clamp(32.0, max_width.max(32.0))
      .round() as u32;
    let Some(Block::Image(image)) = Arc::make_mut(&mut self.document.blocks).get_mut(drag.block_ix) else {
      self.image_resize_drag = None;
      return true;
    };
    if image.sizing == (ImageSizing::Fixed { width_px, height_px: None }) {
      return true;
    }
    image.sizing = ImageSizing::Fixed { width_px, height_px: None };
    image.version = drag.before.version.wrapping_add(1);
    self.invalidate_document_layout_caches();
    cx.notify();
    true
  }

  fn finish_image_resize_drag(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(drag) = self.image_resize_drag.take() else {
      return false;
    };
    let Some(Block::Image(after)) = self.document.blocks.get(drag.block_ix).cloned() else {
      cx.notify();
      return true;
    };
    if after == drag.before {
      cx.notify();
      return true;
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock {
        block_ix: drag.block_ix,
        before: Block::Image(drag.before),
        after: Block::Image(after),
      }],
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed(after_generation, cx);
    true
  }

  fn image_rendered_width(&self, image: &ImageBlock) -> Pixels {
    let available_width = (self.current_layout_width() - self.document.theme.pageless_inset_x * 2.0).max(px(1.0));
    match image.sizing {
      ImageSizing::Fixed { width_px, .. } => px(width_px as f32).min(available_width),
      ImageSizing::FitWidth => available_width,
      ImageSizing::Intrinsic => self
        .document
        .assets
        .assets
        .get(&image.asset_id)
        .and_then(image_asset_intrinsic_size)
        .map(|(width, _)| width.min(available_width))
        .unwrap_or(available_width),
    }
  }

  pub fn set_selected_image_alt_text(&mut self, alt_text: impl Into<SharedString>, cx: &mut Context<Self>) {
    let Some(BlockSelection::Image(block_ix)) = self.selected_block else {
      return;
    };
    let alt_text = alt_text.into();
    self.edit_selected_image(block_ix, cx, |image| {
      image.alt_text = alt_text;
      image.version = image.version.wrapping_add(1);
    });
  }

  fn edit_selected_image(&mut self, block_ix: usize, cx: &mut Context<Self>, update: impl FnOnce(&mut ImageBlock)) {
    let Some(Block::Image(image)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    let mut updated = image.clone();
    update(&mut updated);
    if updated == image {
      return;
    }
    let before = Block::Image(image);
    let after = Block::Image(updated);
    if let Some(block) = Arc::make_mut(&mut self.document.blocks).get_mut(block_ix) {
      *block = after.clone();
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock { block_ix, before, after }],
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed(after_generation, cx);
  }

  pub fn insert_equation(&mut self, source: impl Into<SharedString>, cx: &mut Context<Self>) {
    self.insert_blocks_after_caret(
      vec![Block::Equation(EquationBlock {
        source: source.into(),
        syntax: EquationSyntax::Latex,
        display: EquationDisplay::Display,
        version: 0,
      })],
      cx,
    );
  }

  pub fn insert_image_block(&mut self, asset: AssetRecord, alt_text: impl Into<SharedString>, cx: &mut Context<Self>) {
    self.insert_image_assets(vec![(asset, alt_text.into())], cx);
  }

  fn insert_image_assets(&mut self, assets: Vec<(AssetRecord, SharedString)>, cx: &mut Context<Self>) {
    if assets.is_empty() {
      return;
    }
    let before_document = self.document.clone();
    let before_selection = self.selection.clone();
    let mut blocks = Vec::with_capacity(assets.len());
    for (asset, alt_text) in assets {
      let asset_id = asset.id;
      self.document.assets.assets.insert(asset_id, asset);
      blocks.push(Block::Image(ImageBlock {
        asset_id,
        alt_text,
        caption: None,
        sizing: ImageSizing::FitWidth,
        alignment: BlockAlignment::Center,
        version: 0,
      }));
    }
    self.insert_blocks_after_caret_without_history(blocks);
    self.push_replace_document_history(before_document, before_selection, cx);
  }

  pub fn prompt_insert_image(&mut self, cx: &mut Context<Self>) {
    let paths = cx.prompt_for_paths(PathPromptOptions {
      files: true,
      directories: false,
      multiple: false,
      prompt: Some("Insert image".into()),
    });
    cx.spawn(async move |editor, cx| {
      let Ok(Ok(Some(paths))) = paths.await else {
        return;
      };
      let Some(path) = paths.into_iter().next() else {
        return;
      };
      let Some((asset, alt_text)) = image_asset_from_path(&path) else {
        return;
      };
      editor
        .update(cx, |editor, cx| editor.insert_image_block(asset, alt_text, cx))
        .ok();
    })
    .detach();
  }

  fn on_file_drop(&mut self, paths: &ExternalPaths, window: &mut Window, cx: &mut Context<Self>) {
    let image_assets = paths
      .paths()
      .iter()
      .filter_map(|path| image_asset_from_path(path))
      .collect::<Vec<_>>();
    if image_assets.is_empty() {
      return;
    }
    self.place_block_insertion_from_point(window.mouse_position(), window, cx);
    self.insert_image_assets(image_assets, cx);
  }

  fn place_block_insertion_from_point(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
    let width = self.current_layout_width();
    self.ensure_exact_interaction_paragraph_heights(width, window, cx);
    let viewport = self.scroll_handle.bounds();
    let content_y = (position.y - viewport.top() - self.scroll_handle.offset().y).max(px(0.0));
    if self.height_prefix_index.len() == self.document.blocks.len() {
      let block_ix = self.height_prefix_index.lower_bound(content_y);
      if let Some(selection) = self.selection_for_object_block(block_ix) {
        self.select_block(selection, cx);
        return;
      }
    }
    let offset = self.hit_test_document_position(position, window, cx);
    self.selection = EditorSelection {
      anchor: offset,
      head: offset,
    };
    self.clear_block_selection();
    self.goal_x = None;
    self.reset_caret_blink(cx);
  }

  fn insert_clipboard_image(&mut self, image: Image, cx: &mut Context<Self>) {
    let asset_id = AssetId(uuid::Uuid::new_v4().as_u128());
    let mut hasher = DefaultHasher::new();
    image.bytes.hash(&mut hasher);
    let asset = AssetRecord {
      id: asset_id,
      mime_type: image.format.mime_type().into(),
      original_name: None,
      content_hash: hasher.finish(),
      bytes: Arc::new(image.bytes),
    };
    self.insert_image_block(asset, "Pasted image", cx);
  }

  fn insert_text_into_selected_table_cell(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block else {
      return false;
    };
    if text.is_empty() {
      return true;
    }
    let selection_range = self.table_cell_selection_range();
    let insert_at = selection_range
      .as_ref()
      .map(|range| range.start)
      .unwrap_or(self.table_cell_caret);
    let styles = self
      .selected_table_cell_paragraph()
      .map(|paragraph| table_cell_styles_at(paragraph, insert_at))
      .unwrap_or_default();
    self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
      if let Some(range) = selection_range.clone() {
        delete_range_in_table_cell_paragraph(paragraph, range);
      }
      insert_text_in_table_cell_paragraph(paragraph, insert_at, text, styles);
    });
    self.table_cell_caret = insert_at.saturating_add(text.len());
    self.table_cell_anchor = self.table_cell_caret;
    true
  }

  fn split_selected_table_cell_paragraph(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block else {
      return false;
    };
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix).cloned() else {
      return false;
    };
    let mut updated = table.clone();
    let Some(cell) = updated
      .rows
      .get_mut(row_ix)
      .and_then(|row| row.cells.get_mut(cell_ix))
    else {
      return false;
    };
    let Some(new_paragraph_ix) = split_table_cell_paragraph_at(cell, self.table_cell_block_ix, self.table_cell_caret) else {
      return true;
    };
    if updated == table {
      return true;
    }
    updated.version = updated.version.wrapping_add(1);
    let before = Block::Table(table);
    let after = Block::Table(updated);
    if let Some(block) = Arc::make_mut(&mut self.document.blocks).get_mut(block_ix) {
      *block = after.clone();
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock { block_ix, before, after }],
    });
    self.redo_stack.clear();
    self.table_cell_block_ix = new_paragraph_ix;
    self.table_cell_caret = 0;
    self.invalidate_document_layout_caches();
    self.mark_document_changed(after_generation, cx);
    true
  }

  fn insert_text_into_selected_equation(&mut self, text: &str, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::Equation(block_ix)) = self.selected_block else {
      return false;
    };
    if text.is_empty() {
      return true;
    }
    let selection_range = self.equation_source_selection_range();
    let insert_at = selection_range
      .as_ref()
      .map(|range| range.start)
      .unwrap_or(self.equation_source_caret);
    self.edit_selected_equation(block_ix, cx, |equation| {
      let mut source = equation.source.to_string();
      let insert_at = insert_at.min(source.len());
      if !source.is_char_boundary(insert_at) {
        return;
      }
      if let Some(range) = selection_range.clone()
        && range.start <= range.end
        && range.end <= source.len()
        && source.is_char_boundary(range.start)
        && source.is_char_boundary(range.end)
      {
        source.replace_range(range, "");
      }
      source.insert_str(insert_at, text);
      equation.source = source.into();
      equation.version = equation.version.wrapping_add(1);
    });
    self.equation_source_caret = insert_at.saturating_add(text.len());
    self.equation_source_anchor = self.equation_source_caret;
    true
  }

  fn backspace_selected_table_cell(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block else {
      return false;
    };
    let caret = self.table_cell_caret;
    if caret == 0 {
      let mut merged_caret = None;
      let current_paragraph_ix = self.table_cell_block_ix;
      self.edit_selected_table(cx, |table| {
        let Some(cell) = table
          .rows
          .get_mut(row_ix)
          .and_then(|row| row.cells.get_mut(cell_ix))
        else {
          return;
        };
        merged_caret = merge_table_cell_paragraph_with_previous(cell, current_paragraph_ix);
      });
      if let Some((paragraph_ix, byte)) = merged_caret {
        self.table_cell_block_ix = paragraph_ix;
        self.table_cell_caret = byte;
        cx.notify();
      }
      return true;
    }
    let new_caret = self
      .selected_table_cell_text()
      .and_then(|text| {
        let caret = caret.min(text.len());
        (caret > 0).then(|| {
          text[..caret]
            .char_indices()
            .next_back()
            .map(|(byte, _)| byte)
            .unwrap_or(0)
        })
      })
      .unwrap_or(caret);
    self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
      let caret = caret.min(paragraph.text.len());
      if caret == 0 {
        return;
      }
      let prev = paragraph.text[..caret]
        .char_indices()
        .next_back()
        .map(|(byte, _)| byte)
        .unwrap_or(0);
      delete_range_in_table_cell_paragraph(paragraph, prev..caret);
    });
    self.table_cell_caret = new_caret;
    true
  }

  fn delete_forward_selected_table_cell(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block else {
      return false;
    };
    let Some(text) = self.selected_table_cell_text() else {
      return true;
    };
    let caret = self.table_cell_caret.min(text.len());
    let next = if caret < text.len() {
      text[caret..]
        .char_indices()
        .nth(1)
        .map(|(byte, _)| caret + byte)
        .unwrap_or(text.len())
    } else {
      caret
    };
    if next > caret {
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        delete_range_in_table_cell_paragraph(paragraph, caret..next);
      });
    } else {
      let mut merged_caret = None;
      let current_paragraph_ix = self.table_cell_block_ix;
      self.edit_selected_table(cx, |table| {
        let Some(cell) = table
          .rows
          .get_mut(row_ix)
          .and_then(|row| row.cells.get_mut(cell_ix))
        else {
          return;
        };
        merged_caret = merge_table_cell_paragraph_with_next(cell, current_paragraph_ix);
      });
      if let Some((paragraph_ix, byte)) = merged_caret {
        self.table_cell_block_ix = paragraph_ix;
        self.table_cell_caret = byte;
        cx.notify();
      }
      return true;
    }
    self.table_cell_caret = caret;
    true
  }

  fn backspace_selected_equation(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(BlockSelection::Equation(block_ix)) = self.selected_block else {
      return false;
    };
    let selection_range = self.equation_source_selection_range();
    let caret = self.equation_source_caret;
    let mut next_caret = caret;
    self.edit_selected_equation(block_ix, cx, |equation| {
      let mut source = equation.source.to_string();
      if let Some(range) = selection_range.clone()
        && range.start <= range.end
        && range.end <= source.len()
        && source.is_char_boundary(range.start)
        && source.is_char_boundary(range.end)
      {
        source.replace_range(range.clone(), "");
        next_caret = range.start;
        equation.source = source.into();
        equation.version = equation.version.wrapping_add(1);
        return;
      }
      let caret = caret.min(source.len());
      if caret > 0
        && source.is_char_boundary(caret)
        && let Some((byte, _)) = source[..caret].char_indices().next_back()
      {
        source.replace_range(byte..caret, "");
        next_caret = byte;
        equation.source = source.into();
        equation.version = equation.version.wrapping_add(1);
      }
    });
    self.equation_source_caret = next_caret;
    self.equation_source_anchor = next_caret;
    true
  }

  fn edit_selected_equation(&mut self, block_ix: usize, cx: &mut Context<Self>, update: impl FnOnce(&mut EquationBlock)) {
    let Some(Block::Equation(equation)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    let mut updated = equation.clone();
    update(&mut updated);
    if updated == equation {
      return;
    }
    let before = Block::Equation(equation);
    let after = Block::Equation(updated);
    if let Some(block) = Arc::make_mut(&mut self.document.blocks).get_mut(block_ix) {
      *block = after.clone();
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock { block_ix, before, after }],
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed(after_generation, cx);
  }

  pub(super) fn edit_table_cell_paragraph(
    &mut self,
    block_ix: usize,
    row_ix: usize,
    cell_ix: usize,
    cx: &mut Context<Self>,
    update: impl FnOnce(&mut TableCellParagraph),
  ) {
    let Some(Block::Table(table)) = self.document.blocks.get(block_ix).cloned() else {
      return;
    };
    let mut updated = table.clone();
    let Some(cell) = updated
      .rows
      .get_mut(row_ix)
      .and_then(|row| row.cells.get_mut(cell_ix))
    else {
      return;
    };
    let paragraph_ix = table_cell_paragraph_block_ix(cell, self.table_cell_block_ix).unwrap_or_else(|| {
      cell
        .blocks
        .push(TableCellBlock::Paragraph(default_table_cell_paragraph()));
      cell.blocks.len() - 1
    });
    let TableCellBlock::Paragraph(paragraph) = &mut cell.blocks[paragraph_ix] else {
      return;
    };
    update(paragraph);
    if updated == table {
      return;
    }
    updated.version = updated.version.wrapping_add(1);
    let before = Block::Table(table);
    let after = Block::Table(updated);
    if let Some(block) = Arc::make_mut(&mut self.document.blocks).get_mut(block_ix) {
      *block = after.clone();
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.undo_stack.push(EditRecord {
      before_selection: self.selection.clone(),
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceBlock { block_ix, before, after }],
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed(after_generation, cx);
  }

  pub fn toggle_underline(&mut self, cx: &mut Context<Self>) {
    if self.clear_matching_armed_inline_tool(ArmedInlineTool::Underline, cx) {
      return;
    }
    self.toggle_underline_kind(None, cx);
  }

  /// Toggle any semantic inline style for the current selection or caret.
  ///
  /// The ribbon can call this generic method instead of matching each style to
  /// a shortcut-specific wrapper like `toggle_cite` or `toggle_emphasis`.
  pub fn toggle_semantic_style_for_selection(&mut self, semantic: RunSemanticStyle, cx: &mut Context<Self>) {
    if self.clear_matching_armed_inline_tool(ArmedInlineTool::Semantic(semantic), cx) {
      return;
    }
    self.toggle_semantic_style(semantic, cx);
  }

  pub fn toggle_emphasis(&mut self, cx: &mut Context<Self>) {
    self.toggle_semantic_style(RunSemanticStyle::Emphasis, cx);
  }

  pub fn toggle_cite(&mut self, cx: &mut Context<Self>) {
    self.toggle_semantic_style(RunSemanticStyle::Cite, cx);
  }

  pub fn toggle_condensed(&mut self, cx: &mut Context<Self>) {
    self.toggle_semantic_style(RunSemanticStyle::Condensed, cx);
  }

  pub fn toggle_ultracondensed(&mut self, cx: &mut Context<Self>) {
    self.toggle_semantic_style(RunSemanticStyle::Ultracondensed, cx);
  }

  pub fn set_highlight(&mut self, highlight: HighlightStyle, cx: &mut Context<Self>) {
    if self.clear_matching_armed_inline_tool(ArmedInlineTool::Highlight(highlight), cx) {
      return;
    }
    self.set_highlight_internal(Some(highlight), cx);
  }

  /// Set or clear the highlight style for the current selection or caret.
  ///
  /// `None` clears highlights. `Some(...)` applies the requested highlight, or
  /// toggles it off when the whole selection already has that highlight.
  pub fn set_highlight_for_selection(&mut self, highlight: Option<HighlightStyle>, cx: &mut Context<Self>) {
    self.set_highlight_internal(highlight, cx);
  }

  pub fn clear_highlight(&mut self, cx: &mut Context<Self>) {
    self.set_highlight_internal(None, cx);
  }

  pub fn clear_formatting(&mut self, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        paragraph.paragraph.style = ParagraphStyle::Normal;
        for run in &mut paragraph.paragraph.runs {
          run.styles = RunStyles::default();
        }
        paragraph.paragraph.runs = merge_adjacent_runs(std::mem::take(&mut paragraph.paragraph.runs));
        paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
      });
      return;
    }
    self.apply_document_edit(cx, |editor, cx| {
      if editor.selection.is_caret() {
        let paragraph_ix = editor.selection.head.paragraph;
        clear_whole_paragraph_formatting(&mut editor.document, paragraph_ix);
      } else {
        let range = editor.selection.normalized();
        if selection_contains_whole_paragraph(&editor.document, range.clone()) {
          for paragraph_ix in range.start.paragraph..=range.end.paragraph {
            clear_whole_paragraph_formatting(&mut editor.document, paragraph_ix);
          }
        } else {
          mutate_runs_in_range(&mut editor.document, range, |styles| *styles = RunStyles::default());
        }
      }
      editor.pending_styles = None;
      editor.after_text_mutation(cx);
    });
  }

  pub fn apply_run_style_to_selection(&mut self, style: RunStyle, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      let Some(selection_range) = self.table_cell_selection_range() else {
        return;
      };
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        if paragraph.text.is_empty() {
          return;
        }
        if paragraph.paragraph.runs.is_empty() {
          paragraph.paragraph.runs.push(TextRun {
            len: paragraph.text.len(),
            styles: RunStyles::default(),
          });
        }
        mutate_table_cell_runs_in_range(paragraph, selection_range.clone(), |styles| styles.apply(style));
        paragraph.paragraph.runs = merge_adjacent_runs(std::mem::take(&mut paragraph.paragraph.runs));
        paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
      });
      return;
    }
    if self.selection.is_caret() {
      return;
    }
    self.apply_document_edit(cx, |editor, _| {
      let range = editor.selection.normalized();
      for paragraph_ix in range.start.paragraph..=range.end.paragraph {
        let start = if paragraph_ix == range.start.paragraph { range.start.byte } else { 0 };
        let end = if paragraph_ix == range.end.paragraph {
          range.end.byte
        } else {
          paragraph_text_len(&editor.document.paragraphs[paragraph_ix])
        };
        apply_style_to_paragraph_range(&mut editor.document, paragraph_ix, start..end, style);
      }
    });
  }

  pub fn set_paragraph_style_for_selection(&mut self, style: ParagraphStyle, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        if paragraph.paragraph.style != style {
          paragraph.paragraph.style = style;
          paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
        }
      });
      return;
    }
    self.apply_document_edit(cx, |editor, _| {
      let range = editor.selection.normalized();
      for paragraph_ix in range.start.paragraph..=range.end.paragraph {
        if let Some(paragraph) = paragraphs_mut(&mut editor.document).get_mut(paragraph_ix) {
          if paragraph.style != style {
            paragraph.style = style;
            bump_paragraph_version(paragraph);
          }
        }
      }
    });
  }

  // -------- Action handlers (bound to keystrokes in main.rs) -----------
  // Each handler delegates to a movement/edit primitive defined below.
  // The signatures all match what `cx.listener(...)` expects:
  //   fn(&mut Self, &Action, &mut Window, &mut Context<Self>).

  fn on_move_left(&mut self, _: &MoveLeft, _: &mut Window, cx: &mut Context<Self>) {
    self.move_left(cx);
  }
  fn on_move_right(&mut self, _: &MoveRight, _: &mut Window, cx: &mut Context<Self>) {
    self.move_right(cx);
  }
  fn on_move_up(&mut self, _: &MoveUp, _: &mut Window, cx: &mut Context<Self>) {
    self.move_up(cx);
  }
  fn on_move_down(&mut self, _: &MoveDown, _: &mut Window, cx: &mut Context<Self>) {
    self.move_down(cx);
  }
  fn on_move_line_start(&mut self, _: &MoveLineStart, _: &mut Window, cx: &mut Context<Self>) {
    self.move_line_start(cx);
  }
  fn on_move_line_end(&mut self, _: &MoveLineEnd, _: &mut Window, cx: &mut Context<Self>) {
    self.move_line_end(cx);
  }
  fn on_select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
    self.select_left(cx);
  }
  fn on_select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
    self.select_right(cx);
  }
  fn on_select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
    self.select_up(cx);
  }
  fn on_select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
    self.select_down(cx);
  }
  fn on_select_line_start(&mut self, _: &SelectLineStart, _: &mut Window, cx: &mut Context<Self>) {
    self.select_line_start(cx);
  }
  fn on_select_line_end(&mut self, _: &SelectLineEnd, _: &mut Window, cx: &mut Context<Self>) {
    self.select_line_end(cx);
  }
  fn on_select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
    self.select_all(cx);
  }
  fn on_move_word_left(&mut self, _: &MoveWordLeft, _: &mut Window, cx: &mut Context<Self>) {
    self.move_word_left(cx);
  }
  fn on_move_word_right(&mut self, _: &MoveWordRight, _: &mut Window, cx: &mut Context<Self>) {
    self.move_word_right(cx);
  }
  fn on_select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
    self.select_word_left(cx);
  }
  fn on_select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
    self.select_word_right(cx);
  }
  fn on_delete_word_backward(&mut self, _: &DeleteWordBackward, _: &mut Window, cx: &mut Context<Self>) {
    self.delete_word_backward_command(cx);
  }
  fn on_delete_word_forward(&mut self, _: &DeleteWordForward, _: &mut Window, cx: &mut Context<Self>) {
    self.delete_word_forward_command(cx);
  }
  fn on_page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
    self.page_up(cx);
  }
  fn on_page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
    self.page_down(cx);
  }
  fn on_select_page_up(&mut self, _: &SelectPageUp, _: &mut Window, cx: &mut Context<Self>) {
    self.select_page_up(cx);
  }
  fn on_select_page_down(&mut self, _: &SelectPageDown, _: &mut Window, cx: &mut Context<Self>) {
    self.select_page_down(cx);
  }
  fn on_move_document_start(&mut self, _: &MoveDocumentStart, _: &mut Window, cx: &mut Context<Self>) {
    self.move_document_start(cx);
  }
  fn on_move_document_end(&mut self, _: &MoveDocumentEnd, _: &mut Window, cx: &mut Context<Self>) {
    self.move_document_end(cx);
  }
  fn on_select_document_start(&mut self, _: &SelectDocumentStart, _: &mut Window, cx: &mut Context<Self>) {
    self.select_document_start(cx);
  }
  fn on_select_document_end(&mut self, _: &SelectDocumentEnd, _: &mut Window, cx: &mut Context<Self>) {
    self.select_document_end(cx);
  }
  fn on_copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
    self.copy(cx);
  }
  fn on_cut(&mut self, _: &Cut, _: &mut Window, cx: &mut Context<Self>) {
    self.cut(cx);
  }
  fn on_paste(&mut self, _: &Paste, _: &mut Window, cx: &mut Context<Self>) {
    self.paste(cx);
  }
  fn on_save(&mut self, _: &Save, _: &mut Window, cx: &mut Context<Self>) {
    if let Err(error) = self.save(cx) {
      eprintln!("failed to save: {error}");
    }
  }
  fn on_undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
    self.undo(cx);
  }
  fn on_redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
    self.redo(cx);
  }
  fn on_set_paragraph_pocket(&mut self, _: &SetParagraphPocket, _: &mut Window, cx: &mut Context<Self>) {
    self.set_paragraph_style_for_selection(ParagraphStyle::Pocket, cx);
  }
  fn on_set_paragraph_hat(&mut self, _: &SetParagraphHat, _: &mut Window, cx: &mut Context<Self>) {
    self.set_paragraph_style_for_selection(ParagraphStyle::Hat, cx);
  }
  fn on_set_paragraph_block(&mut self, _: &SetParagraphBlock, _: &mut Window, cx: &mut Context<Self>) {
    self.set_paragraph_style_for_selection(ParagraphStyle::Block, cx);
  }
  fn on_set_paragraph_tag(&mut self, _: &SetParagraphTag, _: &mut Window, cx: &mut Context<Self>) {
    self.set_paragraph_style_for_selection(ParagraphStyle::Tag, cx);
  }
  fn on_set_paragraph_analytic(&mut self, _: &SetParagraphAnalytic, _: &mut Window, cx: &mut Context<Self>) {
    self.set_paragraph_style_for_selection(ParagraphStyle::Analytic, cx);
  }
  fn on_toggle_cite(&mut self, _: &ToggleCite, _: &mut Window, cx: &mut Context<Self>) {
    self.toggle_cite(cx);
  }
  fn on_toggle_underline(&mut self, _: &ToggleUnderline, _: &mut Window, cx: &mut Context<Self>) {
    self.toggle_underline(cx);
  }
  fn on_toggle_emphasis(&mut self, _: &ToggleEmphasis, _: &mut Window, cx: &mut Context<Self>) {
    self.toggle_emphasis(cx);
  }
  fn on_set_highlight_spoken(&mut self, _: &SetHighlightSpoken, _: &mut Window, cx: &mut Context<Self>) {
    self.set_highlight(HighlightStyle::Spoken, cx);
  }
  fn on_clear_formatting(&mut self, _: &ClearFormatting, _: &mut Window, cx: &mut Context<Self>) {
    self.clear_formatting(cx);
  }
  fn on_clear_highlight(&mut self, _: &ClearHighlight, _: &mut Window, cx: &mut Context<Self>) {
    self.clear_highlight(cx);
  }
  fn on_insert_image(&mut self, _: &InsertImage, _: &mut Window, cx: &mut Context<Self>) {
    self.prompt_insert_image(cx);
  }
  fn on_insert_table(&mut self, _: &InsertTable, _: &mut Window, cx: &mut Context<Self>) {
    self.insert_default_table(2, 2, cx);
  }
  fn on_insert_equation(&mut self, _: &InsertEquation, _: &mut Window, cx: &mut Context<Self>) {
    self.insert_equation("x^2 + y^2 = z^2", cx);
  }
  fn on_backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
    self.backspace_command(cx);
  }
  fn on_delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
    self.delete_forward_command(cx);
  }
  fn on_insert_newline(&mut self, _: &InsertNewline, _: &mut Window, cx: &mut Context<Self>) {
    if self.split_selected_table_cell_paragraph(cx) {
      return;
    }
    self.insert_paragraph_break_command(cx);
  }
  fn on_insert_soft_line_break(&mut self, _: &InsertSoftLineBreak, _: &mut Window, cx: &mut Context<Self>) {
    if self.insert_text_into_selected_table_cell(SOFT_LINE_BREAK_STR, cx) {
      return;
    }
    if self.insert_text_into_selected_equation(SOFT_LINE_BREAK_STR, cx) {
      return;
    }
    self.insert_text_command(SOFT_LINE_BREAK_STR, cx);
  }

  // Raw key handler: routes printable characters to `insert_text`. Non-
  // printable keys (arrows, Backspace, etc.) carry `key_char = None` and are
  // ignored here — they are routed via the action system above instead.
  fn on_key_down_event(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
    // If the user is holding a modifier that turns the key into a shortcut
    // (Ctrl/Cmd), don't insert the character. Shift and Alt remain available
    // for things like capital letters and option-letter accented chars.
    let m = &event.keystroke.modifiers;
    if m.control || m.platform {
      return;
    }
    if event.keystroke.key == "tab" {
      if self.move_selected_table_cell(!m.shift, cx) {
        return;
      }
    }
    #[cfg(target_os = "windows")]
    let key_char = event
      .keystroke
      .key_char
      .as_deref()
      .or_else(|| (event.keystroke.key == "space" && !m.alt && !m.function).then_some(" "));

    #[cfg(not(target_os = "windows"))]
    let key_char = event.keystroke.key_char.as_deref();

    let Some(key_char) = key_char else {
      return;
    };
    if key_char.is_empty() {
      return;
    }
    #[cfg(target_os = "windows")]
    {
      let key_char = if window.capslock().on {
        windows_apply_capslock(key_char)
      } else {
        key_char.to_string()
      };
      if self.insert_text_into_selected_table_cell(&key_char, cx) {
        return;
      }
      if self.insert_text_into_selected_equation(&key_char, cx) {
        return;
      }
      self.insert_text_command(&key_char, cx);
    }

    #[cfg(not(target_os = "windows"))]
    {
      let _ = window;
      if self.insert_text_into_selected_table_cell(key_char, cx) {
        return;
      }
      if self.insert_text_into_selected_equation(key_char, cx) {
        return;
      }
      self.insert_text_command(key_char, cx);
    }
  }

  pub(super) fn apply_document_edit(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Self, &mut Context<Self>)) {
    self.apply_document_edit_with_capture_range(cx, None, edit);
  }

  fn apply_document_edit_with_capture_range(
    &mut self,
    cx: &mut Context<Self>,
    capture_range: Option<Range<usize>>,
    edit: impl FnOnce(&mut Self, &mut Context<Self>),
  ) {
    let timing = Instant::now();
    let before_selection = self.selection.clone();
    let before_paragraph_count = self.document.paragraphs.len();
    let before_range = capture_range.unwrap_or_else(|| self.edit_capture_range());
    let before_span = capture_document_span(&self.document, before_range);
    edit(self, cx);
    let paragraph_delta = self.document.paragraphs.len() as isize - before_paragraph_count as isize;
    let after_count = before_span
      .paragraphs
      .len()
      .saturating_add_signed(paragraph_delta)
      .min(
        self
          .document
          .paragraphs
          .len()
          .saturating_sub(before_span.start_paragraph),
      );
    let after_span = capture_document_span(&self.document, before_span.start_paragraph..before_span.start_paragraph + after_count);
    self.finish_document_edit(before_span, before_selection, after_span, cx);
    log_timing("edit command", timing, format!("paragraphs={}", self.document.paragraphs.len()));
  }

  fn edit_capture_range(&self) -> Range<usize> {
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 {
      return 0..0;
    }
    let range = self.selection.normalized();
    let start = range.start.paragraph.saturating_sub(1);
    let end = (range.end.paragraph + 2)
      .min(paragraph_count)
      .max(start + 1);
    start..end
  }

  fn finish_document_edit(
    &mut self,
    before_span: DocumentSpan,
    before_selection: EditorSelection,
    after_span: DocumentSpan,
    cx: &mut Context<Self>,
  ) {
    if before_span == after_span && before_selection == self.selection {
      return;
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let record = EditRecord {
      before_selection,
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceParagraphSpan {
        before: before_span,
        after: after_span,
      }],
    };
    self.undo_stack.push(record);
    self.redo_stack.clear();
    self.mark_document_changed(after_generation, cx);
  }

  fn mark_document_changed(&mut self, generation: u64, cx: &mut Context<Self>) {
    self.edit_generation = generation;
    self.refresh_save_status();
    self.schedule_recovery_write(cx);
    cx.notify();
  }

  fn after_history_restore(&mut self, cx: &mut Context<Self>) {
    self.goal_x = None;
    self.invalidate_document_layout_caches();
    self.refresh_save_status();
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    self.schedule_recovery_write(cx);
    cx.notify();
  }

  fn paragraph_item_sizes(&mut self, window: &mut Window, cx: &mut Context<Self>) -> Rc<Vec<Size<Pixels>>> {
    self
      .paragraph_height_cache
      .resize(self.document.paragraphs.len(), None);
    self
      .paragraph_layout_cache
      .resize(self.document.paragraphs.len(), None);
    let viewport_width = self.scroll_handle.bounds().size.width;
    let has_measured_viewport = viewport_width > px(1.0);
    if !has_measured_viewport {
      self.schedule_viewport_size_refresh(window, cx);
    }
    let width = self
      .measured_item_width
      .unwrap_or(if has_measured_viewport { viewport_width } else { px(900.0) });
    if has_measured_viewport && self.initial_layout_hidden {
      self.ensure_exact_initial_viewport_heights(width, window, cx);
    }
    self.ensure_exact_interaction_paragraph_heights(width, window, cx);
    if let Some(cache) = &self.item_sizes_cache
      && cache.width == width
      && cache.item_count == self.document.blocks.len()
      && cache.height_revision == self.paragraph_height_cache_revision
    {
      return cache.sizes.clone();
    }
    let mut paragraph_ix = 0;
    let sizes = Rc::new(
      self
        .document
        .blocks
        .iter()
        .enumerate()
        .map(|(block_ix, block)| {
          let Block::Paragraph(_) = block else {
            let height = layout_structural_block_at(&self.document, block_ix, width, px(0.0), window, cx)
              .as_ref()
              .map(structural_block_height)
              .unwrap_or_else(|| estimate_structural_block_item_height(&self.document, block_ix, width));
            return size(width, height + self.document.theme.paragraph_after);
          };
          let Some(paragraph) = self.document.paragraphs.get(paragraph_ix) else {
            return size(width, px(1.0));
          };
          let key = paragraph_cache_key(&self.document, paragraph);
          let height = self
            .paragraph_height_cache
            .get(paragraph_ix)
            .and_then(|entry| *entry)
            .filter(|entry| entry.key == key && entry.width == width)
            .map(|entry| entry.height)
            .unwrap_or_else(|| estimate_paragraph_item_height(&self.document, paragraph_ix, width));
          paragraph_ix += 1;
          size(width, height)
        })
        .collect::<Vec<_>>(),
    );
    self.height_prefix_index.rebuild(sizes.as_ref());
    self.item_sizes_cache = Some(ItemSizesCache {
      width,
      item_count: self.document.blocks.len(),
      height_revision: self.paragraph_height_cache_revision,
      sizes: sizes.clone(),
    });
    sizes
  }

  fn ensure_exact_interaction_paragraph_heights(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    let mut ranges = vec![self.active_height_range(), self.predicted_visible_height_range(width)];
    if !self.visible_layout_range.is_empty() {
      let visible_paragraph_range = self.paragraph_range_for_block_range(self.visible_layout_range.clone());
      ranges.push(expand_paragraph_range(visible_paragraph_range, self.document.paragraphs.len(), 2));
    }

    for range in ranges {
      for paragraph_ix in range {
        self.ensure_exact_paragraph_height(paragraph_ix, width, window, cx);
      }
    }
  }

  fn ensure_exact_initial_viewport_heights(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 {
      return;
    }

    let viewport_height = self.scroll_handle.bounds().size.height.max(px(700.0));
    let target_height = viewport_height + px(512.0);
    let mut accumulated = px(0.0);

    for paragraph_ix in 0..paragraph_count {
      self.ensure_exact_paragraph_height(paragraph_ix, width, window, cx);
      let Some(height) = self
        .paragraph_height_cache
        .get(paragraph_ix)
        .and_then(|entry| *entry)
        .map(|entry| entry.height)
      else {
        continue;
      };
      accumulated += height;
      if accumulated >= target_height {
        break;
      }
    }
  }

  fn ensure_exact_paragraph_height(&mut self, paragraph_ix: usize, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    let Some(paragraph) = self.document.paragraphs.get(paragraph_ix) else {
      return;
    };
    let key = paragraph_cache_key(&self.document, paragraph);
    let cache_is_current = self
      .paragraph_height_cache
      .get(paragraph_ix)
      .and_then(|entry| *entry)
      .is_some_and(|entry| entry.key == key && entry.width == width);
    if cache_is_current {
      return;
    }
    let layout = build_single_paragraph_layout(&self.document, paragraph_ix, width, None, window, cx);
    self.cache_paragraph_layout(paragraph_ix, width, Rc::new(layout.clone()));
    self.paragraph_height_cache[paragraph_ix] = Some(ParagraphHeightCacheEntry {
      key,
      width,
      height: layout.size.height,
    });
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
  }

  fn schedule_viewport_size_refresh(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if self.pending_viewport_size_refresh {
      return;
    }
    self.pending_viewport_size_refresh = true;
    cx.on_next_frame(window, |editor, _, cx| {
      editor.pending_viewport_size_refresh = false;
      editor.item_sizes_cache = None;
      cx.notify();
    });
  }

  fn cache_paragraph_layout(&mut self, paragraph_ix: usize, width: Pixels, layout: Rc<LayoutState>) {
    self
      .paragraph_layout_cache
      .resize(self.document.paragraphs.len(), None);
    let Some(paragraph) = layout.paragraphs.first() else {
      return;
    };
    self.paragraph_layout_cache[paragraph_ix] = Some(ParagraphLayoutCacheEntry {
      key: paragraph.cache_key,
      width,
      layout,
    });
  }

  fn cached_paragraph_layout(&self, paragraph_ix: usize, width: Pixels) -> Option<Rc<LayoutState>> {
    let paragraph = self.document.paragraphs.get(paragraph_ix)?;
    let key = paragraph_cache_key(&self.document, paragraph);
    self
      .paragraph_layout_cache
      .get(paragraph_ix)
      .and_then(|entry| entry.as_ref())
      .filter(|entry| entry.key == key && entry.width == width)
      .map(|entry| entry.layout.clone())
  }

  fn document_layout_for_paragraph(&self, paragraph_ix: usize, width: Pixels) -> Option<LayoutState> {
    let mut layout = self
      .cached_paragraph_layout(paragraph_ix, width)?
      .as_ref()
      .clone();
    let viewport = self.scroll_handle.bounds();
    let row_top = if self.height_prefix_index.len() == self.document.blocks.len() {
      self
        .block_ix_for_paragraph(paragraph_ix)
        .map(|block_ix| self.height_prefix_index.item_top(block_ix))
        .unwrap_or(px(0.0))
    } else {
      px(0.0)
    };
    layout.bounds = Some(Bounds::new(
      point(viewport.left(), viewport.top() + self.scroll_handle.offset().y + row_top),
      size(width, layout.size.height),
    ));
    Some(layout)
  }

  fn layout_for_offset(&self, offset: DocumentOffset) -> Option<LayoutState> {
    let width = self.current_layout_width();
    self
      .document_layout_for_paragraph(offset.paragraph, width)
      .or_else(|| {
        self
          .last_layout
          .as_ref()
          .filter(|layout| paragraph_layout(layout, offset.paragraph).is_some())
          .map(|layout| layout.as_ref().clone())
      })
  }

  fn hit_test_cached_position(&self, position: Point<Pixels>) -> Option<DocumentOffset> {
    if let Some(layout) = self.last_layout.as_ref()
      && let (Some(first), Some(last)) = (layout.paragraphs.first(), layout.paragraphs.last())
      && position.y >= first.top
      && position.y <= last.bottom
    {
      let _ = layout.block_paragraph_ix(layout.paragraph_block_ix(first.index).unwrap_or(0));
      return Some(layout.hit_test(position));
    }
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 || self.height_prefix_index.len() != self.document.blocks.len() {
      return None;
    }
    let viewport = self.scroll_handle.bounds();
    let content_y = (position.y - viewport.top() - self.scroll_handle.offset().y).max(px(0.0));
    let block_ix = self.height_prefix_index.lower_bound(content_y);
    let paragraph_ix = self.paragraph_ix_for_block(block_ix)?;
    self
      .document_layout_for_paragraph(paragraph_ix, self.current_layout_width())
      .map(|layout| layout.hit_test(position))
  }

  fn current_layout_width(&self) -> Pixels {
    if let Some(width) = self.measured_item_width {
      return width;
    }
    let viewport_width = self.scroll_handle.bounds().size.width;
    if viewport_width > px(1.0) { viewport_width } else { px(900.0) }
  }

  fn block_ix_for_paragraph(&self, target_paragraph_ix: usize) -> Option<usize> {
    let mut paragraph_ix = 0;
    for (block_ix, block) in self.document.blocks.iter().enumerate() {
      if matches!(block, Block::Paragraph(_)) {
        if paragraph_ix == target_paragraph_ix {
          return Some(block_ix);
        }
        paragraph_ix += 1;
      }
    }
    None
  }

  fn selection_for_object_block(&self, block_ix: usize) -> Option<BlockSelection> {
    match self.document.blocks.get(block_ix) {
      Some(Block::Image(_)) => Some(BlockSelection::Image(block_ix)),
      Some(Block::Equation(_)) => Some(BlockSelection::Equation(block_ix)),
      Some(Block::Table(_)) => Some(BlockSelection::Table(block_ix)),
      Some(Block::Paragraph(_)) | None => None,
    }
  }

  fn immediate_object_after_paragraph(&self, paragraph_ix: usize) -> Option<BlockSelection> {
    let block_ix = self.block_ix_for_paragraph(paragraph_ix)? + 1;
    self.selection_for_object_block(block_ix)
  }

  fn immediate_object_before_paragraph(&self, paragraph_ix: usize) -> Option<BlockSelection> {
    let block_ix = self.block_ix_for_paragraph(paragraph_ix)?.checked_sub(1)?;
    self.selection_for_object_block(block_ix)
  }

  fn paragraph_before_block(&self, target_block_ix: usize) -> Option<usize> {
    let mut paragraph_ix = 0;
    let mut last = None;
    for (block_ix, block) in self.document.blocks.iter().enumerate() {
      if block_ix >= target_block_ix {
        return last;
      }
      if matches!(block, Block::Paragraph(_)) {
        last = Some(paragraph_ix);
        paragraph_ix += 1;
      }
    }
    last
  }

  fn paragraph_after_block(&self, target_block_ix: usize) -> Option<usize> {
    let mut paragraph_ix = 0;
    for (block_ix, block) in self.document.blocks.iter().enumerate() {
      if matches!(block, Block::Paragraph(_)) {
        if block_ix > target_block_ix {
          return Some(paragraph_ix);
        }
        paragraph_ix += 1;
      }
    }
    None
  }

  fn collapse_object_selection(&mut self, dir: HDir, cx: &mut Context<Self>) -> bool {
    let Some(selection) = self.selected_block.take() else {
      return false;
    };
    let block_ix = match selection {
      BlockSelection::Image(block_ix)
      | BlockSelection::Equation(block_ix)
      | BlockSelection::Table(block_ix)
      | BlockSelection::TableCell { block_ix, .. } => block_ix,
    };
    let offset = match dir {
      HDir::Left => self
        .paragraph_before_block(block_ix)
        .map(|paragraph| DocumentOffset {
          paragraph,
          byte: paragraph_text_len(&self.document.paragraphs[paragraph]),
        }),
      HDir::Right => self
        .paragraph_after_block(block_ix)
        .map(|paragraph| DocumentOffset { paragraph, byte: 0 }),
    };
    if let Some(offset) = offset {
      self.selection = EditorSelection {
        anchor: offset,
        head: offset,
      };
      self.scroll_head_into_view();
      self.reset_caret_blink(cx);
      cx.notify();
    } else {
      self.selected_block = Some(selection);
    }
    true
  }

  fn paragraph_ix_for_block(&self, target_block_ix: usize) -> Option<usize> {
    let mut paragraph_ix = 0;
    for (block_ix, block) in self.document.blocks.iter().enumerate() {
      if matches!(block, Block::Paragraph(_)) {
        if block_ix == target_block_ix {
          return Some(paragraph_ix);
        }
        paragraph_ix += 1;
      }
    }
    None
  }

  fn paragraph_range_for_block_range(&self, block_range: Range<usize>) -> Range<usize> {
    let mut first = None;
    let mut last = None;
    for block_ix in block_range {
      if let Some(paragraph_ix) = self.paragraph_ix_for_block(block_ix) {
        first.get_or_insert(paragraph_ix);
        last = Some(paragraph_ix);
      }
    }
    match (first, last) {
      (Some(start), Some(end)) => start..end + 1,
      _ => 0..0,
    }
  }

  fn predicted_visible_height_range(&self, width: Pixels) -> Range<usize> {
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 {
      return 0..0;
    }

    let viewport = self.scroll_handle.bounds();
    let viewport_height = if viewport.size.height > px(1.0) {
      viewport.size.height
    } else {
      px(1000.0)
    };
    let scroll_top = -self.scroll_handle.offset().y;
    let scroll_bottom = scroll_top + viewport_height + px(256.0);
    if self.height_prefix_index.len() == self.document.blocks.len() {
      let start_block = self
        .height_prefix_index
        .lower_bound((scroll_top - px(256.0)).max(px(0.0)));
      let end_block = (self.height_prefix_index.lower_bound(scroll_bottom) + 1).min(self.document.blocks.len());
      let paragraph_range = self.paragraph_range_for_block_range(start_block..end_block.max(start_block + 1));
      if !paragraph_range.is_empty() {
        return expand_paragraph_range(paragraph_range, paragraph_count, 2);
      }
    }
    let mut y = px(0.0);
    let mut start = 0;
    let mut found_start = false;

    for paragraph_ix in 0..paragraph_count {
      let Some(paragraph) = self.document.paragraphs.get(paragraph_ix) else {
        break;
      };
      let key = paragraph_cache_key(&self.document, paragraph);
      let height = self
        .paragraph_height_cache
        .get(paragraph_ix)
        .and_then(|entry| *entry)
        .filter(|entry| entry.key == key && entry.width == width)
        .map(|entry| entry.height)
        .unwrap_or_else(|| estimate_paragraph_item_height(&self.document, paragraph_ix, width));
      let next_y = y + height;
      if !found_start && next_y >= scroll_top - px(256.0) {
        start = paragraph_ix;
        found_start = true;
      }
      if found_start && y > scroll_bottom {
        return expand_paragraph_range(start..paragraph_ix + 1, paragraph_count, 2);
      }
      y = next_y;
    }

    expand_paragraph_range(start..paragraph_count, paragraph_count, 2)
  }

  fn apply_pending_paragraph_snap(&mut self, cx: &mut Context<Self>) {
    let Some((paragraph_ix, remaining)) = self.pending_snap_to_paragraph else {
      return;
    };
    let Some(block_ix) = self.block_ix_for_paragraph(paragraph_ix) else {
      self.pending_snap_to_paragraph = None;
      return;
    };
    if paragraph_ix >= self.document.paragraphs.len() || self.height_prefix_index.len() != self.document.blocks.len() {
      self.pending_snap_to_paragraph = None;
      return;
    }

    let mut offset = self.scroll_handle.offset();
    offset.y = -self.height_prefix_index.item_top(block_ix);
    self.scroll_handle.set_offset(offset);

    if remaining > 1 {
      self.pending_snap_to_paragraph = Some((paragraph_ix, remaining - 1));
      cx.notify();
    } else {
      self.pending_snap_to_paragraph = None;
    }
  }

  fn active_height_range(&self) -> Range<usize> {
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 {
      return 0..0;
    }
    let selection = self.selection.normalized();
    let start = selection.start.paragraph.saturating_sub(1);
    let end = (selection.end.paragraph + 2)
      .min(paragraph_count)
      .max(start + 1);
    start..end
  }

  pub(super) fn update_paragraph_height_cache(
    &mut self,
    paragraph_ix: usize,
    width: Pixels,
    key: ParagraphCacheKey,
    height: Pixels,
    cx: &mut Context<Self>,
  ) {
    self.note_measured_item_width(width, cx);
    self
      .paragraph_height_cache
      .resize(self.document.paragraphs.len(), None);
    let entry = ParagraphHeightCacheEntry { key, width, height };
    if self
      .paragraph_height_cache
      .get(paragraph_ix)
      .copied()
      .flatten()
      == Some(entry)
    {
      return;
    }
    self.paragraph_height_cache[paragraph_ix] = Some(entry);
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
    cx.notify();
  }

  pub(super) fn note_measured_item_width(&mut self, width: Pixels, cx: &mut Context<Self>) {
    if self.measured_item_width == Some(width) {
      return;
    }
    self.measured_item_width = Some(width);
    self.item_sizes_cache = None;
    cx.notify();
  }

  fn begin_visible_layout(&mut self, range: Range<usize>) -> u64 {
    if self.initial_layout_hidden
      && range.start == 0
      && range.end == 1
      && self.document.paragraphs.len() > 1
      && self.scroll_handle.bounds().size.height <= px(1.0)
    {
      // gpui-component's VirtualList measures item 0 in request_layout before
      // prepaint computes the real visible range. Do not let that measurement
      // pass stand in for the startup viewport, or the document can reveal
      // while most visible rows still use estimated heights.
      return self.visible_layout_generation;
    }

    self.visible_layout_generation = self.visible_layout_generation.wrapping_add(1);
    self.visible_layout_range = range.clone();
    self.visible_layout_parts = vec![None; range.end.saturating_sub(range.start)];
    self.visible_layout_generation
  }

  pub(super) fn store_visible_paragraph_layout(&mut self, generation: u64, paragraph_ix: usize, layout: &LayoutState, bounds: Bounds<Pixels>) {
    let Some(block_ix) = self.block_ix_for_paragraph(paragraph_ix) else {
      return;
    };
    if generation != self.visible_layout_generation || !self.visible_layout_range.contains(&block_ix) {
      return;
    }
    self.cache_paragraph_layout(paragraph_ix, layout.width, Rc::new(layout.clone()));
    let Some(source) = layout.paragraphs.first() else {
      return;
    };
    let mut paragraph = source.clone();
    paragraph.shift_y(bounds.origin.y + source.top);
    let part_ix = block_ix - self.visible_layout_range.start;
    if let Some(slot) = self.visible_layout_parts.get_mut(part_ix) {
      *slot = Some(paragraph);
    }

    let paragraphs = self
      .visible_layout_parts
      .iter()
      .filter_map(|paragraph| paragraph.clone())
      .collect::<Vec<_>>();
    if paragraphs.is_empty() {
      return;
    }
    self.last_layout = Some(Rc::new(LayoutState {
      blocks: paragraphs
        .iter()
        .cloned()
        .map(LaidOutBlock::Paragraph)
        .collect(),
      paragraph_to_block: (0..paragraphs.len()).collect(),
      block_to_paragraph: paragraphs
        .iter()
        .map(|paragraph| Some(paragraph.index))
        .collect(),
      paragraphs,
      bounds: Some(Bounds::new(point(bounds.origin.x, px(0.0)), self.scroll_handle.content_size())),
      size: self.scroll_handle.content_size(),
      width: layout.width,
      snap_underline_rules_to_pixels: layout.snap_underline_rules_to_pixels,
    }));
  }

  fn refresh_save_status(&mut self) {
    self.save_status = if self.has_unsaved_changes() {
      SaveStatus::Dirty
    } else {
      SaveStatus::Saved
    };
  }

  fn schedule_recovery_write(&mut self, cx: &mut Context<Self>) {
    let Some(path) = self.recovery_path.clone() else {
      return;
    };
    if !self.has_unsaved_changes() {
      return;
    }
    if self.last_recovery_generation == self.edit_generation {
      return;
    }
    if self.recovery_write_in_progress {
      self.recovery_write_pending = true;
      return;
    }

    self.recovery_write_in_progress = true;
    cx.spawn(async move |editor, cx| {
      Timer::after(Duration::from_millis(750)).await;
      let snapshot_timing = Instant::now();
      let snapshot = editor
        .update(cx, |editor, _| {
          editor.recovery_write_pending = false;
          if !editor.has_unsaved_changes() || editor.last_recovery_generation == editor.edit_generation {
            None
          } else {
            Some((editor.edit_generation, editor.document.clone()))
          }
        })
        .ok();
      log_timing("recovery snapshot", snapshot_timing, "");
      if let Some(Some((generation, document))) = snapshot {
        let write_timing = Instant::now();
        let paragraph_count = document.paragraphs.len();
        let write_result = cx
          .background_executor()
          .spawn(async move { write_db8(path, &document) })
          .await;
        log_timing("recovery write", write_timing, format!("paragraphs={paragraph_count}"));
        match write_result {
          Ok(()) => {
            let _ = editor.update(cx, |editor, _| {
              editor.last_recovery_generation = editor.last_recovery_generation.max(generation);
            });
          },
          Err(error) => {
            eprintln!("failed to write recovery file: {error}");
          },
        }
      }
      let _ = editor.update(cx, |editor, cx| {
        editor.recovery_write_in_progress = false;
        if editor.recovery_write_pending {
          editor.schedule_recovery_write(cx);
        }
      });
    })
    .detach();
  }

  fn move_to_offset(&mut self, new_head: DocumentOffset, extend: bool, cx: &mut Context<Self>) {
    let anchor = if extend { self.selection.anchor } else { new_head };
    let selection = EditorSelection { anchor, head: new_head };
    if self.selection == selection {
      self.goal_x = None;
      return;
    }
    self.selection = selection;
    self.goal_x = None;
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    cx.notify();
  }

  fn word_left(&self, offset: DocumentOffset) -> DocumentOffset {
    previous_debate_word_boundary_in_document(&self.document, offset)
  }

  fn word_right(&self, offset: DocumentOffset) -> DocumentOffset {
    next_debate_word_boundary_in_document(&self.document, offset)
  }

  fn page_move(&mut self, dir: VDir, extend: bool, cx: &mut Context<Self>) {
    let head = self.selection.head;
    let Some(layout) = self.layout_for_offset(head) else {
      return;
    };
    let Some(bounds) = layout.bounds else {
      return;
    };
    let delta = (bounds.size.height - px(40.0)).max(px(40.0));
    let signed_delta = match dir {
      VDir::Up => delta,
      VDir::Down => -delta,
    };
    let old_offset = self.scroll_handle.offset();
    let new_offset = clamp_scroll_offset(&self.scroll_handle, point(old_offset.x, old_offset.y + signed_delta));
    self.scroll_handle.set_offset(new_offset);

    let Some(caret) = caret_bounds(&layout, head, bounds.origin) else {
      cx.notify();
      return;
    };
    let target_y = match dir {
      VDir::Up => (caret.origin.y - delta).max(bounds.top()),
      VDir::Down => (caret.origin.y + delta).min(bounds.bottom()),
    };
    let target = self
      .hit_test_cached_position(point(caret.origin.x, target_y))
      .unwrap_or_else(|| layout.hit_test(point(caret.origin.x, target_y)));
    self.move_to_offset(target, extend, cx);
  }

  pub(super) fn after_text_mutation(&mut self, cx: &mut Context<Self>) {
    self.pending_styles = None;
    self.goal_x = None;
    self.invalidate_document_layout_caches();
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    cx.notify();
  }

  fn insert_rich_fragment(&mut self, fragment: RichClipboardFragment, cx: &mut Context<Self>) {
    if !fragment.blocks.is_empty() {
      self.insert_block_fragment(fragment, cx);
      return;
    }
    if fragment.paragraphs.is_empty() {
      return;
    }
    if !self.selection.is_caret() {
      self.delete_selection_internal();
    }
    let caret = insert_rich_fragment_at(&mut self.document, self.selection.head, &fragment);
    self.selection = EditorSelection { anchor: caret, head: caret };
    self.after_text_mutation(cx);
  }

  fn insert_block_fragment(&mut self, fragment: RichClipboardFragment, cx: &mut Context<Self>) {
    if fragment.blocks.is_empty() {
      return;
    }
    let before_document = self.document.clone();
    let before_selection = self.selection.clone();
    for asset in fragment.assets {
      self.document.assets.assets.insert(
        asset.id,
        AssetRecord {
          id: asset.id,
          mime_type: asset.mime_type.into(),
          original_name: asset.original_name.map(Into::into),
          content_hash: asset.content_hash,
          bytes: Arc::new(asset.bytes),
        },
      );
    }
    self.insert_ordered_block_fragment_after_caret(&fragment.blocks);
    self.push_replace_document_history(before_document, before_selection, cx);
  }

  fn insert_ordered_block_fragment_after_caret(&mut self, input_blocks: &[InputBlock]) {
    let insert_ix = self.prepare_block_insertion_index();
    let insert_paragraph_ix = self
      .document
      .blocks
      .iter()
      .take(insert_ix)
      .filter(|block| matches!(block, Block::Paragraph(_)))
      .count();
    let inserted_paragraph_inputs = input_blocks
      .iter()
      .filter_map(|block| match block {
        InputBlock::Paragraph(paragraph) => Some(paragraph.clone()),
        InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_) => None,
      })
      .collect::<Vec<_>>();
    let inserted_paragraphs = insert_standalone_paragraphs_into_projection(&mut self.document, insert_paragraph_ix, &inserted_paragraph_inputs);
    let mut inserted_paragraph_ix = 0;
    let inserted_blocks = input_blocks
      .iter()
      .map(|block| match block {
        InputBlock::Paragraph(_) => {
          let paragraph = inserted_paragraphs
            .get(inserted_paragraph_ix)
            .cloned()
            .unwrap_or_else(|| Paragraph {
              style: ParagraphStyle::Normal,
              byte_range: 0..0,
              runs: Vec::new(),
              version: 0,
            });
          inserted_paragraph_ix += 1;
          Block::Paragraph(paragraph)
        },
        InputBlock::Image(_) | InputBlock::Equation(_) | InputBlock::Table(_) => block_from_input_block(block),
      })
      .collect::<Vec<_>>();
    let old_blocks = self.document.blocks.as_ref().clone();
    let mut paragraph_ix = 0;
    let mut output = Vec::with_capacity(old_blocks.len() + inserted_blocks.len());
    for (block_ix, block) in old_blocks.iter().enumerate() {
      if block_ix == insert_ix {
        output.extend(inserted_blocks.iter().cloned());
      }
      match block {
        Block::Paragraph(_) => {
          if let Some(paragraph) = self.document.paragraphs.get(paragraph_ix) {
            output.push(Block::Paragraph(paragraph.clone()));
          }
          paragraph_ix += 1;
        },
        Block::Image(_) | Block::Equation(_) | Block::Table(_) => output.push(block.clone()),
      }
    }
    if insert_ix >= old_blocks.len() {
      output.extend(inserted_blocks);
    }
    self.document.blocks = Arc::new(output);
    self.selected_block = None;
    self.item_sizes_cache = None;
    self.last_layout = None;
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
  }

  fn insert_blocks_after_caret(&mut self, blocks: Vec<Block>, cx: &mut Context<Self>) {
    if blocks.is_empty() {
      return;
    }
    let before_document = self.document.clone();
    let before_selection = self.selection.clone();
    self.insert_blocks_after_caret_without_history(blocks);
    self.push_replace_document_history(before_document, before_selection, cx);
  }

  fn insert_blocks_after_caret_without_history(&mut self, blocks: Vec<Block>) {
    if blocks.is_empty() {
      return;
    }
    let insert_ix = self.prepare_block_insertion_index();
    Arc::make_mut(&mut self.document.blocks).splice(insert_ix..insert_ix, blocks);
    self.append_missing_paragraph_blocks();
    self.selected_block = None;
    self.item_sizes_cache = None;
    self.last_layout = None;
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
  }

  fn prepare_block_insertion_index(&mut self) -> usize {
    if let Some(
      BlockSelection::Image(block_ix)
      | BlockSelection::Equation(block_ix)
      | BlockSelection::Table(block_ix)
      | BlockSelection::TableCell { block_ix, .. },
    ) = self.selected_block
    {
      return (block_ix + 1).min(self.document.blocks.len());
    }

    if let Some(insert_ix) = self.remove_empty_caret_paragraph_for_block_insertion() {
      return insert_ix;
    }

    if !self.selection.is_caret() {
      let range = self.selection.normalized();
      let object_indices = self.object_block_indices_in_text_range(range);
      if !object_indices.is_empty() {
        let blocks = Arc::make_mut(&mut self.document.blocks);
        for block_ix in object_indices.into_iter().rev() {
          if block_ix < blocks.len() {
            blocks.remove(block_ix);
          }
        }
      }
      self.delete_selection_internal();
    }

    if let Some(position) = document_position_for_offset(&self.document, self.selection.head) {
      debug_assert_eq!(document_offset_for_position(&self.document, &position), Some(self.selection.head));
      if let DocumentPosition::Text { block_ix, .. } = position {
        return (block_ix + 1).min(self.document.blocks.len());
      }
    }
    self.document.blocks.len()
  }

  fn remove_empty_caret_paragraph_for_block_insertion(&mut self) -> Option<usize> {
    if !self.selection.is_caret() {
      return None;
    }
    let paragraph_ix = self.selection.head.paragraph;
    let paragraph = self.document.paragraphs.get(paragraph_ix)?;
    if self.selection.head.byte != 0 || paragraph_text_len(paragraph) != 0 {
      return None;
    }
    let block_ix = self.block_ix_for_paragraph(paragraph_ix)?;
    let paragraph_count = self.document.paragraphs.len();
    let blocks = Arc::make_mut(&mut self.document.blocks);
    if block_ix < blocks.len() {
      blocks.remove(block_ix);
    }

    if paragraph_count > 1 {
      let range = paragraph_byte_range(&self.document, paragraph_ix);
      if paragraph_ix + 1 < paragraph_count {
        self.document.text.delete(range.start..range.start + 1);
      } else if range.start > 0 {
        self.document.text.delete(range.start - 1..range.start);
      }
      paragraphs_mut(&mut self.document).remove(paragraph_ix);
      rebuild_document_offset_index(&mut self.document);
      let new_paragraph_ix = paragraph_ix.min(self.document.paragraphs.len().saturating_sub(1));
      self.selection = EditorSelection {
        anchor: DocumentOffset {
          paragraph: new_paragraph_ix,
          byte: 0,
        },
        head: DocumentOffset {
          paragraph: new_paragraph_ix,
          byte: 0,
        },
      };
    }
    Some(block_ix)
  }

  fn append_missing_paragraph_blocks(&mut self) {
    let existing = self
      .document
      .blocks
      .iter()
      .filter(|block| matches!(block, Block::Paragraph(_)))
      .count();
    if existing >= self.document.paragraphs.len() {
      return;
    }
    let blocks = Arc::make_mut(&mut self.document.blocks);
    for paragraph in self.document.paragraphs.iter().skip(existing) {
      blocks.push(Block::Paragraph(paragraph.clone()));
    }
  }

  fn push_replace_document_history(&mut self, before_document: Document, before_selection: EditorSelection, cx: &mut Context<Self>) {
    if before_document.text == self.document.text
      && before_document.paragraphs == self.document.paragraphs
      && before_document.blocks == self.document.blocks
      && before_document.assets == self.document.assets
    {
      return;
    }
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    self.undo_stack.push(EditRecord {
      before_selection,
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::ReplaceDocument {
        before: before_document,
        after: self.document.clone(),
      }],
    });
    self.redo_stack.clear();
    self.invalidate_document_layout_caches();
    self.mark_document_changed(after_generation, cx);
  }

  fn insert_plain_text_fragment(&mut self, text: &str, cx: &mut Context<Self>) {
    let normalized = text.replace("\r\n", "\n").replace('\r', "\n");
    if normalized.is_empty() {
      return;
    }
    let paragraph_style = self.document.paragraphs[self.selection.head.paragraph].style;
    let styles = self.styles_at_caret();
    let fragment = RichClipboardFragment {
      format: "debateprocessor.rich-text-fragment.v1".to_string(),
      paragraphs: normalized
        .split('\n')
        .map(|line| InputParagraph {
          style: paragraph_style,
          runs: if line.is_empty() {
            Vec::new()
          } else {
            vec![InputRun {
              text: line.to_string(),
              styles,
            }]
          },
        })
        .collect(),
      blocks: Vec::new(),
      assets: Vec::new(),
    };
    self.insert_rich_fragment(fragment, cx);
  }

  fn toggle_underline_kind(&mut self, explicit_direct: Option<bool>, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      let Some(selection_range) = self.table_cell_selection_range() else {
        return;
      };
      let paragraph_style = self
        .selected_table_cell_paragraph()
        .map(|paragraph| paragraph.paragraph.style)
        .unwrap_or(ParagraphStyle::Normal);
      let direct = explicit_direct.unwrap_or_else(|| matches!(paragraph_style, ParagraphStyle::Tag | ParagraphStyle::Analytic));
      let all_selected = self
        .selected_table_cell_paragraph()
        .map(|paragraph| {
          let range = selection_range.clone();
          !range.is_empty()
            && table_cell_range_all_run_styles(paragraph, range, |styles| {
              if direct {
                styles.direct_underline
              } else {
                styles.semantic == RunSemanticStyle::Underline
              }
            })
        })
        .unwrap_or(false);
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        if paragraph.text.is_empty() {
          return;
        }
        if paragraph.paragraph.runs.is_empty() {
          paragraph.paragraph.runs.push(TextRun {
            len: paragraph.text.len(),
            styles: RunStyles::default(),
          });
        }
        mutate_table_cell_runs_in_range(paragraph, selection_range.clone(), |styles| {
          if direct {
            styles.direct_underline = !all_selected;
          } else if all_selected {
            styles.semantic = RunSemanticStyle::Plain;
          } else {
            styles.semantic = RunSemanticStyle::Underline;
            styles.direct_underline = false;
          }
        });
      });
      return;
    }
    if self.selection.is_caret() {
      let paragraph_style = self.document.paragraphs[self.selection.head.paragraph].style;
      let direct = explicit_direct.unwrap_or_else(|| matches!(paragraph_style, ParagraphStyle::Tag | ParagraphStyle::Analytic));
      let mut styles = self.styles_at_caret();
      if direct {
        styles.direct_underline = !styles.direct_underline;
      } else {
        if styles.semantic == RunSemanticStyle::Underline {
          styles.semantic = RunSemanticStyle::Plain;
        } else {
          styles.semantic = RunSemanticStyle::Underline;
          styles.direct_underline = false;
        }
      }
      self.pending_styles = Some(styles);
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }

    let range = self.selection.normalized();
    let direct = explicit_direct.unwrap_or_else(|| selection_prefers_direct_underline(&self.document, range.clone()));
    let all_selected = selection_all_underline_kind(&self.document, range.clone(), direct);
    self.apply_document_edit(cx, |editor, cx| {
      mutate_runs_in_range(&mut editor.document, range, |styles| {
        if direct {
          styles.direct_underline = !all_selected;
        } else {
          let new_value = !all_selected;
          if new_value {
            styles.semantic = RunSemanticStyle::Underline;
            styles.direct_underline = false;
          } else {
            styles.semantic = RunSemanticStyle::Plain;
          }
        }
      });
      editor.after_text_mutation(cx);
    });
  }

  fn toggle_semantic_style(&mut self, semantic: RunSemanticStyle, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      let Some(selection_range) = self.table_cell_selection_range() else {
        return;
      };
      let all_selected = self
        .selected_table_cell_paragraph()
        .map(|paragraph| {
          let range = selection_range.clone();
          !range.is_empty() && table_cell_range_all_run_styles(paragraph, range, |styles| styles.semantic == semantic)
        })
        .unwrap_or(false);
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        if paragraph.text.is_empty() {
          return;
        }
        if paragraph.paragraph.runs.is_empty() {
          paragraph.paragraph.runs.push(TextRun {
            len: paragraph.text.len(),
            styles: RunStyles::default(),
          });
        }
        mutate_table_cell_runs_in_range(paragraph, selection_range.clone(), |styles| {
          styles.semantic = if all_selected { RunSemanticStyle::Plain } else { semantic };
        });
        paragraph.paragraph.runs = merge_adjacent_runs(std::mem::take(&mut paragraph.paragraph.runs));
        paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
      });
      return;
    }
    if self.selection.is_caret() {
      let mut styles = self.styles_at_caret();
      styles.semantic = if styles.semantic == semantic {
        RunSemanticStyle::Plain
      } else {
        semantic
      };
      self.pending_styles = Some(styles);
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }

    let range = self.selection.normalized();
    let all_selected = selection_all_run_styles(&self.document, range.clone(), |styles| styles.semantic == semantic);
    self.apply_document_edit(cx, |editor, cx| {
      mutate_runs_in_range(&mut editor.document, range, |styles| {
        styles.semantic = if all_selected { RunSemanticStyle::Plain } else { semantic };
      });
      editor.after_text_mutation(cx);
    });
  }

  fn set_highlight_internal(&mut self, highlight: Option<HighlightStyle>, cx: &mut Context<Self>) {
    if let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block {
      let Some(selection_range) = self.table_cell_selection_range() else {
        return;
      };
      let all_selected = self
        .selected_table_cell_paragraph()
        .and_then(|paragraph| {
          highlight.map(|highlight| {
            let range = selection_range.clone();
            !range.is_empty() && table_cell_range_all_run_styles(paragraph, range, |styles| styles.highlight == Some(highlight))
          })
        })
        .unwrap_or(false);
      let target_highlight = if all_selected { None } else { highlight };
      self.edit_table_cell_paragraph(block_ix, row_ix, cell_ix, cx, |paragraph| {
        if paragraph.text.is_empty() {
          return;
        }
        if paragraph.paragraph.runs.is_empty() {
          paragraph.paragraph.runs.push(TextRun {
            len: paragraph.text.len(),
            styles: RunStyles::default(),
          });
        }
        mutate_table_cell_runs_in_range(paragraph, selection_range.clone(), |styles| styles.highlight = target_highlight);
        paragraph.paragraph.runs = merge_adjacent_runs(std::mem::take(&mut paragraph.paragraph.runs));
        paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
      });
      return;
    }
    if self.selection.is_caret() {
      let mut styles = self.styles_at_caret();
      styles.highlight = highlight;
      self.pending_styles = Some(styles);
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }

    let range = self.selection.normalized();
    let all_selected = if let Some(highlight) = highlight {
      selection_all_run_styles(&self.document, range.clone(), |styles| styles.highlight == Some(highlight))
    } else {
      false
    };
    let target_highlight = if all_selected { None } else { highlight };
    self.apply_document_edit(cx, |editor, cx| {
      mutate_runs_in_range(&mut editor.document, range, |styles| styles.highlight = target_highlight);
      editor.after_text_mutation(cx);
    });
  }

  pub(super) fn styles_at_caret(&self) -> RunStyles {
    if let Some(styles) = self.pending_styles {
      return styles;
    }
    let caret = self.selection.head;
    let paragraph = &self.document.paragraphs[caret.paragraph];
    let (run_ix, _) = run_containing(paragraph, caret.byte);
    paragraph
      .runs
      .get(run_ix)
      .map(|run| run.styles)
      .unwrap_or_default()
  }

  // -------- Movement primitives ----------------------------------------

  fn move_horizontal(&mut self, dir: HDir, extend: bool, cx: &mut Context<Self>) {
    if matches!(self.selected_block, Some(BlockSelection::Equation(_))) {
      let source = self.selected_equation_source().unwrap_or_default();
      let caret = self.equation_source_caret.min(source.len());
      let next = match dir {
        HDir::Left if caret > 0 => source[..caret]
          .char_indices()
          .next_back()
          .map(|(byte, _)| byte)
          .unwrap_or(0),
        HDir::Left => 0,
        HDir::Right if caret < source.len() => source[caret..]
          .char_indices()
          .nth(1)
          .map(|(byte, _)| caret + byte)
          .unwrap_or(source.len()),
        HDir::Right => source.len(),
      };
      if extend {
        self.equation_source_caret = next;
      } else {
        self.equation_source_caret = next;
        self.equation_source_anchor = next;
      }
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }
    if !extend && matches!(self.selected_block, Some(BlockSelection::TableCell { .. })) {
      let text = self.selected_table_cell_text().unwrap_or_default();
      match dir {
        HDir::Left if self.table_cell_caret > 0 => {
          self.table_cell_caret = text[..self.table_cell_caret.min(text.len())]
            .char_indices()
            .next_back()
            .map(|(byte, _)| byte)
            .unwrap_or(0);
          cx.notify();
          return;
        },
        HDir::Left => {
          if let Some((paragraph_ix, len)) = self.adjacent_selected_table_cell_paragraph(false) {
            self.table_cell_block_ix = paragraph_ix;
            self.table_cell_caret = len;
            cx.notify();
            return;
          }
        },
        HDir::Right if self.table_cell_caret < text.len() => {
          let caret = self.table_cell_caret.min(text.len());
          self.table_cell_caret = text[caret..]
            .char_indices()
            .nth(1)
            .map(|(byte, _)| caret + byte)
            .unwrap_or(text.len());
          cx.notify();
          return;
        },
        HDir::Right => {
          if let Some((paragraph_ix, _)) = self.adjacent_selected_table_cell_paragraph(true) {
            self.table_cell_block_ix = paragraph_ix;
            self.table_cell_caret = 0;
            cx.notify();
            return;
          }
        },
      }
    }
    if !extend && self.selected_block.is_some() && self.collapse_object_selection(dir, cx) {
      return;
    }
    if !extend && self.selection.is_caret() {
      let head = self.selection.head;
      let object = match dir {
        HDir::Left if head.byte == 0 => self.immediate_object_before_paragraph(head.paragraph),
        HDir::Right if head.byte == paragraph_text_len(&self.document.paragraphs[head.paragraph]) => {
          self.immediate_object_after_paragraph(head.paragraph)
        },
        _ => None,
      };
      if let Some(object) = object {
        self.select_block(object, cx);
        return;
      }
    }
    let new_head = match dir {
      HDir::Left => {
        // Collapsing a selection leftwards jumps to its start without moving.
        if !extend && !self.selection.is_caret() {
          self.selection.normalized().start
        } else {
          self.step_left(self.selection.head)
        }
      },
      HDir::Right => {
        if !extend && !self.selection.is_caret() {
          self.selection.normalized().end
        } else {
          self.step_right(self.selection.head)
        }
      },
    };
    let anchor = if extend { self.selection.anchor } else { new_head };
    let selection = EditorSelection { anchor, head: new_head };
    if self.selection == selection {
      self.goal_x = None;
      return;
    }
    self.selection = selection;
    self.goal_x = None;
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    cx.notify();
  }

  fn step_left(&self, off: DocumentOffset) -> DocumentOffset {
    if off.byte == 0 {
      // At the start of a paragraph: hop to end of previous paragraph (or
      // stay if we're at the document start).
      if off.paragraph == 0 {
        return off;
      }
      let prev = off.paragraph - 1;
      let byte = paragraph_text_len(&self.document.paragraphs[prev]);
      return DocumentOffset { paragraph: prev, byte };
    }
    DocumentOffset {
      paragraph: off.paragraph,
      byte: prev_grapheme_boundary_in_paragraph(&self.document, off.paragraph, off.byte),
    }
  }

  fn step_right(&self, off: DocumentOffset) -> DocumentOffset {
    let len = paragraph_text_len(&self.document.paragraphs[off.paragraph]);
    if off.byte >= len {
      if off.paragraph + 1 >= self.document.paragraphs.len() {
        return off;
      }
      return DocumentOffset {
        paragraph: off.paragraph + 1,
        byte: 0,
      };
    }
    DocumentOffset {
      paragraph: off.paragraph,
      byte: next_grapheme_boundary_in_paragraph(&self.document, off.paragraph, off.byte),
    }
  }

  fn move_vertical(&mut self, dir: VDir, extend: bool, cx: &mut Context<Self>) {
    let head = self.selection.head;
    // Compute the new head while only reading layout snapshots. Use a local
    // scope so we can mutate selection afterwards without borrow conflicts.
    let (new_head, used_goal_x) = {
      let Some(layout) = self.layout_for_offset(head) else {
        return;
      };
      let Some((p_ix, l_ix)) = locate_line(&layout, head) else {
        // The caret can briefly point at a paragraph that the virtual list has
        // not mounted yet. Keep the paragraph moving into view so the next
        // layout frame can restore normal visual-line navigation.
        self.scroll_head_paragraph_into_view(dir);
        cx.notify();
        return;
      };
      let cur_line = &layout.paragraphs[p_ix].lines[l_ix];
      let cur_x = self
        .goal_x
        .unwrap_or_else(|| x_for_byte(cur_line, head.byte));
      let next = match dir {
        VDir::Up => find_line_above(&layout, p_ix, l_ix),
        VDir::Down => find_line_below(&layout, p_ix, l_ix),
      };
      let Some((np, nl)) = next else {
        return self.move_to_adjacent_unmounted_paragraph(dir, extend, cur_x, cx);
      };
      let target_line = &layout.paragraphs[np].lines[nl];
      let new_byte = target_line.hit_test_x(cur_x);
      let new_head = DocumentOffset {
        paragraph: layout.paragraphs[np].index,
        byte: new_byte,
      };
      (new_head, cur_x)
    };
    let anchor = if extend { self.selection.anchor } else { new_head };
    let selection = EditorSelection { anchor, head: new_head };
    if self.selection == selection {
      self.goal_x = Some(used_goal_x);
      return;
    }
    self.selection = selection;
    // Preserve the goal x across the move so repeated Up/Down stays on a
    // straight column.
    self.goal_x = Some(used_goal_x);
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    cx.notify();
  }

  fn move_to_adjacent_unmounted_paragraph(&mut self, dir: VDir, extend: bool, goal_x: Pixels, cx: &mut Context<Self>) {
    let head = self.selection.head;
    let Some(target_paragraph) = self.adjacent_document_paragraph(head.paragraph, dir) else {
      return;
    };
    let target_byte = match self.layout_for_offset(DocumentOffset {
      paragraph: target_paragraph,
      byte: 0,
    }) {
      Some(layout) => {
        let Some(paragraph) = paragraph_layout(&layout, target_paragraph) else {
          return;
        };
        let line = match dir {
          VDir::Up => paragraph.lines.last(),
          VDir::Down => paragraph.lines.first(),
        };
        line
          .map(|line| line.hit_test_x(goal_x))
          .unwrap_or_else(|| match dir {
            VDir::Up => paragraph_text_len(&self.document.paragraphs[target_paragraph]),
            VDir::Down => 0,
          })
      },
      None => match dir {
        VDir::Up => paragraph_text_len(&self.document.paragraphs[target_paragraph]),
        VDir::Down => 0,
      },
    };
    let new_head = DocumentOffset {
      paragraph: target_paragraph,
      byte: target_byte,
    };
    let anchor = if extend { self.selection.anchor } else { new_head };
    self.selection = EditorSelection { anchor, head: new_head };
    self.goal_x = Some(goal_x);
    self.scroll_paragraph_into_view(target_paragraph, dir);
    self.reset_caret_blink(cx);
    cx.notify();
  }

  fn scroll_head_paragraph_into_view(&self, dir: VDir) {
    self.scroll_paragraph_into_view(self.selection.head.paragraph, dir);
  }

  fn scroll_paragraph_into_view(&self, paragraph_ix: usize, dir: VDir) {
    if paragraph_ix >= self.document.paragraphs.len() {
      return;
    }
    let strategy = match dir {
      VDir::Up => ScrollStrategy::Bottom,
      VDir::Down => ScrollStrategy::Top,
    };
    self.scroll_handle.scroll_to_item(paragraph_ix, strategy);
  }

  fn adjacent_document_paragraph(&self, paragraph_ix: usize, dir: VDir) -> Option<usize> {
    match dir {
      VDir::Up => paragraph_ix.checked_sub(1),
      VDir::Down => (paragraph_ix + 1 < self.document.paragraphs.len()).then_some(paragraph_ix + 1),
    }
  }

  fn hit_test_document_position(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) -> DocumentOffset {
    if let Some(layout) = self.last_layout.as_ref()
      && let (Some(first), Some(last)) = (layout.paragraphs.first(), layout.paragraphs.last())
      && position.y >= first.top
      && position.y <= last.bottom
    {
      return layout.hit_test(position);
    }

    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 {
      return DocumentOffset::default();
    }
    let viewport = self.scroll_handle.bounds();
    let width = if viewport.size.width > px(1.0) { viewport.size.width } else { px(900.0) };
    self.ensure_exact_interaction_paragraph_heights(width, window, cx);
    let content_y = (position.y - viewport.top() - self.scroll_handle.offset().y).max(px(0.0));
    let paragraph_ix = if self.height_prefix_index.len() == self.document.blocks.len() {
      let block_ix = self.height_prefix_index.lower_bound(content_y);
      self
        .paragraph_ix_for_block(block_ix)
        .unwrap_or(self.selection.head.paragraph.min(paragraph_count - 1))
    } else {
      self.selection.head.paragraph.min(paragraph_count - 1)
    };
    if let Some(layout) = self.document_layout_for_paragraph(paragraph_ix, width) {
      return layout.hit_test(position);
    }
    let layout = Rc::new(build_single_paragraph_layout(&self.document, paragraph_ix, width, None, window, cx));
    self.cache_paragraph_layout(paragraph_ix, width, layout.clone());
    let mut layout = layout.as_ref().clone();
    let row_top = if self.height_prefix_index.len() == self.document.blocks.len() {
      self
        .block_ix_for_paragraph(paragraph_ix)
        .map(|block_ix| self.height_prefix_index.item_top(block_ix))
        .unwrap_or(px(0.0))
    } else {
      px(0.0)
    };
    layout.bounds = Some(Bounds::new(
      point(viewport.left(), viewport.top() + self.scroll_handle.offset().y + row_top),
      size(width, layout.size.height),
    ));
    layout.hit_test(position)
  }

  // Home / End: jump to the start or end of the current visual (wrapped) line.
  // We resolve which `LaidOutLine` the caret sits on, then snap to its byte
  // range endpoints. This is why Home/End work correctly across soft wraps
  // without any renderer changes.
  fn move_line_edge(&mut self, start: bool, extend: bool, cx: &mut Context<Self>) {
    let head = self.selection.head;
    let new_byte = {
      let Some(layout) = self.layout_for_offset(head) else {
        return;
      };
      let Some((p_ix, l_ix)) = locate_line(&layout, head) else {
        return;
      };
      let line = &layout.paragraphs[p_ix].lines[l_ix];
      if start { line.start_byte } else { line.end_byte }
    };
    let new = DocumentOffset {
      paragraph: head.paragraph,
      byte: new_byte,
    };
    let anchor = if extend { self.selection.anchor } else { new };
    let selection = EditorSelection { anchor, head: new };
    if self.selection == selection {
      self.goal_x = None;
      return;
    }
    self.selection = selection;
    self.goal_x = None;
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    cx.notify();
  }

  // -------- Edit primitives --------------------------------------------

  fn insert_text(&mut self, text: &str, cx: &mut Context<Self>) {
    if text.is_empty() {
      return;
    }
    if !self.selection.is_caret() {
      self.delete_selection_internal();
    }
    let caret = self.selection.head;
    // Inherit styles from the run that contains the caret. With left-bias at
    // run boundaries this matches Word's "type continues the previous run's
    // styling" behavior.
    let styles = if let Some(styles) = self.pending_styles {
      styles
    } else {
      let paragraph = &self.document.paragraphs[caret.paragraph];
      let (run_ix, _) = run_containing(paragraph, caret.byte);
      paragraph
        .runs
        .get(run_ix)
        .map(|r| r.styles)
        .unwrap_or_default()
    };
    insert_text_at(&mut self.document, caret.paragraph, caret.byte, text, styles);
    let new = DocumentOffset {
      paragraph: caret.paragraph,
      byte: caret.byte + text.len(),
    };
    self.selection = EditorSelection { anchor: new, head: new };
    self.after_text_mutation(cx);
  }

  // Helper for shared selection-deletion logic. Does NOT call `cx.notify()`.
  fn delete_selection_internal(&mut self) -> bool {
    if self.selection.is_caret() {
      return false;
    }
    let range = self.selection.normalized();
    if range.start.paragraph == range.end.paragraph {
      delete_range_in_paragraph(&mut self.document, range.start.paragraph, range.start.byte..range.end.byte);
    } else {
      // Cross-paragraph selection: delete the tail of the start paragraph,
      // the head of the end paragraph, then merge the end paragraph's
      // remaining runs onto the end of the start paragraph. Intermediate
      // paragraphs are dropped wholesale.
      delete_cross_paragraph_range(&mut self.document, range.clone());
    }
    self.selection = EditorSelection {
      anchor: range.start,
      head: range.start,
    };
    true
  }

  fn backspace(&mut self, cx: &mut Context<Self>) {
    if self.backspace_selected_table_cell(cx) {
      return;
    }
    if self.backspace_selected_equation(cx) {
      return;
    }
    if self.delete_selected_block(cx) {
      return;
    }
    if !self.selection.is_caret() {
      self.delete_selection_internal();
      self.after_text_mutation(cx);
      return;
    }
    let caret = self.selection.head;
    if caret.byte == 0 {
      if let Some(object) = self.immediate_object_before_paragraph(caret.paragraph) {
        self.select_block(object, cx);
        return;
      }
      // Joining backwards: merge this paragraph onto the previous one. The
      // caret lands at the join seam.
      if caret.paragraph == 0 {
        return;
      }
      let prev_ix = caret.paragraph - 1;
      let prev_len = paragraph_text_len(&self.document.paragraphs[prev_ix]);
      delete_cross_paragraph_range(
        &mut self.document,
        DocumentOffset {
          paragraph: prev_ix,
          byte: prev_len,
        }..caret,
      );
      let new = DocumentOffset {
        paragraph: prev_ix,
        byte: prev_len,
      };
      self.selection = EditorSelection { anchor: new, head: new };
    } else {
      let prev = prev_grapheme_boundary_in_paragraph(&self.document, caret.paragraph, caret.byte);
      delete_range_in_paragraph(&mut self.document, caret.paragraph, prev..caret.byte);
      let new = DocumentOffset {
        paragraph: caret.paragraph,
        byte: prev,
      };
      self.selection = EditorSelection { anchor: new, head: new };
    }
    self.after_text_mutation(cx);
  }

  fn delete_forward(&mut self, cx: &mut Context<Self>) {
    if self.delete_forward_selected_table_cell(cx) {
      return;
    }
    if self.delete_selected_block(cx) {
      return;
    }
    if !self.selection.is_caret() {
      self.delete_selection_internal();
      self.after_text_mutation(cx);
      return;
    }
    let caret = self.selection.head;
    let para_len = paragraph_text_len(&self.document.paragraphs[caret.paragraph]);
    if caret.byte == para_len {
      if let Some(object) = self.immediate_object_after_paragraph(caret.paragraph) {
        self.select_block(object, cx);
        return;
      }
      // Joining forwards: pull the next paragraph's runs onto this one.
      if caret.paragraph + 1 >= self.document.paragraphs.len() {
        return;
      }
      delete_cross_paragraph_range(
        &mut self.document,
        caret..DocumentOffset {
          paragraph: caret.paragraph + 1,
          byte: 0,
        },
      );
    } else {
      let next = next_grapheme_boundary_in_paragraph(&self.document, caret.paragraph, caret.byte);
      delete_range_in_paragraph(&mut self.document, caret.paragraph, caret.byte..next);
    }
    self.after_text_mutation(cx);
  }

  fn insert_paragraph_break(&mut self, cx: &mut Context<Self>) {
    if !self.selection.is_caret() {
      self.delete_selection_internal();
    }
    let caret = self.selection.head;
    split_paragraph_at(&mut self.document, caret.paragraph, caret.byte);
    let new = DocumentOffset {
      paragraph: caret.paragraph + 1,
      byte: 0,
    };
    self.selection = EditorSelection { anchor: new, head: new };
    self.after_text_mutation(cx);
  }

  fn on_mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
    window.focus(&self.focus_handle);
    self.image_resize_drag = None;
    self.table_column_resize_drag = None;
    self.clear_block_selection();
    self.last_drag_position = Some(event.position);
    self.goal_x = None;
    let offset = self.hit_test_document_position(event.position, window, cx);
    self.drag_anchor = None;
    self.smart_selection_left_anchor_word = false;
    self.smart_selection_exact_override = false;
    if event.click_count <= 1 && !event.modifiers.shift && !self.selection.is_caret() && offset_in_range(offset, self.selection.normalized()) {
      self.selecting = false;
      self.pending_text_drag = Some(PendingTextDrag {
        start_position: event.position,
        source_selection: self.selection.clone(),
      });
      self.active_text_drag = None;
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }
    self.pending_text_drag = None;
    self.active_text_drag = None;
    self.selecting = true;
    self.drag_granularity = match event.click_count {
      0 | 1 => SelectionGranularity::Character,
      2 => SelectionGranularity::Word,
      _ => SelectionGranularity::Paragraph,
    };
    self.selection = match self.drag_granularity {
      SelectionGranularity::Character if event.modifiers.shift => EditorSelection {
        anchor: self.selection.anchor,
        head: offset,
      },
      SelectionGranularity::Character => EditorSelection {
        anchor: offset,
        head: offset,
      },
      SelectionGranularity::Word => selection_for_word_at(&self.document, offset),
      SelectionGranularity::Paragraph => selection_for_paragraph_at(&self.document, offset.paragraph),
    };
    self.drag_anchor = Some(self.selection.anchor);
    self.reset_caret_blink(cx);
    cx.notify();
  }

  fn on_mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
    if self.update_table_column_resize_drag(event.position, cx) {
      return;
    }
    if self.update_image_resize_drag(event.position, cx) {
      return;
    }
    if event.dragging()
      && let Some(BlockSelection::TableCell { block_ix, row_ix, cell_ix }) = self.selected_block
      && let Some((
        BlockSelection::TableCell {
          row_ix: hit_row,
          cell_ix: hit_cell,
          ..
        },
        paragraph_ix,
        byte,
      )) = self.table_cell_selection_at(block_ix, event.position, window, cx)
      && row_ix == hit_row
      && cell_ix == hit_cell
    {
      self.table_cell_block_ix = paragraph_ix;
      self.table_cell_caret = byte;
      self.last_drag_position = Some(event.position);
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }
    if let Some(pending_drag) = self.pending_text_drag.clone() {
      self.last_drag_position = Some(event.position);
      if point_distance_squared(pending_drag.start_position, event.position) < 16.0 {
        return;
      }
      let source_range = pending_drag.source_selection.normalized();
      self.active_text_drag = Some(ActiveTextDrag {
        source_range: source_range.clone(),
        fragment: selected_rich_fragment(&self.document, source_range),
      });
      self.selection = pending_drag.source_selection;
      self.pending_text_drag = None;
    }
    if self.active_text_drag.is_some() {
      self.last_drag_position = Some(event.position);
      self.autoscroll_for_drag(event.position);
      self.ensure_drag_autoscroll_task(cx);
      let drop = self.hit_test_document_position(event.position, window, cx);
      let selection = EditorSelection { anchor: drop, head: drop };
      if self.selection != selection {
        self.selection = selection;
        self.scroll_head_into_view();
        self.reset_caret_blink(cx);
      }
      cx.notify();
      return;
    }
    if !self.selecting {
      return;
    }
    self.last_drag_position = Some(event.position);
    self.autoscroll_for_drag(event.position);
    self.ensure_drag_autoscroll_task(cx);
    let head = self.hit_test_document_position(event.position, window, cx);
    let anchor = self.drag_anchor.unwrap_or(self.selection.anchor);
    if self.config.smart_word_selection && self.drag_granularity == SelectionGranularity::Character && !event.modifiers.alt {
      if !offset_is_in_same_word_as(&self.document, anchor, head) {
        self.smart_selection_left_anchor_word = true;
      } else if self.smart_selection_left_anchor_word {
        self.smart_selection_exact_override = true;
      }
    }
    let selection = expand_mouse_selection(
      &self.document,
      anchor,
      head,
      self.drag_granularity,
      MouseSelectionOptions {
        smart_word_selection: self.config.smart_word_selection,
        exact: event.modifiers.alt || self.smart_selection_exact_override,
      },
    );
    if self.selection != selection {
      self.selection = selection;
      self.scroll_head_into_view();
      self.reset_caret_blink(cx);
      cx.notify();
    } else {
      cx.notify();
    }
  }

  fn on_mouse_up(&mut self, event: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
    if self.finish_table_column_resize_drag(cx) {
      self.selecting = false;
      self.drag_granularity = SelectionGranularity::Character;
      self.drag_anchor = None;
      self.smart_selection_left_anchor_word = false;
      self.smart_selection_exact_override = false;
      self.last_drag_position = None;
      self.autoscroll_active = false;
      return;
    }
    if self.finish_image_resize_drag(cx) {
      self.selecting = false;
      self.drag_granularity = SelectionGranularity::Character;
      self.drag_anchor = None;
      self.smart_selection_left_anchor_word = false;
      self.smart_selection_exact_override = false;
      self.last_drag_position = None;
      self.autoscroll_active = false;
      return;
    }
    if let Some(active_drag) = self.active_text_drag.take() {
      let drop = self.hit_test_document_position(event.position, window, cx);
      self.move_rich_text_fragment(active_drag, drop, cx);
    } else if self.pending_text_drag.take().is_some() {
      let caret = self.hit_test_document_position(event.position, window, cx);
      self.selection = EditorSelection { anchor: caret, head: caret };
      self.scroll_head_into_view();
      self.reset_caret_blink(cx);
      cx.notify();
    }
    if self.selecting {
      self.apply_armed_inline_tool_to_selection(cx);
    }
    self.selecting = false;
    self.drag_granularity = SelectionGranularity::Character;
    self.drag_anchor = None;
    self.smart_selection_left_anchor_word = false;
    self.smart_selection_exact_override = false;
    self.last_drag_position = None;
    self.autoscroll_active = false;
  }

  fn move_rich_text_fragment(&mut self, drag: ActiveTextDrag, drop: DocumentOffset, cx: &mut Context<Self>) {
    if offset_in_range(drop, drag.source_range.clone()) {
      self.selection = EditorSelection {
        anchor: drag.source_range.start,
        head: drag.source_range.end,
      };
      cx.notify();
      return;
    }
    let before_selection = EditorSelection {
      anchor: drag.source_range.start,
      head: drag.source_range.end,
    };
    let before_generation = self.edit_generation;
    let after_generation = self.next_edit_generation;
    self.next_edit_generation = self.next_edit_generation.wrapping_add(1);
    let source_range = drag.source_range.clone();
    let adjusted_drop = adjust_drop_after_source_delete(drop, source_range.clone());
    self.selection = before_selection.clone();
    self.delete_selection_internal();
    let inserted_start = adjusted_drop;
    let inserted_end = insert_rich_fragment_at(&mut self.document, inserted_start, &drag.fragment);
    self.selection = EditorSelection {
      anchor: inserted_end,
      head: inserted_end,
    };
    self.undo_stack.push(EditRecord {
      before_selection,
      before_generation,
      after_selection: self.selection.clone(),
      after_generation,
      operations: vec![EditOperation::MoveRichText {
        source_range,
        adjusted_drop,
        inserted_range: inserted_start..inserted_end,
        fragment: drag.fragment,
      }],
    });
    self.redo_stack.clear();
    self.after_text_mutation(cx);
    self.mark_document_changed(after_generation, cx);
  }

  pub(super) fn reset_caret_blink(&mut self, cx: &mut Context<Self>) {
    self.caret_visible = true;
    self.ensure_caret_blink_task(cx);
  }

  fn ensure_caret_blink_task(&mut self, cx: &mut Context<Self>) {
    if self.caret_blink_active {
      return;
    }
    self.caret_blink_active = true;
    cx.spawn(async move |editor, cx| {
      loop {
        Timer::after(Duration::from_millis(530)).await;
        let keep_running = editor
          .update(cx, |editor, cx| {
            if !editor.caret_blink_active {
              return false;
            }
            editor.caret_visible = !editor.caret_visible;
            cx.notify();
            true
          })
          .unwrap_or(false);
        if !keep_running {
          break;
        }
      }
    })
    .detach();
  }

  fn ensure_focus_subscriptions(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if !self.focus_subscriptions.is_empty() {
      return;
    }
    let focus_handle = self.focus_handle.clone();
    self
      .focus_subscriptions
      .push(cx.on_focus(&focus_handle, window, |editor, _, cx| {
        editor.reset_caret_blink(cx);
        cx.notify();
      }));
    let focus_handle = self.focus_handle.clone();
    self
      .focus_subscriptions
      .push(cx.on_blur(&focus_handle, window, |editor, _, cx| {
        editor.caret_blink_active = false;
        editor.caret_visible = false;
        cx.notify();
      }));
  }

  fn scroll_head_into_view(&self) {
    let Some(layout) = self.layout_for_offset(self.selection.head) else {
      return;
    };
    let Some(bounds) = layout.bounds else {
      return;
    };
    let Some(caret) = caret_bounds(&layout, self.selection.head, bounds.origin) else {
      return;
    };
    scroll_rect_into_view(&self.scroll_handle, caret, px(4.0));
  }

  fn autoscroll_for_drag(&self, position: Point<Pixels>) -> bool {
    let viewport = self.scroll_handle.bounds();
    let step = drag_autoscroll_step(viewport, position);
    step != px(0.0) && scroll_by(&self.scroll_handle, step)
  }

  fn ensure_drag_autoscroll_task(&mut self, cx: &mut Context<Self>) {
    if self.autoscroll_active || !self.selecting {
      return;
    }
    let Some(position) = self.last_drag_position else {
      return;
    };
    if drag_autoscroll_step(self.scroll_handle.bounds(), position) == px(0.0) {
      return;
    }

    self.autoscroll_active = true;
    cx.spawn(async move |editor, cx| {
      loop {
        Timer::after(Duration::from_millis(16)).await;
        let keep_running = editor
          .update(cx, |editor, cx| {
            let Some(position) = editor.last_drag_position else {
              editor.autoscroll_active = false;
              return false;
            };
            if !editor.selecting {
              editor.autoscroll_active = false;
              return false;
            }

            if !editor.autoscroll_for_drag(position) {
              editor.autoscroll_active = false;
              return false;
            }

            if let Some(layout) = &editor.last_layout {
              let head = layout.hit_test(position);
              if editor.selection.head != head {
                editor.selection.head = head;
              }
            }
            cx.notify();
            true
          })
          .unwrap_or(false);
        if !keep_running {
          break;
        }
      }
    })
    .detach();
  }
}

impl Focusable for RichTextEditor {
  fn focus_handle(&self, _: &App) -> FocusHandle {
    self.focus_handle.clone()
  }
}

impl Render for RichTextEditor {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    self.ensure_focus_subscriptions(window, cx);
    if self.image_resize_drag.is_some() {
      let editor = cx.entity();
      window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
        if phase.bubble() {
          editor.update(cx, |editor, cx| {
            editor.finish_image_resize_drag(cx);
          });
        }
      });
    }
    let hide_until_viewport_measured = self.scroll_handle.bounds().size.width <= px(1.0);
    let item_sizes = self.paragraph_item_sizes(window, cx);
    let has_startup_layout_width = self.measured_item_width.is_some() || self.document.paragraphs.is_empty();
    if !hide_until_viewport_measured && self.initial_layout_hidden && has_startup_layout_width {
      // The VirtualList row positions and the paragraph layouts now agree on
      // width, so the first visible frame can use the same geometry that later
      // interaction frames use.
      self.initial_layout_hidden = false;
    }
    let hide_initial_layout = hide_until_viewport_measured || self.initial_layout_hidden;
    self.apply_pending_paragraph_snap(cx);
    let scroll_handle = self.scroll_handle.clone();
    let render_item_sizes = item_sizes.clone();
    div()
      .size_full()
      .id("rich-text-editor")
      .relative()
      .bg(rgb(0xffffff))
      .track_focus(&self.focus_handle(cx))
      .key_context("RichTextEditor")
      .cursor(CursorStyle::IBeam)
      // Action handlers — these resolve via the keymap registered in main.rs
      // because the focused element's context matches "RichTextEditor".
      .on_action(cx.listener(Self::on_move_left))
      .on_action(cx.listener(Self::on_move_right))
      .on_action(cx.listener(Self::on_move_up))
      .on_action(cx.listener(Self::on_move_down))
      .on_action(cx.listener(Self::on_move_line_start))
      .on_action(cx.listener(Self::on_move_line_end))
      .on_action(cx.listener(Self::on_select_left))
      .on_action(cx.listener(Self::on_select_right))
      .on_action(cx.listener(Self::on_select_up))
      .on_action(cx.listener(Self::on_select_down))
      .on_action(cx.listener(Self::on_select_line_start))
      .on_action(cx.listener(Self::on_select_line_end))
      .on_action(cx.listener(Self::on_select_all))
      .on_action(cx.listener(Self::on_move_word_left))
      .on_action(cx.listener(Self::on_move_word_right))
      .on_action(cx.listener(Self::on_select_word_left))
      .on_action(cx.listener(Self::on_select_word_right))
      .on_action(cx.listener(Self::on_delete_word_backward))
      .on_action(cx.listener(Self::on_delete_word_forward))
      .on_action(cx.listener(Self::on_page_up))
      .on_action(cx.listener(Self::on_page_down))
      .on_action(cx.listener(Self::on_select_page_up))
      .on_action(cx.listener(Self::on_select_page_down))
      .on_action(cx.listener(Self::on_move_document_start))
      .on_action(cx.listener(Self::on_move_document_end))
      .on_action(cx.listener(Self::on_select_document_start))
      .on_action(cx.listener(Self::on_select_document_end))
      .on_action(cx.listener(Self::on_copy))
      .on_action(cx.listener(Self::on_cut))
      .on_action(cx.listener(Self::on_paste))
      .on_action(cx.listener(Self::on_save))
      .on_action(cx.listener(Self::on_undo))
      .on_action(cx.listener(Self::on_redo))
      .on_action(cx.listener(Self::on_set_paragraph_pocket))
      .on_action(cx.listener(Self::on_set_paragraph_hat))
      .on_action(cx.listener(Self::on_set_paragraph_block))
      .on_action(cx.listener(Self::on_set_paragraph_tag))
      .on_action(cx.listener(Self::on_set_paragraph_analytic))
      .on_action(cx.listener(Self::on_toggle_cite))
      .on_action(cx.listener(Self::on_toggle_underline))
      .on_action(cx.listener(Self::on_toggle_emphasis))
      .on_action(cx.listener(Self::on_set_highlight_spoken))
      .on_action(cx.listener(Self::on_clear_formatting))
      .on_action(cx.listener(Self::on_clear_highlight))
      .on_action(cx.listener(Self::on_insert_image))
      .on_action(cx.listener(Self::on_insert_table))
      .on_action(cx.listener(Self::on_insert_equation))
      .on_action(cx.listener(Self::on_backspace))
      .on_action(cx.listener(Self::on_delete))
      .on_action(cx.listener(Self::on_insert_newline))
      .on_action(cx.listener(Self::on_insert_soft_line_break))
      // Catch printable characters (anything with a `key_char`) and insert
      // them as text. Action keys (arrows, Enter, etc.) have `key_char = None`
      // so they fall through to the action system above.
      .on_key_down(cx.listener(Self::on_key_down_event))
      .on_mouse_down(MouseButton::Left, cx.listener(Self::on_mouse_down))
      .on_mouse_move(cx.listener(Self::on_mouse_move))
      .on_mouse_up(MouseButton::Left, cx.listener(Self::on_mouse_up))
      .on_mouse_up_out(MouseButton::Left, cx.listener(Self::on_mouse_up))
      .drag_over::<ExternalPaths>(|style, _, _, _| style)
      .on_drop(cx.listener(Self::on_file_drop))
      .child(
        v_virtual_list(cx.entity(), "rich-text-virtual-document", item_sizes, move |editor, range, _window, cx| {
          let generation = editor.begin_visible_layout(range.clone());
          range
            .map(|block_ix| {
              if let Some(paragraph_ix) = editor.paragraph_ix_for_block(block_ix) {
                VirtualParagraphElement {
                  editor: cx.entity(),
                  paragraph_ix,
                  generation,
                  layout: WordElementLayout::default(),
                }
                .into_any_element()
              } else {
                let editor_entity = cx.entity();
                let selection = match editor.document.blocks.get(block_ix) {
                  Some(Block::Image(_)) => Some(BlockSelection::Image(block_ix)),
                  Some(Block::Equation(_)) => Some(BlockSelection::Equation(block_ix)),
                  Some(Block::Table(_)) => Some(BlockSelection::Table(block_ix)),
                  Some(Block::Paragraph(_)) | None => None,
                };
                let editor_for_down = editor_entity.clone();
                div()
                  .size_full()
                  .on_mouse_down(MouseButton::Left, move |event, window, cx| {
                    cx.stop_propagation();
                    editor_for_down.update(cx, |editor, cx| {
                      if editor.start_table_column_resize_if_hit(block_ix, event.position, window, cx) {
                        return;
                      }
                      if let Some(selection) = editor.selection_for_object_block(block_ix) {
                        editor.select_block_from_click(block_ix, selection, event.position, window, cx);
                      }
                    });
                  })
                  .when_some(selection, |this, selection| {
                    let editor_entity = editor_entity.clone();
                    this.on_mouse_up(MouseButton::Left, move |event, window, cx| {
                      cx.stop_propagation();
                      editor_entity.update(cx, |editor, cx| {
                        if editor.finish_table_column_resize_drag(cx) {
                          return;
                        }
                        if !matches!(editor.selected_block, Some(BlockSelection::TableCell { .. })) {
                          editor.select_block_from_click(block_ix, selection, event.position, window, cx);
                        }
                      });
                    })
                  })
                  .child(match editor.document.blocks.get(block_ix) {
                    Some(Block::Image(image)) => render_image_block(
                      &editor.document,
                      image,
                      block_ix,
                      render_item_sizes.get(block_ix).copied().unwrap_or_else(|| size(px(900.0), px(1.0))),
                      editor.selected_block,
                      editor_entity.clone(),
                    ),
                    Some(Block::Equation(equation)) => render_equation_block(
                      &editor.document,
                      equation,
                      block_ix,
                      render_item_sizes.get(block_ix).copied().unwrap_or_else(|| size(px(900.0), px(1.0))),
                      editor.selected_block == Some(BlockSelection::Equation(block_ix)) || editor.block_is_inside_text_selection(block_ix),
                      editor.equation_source_selection_for_render(block_ix),
                    ),
                    Some(Block::Table(_)) | Some(Block::Paragraph(_)) | None => VirtualBlockElement {
                      editor: editor_entity,
                      block_ix,
                      layout: WordElementLayout::default(),
                    }
                    .into_any_element(),
                  })
                  .into_any_element()
              }
            })
            .collect::<Vec<_>>()
        })
        .track_scroll(&scroll_handle)
        .when(hide_initial_layout, |this| this.opacity(0.0)),
      )
      .child(
        div()
          .absolute()
          .top_0()
          .left_0()
          .right_0()
          .bottom_0()
          .child(Scrollbar::vertical(&self.scroll_handle).scrollbar_show(ScrollbarShow::Always)),
      )
  }
}

#[cfg(target_os = "windows")]
fn windows_apply_capslock(text: &str) -> String {
  // GPUI 0.2.2's Windows key_char generation does not include Caps Lock in
  // the ToUnicode keyboard state. For normal letter input, Caps Lock inverts
  // the Shift-produced case; non-letter keys should pass through unchanged.
  let mut chars = text.chars();
  let Some(ch) = chars.next() else {
    return String::new();
  };
  if chars.next().is_none() && ch.is_ascii_alphabetic() {
    if ch.is_ascii_lowercase() {
      ch.to_ascii_uppercase().to_string()
    } else {
      ch.to_ascii_lowercase().to_string()
    }
  } else {
    text.to_string()
  }
}

fn expand_paragraph_range(range: Range<usize>, paragraph_count: usize, padding: usize) -> Range<usize> {
  if paragraph_count == 0 {
    return 0..0;
  }
  let start = range.start.saturating_sub(padding).min(paragraph_count);
  let end = range
    .end
    .saturating_add(padding)
    .min(paragraph_count)
    .max(start);
  start..end
}

fn default_table_row(columns: usize) -> TableRow {
  TableRow {
    cells: (0..columns).map(|_| default_table_cell()).collect(),
  }
}

fn default_table_cell() -> TableCell {
  TableCell {
    blocks: vec![TableCellBlock::Paragraph(default_table_cell_paragraph())],
    row_span: 1,
    col_span: 1,
  }
}

fn table_column_count(table: &TableBlock) -> usize {
  table
    .rows
    .iter()
    .map(|row| {
      row
        .cells
        .iter()
        .map(|cell| cell.col_span.max(1) as usize)
        .sum::<usize>()
    })
    .max()
    .unwrap_or(0)
    .max(table.column_widths.len())
}

fn fixed_table_column_widths_from_layout(table: &TableBlock, layout: &LaidOutTable) -> Vec<u32> {
  let column_count = table_column_count(table).max(1);
  let mut widths = vec![120; column_count];
  for (ix, width) in table.column_widths.iter().enumerate() {
    if ix < widths.len() {
      if let TableColumnWidth::FixedPx(width) = width {
        widths[ix] = *width;
      }
    }
  }
  let Some(first_layout_row) = layout.rows.first() else {
    return widths;
  };
  let Some(first_data_row) = table.rows.first() else {
    return widths;
  };
  let mut logical_column_ix = 0usize;
  for (cell_ix, cell_layout) in first_layout_row.cells.iter().enumerate() {
    let span = first_data_row
      .cells
      .get(cell_ix)
      .map(|cell| cell.col_span.max(1) as usize)
      .unwrap_or(1);
    let cell_width: f32 = cell_layout.bounds.size.width.into();
    let per_column = (cell_width / span as f32).max(32.0).round() as u32;
    for ix in logical_column_ix..logical_column_ix.saturating_add(span).min(widths.len()) {
      if !matches!(table.column_widths.get(ix), Some(TableColumnWidth::FixedPx(_))) {
        widths[ix] = per_column;
      }
    }
    logical_column_ix = logical_column_ix.saturating_add(span);
  }
  widths
}

fn default_table_cell_paragraph() -> TableCellParagraph {
  TableCellParagraph {
    paragraph: Paragraph {
      style: ParagraphStyle::Normal,
      byte_range: 0..0,
      runs: Vec::new(),
      version: 0,
    },
    text: String::new(),
  }
}

pub(super) fn table_cell_paragraph_block_ix(cell: &TableCell, preferred: usize) -> Option<usize> {
  if matches!(cell.blocks.get(preferred), Some(TableCellBlock::Paragraph(_))) {
    return Some(preferred);
  }
  cell
    .blocks
    .iter()
    .position(|block| matches!(block, TableCellBlock::Paragraph(_)))
}

fn previous_table_cell_paragraph_block_ix(cell: &TableCell, current_ix: usize) -> Option<usize> {
  cell
    .blocks
    .get(..current_ix)?
    .iter()
    .rposition(|block| matches!(block, TableCellBlock::Paragraph(_)))
}

fn next_table_cell_paragraph_block_ix(cell: &TableCell, current_ix: usize) -> Option<usize> {
  cell
    .blocks
    .iter()
    .enumerate()
    .skip(current_ix.saturating_add(1))
    .find_map(|(ix, block)| matches!(block, TableCellBlock::Paragraph(_)).then_some(ix))
}

pub(super) fn split_table_cell_paragraph_at(cell: &mut TableCell, paragraph_block_ix: usize, byte: usize) -> Option<usize> {
  let paragraph_ix = table_cell_paragraph_block_ix(cell, paragraph_block_ix).unwrap_or_else(|| {
    cell
      .blocks
      .push(TableCellBlock::Paragraph(default_table_cell_paragraph()));
    cell.blocks.len() - 1
  });
  let TableCellBlock::Paragraph(paragraph) = cell.blocks.get_mut(paragraph_ix)? else {
    return None;
  };
  let byte = byte.min(paragraph.text.len());
  if !paragraph.text.is_char_boundary(byte) {
    return None;
  }
  let right_text = paragraph.text[byte..].to_string();
  paragraph.text.truncate(byte);
  let (left_runs, right_runs) = split_runs_at(&paragraph.paragraph.runs, byte);
  paragraph.paragraph.runs = left_runs;
  paragraph.paragraph.byte_range = 0..paragraph.text.len();
  paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
  let new_paragraph = TableCellParagraph {
    paragraph: Paragraph {
      style: paragraph.paragraph.style,
      byte_range: 0..right_text.len(),
      runs: right_runs,
      version: paragraph.paragraph.version,
    },
    text: right_text,
  };
  cell
    .blocks
    .insert(paragraph_ix + 1, TableCellBlock::Paragraph(new_paragraph));
  Some(paragraph_ix + 1)
}

pub(super) fn insert_table_cell_paragraphs_at(
  cell: &mut TableCell,
  paragraph_block_ix: usize,
  byte: usize,
  paragraphs: &[InputParagraph],
) -> Option<(usize, usize)> {
  if paragraphs.is_empty() {
    return Some((paragraph_block_ix, byte));
  }
  let paragraph_ix = table_cell_paragraph_block_ix(cell, paragraph_block_ix).unwrap_or_else(|| {
    cell
      .blocks
      .push(TableCellBlock::Paragraph(default_table_cell_paragraph()));
    cell.blocks.len() - 1
  });
  let TableCellBlock::Paragraph(paragraph) = cell.blocks.get_mut(paragraph_ix)? else {
    return None;
  };
  let byte = byte.min(paragraph.text.len());
  if !paragraph.text.is_char_boundary(byte) {
    return None;
  }

  let right_text = paragraph.text[byte..].to_string();
  paragraph.text.truncate(byte);
  let (left_runs, right_runs) = split_runs_at(&paragraph.paragraph.runs, byte);
  paragraph.paragraph.runs = left_runs;
  paragraph.paragraph.byte_range = 0..paragraph.text.len();
  paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);

  let mut inserted = paragraphs
    .iter()
    .map(table_cell_paragraph_from_input_paragraph)
    .collect::<Vec<_>>();
  let first = inserted.remove(0);
  let first_insert_start = paragraph.text.len();
  paragraph.text.push_str(&first.text);
  let mut paragraph_runs = std::mem::take(&mut paragraph.paragraph.runs);
  paragraph_runs.extend(first.paragraph.runs);
  paragraph.paragraph.runs = merge_adjacent_runs(paragraph_runs);
  paragraph.paragraph.byte_range = 0..paragraph.text.len();
  paragraph.paragraph.version = paragraph.paragraph.version.wrapping_add(1);
  let mut caret_block_ix = paragraph_ix;
  let mut caret_byte = first_insert_start + first.text.len();

  let mut insert_ix = paragraph_ix + 1;
  for inserted_paragraph in inserted {
    caret_block_ix = insert_ix;
    caret_byte = inserted_paragraph.text.len();
    cell
      .blocks
      .insert(insert_ix, TableCellBlock::Paragraph(inserted_paragraph));
    insert_ix += 1;
  }

  let TableCellBlock::Paragraph(caret_paragraph) = cell.blocks.get_mut(caret_block_ix)? else {
    return None;
  };
  caret_paragraph.text.push_str(&right_text);
  let mut caret_runs = std::mem::take(&mut caret_paragraph.paragraph.runs);
  caret_runs.extend(right_runs);
  caret_paragraph.paragraph.runs = merge_adjacent_runs(caret_runs);
  caret_paragraph.paragraph.byte_range = 0..caret_paragraph.text.len();
  caret_paragraph.paragraph.version = caret_paragraph.paragraph.version.wrapping_add(1);
  Some((caret_block_ix, caret_byte))
}

pub(super) fn merge_table_cell_paragraph_with_previous(cell: &mut TableCell, paragraph_block_ix: usize) -> Option<(usize, usize)> {
  let current_ix = table_cell_paragraph_block_ix(cell, paragraph_block_ix)?;
  let previous_ix = previous_table_cell_paragraph_block_ix(cell, current_ix)?;
  let TableCellBlock::Paragraph(current) = cell.blocks.remove(current_ix) else {
    return None;
  };
  let TableCellBlock::Paragraph(previous) = cell.blocks.get_mut(previous_ix)? else {
    return None;
  };
  let caret = previous.text.len();
  previous.text.push_str(&current.text);
  let mut runs = std::mem::take(&mut previous.paragraph.runs);
  runs.extend(current.paragraph.runs);
  previous.paragraph.runs = merge_adjacent_runs(runs);
  previous.paragraph.byte_range = 0..previous.text.len();
  previous.paragraph.version = previous.paragraph.version.wrapping_add(1);
  Some((previous_ix, caret))
}

fn merge_table_cell_paragraph_with_next(cell: &mut TableCell, paragraph_block_ix: usize) -> Option<(usize, usize)> {
  let current_ix = table_cell_paragraph_block_ix(cell, paragraph_block_ix)?;
  let next_ix = next_table_cell_paragraph_block_ix(cell, current_ix)?;
  let TableCellBlock::Paragraph(next) = cell.blocks.remove(next_ix) else {
    return None;
  };
  let TableCellBlock::Paragraph(current) = cell.blocks.get_mut(current_ix)? else {
    return None;
  };
  let caret = current.text.len();
  current.text.push_str(&next.text);
  let mut runs = std::mem::take(&mut current.paragraph.runs);
  runs.extend(next.paragraph.runs);
  current.paragraph.runs = merge_adjacent_runs(runs);
  current.paragraph.byte_range = 0..current.text.len();
  current.paragraph.version = current.paragraph.version.wrapping_add(1);
  Some((current_ix, caret))
}

fn table_cell_styles_at(cell_paragraph: &TableCellParagraph, byte: usize) -> RunStyles {
  let (run_ix, _) = run_containing(&cell_paragraph.paragraph, byte.min(cell_paragraph.text.len()));
  cell_paragraph
    .paragraph
    .runs
    .get(run_ix)
    .map(|run| run.styles)
    .unwrap_or_default()
}

fn insert_text_in_table_cell_paragraph(cell_paragraph: &mut TableCellParagraph, byte: usize, text: &str, styles: RunStyles) {
  if text.is_empty() {
    return;
  }
  let byte = byte.min(cell_paragraph.text.len());
  if !cell_paragraph.text.is_char_boundary(byte) {
    return;
  }
  let insert_len = text.len();
  cell_paragraph.text.insert_str(byte, text);
  let (mut left, right) = split_runs_at(&cell_paragraph.paragraph.runs, byte);
  left.push(TextRun { len: insert_len, styles });
  left.extend(right);
  cell_paragraph.paragraph.runs = merge_adjacent_runs(left);
  cell_paragraph.paragraph.byte_range = 0..cell_paragraph.text.len();
  cell_paragraph.paragraph.version = cell_paragraph.paragraph.version.wrapping_add(1);
}

fn delete_range_in_table_cell_paragraph(cell_paragraph: &mut TableCellParagraph, range: Range<usize>) {
  let start = range.start.min(cell_paragraph.text.len());
  let end = range.end.min(cell_paragraph.text.len()).max(start);
  if start == end || !cell_paragraph.text.is_char_boundary(start) || !cell_paragraph.text.is_char_boundary(end) {
    return;
  }
  cell_paragraph.text.replace_range(start..end, "");
  let mut output = Vec::new();
  let mut offset = 0;
  for run in std::mem::take(&mut cell_paragraph.paragraph.runs) {
    let run_start = offset;
    let run_end = offset + run.len;
    offset = run_end;
    if run_end <= start || run_start >= end {
      output.push(run);
      continue;
    }
    if run_start < start {
      output.push(TextRun {
        len: start - run_start,
        styles: run.styles,
      });
    }
    if run_end > end {
      output.push(TextRun {
        len: run_end - end,
        styles: run.styles,
      });
    }
  }
  cell_paragraph.paragraph.runs = merge_adjacent_runs(output);
  cell_paragraph.paragraph.byte_range = 0..cell_paragraph.text.len();
  cell_paragraph.paragraph.version = cell_paragraph.paragraph.version.wrapping_add(1);
}

fn table_cell_range_all_run_styles(cell_paragraph: &TableCellParagraph, range: Range<usize>, predicate: impl Fn(RunStyles) -> bool) -> bool {
  if range.start >= range.end {
    return false;
  }
  let mut offset = 0;
  let mut saw_run = false;
  for run in &cell_paragraph.paragraph.runs {
    let run_start = offset;
    let run_end = offset + run.len;
    offset = run_end;
    if run_end <= range.start || run_start >= range.end {
      continue;
    }
    saw_run = true;
    if !predicate(run.styles) {
      return false;
    }
  }
  saw_run
}

pub(super) fn mutate_table_cell_runs_in_range(
  cell_paragraph: &mut TableCellParagraph,
  range: Range<usize>,
  mut mutate: impl FnMut(&mut RunStyles),
) {
  let start = range.start.min(cell_paragraph.text.len());
  let end = range.end.min(cell_paragraph.text.len());
  if start >= end {
    return;
  }
  let mut new_runs = Vec::with_capacity(cell_paragraph.paragraph.runs.len() + 2);
  let mut offset = 0;
  let old_runs = std::mem::take(&mut cell_paragraph.paragraph.runs);
  for run in &old_runs {
    let run_start = offset;
    let run_end = offset + run.len;
    offset = run_end;
    if run_end <= start || run_start >= end {
      new_runs.push(run.clone());
      continue;
    }
    if run_start < start {
      new_runs.push(TextRun {
        len: start - run_start,
        styles: run.styles,
      });
    }
    let selected_start = run_start.max(start);
    let selected_end = run_end.min(end);
    let mut selected_styles = run.styles;
    mutate(&mut selected_styles);
    new_runs.push(TextRun {
      len: selected_end - selected_start,
      styles: selected_styles,
    });
    if run_end > end {
      new_runs.push(TextRun {
        len: run_end - end,
        styles: run.styles,
      });
    }
  }
  cell_paragraph.paragraph.runs = merge_adjacent_runs(new_runs);
  cell_paragraph.paragraph.version = cell_paragraph.paragraph.version.wrapping_add(1);
}

fn insert_standalone_paragraphs_into_projection(
  document: &mut Document,
  insert_paragraph_ix: usize,
  inserted: &[InputParagraph],
) -> Vec<Paragraph> {
  if inserted.is_empty() {
    return Vec::new();
  }
  let mut entries = document
    .paragraphs
    .iter()
    .enumerate()
    .map(|(paragraph_ix, paragraph)| (paragraph.clone(), paragraph_text(document, paragraph_ix)))
    .collect::<Vec<_>>();
  let inserted_entries = inserted
    .iter()
    .map(|paragraph| {
      let text = input_paragraph_text(paragraph);
      (paragraph_from_input_paragraph(paragraph), text)
    })
    .collect::<Vec<_>>();
  let insert_ix = insert_paragraph_ix.min(entries.len());
  entries.splice(insert_ix..insert_ix, inserted_entries.clone());

  let mut text = String::new();
  let mut byte = 0;
  let mut paragraphs = Vec::with_capacity(entries.len());
  for (ix, (mut paragraph, paragraph_text)) in entries.into_iter().enumerate() {
    if ix > 0 {
      text.push('\n');
      byte += 1;
    }
    let start = byte;
    text.push_str(&paragraph_text);
    byte += paragraph_text.len();
    paragraph.byte_range = start..byte;
    paragraphs.push(paragraph);
  }
  let inserted_paragraphs = paragraphs[insert_ix..insert_ix + inserted.len()].to_vec();
  document.text = Rope::from(text);
  document.paragraphs = Arc::new(paragraphs);
  document.offset_index = ParagraphOffsetIndex::new(&document.paragraphs);
  inserted_paragraphs
}

fn render_image_block(
  document: &Document,
  image: &ImageBlock,
  block_ix: usize,
  row_size: Size<Pixels>,
  selected_block: Option<BlockSelection>,
  editor: Entity<RichTextEditor>,
) -> gpui::AnyElement {
  let selected = selected_block == Some(BlockSelection::Image(block_ix));
  let Some(asset) = document.assets.assets.get(&image.asset_id) else {
    return reserved_object_frame(document, row_size, selected)
      .child("Missing image")
      .into_any_element();
  };
  let Some(format) = ImageFormat::from_mime_type(asset.mime_type.as_ref()) else {
    return reserved_object_frame(document, row_size, selected)
      .child("Unsupported image")
      .into_any_element();
  };
  let gpui_image = Image::from_bytes(format, asset.bytes.as_ref().clone());
  image_object_frame(document, image, asset, row_size, selected)
    .child(
      img(Arc::new(gpui_image))
        .size_full()
        .object_fit(gpui::ObjectFit::Contain)
        .with_loading(|| div().size_full().bg(rgb(0xffffff)).into_any_element())
        .with_fallback(|| {
          div()
            .size_full()
            .bg(rgb(0xffffff))
            .child("Image unavailable")
            .into_any_element()
        }),
    )
    .when(selected, |this| this.children(image_resize_handles(editor, block_ix)))
    .into_any_element()
}

fn image_resize_handles(editor: Entity<RichTextEditor>, block_ix: usize) -> Vec<gpui::AnyElement> {
  [
    ImageResizeHandle::TopLeft,
    ImageResizeHandle::TopRight,
    ImageResizeHandle::Left,
    ImageResizeHandle::Right,
    ImageResizeHandle::BottomLeft,
    ImageResizeHandle::BottomRight,
  ]
  .into_iter()
  .map(|handle| image_resize_handle(editor.clone(), block_ix, handle))
  .collect()
}

fn image_resize_handle(editor: Entity<RichTextEditor>, block_ix: usize, handle: ImageResizeHandle) -> gpui::AnyElement {
  let cursor = match handle {
    ImageResizeHandle::Left | ImageResizeHandle::Right => CursorStyle::ResizeLeftRight,
    ImageResizeHandle::TopLeft | ImageResizeHandle::BottomRight => CursorStyle::ResizeUpLeftDownRight,
    ImageResizeHandle::TopRight | ImageResizeHandle::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
  };
  div()
    .absolute()
    .when(
      matches!(
        handle,
        ImageResizeHandle::Left | ImageResizeHandle::TopLeft | ImageResizeHandle::BottomLeft
      ),
      |this| this.left(px(-4.0)),
    )
    .when(
      matches!(
        handle,
        ImageResizeHandle::Right | ImageResizeHandle::TopRight | ImageResizeHandle::BottomRight
      ),
      |this| this.right(px(-4.0)),
    )
    .when(matches!(handle, ImageResizeHandle::TopLeft | ImageResizeHandle::TopRight), |this| {
      this.top(px(-4.0))
    })
    .when(matches!(handle, ImageResizeHandle::BottomLeft | ImageResizeHandle::BottomRight), |this| {
      this.bottom(px(-4.0))
    })
    .when(handle == ImageResizeHandle::Left || handle == ImageResizeHandle::Right, |this| {
      this.top(px(24.0))
    })
    .size(px(9.0))
    .bg(rgb(0xffffff))
    .border_1()
    .border_color(rgb(0x0969da))
    .cursor(cursor)
    .on_mouse_down(MouseButton::Left, move |event, window, cx| {
      cx.stop_propagation();
      editor.update(cx, |editor, cx| {
        editor.start_image_resize_drag(block_ix, handle, event.position, window, cx);
      });
    })
    .into_any_element()
}

fn render_equation_block(
  document: &Document,
  equation: &EquationBlock,
  block_ix: usize,
  row_size: Size<Pixels>,
  selected: bool,
  source_selection: Option<EquationSourceSelection>,
) -> gpui::AnyElement {
  let _ = block_ix;
  let frame = reserved_object_frame(document, row_size, selected);
  let equation_width = {
    let source_width = equation.source.len().max(4) as f32 * 26.0;
    let max_width: f32 = (row_size.width - document.theme.pageless_inset_x * 2.0)
      .max(px(240.0))
      .into();
    px(source_width.clamp(240.0, max_width))
  };
  let source_strip = || {
    div()
      .w_full()
      .px_2()
      .py_1()
      .text_xs()
      .font_family("Consolas")
      .text_color(rgb(0x000000))
      .bg(rgb(0xf6f8fa))
      .flex()
      .flex_row()
      .children(equation_source_text_elements(&equation.source, source_selection))
      .into_any_element()
  };
  match EquationRenderer::png_bytes(equation) {
    Ok(png) => {
      let image = Image::from_bytes(ImageFormat::Png, png.as_ref().clone());
      frame
        .child(
          div()
            .size_full()
            .flex()
            .flex_col()
            .items_center()
            .justify_center()
            .gap_1()
            .child(
              img(Arc::new(image))
                .w(equation_width)
                .h(px(60.0))
                .object_fit(gpui::ObjectFit::ScaleDown)
                .with_loading(|| div().size_full().bg(rgb(0xffffff)).into_any_element())
                .with_fallback(|| {
                  div()
                    .size_full()
                    .bg(rgb(0xffffff))
                    .child("Equation unavailable")
                    .into_any_element()
                }),
            )
            .when(selected, |this| this.child(source_strip())),
        )
        .into_any_element()
    },
    Err(error) => frame
      .child(
        div()
          .size_full()
          .flex()
          .flex_col()
          .items_center()
          .justify_center()
          .gap_1()
          .font_family("Cambria Math")
          .text_size(px(18.0))
          .text_color(rgb(0x000000))
          .child(div().text_xs().text_color(rgb(0xa40000)).child(error))
          .child(source_strip()),
      )
      .into_any_element(),
  }
}

fn equation_source_text_elements(source: &str, selection: Option<EquationSourceSelection>) -> Vec<gpui::AnyElement> {
  let range = selection.and_then(|selection| {
    if selection.anchor == selection.caret {
      None
    } else {
      Some(selection.anchor.min(selection.caret)..selection.anchor.max(selection.caret))
    }
  });
  let caret = selection.map(|selection| selection.caret.min(source.len()));
  let caret_visible = selection
    .map(|selection| selection.caret_visible)
    .unwrap_or(false);
  let mut children = Vec::new();
  let mut rendered_caret = false;
  for (byte, ch) in source.char_indices() {
    if caret == Some(byte) && caret_visible {
      children.push(
        div()
          .text_color(rgb(0x000000))
          .child("|")
          .into_any_element(),
      );
      rendered_caret = true;
    }
    let end = byte + ch.len_utf8();
    let selected = range
      .as_ref()
      .is_some_and(|range| byte < range.end && end > range.start);
    children.push(
      div()
        .when(selected, |this| this.bg(rgb(0x0969da)).text_color(rgb(0xffffff)))
        .child(ch.to_string())
        .into_any_element(),
    );
  }
  if !rendered_caret && caret == Some(source.len()) && caret_visible {
    children.push(
      div()
        .text_color(rgb(0x000000))
        .child("|")
        .into_any_element(),
    );
  }
  children
}

struct EquationRenderer;

impl EquationRenderer {
  fn svg_bytes(equation: &EquationBlock) -> Result<Arc<Vec<u8>>, String> {
    static CACHE: OnceLock<Mutex<HashMap<(String, bool), Result<Arc<Vec<u8>>, String>>>> = OnceLock::new();
    let display = matches!(equation.display, EquationDisplay::Display);
    let key = (equation.source.to_string(), display);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = cache.lock().ok().and_then(|cache| cache.get(&key).cloned()) {
      return cached;
    }
    let result = if display {
      mathjax_svg::convert_to_svg(&key.0)
    } else {
      mathjax_svg::convert_to_svg_inline(&key.0)
    }
    .map(|svg| Arc::new(pad_mathjax_svg_viewbox(&svg).into_bytes()))
    .map_err(|error| error.to_string());
    if let Ok(mut cache) = cache.lock() {
      cache.insert(key, result.clone());
    }
    result
  }

  fn png_bytes(equation: &EquationBlock) -> Result<Arc<Vec<u8>>, String> {
    static CACHE: OnceLock<Mutex<HashMap<(String, bool), Result<Arc<Vec<u8>>, String>>>> = OnceLock::new();
    let display = matches!(equation.display, EquationDisplay::Display);
    let key = (equation.source.to_string(), display);
    let cache = CACHE.get_or_init(|| Mutex::new(HashMap::new()));
    if let Some(cached) = cache.lock().ok().and_then(|cache| cache.get(&key).cloned()) {
      return cached;
    }
    let result = Self::svg_bytes(equation)
      .and_then(|svg| rasterize_svg_to_png(svg.as_ref()))
      .map(Arc::new);
    if let Ok(mut cache) = cache.lock() {
      cache.insert(key, result.clone());
    }
    result
  }
}

fn rasterize_svg_to_png(svg: &[u8]) -> Result<Vec<u8>, String> {
  const EQUATION_RASTER_SCALE: f32 = 4.0;
  let tree = resvg::usvg::Tree::from_data(svg, &resvg::usvg::Options::default()).map_err(|error| error.to_string())?;
  let svg_size = tree.size();
  let width = (svg_size.width() * EQUATION_RASTER_SCALE).ceil().max(1.0) as u32;
  let height = (svg_size.height() * EQUATION_RASTER_SCALE).ceil().max(1.0) as u32;
  let mut pixmap = resvg::tiny_skia::Pixmap::new(width, height).ok_or_else(|| "equation SVG has invalid raster size".to_string())?;
  resvg::render(
    &tree,
    resvg::tiny_skia::Transform::from_scale(EQUATION_RASTER_SCALE, EQUATION_RASTER_SCALE),
    &mut pixmap.as_mut(),
  );

  pixmap.encode_png().map_err(|error| error.to_string())
}

fn pad_mathjax_svg_viewbox(svg: &str) -> String {
  let Some(viewbox_start) = svg.find("viewBox=\"") else {
    return svg.to_string();
  };
  let values_start = viewbox_start + "viewBox=\"".len();
  let Some(values_end) = svg[values_start..]
    .find('"')
    .map(|offset| values_start + offset)
  else {
    return svg.to_string();
  };
  let values = &svg[values_start..values_end];
  let mut parts = values
    .split_whitespace()
    .filter_map(|part| part.parse::<f32>().ok());
  let (Some(x), Some(y), Some(width), Some(height)) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
    return svg.to_string();
  };
  let top_pad = height * 0.08;
  let bottom_pad = height * 0.18;
  let replacement = format!("{} {} {} {}", x, y - top_pad, width, height + top_pad + bottom_pad);
  let mut output = String::with_capacity(svg.len() + replacement.len());
  output.push_str(&svg[..values_start]);
  output.push_str(&replacement);
  output.push_str(&svg[values_end..]);
  output
}

fn reserved_object_frame(document: &Document, row_size: Size<Pixels>, selected: bool) -> gpui::Div {
  let object_height = (row_size.height - document.theme.paragraph_after).max(px(1.0));
  let object_width = (row_size.width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  div()
    .relative()
    .w(object_width)
    .h(object_height)
    .ml(document.theme.pageless_inset_x)
    .mr(document.theme.pageless_inset_x)
    .mb(document.theme.paragraph_after)
    .overflow_hidden()
    .bg(rgb(0xffffff))
    .border_1()
    .border_color(if selected { rgb(0x0969da) } else { rgb(0xffffff) })
}

fn image_object_frame(document: &Document, image: &ImageBlock, asset: &AssetRecord, row_size: Size<Pixels>, selected: bool) -> gpui::Div {
  let available_width = (row_size.width - document.theme.pageless_inset_x * 2.0).max(px(1.0));
  let intrinsic = image_asset_intrinsic_size(asset);
  let object_width = match image.sizing {
    ImageSizing::Fixed { width_px, .. } => px(width_px as f32).min(available_width),
    ImageSizing::FitWidth => available_width,
    ImageSizing::Intrinsic => intrinsic
      .map(|(width, _)| width.min(available_width))
      .unwrap_or(available_width),
  };
  let object_height = (row_size.height - document.theme.paragraph_after).max(px(1.0));
  let left_margin = document.theme.pageless_inset_x
    + match image.alignment {
      BlockAlignment::Left => px(0.0),
      BlockAlignment::Center => (available_width - object_width).max(px(0.0)) / 2.0,
      BlockAlignment::Right => (available_width - object_width).max(px(0.0)),
    };
  div()
    .relative()
    .w(object_width)
    .h(object_height)
    .ml(left_margin)
    .mr(document.theme.pageless_inset_x)
    .mb(document.theme.paragraph_after)
    .overflow_hidden()
    .bg(rgb(0xffffff))
    .border_1()
    .border_color(if selected { rgb(0x0969da) } else { rgb(0xffffff) })
}

fn image_asset_intrinsic_size(asset: &AssetRecord) -> Option<(Pixels, Pixels)> {
  let size = imagesize::blob_size(asset.bytes.as_ref()).ok()?;
  if size.width == 0 || size.height == 0 {
    return None;
  }
  Some((px(size.width as f32), px(size.height as f32)))
}

fn image_asset_from_path(path: &Path) -> Option<(AssetRecord, SharedString)> {
  let bytes = fs::read(path).ok()?;
  let format = image_format_for_path(path)?;
  let original_name = path
    .file_name()
    .map(|name| name.to_string_lossy().to_string());
  let alt_text: SharedString = original_name.clone().unwrap_or_default().into();
  let mut hasher = DefaultHasher::new();
  bytes.hash(&mut hasher);
  Some((
    AssetRecord {
      id: AssetId(uuid::Uuid::new_v4().as_u128()),
      mime_type: format.mime_type().into(),
      original_name: original_name.map(Into::into),
      content_hash: hasher.finish(),
      bytes: Arc::new(bytes),
    },
    alt_text,
  ))
}

fn image_format_for_path(path: &Path) -> Option<ImageFormat> {
  match path
    .extension()?
    .to_string_lossy()
    .to_ascii_lowercase()
    .as_str()
  {
    "png" => Some(ImageFormat::Png),
    "jpg" | "jpeg" => Some(ImageFormat::Jpeg),
    "webp" => Some(ImageFormat::Webp),
    "gif" => Some(ImageFormat::Gif),
    "svg" => Some(ImageFormat::Svg),
    "bmp" => Some(ImageFormat::Bmp),
    "tif" | "tiff" => Some(ImageFormat::Tiff),
    _ => None,
  }
}

fn block_fragment_plain_text(fragment: &RichClipboardFragment) -> String {
  let mut parts = fragment
    .paragraphs
    .iter()
    .map(input_paragraph_text)
    .collect::<Vec<_>>();
  parts.extend(fragment.blocks.iter().map(|block| match block {
    InputBlock::Paragraph(paragraph) => input_paragraph_text(paragraph),
    InputBlock::Image(image) => {
      if image.alt_text.is_empty() {
        "[Image]".to_string()
      } else {
        image.alt_text.clone()
      }
    },
    InputBlock::Equation(equation) => equation.source.clone(),
    InputBlock::Table(table) => table_plain_text(table),
  }));
  parts.join("\n")
}

fn table_plain_text(table: &InputTableBlock) -> String {
  table
    .rows
    .iter()
    .map(|row| {
      row
        .cells
        .iter()
        .map(|cell| {
          cell
            .blocks
            .iter()
            .map(|block| match block {
              InputTableCellBlock::Paragraph(paragraph) => input_paragraph_text(paragraph),
              InputTableCellBlock::Table(table) => table_plain_text(table),
            })
            .collect::<Vec<_>>()
            .join("\n")
        })
        .collect::<Vec<_>>()
        .join("\t")
    })
    .collect::<Vec<_>>()
    .join("\n")
}

pub(super) fn input_paragraph_text(paragraph: &InputParagraph) -> String {
  paragraph.runs.iter().map(|run| run.text.as_str()).collect()
}

fn collect_block_assets(block: &Block, assets: &AssetStore, output: &mut Vec<InputAsset>) {
  match block {
    Block::Image(image) => {
      if let Some(asset) = assets.assets.get(&image.asset_id) {
        output.push(InputAsset {
          id: asset.id,
          mime_type: asset.mime_type.to_string(),
          original_name: asset.original_name.as_ref().map(ToString::to_string),
          content_hash: asset.content_hash,
          bytes: asset.bytes.as_ref().clone(),
        });
      }
    },
    Block::Table(table) => {
      for row in &table.rows {
        for cell in &row.cells {
          for block in &cell.blocks {
            if let TableCellBlock::Table(table) = block {
              collect_block_assets(&Block::Table(table.clone()), assets, output);
            }
          }
        }
      }
    },
    Block::Paragraph(_) | Block::Equation(_) => {},
  }
}

fn input_block_from_block(block: &Block) -> InputBlock {
  match block {
    Block::Paragraph(paragraph) => InputBlock::Paragraph(input_paragraph_from_paragraph(paragraph)),
    Block::Image(image) => InputBlock::Image(InputImageBlock {
      asset_id: image.asset_id,
      alt_text: image.alt_text.to_string(),
      caption: image.caption.as_ref().map(input_paragraph_from_paragraph),
      sizing: match image.sizing {
        ImageSizing::Intrinsic => InputImageSizing::Intrinsic,
        ImageSizing::FitWidth => InputImageSizing::FitWidth,
        ImageSizing::Fixed { width_px, height_px } => InputImageSizing::Fixed { width_px, height_px },
      },
      alignment: input_alignment_from_alignment(image.alignment),
    }),
    Block::Equation(equation) => InputBlock::Equation(InputEquationBlock {
      source: equation.source.to_string(),
      syntax: InputEquationSyntax::Latex,
      display: match equation.display {
        EquationDisplay::Display => InputEquationDisplay::Display,
        EquationDisplay::InlineLikeParagraph => InputEquationDisplay::InlineLikeParagraph,
      },
    }),
    Block::Table(table) => InputBlock::Table(input_table_from_table(table)),
  }
}

fn block_from_input_block(block: &InputBlock) -> Block {
  match block {
    InputBlock::Paragraph(paragraph) => Block::Paragraph(paragraph_from_input_paragraph(paragraph)),
    InputBlock::Image(image) => Block::Image(ImageBlock {
      asset_id: image.asset_id,
      alt_text: image.alt_text.clone().into(),
      caption: image.caption.as_ref().map(paragraph_from_input_paragraph),
      sizing: match image.sizing {
        InputImageSizing::Intrinsic => ImageSizing::Intrinsic,
        InputImageSizing::FitWidth => ImageSizing::FitWidth,
        InputImageSizing::Fixed { width_px, height_px } => ImageSizing::Fixed { width_px, height_px },
      },
      alignment: alignment_from_input_alignment(image.alignment),
      version: 0,
    }),
    InputBlock::Equation(equation) => Block::Equation(EquationBlock {
      source: equation.source.clone().into(),
      syntax: EquationSyntax::Latex,
      display: match equation.display {
        InputEquationDisplay::Display => EquationDisplay::Display,
        InputEquationDisplay::InlineLikeParagraph => EquationDisplay::InlineLikeParagraph,
      },
      version: 0,
    }),
    InputBlock::Table(table) => Block::Table(table_from_input_table(table)),
  }
}

fn input_paragraph_from_paragraph(paragraph: &Paragraph) -> InputParagraph {
  InputParagraph {
    style: paragraph.style,
    runs: paragraph
      .runs
      .iter()
      .map(|run| InputRun {
        text: String::new(),
        styles: run.styles,
      })
      .collect(),
  }
}

fn input_paragraph_from_document_range(document: &Document, paragraph_ix: usize, range: Range<usize>) -> InputParagraph {
  let paragraph = &document.paragraphs[paragraph_ix];
  let paragraph_range = paragraph_byte_range(document, paragraph_ix);
  let start = range.start.min(paragraph_text_len(paragraph));
  let end = range.end.min(paragraph_text_len(paragraph)).max(start);
  let mut runs = Vec::new();
  let mut offset = 0;
  for run in &paragraph.runs {
    let run_start = offset;
    let run_end = offset + run.len;
    offset = run_end;
    let clipped_start = run_start.max(start);
    let clipped_end = run_end.min(end);
    if clipped_start < clipped_end {
      runs.push(InputRun {
        text: document_text_slice(document, paragraph_range.start + clipped_start..paragraph_range.start + clipped_end),
        styles: run.styles,
      });
    }
  }
  InputParagraph {
    style: paragraph.style,
    runs,
  }
}

pub(super) fn input_paragraph_from_table_cell_paragraph(paragraph: &TableCellParagraph) -> InputParagraph {
  let mut byte = 0;
  InputParagraph {
    style: paragraph.paragraph.style,
    runs: paragraph
      .paragraph
      .runs
      .iter()
      .map(|run| {
        let start = byte;
        let end = (start + run.len).min(paragraph.text.len());
        byte = end;
        InputRun {
          text: paragraph.text.get(start..end).unwrap_or("").to_string(),
          styles: run.styles,
        }
      })
      .collect(),
  }
}

fn input_paragraph_from_table_cell_range(paragraph: &TableCellParagraph, range: Range<usize>) -> InputParagraph {
  let start = range.start.min(paragraph.text.len());
  let end = range.end.min(paragraph.text.len()).max(start);
  let mut runs = Vec::new();
  let mut byte = 0;
  for run in &paragraph.paragraph.runs {
    let run_start = byte;
    let run_end = run_start + run.len;
    byte = run_end;
    let clipped_start = run_start.max(start);
    let clipped_end = run_end.min(end);
    if clipped_start < clipped_end {
      runs.push(InputRun {
        text: paragraph
          .text
          .get(clipped_start..clipped_end)
          .unwrap_or("")
          .to_string(),
        styles: run.styles,
      });
    }
  }
  InputParagraph {
    style: paragraph.paragraph.style,
    runs,
  }
}

fn paragraph_from_input_paragraph(paragraph: &InputParagraph) -> Paragraph {
  let len = paragraph.runs.iter().map(|run| run.text.len()).sum();
  Paragraph {
    style: paragraph.style,
    byte_range: 0..len,
    runs: merge_adjacent_runs(
      paragraph
        .runs
        .iter()
        .map(|run| TextRun {
          len: run.text.len(),
          styles: run.styles,
        })
        .collect(),
    ),
    version: 0,
  }
}

pub(super) fn table_cell_paragraph_from_input_paragraph(paragraph: &InputParagraph) -> TableCellParagraph {
  let text = input_paragraph_text(paragraph);
  TableCellParagraph {
    paragraph: paragraph_from_input_paragraph(paragraph),
    text,
  }
}

fn input_table_from_table(table: &TableBlock) -> InputTableBlock {
  InputTableBlock {
    rows: table
      .rows
      .iter()
      .map(|row| InputTableRow {
        cells: row
          .cells
          .iter()
          .map(|cell| InputTableCell {
            blocks: cell
              .blocks
              .iter()
              .map(|block| match block {
                TableCellBlock::Paragraph(paragraph) => InputTableCellBlock::Paragraph(input_paragraph_from_table_cell_paragraph(paragraph)),
                TableCellBlock::Table(table) => InputTableCellBlock::Table(input_table_from_table(table)),
              })
              .collect(),
            row_span: cell.row_span,
            col_span: cell.col_span,
          })
          .collect(),
      })
      .collect(),
    column_widths: table
      .column_widths
      .iter()
      .map(|width| match *width {
        TableColumnWidth::Auto => InputTableColumnWidth::Auto,
        TableColumnWidth::FixedPx(px) => InputTableColumnWidth::FixedPx(px),
        TableColumnWidth::Fraction(fraction) => InputTableColumnWidth::Fraction(fraction),
      })
      .collect(),
    style: InputTableStyle {
      header_row: table.style.header_row,
    },
  }
}

fn table_from_input_table(table: &InputTableBlock) -> TableBlock {
  TableBlock {
    rows: table
      .rows
      .iter()
      .map(|row| TableRow {
        cells: row
          .cells
          .iter()
          .map(|cell| TableCell {
            blocks: cell
              .blocks
              .iter()
              .map(|block| match block {
                InputTableCellBlock::Paragraph(paragraph) => TableCellBlock::Paragraph(table_cell_paragraph_from_input_paragraph(paragraph)),
                InputTableCellBlock::Table(table) => TableCellBlock::Table(table_from_input_table(table)),
              })
              .collect(),
            row_span: cell.row_span,
            col_span: cell.col_span,
          })
          .collect(),
      })
      .collect(),
    column_widths: table
      .column_widths
      .iter()
      .map(|width| match *width {
        InputTableColumnWidth::Auto => TableColumnWidth::Auto,
        InputTableColumnWidth::FixedPx(px) => TableColumnWidth::FixedPx(px),
        InputTableColumnWidth::Fraction(fraction) => TableColumnWidth::Fraction(fraction),
      })
      .collect(),
    style: TableStyle {
      header_row: table.style.header_row,
    },
    version: 0,
  }
}

fn input_alignment_from_alignment(alignment: BlockAlignment) -> InputBlockAlignment {
  match alignment {
    BlockAlignment::Left => InputBlockAlignment::Left,
    BlockAlignment::Center => InputBlockAlignment::Center,
    BlockAlignment::Right => InputBlockAlignment::Right,
  }
}

fn alignment_from_input_alignment(alignment: InputBlockAlignment) -> BlockAlignment {
  match alignment {
    InputBlockAlignment::Left => BlockAlignment::Left,
    InputBlockAlignment::Center => BlockAlignment::Center,
    InputBlockAlignment::Right => BlockAlignment::Right,
  }
}
