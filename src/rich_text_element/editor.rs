use std::{
  fs, io,
  ops::Range,
  path::PathBuf,
  rc::Rc,
  time::{Duration, Instant},
};

use gpui::{
  App, Bounds, ClipboardItem, Context, CursorStyle, FocusHandle, Focusable, IntoElement, KeyDownEvent, MouseButton, MouseDownEvent,
  MouseMoveEvent, MouseUpEvent, Pixels, Point, Render, Size, Subscription, Timer, Window, actions, div, point, prelude::*, px, size,
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
    ToggleUnderline,
    ToggleEmphasis,
    ClearHighlight,
    Backspace,
    Delete,
    InsertNewline,
  ]
);

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
  patch: DocumentPatch,
}

#[derive(Clone, Debug)]
struct DocumentPatch {
  before: DocumentSpan,
  after: DocumentSpan,
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
  pending_styles: Option<RunStyles>,
  selecting: bool,
  drag_granularity: SelectionGranularity,
  last_drag_position: Option<Point<Pixels>>,
  autoscroll_active: bool,
  pub(super) caret_visible: bool,
  caret_blink_active: bool,
  last_layout: Option<Rc<LayoutState>>,
  paragraph_height_cache: Vec<Option<ParagraphHeightCacheEntry>>,
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
      pending_styles: None,
      selecting: false,
      drag_granularity: SelectionGranularity::Character,
      last_drag_position: None,
      autoscroll_active: false,
      caret_visible: true,
      caret_blink_active: false,
      last_layout: None,
      paragraph_height_cache: vec![None; paragraph_count],
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

