use std::{
  fs, io,
  ops::Range,
  path::PathBuf,
  rc::Rc,
  time::{Duration, Instant},
};

use gpui::{
  App, Bounds, ClipboardItem, Context, CursorStyle, FocusHandle, Focusable, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
  MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, ScrollStrategy, Size, Subscription, Timer, Window, actions, div, point, prelude::*,
  px, rgb, size,
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
        byte: drop.byte.saturating_sub(source.end.byte - source.start.byte),
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

struct ItemSizesCache {
  width: Pixels,
  paragraph_count: usize,
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
  pending_styles: Option<RunStyles>,
  selecting: bool,
  drag_granularity: SelectionGranularity,
  last_drag_position: Option<Point<Pixels>>,
  pending_text_drag: Option<PendingTextDrag>,
  active_text_drag: Option<ActiveTextDrag>,
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
      selecting: false,
      drag_granularity: SelectionGranularity::Character,
      last_drag_position: None,
      pending_text_drag: None,
      active_text_drag: None,
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

  pub fn save_status(&self) -> &SaveStatus {
    &self.save_status
  }

  pub fn selection(&self) -> &EditorSelection {
    &self.selection
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
    if self.active_text_drag.is_some() {
      px(2.0)
    } else {
      px(1.0)
    }
  }

  pub fn find_text(&self, query: &str) -> Vec<Range<DocumentOffset>> {
    find_text_ranges(&self.document, query)
  }

  pub fn style_state(&self) -> RichTextEditorStyleState {
    let range = self.selection.normalized();
    let paragraph_style = selection_state_from_values((range.start.paragraph..=range.end.paragraph).filter_map(|paragraph_ix| {
      self.document.paragraphs.get(paragraph_ix).map(|paragraph| paragraph.style)
    }));

    let run_styles = if self.selection.is_caret() {
      vec![self.styles_at_caret()]
    } else {
      selection_run_styles(&self.document, range)
    };

    RichTextEditorStyleState {
      paragraph_style,
      semantic: selection_state_from_values(run_styles.iter().map(|styles| styles.semantic)),
      underline: selection_state_from_values(run_styles.iter().map(|styles| styles.direct_underline || styles.semantic == RunSemanticStyle::Underline)),
      highlight: selection_state_from_values(run_styles.iter().map(|styles| styles.highlight)),
    }
  }

  pub fn has_unsaved_changes(&self) -> bool {
    self.edit_generation != self.saved_generation
  }

  pub fn edit_generation(&self) -> u64 {
    self.edit_generation
  }

  pub fn update_document_theme(
    &mut self,
    update: impl FnOnce(&mut DocumentTheme),
    cx: &mut Context<Self>,
  ) {
    update(&mut self.document.theme);
    self.invalidate_document_theme_layout(cx);
  }

  fn invalidate_document_theme_layout(&mut self, cx: &mut Context<Self>) {
    self.last_layout = None;
    self.paragraph_layout_cache.clear();
    self.paragraph_height_cache = vec![None; self.document.paragraphs.len()];
    self.paragraph_height_cache_revision = self.paragraph_height_cache_revision.wrapping_add(1);
    self.item_sizes_cache = None;
    self.height_prefix_index = HeightPrefixIndex::default();
    cx.notify();
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
    self.apply_document_edit(cx, |editor, cx| editor.backspace(cx));
  }

  pub fn delete_forward_command(&mut self, cx: &mut Context<Self>) {
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
    if self.selection.is_caret() {
      return;
    }
    let text = selected_plain_text(&self.document, self.selection.normalized());
    let fragment = selected_rich_fragment(&self.document, self.selection.normalized());
    cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(text, fragment));
    self.paste_cache = None;
  }

  pub fn cut(&mut self, cx: &mut Context<Self>) {
    self.copy(cx);
    self.apply_document_edit(cx, |editor, cx| {
      editor.delete_selection_internal();
      editor.after_text_mutation(cx);
    });
  }

  pub fn paste(&mut self, cx: &mut Context<Self>) {
    let Some(item) = cx.read_from_clipboard() else {
      return;
    };
    if let Some(metadata) = item.metadata() {
      if let Some(PasteCache::Rich { metadata: cached_metadata, fragment }) = &self.paste_cache
        && cached_metadata == metadata
      {
        let fragment = fragment.clone();
        self.apply_document_edit(cx, |editor, cx| editor.insert_rich_fragment(fragment, cx));
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
        self.apply_document_edit(cx, |editor, cx| editor.insert_rich_fragment(fragment, cx));
        return;
      }
    }
    if let Some(text) = item.text() {
      if let Some(PasteCache::Plain { text: cached_text }) = &self.paste_cache
        && cached_text == &text
      {
        let text = cached_text.clone();
        self.apply_document_edit(cx, |editor, cx| editor.insert_plain_text_fragment(&text, cx));
        return;
      }
      self.paste_cache = Some(PasteCache::Plain { text: text.clone() });
      self.apply_document_edit(cx, |editor, cx| editor.insert_plain_text_fragment(&text, cx));
    }
  }

  pub fn toggle_underline(&mut self, cx: &mut Context<Self>) {
    self.toggle_underline_kind(None, cx);
  }

  /// Toggle any semantic inline style for the current selection or caret.
  ///
  /// The ribbon can call this generic method instead of matching each style to
  /// a shortcut-specific wrapper like `toggle_cite` or `toggle_emphasis`.
  pub fn toggle_semantic_style_for_selection(
    &mut self,
    semantic: RunSemanticStyle,
    cx: &mut Context<Self>,
  ) {
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
    self.set_highlight_internal(Some(highlight), cx);
  }

  /// Set or clear the highlight style for the current selection or caret.
  ///
  /// `None` clears highlights. `Some(...)` applies the requested highlight, or
  /// toggles it off when the whole selection already has that highlight.
  pub fn set_highlight_for_selection(
    &mut self,
    highlight: Option<HighlightStyle>,
    cx: &mut Context<Self>,
  ) {
    self.set_highlight_internal(highlight, cx);
  }

  pub fn clear_highlight(&mut self, cx: &mut Context<Self>) {
    self.set_highlight_internal(None, cx);
  }

  pub fn clear_formatting(&mut self, cx: &mut Context<Self>) {
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
  fn on_backspace(&mut self, _: &Backspace, _: &mut Window, cx: &mut Context<Self>) {
    self.backspace_command(cx);
  }
  fn on_delete(&mut self, _: &Delete, _: &mut Window, cx: &mut Context<Self>) {
    self.delete_forward_command(cx);
  }
  fn on_insert_newline(&mut self, _: &InsertNewline, _: &mut Window, cx: &mut Context<Self>) {
    self.insert_paragraph_break_command(cx);
  }
  fn on_insert_soft_line_break(&mut self, _: &InsertSoftLineBreak, _: &mut Window, cx: &mut Context<Self>) {
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
      self.insert_text_command(&key_char, cx);
    }

    #[cfg(not(target_os = "windows"))]
    {
      let _ = window;
      self.insert_text_command(key_char, cx);
    }
  }

  fn apply_document_edit(&mut self, cx: &mut Context<Self>, edit: impl FnOnce(&mut Self, &mut Context<Self>)) {
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
    self.last_layout = None;
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
      && cache.paragraph_count == self.document.paragraphs.len()
      && cache.height_revision == self.paragraph_height_cache_revision
    {
      return cache.sizes.clone();
    }
    let sizes = Rc::new(
      self
        .document
        .paragraphs
        .iter()
        .enumerate()
        .map(|(paragraph_ix, paragraph)| {
          let key = paragraph_cache_key(&self.document, paragraph);
          let height = self
            .paragraph_height_cache
            .get(paragraph_ix)
            .and_then(|entry| *entry)
            .filter(|entry| entry.key == key && entry.width == width)
            .map(|entry| entry.height)
            .unwrap_or_else(|| estimate_paragraph_item_height(&self.document, paragraph_ix, width));
          size(width, height)
        })
        .collect::<Vec<_>>(),
    );
    self.height_prefix_index.rebuild(sizes.as_ref());
    self.item_sizes_cache = Some(ItemSizesCache {
      width,
      paragraph_count: self.document.paragraphs.len(),
      height_revision: self.paragraph_height_cache_revision,
      sizes: sizes.clone(),
    });
    sizes
  }

  fn ensure_exact_interaction_paragraph_heights(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    let mut ranges = vec![self.active_height_range(), self.predicted_visible_height_range(width)];
    if !self.visible_layout_range.is_empty() {
      ranges.push(expand_paragraph_range(
        self.visible_layout_range.clone(),
        self.document.paragraphs.len(),
        2,
      ));
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
    let mut layout = self.cached_paragraph_layout(paragraph_ix, width)?.as_ref().clone();
    let viewport = self.scroll_handle.bounds();
    let row_top = if self.height_prefix_index.len() == self.document.paragraphs.len() {
      self.height_prefix_index.item_top(paragraph_ix)
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
    self.document_layout_for_paragraph(offset.paragraph, width).or_else(|| {
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
      return Some(layout.hit_test(position));
    }
    let paragraph_count = self.document.paragraphs.len();
    if paragraph_count == 0 || self.height_prefix_index.len() != paragraph_count {
      return None;
    }
    let viewport = self.scroll_handle.bounds();
    let content_y = (position.y - viewport.top() - self.scroll_handle.offset().y).max(px(0.0));
    let paragraph_ix = self.height_prefix_index.lower_bound(content_y);
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
    if self.height_prefix_index.len() == paragraph_count {
      let start = self.height_prefix_index.lower_bound((scroll_top - px(256.0)).max(px(0.0)));
      let end = (self.height_prefix_index.lower_bound(scroll_bottom) + 1).min(paragraph_count);
      return expand_paragraph_range(start..end.max(start + 1), paragraph_count, 2);
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
    if paragraph_ix >= self.document.paragraphs.len() || self.height_prefix_index.len() != self.document.paragraphs.len() {
      self.pending_snap_to_paragraph = None;
      return;
    }

    let mut offset = self.scroll_handle.offset();
    offset.y = -self.height_prefix_index.item_top(paragraph_ix);
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
    if generation != self.visible_layout_generation || !self.visible_layout_range.contains(&paragraph_ix) {
      return;
    }
    self.cache_paragraph_layout(paragraph_ix, layout.width, Rc::new(layout.clone()));
    let Some(source) = layout.paragraphs.first() else {
      return;
    };
    let mut paragraph = source.clone();
    paragraph.shift_y(bounds.origin.y + source.top);
    let part_ix = paragraph_ix - self.visible_layout_range.start;
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

  fn after_text_mutation(&mut self, cx: &mut Context<Self>) {
    self.pending_styles = None;
    self.goal_x = None;
    self.scroll_head_into_view();
    self.reset_caret_blink(cx);
    cx.notify();
  }

  fn insert_rich_fragment(&mut self, fragment: RichClipboardFragment, cx: &mut Context<Self>) {
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
    };
    self.insert_rich_fragment(fragment, cx);
  }

  fn toggle_underline_kind(&mut self, explicit_direct: Option<bool>, cx: &mut Context<Self>) {
    if self.selection.is_caret() {
      let paragraph_style = self.document.paragraphs[self.selection.head.paragraph].style;
      let direct =
        explicit_direct.unwrap_or_else(|| matches!(paragraph_style, ParagraphStyle::Tag | ParagraphStyle::Analytic));
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

  fn styles_at_caret(&self) -> RunStyles {
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
    let width = if viewport.size.width > px(1.0) {
      viewport.size.width
    } else {
      px(900.0)
    };
    self.ensure_exact_interaction_paragraph_heights(width, window, cx);
    let content_y = (position.y - viewport.top() - self.scroll_handle.offset().y).max(px(0.0));
    let paragraph_ix = if self.height_prefix_index.len() == paragraph_count {
      self.height_prefix_index.lower_bound(content_y)
    } else {
      self.selection.head.paragraph.min(paragraph_count - 1)
    };
    if let Some(layout) = self.document_layout_for_paragraph(paragraph_ix, width) {
      return layout.hit_test(position);
    }
    let layout = Rc::new(build_single_paragraph_layout(&self.document, paragraph_ix, width, None, window, cx));
    self.cache_paragraph_layout(paragraph_ix, width, layout.clone());
    let mut layout = layout.as_ref().clone();
    let row_top = if self.height_prefix_index.len() == paragraph_count {
      self.height_prefix_index.item_top(paragraph_ix)
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
    if !self.selection.is_caret() {
      self.delete_selection_internal();
      self.after_text_mutation(cx);
      return;
    }
    let caret = self.selection.head;
    if caret.byte == 0 {
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
    if !self.selection.is_caret() {
      self.delete_selection_internal();
      self.after_text_mutation(cx);
      return;
    }
    let caret = self.selection.head;
    let para_len = paragraph_text_len(&self.document.paragraphs[caret.paragraph]);
    if caret.byte == para_len {
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
    self.last_drag_position = Some(event.position);
    self.goal_x = None;
    let offset = self.hit_test_document_position(event.position, window, cx);
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
    self.reset_caret_blink(cx);
    cx.notify();
  }

  fn on_mouse_move(&mut self, event: &MouseMoveEvent, window: &mut Window, cx: &mut Context<Self>) {
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
    let selection = expand_drag_selection(&self.document, self.selection.anchor, head, self.drag_granularity);
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
    if let Some(active_drag) = self.active_text_drag.take() {
      let drop = self.hit_test_document_position(event.position, window, cx);
      self.move_rich_text_fragment(active_drag, drop, cx);
    } else if self.pending_text_drag.take().is_some() {
      let caret = self.hit_test_document_position(event.position, window, cx);
      self.selection = EditorSelection {
        anchor: caret,
        head: caret,
      };
      self.scroll_head_into_view();
      self.reset_caret_blink(cx);
      cx.notify();
    }
    self.selecting = false;
    self.drag_granularity = SelectionGranularity::Character;
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

  fn reset_caret_blink(&mut self, cx: &mut Context<Self>) {
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
      .child(
        v_virtual_list(cx.entity(), "rich-text-virtual-document", item_sizes, |editor, range, _window, cx| {
          let generation = editor.begin_visible_layout(range.clone());
          range
            .map(|paragraph_ix| VirtualParagraphElement {
              editor: cx.entity(),
              paragraph_ix,
              generation,
              layout: WordElementLayout::default(),
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
  let end = range.end.saturating_add(padding).min(paragraph_count).max(start);
  start..end
}