  pub fn has_unsaved_changes(&self) -> bool {
    self.edit_generation != self.saved_generation
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

  pub fn undo(&mut self, cx: &mut Context<Self>) {
    let Some(record) = self.undo_stack.pop() else {
      return;
    };
    let restored_generation = record.before_generation;
    apply_document_span_replacement(&mut self.document, &record.patch.after, &record.patch.before);
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
    apply_document_span_replacement(&mut self.document, &record.patch.before, &record.patch.after);
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

  pub fn copy(&self, cx: &mut Context<Self>) {
    if self.selection.is_caret() {
      return;
    }
    let text = selected_plain_text(&self.document, self.selection.normalized());
    let fragment = selected_rich_fragment(&self.document, self.selection.normalized());
    cx.write_to_clipboard(ClipboardItem::new_string_with_json_metadata(text, fragment));
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
    if let Some(fragment) = item
      .metadata()
      .and_then(|metadata| serde_json::from_str::<RichClipboardFragment>(metadata).ok())
      .filter(|fragment| fragment.format == "debateprocessor.rich-text-fragment.v1")
    {
      self.apply_document_edit(cx, |editor, cx| editor.insert_rich_fragment(fragment, cx));
    } else if let Some(text) = item.text() {
      self.apply_document_edit(cx, |editor, cx| editor.insert_plain_text_fragment(&text, cx));
    }
  }

  pub fn toggle_underline(&mut self, cx: &mut Context<Self>) {
    self.toggle_underline_kind(None, cx);
  }

  pub fn toggle_emphasis(&mut self, cx: &mut Context<Self>) {
    self.toggle_run_style_flag(|styles| styles.emphasis, |styles, value| styles.emphasis = value, cx);
  }

  pub fn set_highlight(&mut self, highlight: HighlightStyle, cx: &mut Context<Self>) {
    self.set_highlight_internal(Some(highlight), cx);
  }

  pub fn clear_highlight(&mut self, cx: &mut Context<Self>) {
    self.set_highlight_internal(None, cx);
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
        if let Some(paragraph) = editor.document.paragraphs.get_mut(paragraph_ix) {
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
  fn on_toggle_underline(&mut self, _: &ToggleUnderline, _: &mut Window, cx: &mut Context<Self>) {
    self.toggle_underline(cx);
  }
  fn on_toggle_emphasis(&mut self, _: &ToggleEmphasis, _: &mut Window, cx: &mut Context<Self>) {
    self.toggle_emphasis(cx);
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
    let timing = Instant::now();
    let before_selection = self.selection.clone();
    let before_paragraph_count = self.document.paragraphs.len();
    let before_range = self.edit_capture_range();
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
      patch: DocumentPatch {
        before: before_span,
        after: after_span,
      },
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
    let viewport_width = self.scroll_handle.bounds().size.width;
    let width = if viewport_width > px(1.0) { viewport_width } else { px(900.0) };
    self.ensure_exact_active_paragraph_heights(width, window, cx);
    Rc::new(
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
        .collect(),
    )
  }

  fn ensure_exact_active_paragraph_heights(&mut self, width: Pixels, window: &mut Window, cx: &mut Context<Self>) {
    for paragraph_ix in self.active_height_range() {
      let Some(paragraph) = self.document.paragraphs.get(paragraph_ix) else {
        continue;
      };
      let key = paragraph_cache_key(&self.document, paragraph);
      let cache_is_current = self
        .paragraph_height_cache
        .get(paragraph_ix)
        .and_then(|entry| *entry)
        .is_some_and(|entry| entry.key == key && entry.width == width);
      if cache_is_current {
        continue;
      }
      let layout = build_single_paragraph_layout(&self.document, paragraph_ix, width, None, window, cx);
      self.paragraph_height_cache[paragraph_ix] = Some(ParagraphHeightCacheEntry {
        key,
        width,
        height: layout.size.height,
      });
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
    cx.notify();
  }

  fn begin_visible_layout(&mut self, range: Range<usize>) -> u64 {
    self.visible_layout_generation = self.visible_layout_generation.wrapping_add(1);
    self.visible_layout_range = range.clone();
    self.visible_layout_parts = vec![None; range.end.saturating_sub(range.start)];
    self.visible_layout_generation
  }

  pub(super) fn store_visible_paragraph_layout(&mut self, generation: u64, paragraph_ix: usize, layout: &LayoutState, bounds: Bounds<Pixels>) {
    if generation != self.visible_layout_generation || !self.visible_layout_range.contains(&paragraph_ix) {
      return;
    }
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
          editor.document.clone()
        })
        .ok();
      log_timing("recovery snapshot", snapshot_timing, "");
      if let Some(document) = snapshot {
        let write_timing = Instant::now();
        let paragraph_count = document.paragraphs.len();
        let write_result = cx
          .background_executor()
          .spawn(async move { write_db8(path, &document) })
          .await;
        log_timing("recovery write", write_timing, format!("paragraphs={paragraph_count}"));
        if let Err(error) = write_result {
          eprintln!("failed to write recovery file: {error}");
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
    let Some(layout) = self.last_layout.as_ref() else {
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

    let head = self.selection.head;
    let Some(caret) = caret_bounds(layout, head, bounds.origin) else {
      cx.notify();
      return;
    };
    let target_y = match dir {
      VDir::Up => (caret.origin.y - delta).max(bounds.top()),
      VDir::Down => (caret.origin.y + delta).min(bounds.bottom()),
    };
    let target = layout.hit_test(point(caret.origin.x, target_y));
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
    let mut caret = self.selection.head;
    for (paragraph_ix, paragraph) in fragment.paragraphs.iter().enumerate() {
      if paragraph_ix > 0 {
        split_paragraph_at(&mut self.document, caret.paragraph, caret.byte);
        caret = DocumentOffset {
          paragraph: caret.paragraph + 1,
          byte: 0,
        };
        if let Some(target) = self.document.paragraphs.get_mut(caret.paragraph) {
          target.style = paragraph.style;
          bump_paragraph_version(target);
        }
      }
      for run in &paragraph.runs {
        insert_text_at(&mut self.document, caret.paragraph, caret.byte, &run.text, run.styles);
        caret.byte += run.text.len();
      }
    }
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
        explicit_direct.unwrap_or_else(|| matches!(paragraph_style, ParagraphStyle::Tag | ParagraphStyle::Analytic | ParagraphStyle::Undertag));
      let mut styles = self.styles_at_caret();
      if direct {
        styles.direct_underline = !styles.direct_underline;
      } else {
        styles.style_underline = !styles.style_underline;
      }
      self.pending_styles = Some(styles);
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }

    let range = self.selection.normalized();
    let direct = explicit_direct.unwrap_or_else(|| selection_prefers_direct_underline(&self.document, range.clone()));
    let has_any = selection_has_underline_kind(&self.document, range.clone(), direct);
    self.apply_document_edit(cx, |editor, cx| {
      mutate_runs_in_range(&mut editor.document, range, |styles| {
        if direct {
          styles.direct_underline = !has_any;
        } else {
          styles.style_underline = !has_any;
        }
      });
      editor.after_text_mutation(cx);
    });
  }

  fn toggle_run_style_flag(&mut self, get: impl Fn(RunStyles) -> bool, set: impl Fn(&mut RunStyles, bool), cx: &mut Context<Self>) {
    if self.selection.is_caret() {
      let mut styles = self.styles_at_caret();
      let new_value = !get(styles);
      set(&mut styles, new_value);
      self.pending_styles = Some(styles);
      self.reset_caret_blink(cx);
      cx.notify();
      return;
    }

    let range = self.selection.normalized();
    let has_any = selection_run_styles(&self.document, range.clone())
      .into_iter()
      .any(&get);
    self.apply_document_edit(cx, |editor, cx| {
      mutate_runs_in_range(&mut editor.document, range, |styles| set(styles, !has_any));
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
    self.apply_document_edit(cx, |editor, cx| {
      mutate_runs_in_range(&mut editor.document, range, |styles| styles.highlight = highlight);
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
    // Compute the new head while only reading from `self.last_layout`. Use a
    // local scope so we can mutate other fields afterwards without conflict.
    let (new_head, used_goal_x) = {
      let Some(layout) = self.last_layout.as_ref() else {
        return;
      };
      let Some((p_ix, l_ix)) = locate_line(layout, head) else {
        return;
      };
      let cur_line = &layout.paragraphs[p_ix].lines[l_ix];
      let cur_x = self
        .goal_x
        .unwrap_or_else(|| x_for_byte(cur_line, head.byte));
      let next = match dir {
        VDir::Up => find_line_above(layout, p_ix, l_ix),
        VDir::Down => find_line_below(layout, p_ix, l_ix),
      };
      let Some((np, nl)) = next else {
        return;
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

  // Home / End: jump to the start or end of the current visual (wrapped) line.
  // We resolve which `LaidOutLine` the caret sits on, then snap to its byte
  // range endpoints. This is why Home/End work correctly across soft wraps
  // without any renderer changes.
  fn move_line_edge(&mut self, start: bool, extend: bool, cx: &mut Context<Self>) {
    let head = self.selection.head;
    let new_byte = {
      let Some(layout) = self.last_layout.as_ref() else {
        return;
      };
      let Some((p_ix, l_ix)) = locate_line(layout, head) else {
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
    self.selecting = true;
    self.last_drag_position = Some(event.position);
    self.goal_x = None;
    let offset = self
      .last_layout
      .as_ref()
      .map(|layout| layout.hit_test(event.position))
      .unwrap_or_default();
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

  fn on_mouse_move(&mut self, event: &MouseMoveEvent, _window: &mut Window, cx: &mut Context<Self>) {
    if !self.selecting {
      return;
    }
    self.last_drag_position = Some(event.position);
    self.autoscroll_for_drag(event.position);
    self.ensure_drag_autoscroll_task(cx);
    if let Some(layout) = &self.last_layout {
      let head = layout.hit_test(event.position);
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
  }

  fn on_mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, _: &mut Context<Self>) {
    self.selecting = false;
    self.drag_granularity = SelectionGranularity::Character;
    self.last_drag_position = None;
    self.autoscroll_active = false;
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
    let Some(layout) = self.last_layout.as_ref() else {
      return;
    };
    let Some(bounds) = layout.bounds else {
      return;
    };
    let Some(caret) = caret_bounds(layout, self.selection.head, bounds.origin) else {
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
    let item_sizes = self.paragraph_item_sizes(window, cx);
    let scroll_handle = self.scroll_handle.clone();
    div()
      .size_full()
      .id("rich-text-editor")
      .relative()
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
      .on_action(cx.listener(Self::on_toggle_underline))
      .on_action(cx.listener(Self::on_toggle_emphasis))
      .on_action(cx.listener(Self::on_clear_highlight))
      .on_action(cx.listener(Self::on_backspace))
      .on_action(cx.listener(Self::on_delete))
      .on_action(cx.listener(Self::on_insert_newline))
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
        .track_scroll(&scroll_handle),
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
