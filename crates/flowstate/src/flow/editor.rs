use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use flowstate_collab::flow::{FlowDocHandle, FlowIoHandle, FlowLocalOutcome, FlowRuntime, FlowStreamItem, FlowWriteRejected};
use flowstate_flow::{AnnotationOriginator, CellId, ColumnId, FlowBoardProjection, FlowIntent, RowId, SheetId, VersionVector};
use gpui::{
  AnyElement, App, Bounds, Context, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent, KeyUpEvent,
  MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render, ScrollHandle, ScrollWheelEvent, SharedString, Subscription, Task, Window,
  canvas, div, point, prelude::*, px, rgba,
};
use gpui_component::ActiveTheme as _;
use gpui_component::PixelsExt as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use gpui_component::{IconName, Sizable as _};

use crate::{
  app_settings::load_document_theme,
  flow::{cell_theme::apply_flow_cell_theme, flow_side_palette},
  rich_text_element::{RichTextDocumentElement, RichTextEditor},
};

mod annotation;
mod cell_editing;
mod grid_layout;
mod grid_nav;
mod preview;
mod zoom;

pub(crate) use preview::{FlowPreview, render_flow_board_preview};

pub use grid_nav::{GridDirection, RelativePosition};

use annotation::paint_stroke;
use grid_layout::{BOARD_PADDING, CellMeasurement, GUTTER_WIDTH, GridLayout, HEADER_HEIGHT, MIN_COLUMN_WIDTH};

/// A point in BOARD MODEL space (grid-origin relative, zoom 1). Render-side
/// only — the schema's stroke geometry is stroke-local (rigid-body law).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub(crate) struct BoardPoint {
  pub x: f32,
  pub y: f32,
}

#[derive(Clone, Debug)]
pub enum FlowEditorEvent {
  Changed,
  ActiveCellChanged(Option<CellId>),
  ActiveSheetChanged(Option<SheetId>),
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum AnnotationTool {
  #[default]
  None,
  Marker,
  Eraser,
}

/// R6: the replay scrubber's state — a historical board checked out on a
/// fork, never the live doc.
struct FlowScrubber {
  fraction: f32,
  board: FlowBoardProjection,
  shown_ops: usize,
  total_ops: usize,
  /// The checked-out position's frontier — Restore's input (any thumb
  /// position restores, not just marks).
  frontier: Vec<u8>,
  /// Tape marks: the checkpoint records at their timeline positions.
  marks: Vec<crate::history_tape::TapeMark>,
  mark_frontiers: std::collections::HashMap<u128, Vec<u8>>,
  selected_mark: Option<u128>,
}

/// This replica's own board focus (spec S11) as published into presence.
pub struct FlowPresenceSnapshot {
  pub sheet: Option<SheetId>,
  pub cell: Option<CellId>,
  pub editing: bool,
  /// (head, anchor) encoded Loro cursors within the focused cell's text.
  pub caret: Option<(Vec<u8>, Vec<u8>)>,
}

/// A peer's hand on the board (spec S11): rendered as a presence ring +
/// name chip on their focused cell, and as a colored dot on the sheet
/// switcher when they're on another sheet.
#[derive(Clone, Debug)]
pub struct FlowExternalPresence {
  pub key: String,
  pub name: SharedString,
  pub color_rgb: u32,
  pub sheet: Option<SheetId>,
  pub cell: Option<CellId>,
  pub editing: bool,
}

/// A spoken refusal (F3): message toast + optional cell shake.
struct RefusalNotice {
  message: String,
  cell: Option<CellId>,
  at: std::time::Instant,
}

struct PanDragState {
  pointer_anchor: gpui::Point<gpui::Pixels>,
  scroll_anchor: gpui::Point<gpui::Pixels>,
  pending_position: gpui::Point<gpui::Pixels>,
  frame_scheduled: bool,
}

// ---- drag payloads (slot drops are geometry, not interpretation) -----------

#[derive(Clone)]
pub(super) struct FlowCellDrag {
  pub cell_id: CellId,
}

pub(super) struct FlowCellDragPreview {
  label: SharedString,
  fill: gpui::Hsla,
  text_color: gpui::Hsla,
}

impl Render for FlowCellDragPreview {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    // A faithful mini-card: the cell's own fill and text color, lifted off the
    // sheet with a soft shadow so it reads as "picked up," not a foreign chip.
    div()
      .px(px(8.0))
      .py(px(5.0))
      .rounded(px(4.0))
      .bg(self.fill)
      .border_1()
      .border_color(cx.theme().primary)
      .shadow_lg()
      .text_size(px(12.0))
      .text_color(self.text_color)
      .max_w(px(260.0))
      .overflow_hidden()
      .child(self.label.clone())
  }
}

#[derive(Clone)]
pub(super) struct RowDrag {
  pub row_id: RowId,
}

struct RowDragPreview {
  label: SharedString,
}

impl Render for RowDragPreview {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .px_2()
      .py_0p5()
      .rounded(px(4.0))
      .bg(cx.theme().popover.opacity(0.92))
      .border_1()
      .border_color(cx.theme().border)
      .text_size(px(11.0))
      .text_color(cx.theme().muted_foreground)
      .child(self.label.clone())
  }
}

#[derive(Clone)]
pub(super) struct ColumnDrag {
  pub column_id: ColumnId,
}

struct ColumnDragPreview {
  label: SharedString,
}

impl Render for ColumnDragPreview {
  fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .px_3()
      .py_1()
      .rounded(px(4.0))
      .bg(cx.theme().popover.opacity(0.92))
      .border_1()
      .border_color(cx.theme().border)
      .text_size(px(12.0))
      .child(self.label.clone())
  }
}

#[derive(Clone)]
pub(super) struct ColumnResizeDrag {
  pub column_id: ColumnId,
  pub start_width: f32,
}

struct EmptyDragPreview;

impl Render for EmptyDragPreview {
  fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
    gpui::Empty
  }
}

struct ColumnResizeState {
  column_id: ColumnId,
  start_width: f32,
  start_x: Option<f32>,
  live_width: f32,
}

pub struct FlowEditor {
  /// THE write authority (spec invariant 5): solo and collaborative flows
  /// receive this identical gated handle. Every mutation is a [`FlowIntent`]
  /// applied through it; the editor never holds document state of its own.
  handle: Arc<FlowDocHandle>,
  /// The flow's background I/O service (saves, recovery encodes, and — once a
  /// session attaches — transport pulls).
  io: FlowIoHandle,
  /// Drained copy of the runtime's board projection — THE render source.
  board: FlowBoardProjection,
  /// Render cache of per-cell projections (populated lazily during paint,
  /// invalidated whenever the underlying cell content may have changed).
  cell_documents: std::cell::RefCell<std::collections::HashMap<CellId, flowstate_document::DocumentProjection>>,
  /// Cached (`can_undo`, `can_redo`) so render never takes the gate.
  undo_state: (bool, bool),
  path: Option<PathBuf>,
  dirty: bool,
  active_sheet: Option<SheetId>,
  active_cell: Option<CellId>,
  /// The Excel cursor: a (row, column) slot over the extended grid (real
  /// rows + the ghost run). `active_cell` is the occupant when there is one.
  cursor: Option<(usize, usize)>,
  collapsed_outline_items: HashSet<uuid::Uuid>,
  annotation_tool: AnnotationTool,
  hidden_annotation_sheets: HashSet<SheetId>,
  hidden_annotation_originators: HashSet<AnnotationOriginator>,
  local_annotation_originator: AnnotationOriginator,
  /// I-S2: the pen's current color (theme-derived swatches on the marker
  /// chip). Flowing tradition is color-coded pens.
  marker_color_rgba: u32,
  /// The in-progress ink stroke in board model space (grid-origin relative);
  /// committed as a rigid-body grid-anchored stroke (D6).
  drawing_points: Vec<BoardPoint>,
  cell_editors: std::collections::HashMap<CellId, Entity<RichTextEditor>>,
  cell_editor_themes: std::collections::HashMap<CellId, (gpui::Hsla, gpui::Hsla, u32)>,
  cell_editor_subscriptions: std::collections::HashMap<CellId, Subscription>,
  /// Live cell drag: the dragged card and the slot under the pointer.
  dragging_cell: Option<CellId>,
  drag_target: Option<(usize, usize)>,
  /// Live row drag: gap index (0..=rows) the bar renders at + its anchor.
  dragging_row: Option<RowId>,
  row_drop_gap: Option<usize>,
  /// Live column drag: gap index (0..=columns).
  dragging_column: Option<ColumnId>,
  column_drop_gap: Option<usize>,
  column_resize: Option<ColumnResizeState>,
  /// W2 multi-select: the set every op applies to (always contains the
  /// active cell while non-empty).
  selected_cells: HashSet<CellId>,
  /// W2 range grammar: the (row, column) a shift-extend measures its rectangle
  /// from. Set on plain click / ctrl-toggle; untouched by a shift-extend.
  selection_anchor: Option<(usize, usize)>,
  refusal: Option<RefusalNotice>,
  /// `FLOWSTATE_INTENT_LOG` debug overlay: the last few intents + outcomes.
  intent_log: std::collections::VecDeque<SharedString>,
  /// True while a live collaboration session drains the publish queue; the
  /// editor must NOT sink it then (S9 — the session is the drainer).
  session_attached: bool,
  /// S10: pathless collaboration tabs write a debounced `.fl0` recovery file
  /// here so a crash never loses a joined flow.
  recovery_path: Option<PathBuf>,
  recovery_write_pending: bool,
  /// S11: remote hands on the board.
  external_presences: Vec<FlowExternalPresence>,
  /// R6 scrubber: a read-only historical board under the replay slider.
  scrubber: Option<FlowScrubber>,
  /// Painted bounds of the history tape's track (drag math reads it back).
  tape_bounds: std::rc::Rc<std::cell::Cell<Option<Bounds<gpui::Pixels>>>>,
  cell_bounds: std::collections::HashMap<CellId, Bounds<gpui::Pixels>>,
  cell_measurements: std::collections::HashMap<CellId, CellMeasurement>,
  /// Scroll offset the last element build saw — the frozen chrome resyncs
  /// (one notify) when paint observes a different offset.
  built_scroll_offset: std::cell::Cell<gpui::Point<gpui::Pixels>>,
  /// The viewport size the element tree was last built for — a window
  /// resize must rebuild the visible-row window just like a scroll does.
  built_viewport: std::cell::Cell<gpui::Size<gpui::Pixels>>,
  board_scroll: ScrollHandle,
  board_zoom: f32,
  camera_center: Option<BoardPoint>,
  camera_apply_pending: bool,
  viewport_origin: BoardPoint,
  space_pan_armed: bool,
  pan_drag: Option<PanDragState>,
  /// G: a right-button drag is inking (no tool armed). The active stroke draws
  /// in `active_ink_color` — the user's profile color — instead of the ribbon
  /// marker color.
  right_inking: bool,
  active_ink_color: Option<u32>,
  /// A pan that actually moved swallows the click that would otherwise fire on
  /// release — so dragging to pan never selects/edits the cell you started on.
  suppress_click: bool,
  focus_handle: FocusHandle,
}

impl FlowEditor {
  /// Wire the editor onto a live gated runtime (spec S8): `handle` is the one
  /// write authority, `io` the flow's I/O service over the same gate. Solo
  /// open and collaborative join both land here.
  pub fn new_with_runtime(
    handle: Arc<FlowDocHandle>,
    io: FlowIoHandle,
    path: Option<PathBuf>,
    _window: &mut Window,
    cx: &mut Context<Self>,
  ) -> Self {
    let board = handle.board_projection().unwrap_or_default();
    let undo_state = handle.can_undo().unwrap_or((false, false));
    let active_sheet = board.sheets.first().map(|sheet| sheet.id);
    let collapsed_outline_items = board.sheets.iter().map(|sheet| sheet.id).collect();
    Self {
      handle,
      io,
      board,
      cell_documents: std::cell::RefCell::new(std::collections::HashMap::new()),
      undo_state,
      path,
      dirty: false,
      active_sheet,
      active_cell: None,
      cursor: None,
      collapsed_outline_items,
      annotation_tool: AnnotationTool::None,
      hidden_annotation_sheets: HashSet::new(),
      hidden_annotation_originators: HashSet::new(),
      // I-S1: strokes author under the durable user identity.
      local_annotation_originator: AnnotationOriginator(crate::app_settings::load_local_user_identity().0.to_string()),
      marker_color_rgba: 0xf59e_0bff,
      drawing_points: Vec::new(),
      cell_editors: std::collections::HashMap::new(),
      cell_editor_themes: std::collections::HashMap::new(),
      cell_editor_subscriptions: std::collections::HashMap::new(),
      dragging_cell: None,
      drag_target: None,
      dragging_row: None,
      row_drop_gap: None,
      dragging_column: None,
      column_drop_gap: None,
      column_resize: None,
      selected_cells: HashSet::new(),
      selection_anchor: None,
      refusal: None,
      intent_log: std::collections::VecDeque::new(),
      session_attached: false,
      recovery_path: None,
      recovery_write_pending: false,
      external_presences: Vec::new(),
      scrubber: None,
      tape_bounds: std::rc::Rc::default(),
      cell_bounds: std::collections::HashMap::new(),
      cell_measurements: std::collections::HashMap::new(),
      built_scroll_offset: std::cell::Cell::new(point(px(0.0), px(0.0))),
      built_viewport: std::cell::Cell::new(gpui::size(px(0.0), px(0.0))),
      board_scroll: ScrollHandle::new(),
      board_zoom: 1.0,
      camera_center: None,
      camera_apply_pending: false,
      viewport_origin: BoardPoint::default(),
      space_pan_armed: false,
      pan_drag: None,
      right_inking: false,
      active_ink_color: None,
      suppress_click: false,
      focus_handle: cx.focus_handle(),
    }
  }

  pub fn blank(window: &mut Window, cx: &mut Context<Self>) -> Self {
    let (handle, gate) = FlowDocHandle::new(FlowRuntime::new_empty());
    let io = FlowIoHandle::spawn(gate).expect("flow I/O service spawns");
    Self::new_with_runtime(Arc::new(handle), io, None, window, cx)
  }

  pub fn handle(&self) -> &Arc<FlowDocHandle> {
    &self.handle
  }

  pub fn io(&self) -> &FlowIoHandle {
    &self.io
  }

  /// The drained board projection — THE render source (never take the gate
  /// during paint).
  pub fn board(&self) -> &FlowBoardProjection {
    &self.board
  }

  /// A cell's full rich-text projection, lazily materialized through the
  /// authority and cached until the next content-bearing change.
  fn cell_document(&self, cell_id: CellId) -> Option<flowstate_document::DocumentProjection> {
    if let Some(document) = self.cell_documents.borrow().get(&cell_id) {
      return Some(document.clone());
    }
    let document = self.handle.cell_projection(cell_id).ok()?;
    self
      .cell_documents
      .borrow_mut()
      .insert(cell_id, document.clone());
    Some(document)
  }

  /// The `FLOWSTATE_INTENT_LOG` debug overlay: alpha field calibration wants
  /// to SEE what each gesture committed.
  fn intent_log_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("FLOWSTATE_INTENT_LOG").is_ok_and(|value| !matches!(value.trim(), "" | "0" | "false")))
  }

  pub(super) fn log_intent(&mut self, entry: String) {
    if !Self::intent_log_enabled() {
      return;
    }
    self.intent_log.push_back(entry.into());
    while self.intent_log.len() > 10 {
      self.intent_log.pop_front();
    }
  }

  /// Apply one structural intent through the authority and integrate the
  /// synchronous outcome (board copy, stream drains, undo state, publish).
  pub(super) fn apply_intent(&mut self, intent: &FlowIntent, cx: &mut Context<Self>) -> Result<FlowLocalOutcome, FlowWriteRejected> {
    let class = intent.class();
    let outcome = self.handle.apply(intent);
    self.log_intent(match &outcome {
      Ok(_) => format!("{class} ✓"),
      Err(error) => format!("{class} ✗ {error}"),
    });
    let outcome = outcome?;
    self.board = outcome.board.clone();
    for cell in &outcome.content_cells {
      self.cell_documents.borrow_mut().remove(cell);
      if let Some(editor) = self.cell_editors.get(cell).cloned() {
        editor.update(cx, |editor, cx| editor.sync_projection_from_authority(cx));
      }
    }
    self.after_local_change(cx);
    Ok(outcome)
  }

  /// Post-change bookkeeping shared by every mutation path.
  fn after_local_change(&mut self, cx: &mut Context<Self>) {
    if let Ok(items) = self.handle.drain_board_stream()
      && let Some(FlowStreamItem::Board(board)) = items.into_iter().next_back()
    {
      self.board = *board;
    }
    self.undo_state = self.handle.can_undo().unwrap_or((false, false));
    self.prune_dead_cells(cx);
    self.sink_publish_queue();
    self.schedule_recovery_write(cx);
  }

  fn sink_publish_queue(&self) {
    if self.session_attached {
      return; // the live session drains + broadcasts the queue
    }
    use flowstate_collab::local_write::GateHolder;
    if let Ok(mut guard) = self.handle.gate().lock(GateHolder::DocumentService) {
      let _ = guard.take_pending_publish();
    }
  }

  /// S9: a live collaboration session attached (or detached).
  pub fn set_session_attached(&mut self, attached: bool, cx: &mut Context<Self>) {
    self.session_attached = attached;
    cx.notify();
  }

  /// S9: the session applied remote updates through the flow I/O service —
  /// refresh everything from the runtime.
  pub fn on_remote_updates_applied(&mut self, cx: &mut Context<Self>) {
    self.resync_from_runtime(cx);
    cx.notify();
  }

  /// S11: this replica's own presence focus.
  pub fn presence_focus(&self, cx: &App) -> FlowPresenceSnapshot {
    let editing = self
      .active_cell
      .is_some_and(|cell| self.cell_editors.contains_key(&cell));
    let caret = self.active_cell.and_then(|cell| {
      use crate::rich_text_element::LocalWriteAuthority as _;
      let editor = self.cell_editors.get(&cell)?.read(cx);
      let selection = editor.selection().clone();
      let frontier = editor.document().frontier.clone();
      self
        .handle
        .cell_authority(cell)
        .encode_selection_anchor(&selection, &frontier)
    });
    FlowPresenceSnapshot {
      sheet: self.active_sheet,
      cell: self.active_cell,
      editing,
      caret,
    }
  }

  /// S11: install remote hands + forward exact peer carets into any OPEN cell
  /// editors (resolved from Loro cursor bytes under the flow gate).
  pub fn set_external_presences(
    &mut self,
    hands: Vec<FlowExternalPresence>,
    carets: Vec<(CellId, u32, Vec<u8>, Vec<u8>)>,
    cx: &mut Context<Self>,
  ) {
    use crate::rich_text_element::LocalWriteAuthority as _;
    self.external_presences = hands;
    let mut by_cell: std::collections::HashMap<CellId, Vec<crate::rich_text_element::ExternalCaret>> = std::collections::HashMap::new();
    for (cell, color_rgb, head, anchor) in carets {
      if let Some((head_offset, _)) = self
        .handle
        .cell_authority(cell)
        .resolve_selection_anchor(&head, &anchor)
      {
        by_cell
          .entry(cell)
          .or_default()
          .push(crate::rich_text_element::ExternalCaret {
            offset: head_offset,
            visual_gravity: crate::rich_text_element::VisualGravity::Downstream,
            color_rgb,
          });
      }
    }
    for (cell, editor) in &self.cell_editors {
      let carets = by_cell.remove(cell).unwrap_or_default();
      editor.update(cx, |editor, cx| editor.set_external_carets(carets, cx));
    }
    cx.notify();
  }

  pub fn external_presences(&self) -> &[FlowExternalPresence] {
    &self.external_presences
  }

  /// R6: toggle the replay scrubber (read-only checkout over the oplog).
  pub fn toggle_history_scrubber(&mut self, cx: &mut Context<Self>) {
    if self.scrubber.take().is_some() {
      cx.notify();
      return;
    }
    self.set_scrubber_fraction(1.0, cx);
  }

  pub fn history_scrubbing(&self) -> bool {
    self.scrubber.is_some()
  }

  fn set_scrubber_fraction(&mut self, fraction: f32, cx: &mut Context<Self>) {
    match self.handle.history_board_at(fraction) {
      Ok((board, shown_ops, total_ops, frontier)) => {
        let (marks, mark_frontiers) = self
          .scrubber
          .take()
          .map_or_else(|| self.load_tape_marks(), |scrubber| (scrubber.marks, scrubber.mark_frontiers));
        self.scrubber = Some(FlowScrubber {
          fraction: fraction.clamp(0.0, 1.0),
          board,
          shown_ops,
          total_ops,
          frontier,
          marks,
          mark_frontiers,
          selected_mark: None,
        });
        cx.notify();
      },
      Err(error) => {
        tracing::warn!(%error, "flow history checkout failed");
        self.refuse(format!("history replay failed: {error}"), None, cx);
      },
    }
  }

  /// H-S6: the checkpoint records positioned on the replay timeline.
  fn load_tape_marks(&self) -> (Vec<crate::history_tape::TapeMark>, std::collections::HashMap<u128, Vec<u8>>) {
    let Ok(checkpoints) = self.handle.flow_checkpoints() else {
      return (Vec::new(), std::collections::HashMap::new());
    };
    let frontiers: Vec<Vec<u8>> = checkpoints.iter().map(|checkpoint| checkpoint.frontier.clone()).collect();
    let positions = self
      .handle
      .history_timeline_positions(&frontiers)
      .unwrap_or_else(|_| vec![None; checkpoints.len()]);
    let mut marks = Vec::new();
    let mut mark_frontiers = std::collections::HashMap::new();
    for (checkpoint, position) in checkpoints.into_iter().zip(positions) {
      let Some(position) = position else { continue };
      mark_frontiers.insert(checkpoint.checkpoint_id, checkpoint.frontier.clone());
      marks.push(crate::history_tape::TapeMark {
        id: checkpoint.checkpoint_id,
        position,
        kind: checkpoint.kind,
        title: checkpoint.title.into(),
      });
    }
    (marks, mark_frontiers)
  }

  /// H-S6: a mark is a landmark — clicking one checks out its EXACT frontier.
  fn checkout_tape_mark(&mut self, mark_id: u128, cx: &mut Context<Self>) {
    let Some(scrubber) = &self.scrubber else { return };
    let Some(frontier) = scrubber.mark_frontiers.get(&mark_id).cloned() else {
      return;
    };
    let position = scrubber
      .marks
      .iter()
      .find(|mark| mark.id == mark_id)
      .map_or(1.0, |mark| mark.position);
    match self.handle.board_at_frontier(&frontier) {
      Ok(board) => {
        if let Some(scrubber) = self.scrubber.as_mut() {
          scrubber.board = board;
          scrubber.fraction = position;
          scrubber.frontier = frontier;
          scrubber.selected_mark = Some(mark_id);
        }
        cx.notify();
      },
      Err(error) => {
        tracing::warn!(%error, "flow checkpoint checkout failed");
        self.refuse(format!("checkpoint checkout failed: {error}"), None, cx);
      },
    }
  }

  /// H-S6 restore, from wherever the tape sits (the .db8 law rides below).
  fn restore_scrubber_position(&mut self, cx: &mut Context<Self>) {
    let Some(frontier) = self.scrubber.as_ref().map(|scrubber| scrubber.frontier.clone()) else {
      return;
    };
    match self.handle.restore_flow_frontier(&frontier) {
      Ok(()) => {
        self.scrubber = None;
        self.resync_from_runtime(cx);
        cx.notify();
      },
      Err(error) => {
        tracing::warn!(%error, "flow restore failed");
        self.refuse(format!("restore failed: {error}"), None, cx);
      },
    }
  }

  /// H-S6: pin the present as a named moment (tape marks refresh).
  fn pin_present_moment(&mut self, cx: &mut Context<Self>) {
    match self.handle.create_flow_checkpoint(None) {
      Ok(_) => {
        if self.scrubber.is_some() {
          let (marks, mark_frontiers) = self.load_tape_marks();
          if let Some(scrubber) = self.scrubber.as_mut() {
            scrubber.marks = marks;
            scrubber.mark_frontiers = mark_frontiers;
          }
        }
        cx.notify();
      },
      Err(error) => {
        tracing::warn!(%error, "flow checkpoint failed");
        self.refuse(format!("checkpoint failed: {error}"), None, cx);
      },
    }
  }

  /// Tape scrub with landmark magnetism.
  fn scrub_tape_to(&mut self, fraction: f32, cx: &mut Context<Self>) {
    const SNAP: f32 = 0.015;
    let snapped = self.scrubber.as_ref().and_then(|scrubber| {
      scrubber
        .marks
        .iter()
        .find(|mark| (mark.position - fraction).abs() <= SNAP)
        .map(|mark| mark.id)
    });
    match snapped {
      Some(mark_id) => self.checkout_tape_mark(mark_id, cx),
      None => self.set_scrubber_fraction(fraction, cx),
    }
  }

  /// The read-only replay view: the historical grid's summaries in column
  /// order + the scrub bar. No editors, no drags — replay only (R6).
  fn render_history_view(&mut self, cx: &mut Context<Self>) -> AnyElement {
    let Some(scrubber) = &self.scrubber else {
      return div().into_any_element();
    };
    let zoom = self.board_zoom;
    let sheet = self
      .active_sheet
      .and_then(|sheet_id| {
        scrubber
          .board
          .sheets
          .iter()
          .find(|sheet| sheet.id == sheet_id)
      })
      .or_else(|| scrubber.board.sheets.first());
    let fraction = scrubber.fraction;
    let tape_marks = scrubber.marks.clone();
    let selected_mark = scrubber.selected_mark;
    let label: SharedString = format!("history · {} / {} ops", scrubber.shown_ops, scrubber.total_ops).into();
    let weak = cx.entity().downgrade();
    let columns: Vec<AnyElement> = sheet
      .map(|sheet| {
        sheet
          .columns
          .iter()
          .map(|column| {
            let side = flow_side_palette(column.side, cx);
            div()
              .w(px(column.width.unwrap_or(280.0) * zoom))
              .flex_none()
              .flex_col()
              .gap(px(8.0 * zoom))
              .bg(side.base.opacity(0.035))
              .rounded(px(9.0 * zoom))
              .p(px(8.0 * zoom))
              .child(
                div()
                  .font_weight(gpui::FontWeight::BOLD)
                  .text_color(side.base)
                  .text_size(px(13.0 * zoom))
                  .child(SharedString::from(column.label.clone())),
              )
              .children(sheet.cells().filter(|cell| cell.column_id == column.id).map(|cell| {
                div()
                  .w_full()
                  .p(px(8.0 * zoom))
                  .rounded(px(9.0 * zoom))
                  .bg(side.base.opacity(0.08))
                  .text_size(px(12.0 * zoom))
                  .text_color(
                    cx.theme()
                      .foreground
                      .opacity(if cell.summary.struck { 0.45 } else { 0.9 }),
                  )
                  .when(cell.summary.struck, |this| this.line_through())
                  .child(SharedString::from(cell.summary.summary_text.to_string()))
                  .into_any_element()
              }))
              .into_any_element()
          })
          .collect()
      })
      .unwrap_or_default();
    div()
      .size_full()
      .flex()
      .flex_col()
      .child(
        div().flex_1().overflow_hidden().child(
          div()
            .flex()
            .gap(px(16.0 * zoom))
            .p(px(16.0 * zoom))
            .children(columns),
        ),
      )
      .child(
        div()
          .flex_none()
          .h(px(44.0))
          .px_4()
          .flex()
          .items_center()
          .gap_3()
          .bg(cx.theme().popover.opacity(0.9))
          .border_t_1()
          .border_color(cx.theme().border)
          .child(
            div()
              .text_size(px(11.0))
              .text_color(cx.theme().muted_foreground)
              .whitespace_nowrap()
              .child(label),
          )
          .child(crate::history_tape::history_tape(
            "flow-history-tape",
            fraction,
            tape_marks,
            selected_mark,
            self.tape_bounds.clone(),
            {
              let weak = weak.clone();
              std::rc::Rc::new(move |fraction, _window, cx| {
                let _ = weak.update(cx, |editor, cx| editor.scrub_tape_to(fraction, cx));
              })
            },
            {
              let weak = weak.clone();
              std::rc::Rc::new(move |mark_id, _window, cx| {
                let _ = weak.update(cx, |editor, cx| editor.checkout_tape_mark(mark_id, cx));
              })
            },
            cx,
          ))
          .child(
            gpui_component::button::Button::new("flow-history-restore")
              .primary()
              .xsmall()
              .label("Restore")
              .tooltip("Bring this moment back as a new edit — the present is pinned first, and the restore is undoable")
              .on_click(cx.listener(|editor, _, _, cx| editor.restore_scrubber_position(cx))),
          )
          .child(
            gpui_component::button::Button::new("flow-history-pin")
              .xsmall()
              .label("Pin now")
              .tooltip("Pin the present as a named checkpoint mark")
              .on_click(cx.listener(|editor, _, _, cx| editor.pin_present_moment(cx))),
          )
          .child(
            gpui_component::button::Button::new("flow-history-exit")
              .xsmall()
              .label("Exit")
              .on_click(cx.listener(|editor, _, _, cx| editor.toggle_history_scrubber(cx))),
          ),
      )
      .into_any_element()
  }

  /// Test-only view of an open cell editor (headless presence assertions).
  pub fn cell_editor_for_test(&self, cell_id: CellId) -> Option<&Entity<RichTextEditor>> {
    self.cell_editors.get(&cell_id)
  }

  /// Drop editors/caches for cells that no longer exist on the board.
  fn prune_dead_cells(&mut self, _cx: &mut Context<Self>) {
    let live: HashSet<CellId> = self
      .board
      .sheets
      .iter()
      .flat_map(|sheet| sheet.cells().map(|cell| cell.id))
      .collect();
    self.cell_editors.retain(|id, _| live.contains(id));
    self.cell_editor_themes.retain(|id, _| live.contains(id));
    self
      .cell_editor_subscriptions
      .retain(|id, _| live.contains(id));
    self.cell_bounds.retain(|id, _| live.contains(id));
    self.cell_measurements.retain(|id, _| live.contains(id));
    self
      .cell_documents
      .borrow_mut()
      .retain(|id, _| live.contains(id));
    if let Some(active) = self.active_cell
      && !live.contains(&active)
    {
      self.active_cell = None;
    }
    self.selected_cells.retain(|id| live.contains(id));
  }

  /// Refresh EVERYTHING from the runtime after a change whose cell footprint
  /// is unknown (undo/redo, remote imports).
  fn resync_from_runtime(&mut self, cx: &mut Context<Self>) {
    if let Ok(board) = self.handle.board_projection() {
      self.board = board;
    }
    let _ = self.handle.drain_board_stream();
    self.cell_documents.borrow_mut().clear();
    self.prune_dead_cells(cx);
    for editor in self.cell_editors.values() {
      editor.update(cx, |editor, cx| editor.sync_projection_from_authority(cx));
    }
    self.undo_state = self.handle.can_undo().unwrap_or((false, false));
    self.sink_publish_queue();
  }

  pub fn version_vector(&self) -> VersionVector {
    use flowstate_collab::local_write::GateHolder;
    self
      .handle
      .gate()
      .lock(GateHolder::DocumentService)
      .map(|guard| guard.oplog_vv())
      .unwrap_or_default()
  }

  /// Import remote flow updates (transitional wiring until the session enum
  /// split lands — live sessions import through [`FlowIoHandle`]).
  pub fn import_collaboration_updates(&mut self, bytes: &[u8], cx: &mut Context<Self>) -> anyhow::Result<()> {
    self
      .handle
      .import_remote_updates(&[bytes])
      .map_err(|error| anyhow::anyhow!(error.to_string()))?;
    self.resync_from_runtime(cx);
    self.changed(self.active_cell, cx);
    Ok(())
  }

  pub fn active_sheet(&self) -> Option<SheetId> {
    self.active_sheet
  }

  pub fn active_cell(&self) -> Option<CellId> {
    self.active_cell
  }

  /// Test accessor: the width the active cell's editor is laying its text out
  /// at. Guards that `ensure_cell_editor` seeds the column's content width — if
  /// the seed is dropped this reads the ~900px+ unmeasured fallback instead of
  /// the narrow column box, which is exactly the focus-shifts-the-row bug.
  pub fn active_cell_layout_width(&self, cx: &App) -> Option<gpui::Pixels> {
    let editor = self.cell_editors.get(&self.active_cell?)?;
    editor.read(cx).benchmark_measured_item_width()
  }

  /// Test helper: forget a cell's built editor so the next `activate_cell`
  /// rebuilds it FRESH — the exact first-focus path (before any render pass has
  /// settled its width to the cell bounds). Lets a headless test observe the
  /// seed on the frame the user actually sees the shift.
  pub fn benchmark_forget_cell_editor(&mut self, cell_id: CellId) {
    self.cell_editors.remove(&cell_id);
    self.cell_editor_subscriptions.remove(&cell_id);
    self.cell_editor_themes.remove(&cell_id);
    if self.active_cell == Some(cell_id) {
      self.active_cell = None;
    }
  }

  /// Enter a cell for editing: focus its editor with the caret at the END of
  /// its text, so typing appends (Excel muscle memory) rather than prepending.
  pub fn focus_active_cell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    cx.on_next_frame(window, |flow, window, cx| {
      let Some(editor) = flow
        .active_cell
        .and_then(|cell| flow.cell_editors.get(&cell))
        .cloned()
      else {
        return;
      };
      editor.read(cx).focus_handle(cx).focus(window);
      editor.update(cx, |editor, cx| editor.move_document_end(cx));
    });
  }

  /// Focus whatever the grid cursor now sits on after a keyboard move: the
  /// active cell's editor for an occupied slot, or the board itself for an
  /// empty slot (so typing there materializes a cell, matching an empty-slot
  /// click). Used by Enter/Backspace navigation, which can cross empty slots.
  pub fn focus_cursor_slot(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if self.active_cell.is_some() {
      self.focus_active_cell(window, cx);
    } else {
      self.focus_handle.focus(window);
    }
  }

  pub fn annotation_tool(&self) -> AnnotationTool {
    self.annotation_tool
  }

  pub fn annotations_visible(&self) -> bool {
    self
      .active_sheet
      .is_some_and(|sheet| !self.hidden_annotation_sheets.contains(&sheet))
  }

  pub fn document_path(&self) -> Option<&PathBuf> {
    self.path.as_ref()
  }

  pub fn set_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
    self.path = Some(path);
    cx.notify();
  }

  pub fn has_unsaved_changes(&self) -> bool {
    self.dirty
  }

  pub fn activate_sheet(&mut self, sheet_id: SheetId, cx: &mut Context<Self>) {
    if self.board.sheets.iter().any(|sheet| sheet.id == sheet_id) {
      self.active_sheet = Some(sheet_id);
      self.active_cell = None;
      self.cursor = None;
      cx.emit(FlowEditorEvent::ActiveSheetChanged(Some(sheet_id)));
      cx.notify();
    }
  }

  pub fn activate_cell(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    let located = self.board.sheets.iter().find_map(|sheet| {
      sheet
        .find_cell(cell_id)
        .map(|cell| (sheet.id, cell.summary.uses_summary_projection, sheet.cell_position(cell_id)))
    });
    if let Some((sheet_id, uses_summary_projection, position)) = located {
      if !uses_summary_projection
        && self
          .apply_intent(&FlowIntent::EnsureCellEditable { sheet_id, cell_id }, cx)
          .is_ok()
      {
        self.dirty = true;
        cx.emit(FlowEditorEvent::Changed);
      }
      self.ensure_cell_editor(cell_id, cx);
      self.active_sheet = Some(sheet_id);
      self.active_cell = Some(cell_id);
      if let Some(position) = position {
        self.cursor = Some(position);
      }
      // Selecting/activating never snaps the viewport — only keyboard
      // navigation scrolls (so an off-screen arrow-move stays visible).
      cx.emit(FlowEditorEvent::ActiveCellChanged(Some(cell_id)));
      cx.notify();
    }
  }

  pub fn outline_item_expanded(&self, id: uuid::Uuid) -> bool {
    !self.collapsed_outline_items.contains(&id)
  }

  pub fn toggle_outline_item(&mut self, id: uuid::Uuid, cx: &mut Context<Self>) {
    if !self.collapsed_outline_items.remove(&id) {
      self.collapsed_outline_items.insert(id);
    }
    cx.notify();
  }

  pub fn active_cell_is_struck(&self) -> bool {
    self
      .active_sheet_ref()
      .zip(self.active_cell)
      .and_then(|(sheet, cell)| sheet.find_cell(cell))
      .is_some_and(|cell| cell.summary.struck)
  }

  pub fn active_cell_is_empty(&self) -> bool {
    self
      .active_sheet_ref()
      .zip(self.active_cell)
      .and_then(|(sheet, cell)| sheet.find_cell(cell))
      .is_some_and(|cell| cell.summary.is_empty)
  }

  pub fn create_sheet(&mut self, cx: &mut Context<Self>) {
    self.create_sheet_of_type(0, cx);
  }

  pub fn create_sheet_of_type(&mut self, sheet_type_index: usize, cx: &mut Context<Self>) {
    let Some(sheet_type_id) = self
      .board
      .format
      .sheet_types
      .get(sheet_type_index)
      .map(|sheet_type| sheet_type.id)
    else {
      return;
    };
    let sheet_id = uuid::Uuid::new_v4();
    let name = format!("Sheet {}", self.board.sheets.len() + 1);
    if self
      .apply_intent(
        &FlowIntent::CreateSheet {
          sheet_id,
          name,
          sheet_type_id,
        },
        cx,
      )
      .is_ok()
    {
      self.collapsed_outline_items.insert(sheet_id);
      self.active_sheet = Some(sheet_id);
      self.active_cell = None;
      self.cursor = Some((0, 0));
      self.dirty = true;
      cx.emit(FlowEditorEvent::Changed);
      cx.emit(FlowEditorEvent::ActiveSheetChanged(Some(sheet_id)));
      cx.notify();
    }
  }

  /// The keyboard new-sheet verb creates the ACTIVE sheet's type.
  pub fn create_sheet_matching_active(&mut self, cx: &mut Context<Self>) {
    let type_index = self
      .active_sheet
      .and_then(|sheet_id| self.board.sheets.iter().find(|sheet| sheet.id == sheet_id))
      .and_then(|sheet| {
        self
          .board
          .format
          .sheet_types
          .iter()
          .position(|sheet_type| sheet_type.id == sheet.sheet_type_id)
      })
      .unwrap_or(0);
    self.create_sheet_of_type(type_index, cx);
  }

  /// Sheet deletion is destructive (cells + ink go with it) — confirm.
  pub fn confirm_delete_active_sheet(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(name) = self
      .active_sheet
      .and_then(|sheet_id| self.board.sheets.iter().find(|sheet| sheet.id == sheet_id))
      .map(|sheet| sheet.name.clone())
    else {
      return;
    };
    let detail = format!("\"{name}\" and everything on it — cells and ink — will be deleted. Undo can bring it back.");
    let answer = window.prompt(
      gpui::PromptLevel::Warning,
      "Delete this sheet?",
      Some(&detail),
      &[gpui::PromptButton::ok("Delete"), gpui::PromptButton::cancel("Cancel")],
      cx,
    );
    cx.spawn(async move |editor, cx| {
      if matches!(answer.await, Ok(0)) {
        let _ = editor.update(cx, |editor, cx| editor.delete_active_sheet(cx));
      }
    })
    .detach();
  }

  /// Session restore — reopen on the sheet you were flowing, with your
  /// ink-visibility choices intact.
  pub fn restore_ui_state(&mut self, active_sheet: Option<uuid::Uuid>, hidden_ink_sheets: &[uuid::Uuid], cx: &mut Context<Self>) {
    if let Some(sheet_id) = active_sheet
      && self.board.sheets.iter().any(|sheet| sheet.id == sheet_id)
    {
      self.active_sheet = Some(sheet_id);
      cx.emit(FlowEditorEvent::ActiveSheetChanged(Some(sheet_id)));
    }
    for sheet_id in hidden_ink_sheets {
      if self.board.sheets.iter().any(|sheet| sheet.id == *sheet_id) {
        self.hidden_annotation_sheets.insert(*sheet_id);
      }
    }
    cx.notify();
  }

  pub fn hidden_ink_sheets(&self) -> Vec<uuid::Uuid> {
    self.hidden_annotation_sheets.iter().copied().collect()
  }

  pub fn active_sheet_name(&self) -> Option<String> {
    self
      .active_sheet
      .and_then(|sheet_id| self.board.sheets.iter().find(|sheet| sheet.id == sheet_id))
      .map(|sheet| sheet.name.clone())
  }

  pub fn rename_active_sheet(&mut self, name: impl Into<String>, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else {
      return;
    };
    if self
      .apply_intent(&FlowIntent::RenameSheet { sheet_id, name: name.into() }, cx)
      .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }

  pub fn delete_active_sheet(&mut self, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else {
      return;
    };
    if self
      .apply_intent(&FlowIntent::DeleteSheet { sheet_id }, cx)
      .is_ok()
    {
      self.collapsed_outline_items.remove(&sheet_id);
      self.active_sheet = self.board.sheets.first().map(|sheet| sheet.id);
      self.active_cell = None;
      self.cursor = None;
      self.dirty = true;
      cx.emit(FlowEditorEvent::Changed);
      cx.emit(FlowEditorEvent::ActiveSheetChanged(self.active_sheet));
      cx.emit(FlowEditorEvent::ActiveCellChanged(None));
      cx.notify();
    }
  }

  pub fn move_active_sheet(&mut self, direction: isize, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else {
      return;
    };
    let Some(index) = self
      .board
      .sheets
      .iter()
      .position(|candidate| candidate.id == sheet_id)
    else {
      return;
    };
    let target = index
      .saturating_add_signed(direction)
      .min(self.board.sheets.len().saturating_sub(1));
    if target == index {
      return;
    }
    let before = if target > index {
      self.board.sheets.get(target + 1).map(|sheet| sheet.id)
    } else {
      self.board.sheets.get(target).map(|sheet| sheet.id)
    };
    if self
      .apply_intent(&FlowIntent::MoveSheet { sheet_id, before }, cx)
      .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }

  /// I-S3: identity-anchored reorder from the sheet strip's drag.
  pub fn move_sheet_before(&mut self, sheet_id: SheetId, before: Option<SheetId>, cx: &mut Context<Self>) {
    if before == Some(sheet_id) {
      return;
    }
    let Some(index) = self
      .board
      .sheets
      .iter()
      .position(|candidate| candidate.id == sheet_id)
    else {
      return;
    };
    let successor = self.board.sheets.get(index + 1).map(|sheet| sheet.id);
    if before == successor || (before.is_none() && successor.is_none()) {
      return;
    }
    if self
      .apply_intent(&FlowIntent::MoveSheet { sheet_id, before }, cx)
      .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }

  fn set_cell_bounds(&mut self, cell_id: CellId, bounds: Bounds<gpui::Pixels>, cx: &mut Context<Self>) {
    self.cell_bounds.insert(cell_id, bounds);
    let screen_height = bounds.size.height.as_f32();
    let model_height_changed = match self.cell_measurements.entry(cell_id) {
      std::collections::hash_map::Entry::Occupied(mut entry) => entry.get_mut().update(screen_height, self.board_zoom),
      std::collections::hash_map::Entry::Vacant(entry) => {
        entry.insert(CellMeasurement::new(screen_height, self.board_zoom));
        true
      },
    };
    if model_height_changed {
      cx.notify();
    }
  }

  pub(super) fn scroll_cursor_into_view(&mut self) {
    if let Some((row_ix, column_ix)) = self.cursor {
      self.scroll_slot_into_view(row_ix, column_ix);
    }
  }

  fn scroll_slot_into_view(&mut self, row_ix: usize, column_ix: usize) {
    let Some(sheet) = self.active_sheet_ref() else { return };
    let layout = GridLayout::compute(sheet, &self.cell_measurements);
    let zoom = self.board_zoom;
    let (x, y) = layout.slot_origin(row_ix, column_ix);
    let (origin_x, origin_y) = grid_origin_model();
    let left = px((origin_x + x) * zoom);
    let top = px((origin_y + y) * zoom);
    let right = left + px(layout.column_widths.get(column_ix).copied().unwrap_or(0.0) * zoom);
    let bottom = top + px((layout.row_height(row_ix) + grid_layout::ROW_GAP) * zoom);
    let viewport = self.board_scroll.bounds().size;
    let frozen_x = px((GUTTER_WIDTH + BOARD_PADDING) * zoom);
    let frozen_y = px((HEADER_HEIGHT + BOARD_PADDING) * zoom);
    let mut offset = self.board_scroll.offset();
    if left + offset.x < frozen_x {
      offset.x = frozen_x - left;
    } else if right + offset.x > viewport.width {
      offset.x = viewport.width - right;
    }
    if top + offset.y < frozen_y {
      offset.y = frozen_y - top;
    } else if bottom + offset.y > viewport.height {
      offset.y = viewport.height - bottom;
    }
    self.set_user_scroll_offset(offset);
  }

  /// The accent color for a cell: its column's side palette.
  fn cell_text_color(&self, cell_id: CellId, cx: &App) -> gpui::Hsla {
    self
      .board
      .sheets
      .iter()
      .find_map(|sheet| {
        let cell = sheet.find_cell(cell_id)?;
        let column = sheet.columns.iter().find(|column| column.id == cell.column_id)?;
        Some(flow_side_palette(column.side, cx).base)
      })
      .unwrap_or(cx.theme().foreground)
  }

  fn begin_pan(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
    // A fresh press clears the swallow flag; only real movement re-arms it.
    self.suppress_click = false;
    self.pan_drag = Some(PanDragState {
      pointer_anchor: position,
      scroll_anchor: self.board_scroll.offset(),
      pending_position: position,
      frame_scheduled: false,
    });
    cx.notify();
  }

  fn queue_pan(&mut self, position: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
    let (anchor, already_scheduled) = match self.pan_drag.as_mut() {
      Some(pan_drag) => {
        pan_drag.pending_position = position;
        (pan_drag.pointer_anchor, pan_drag.frame_scheduled)
      },
      None => return,
    };
    // Past a few pixels this is a pan, not a click — swallow the release click
    // so it never lands as a selection on the cell the drag began on.
    let dx = (position.x - anchor.x).as_f32();
    let dy = (position.y - anchor.y).as_f32();
    if dx.hypot(dy) > 4.0 {
      self.suppress_click = true;
    }
    if already_scheduled {
      return;
    }
    if let Some(pan_drag) = self.pan_drag.as_mut() {
      pan_drag.frame_scheduled = true;
    }
    cx.on_next_frame(window, |editor, _, cx| {
      let offset = {
        let Some(pan_drag) = editor.pan_drag.as_mut() else {
          return;
        };
        pan_drag.frame_scheduled = false;
        pan_drag.scroll_anchor + (pan_drag.pending_position - pan_drag.pointer_anchor)
      };
      editor.set_user_scroll_offset(offset);
      cx.notify();
    });
  }

  fn finish_space_pan(&mut self, cx: &mut Context<Self>) {
    self.pan_drag = None;
    cx.notify();
  }

  /// W2: the set an op applies to — the multi-selection when one exists,
  /// else the active cell alone. Returned in row-major order.
  fn operation_set(&self, sheet_id: SheetId) -> Vec<CellId> {
    if self.selected_cells.is_empty() {
      return self.active_cell.into_iter().collect();
    }
    self
      .board
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)
      .map(|sheet| {
        sheet
          .cells()
          .map(|cell| cell.id)
          .filter(|id| self.selected_cells.contains(id))
          .collect()
      })
      .unwrap_or_default()
  }

  pub fn selected_cells(&self) -> &HashSet<CellId> {
    &self.selected_cells
  }

  pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
    if !self.selected_cells.is_empty() {
      self.selected_cells.clear();
      cx.notify();
    }
  }

  /// W2 shift-click: toggle a cell in the multi-selection.
  pub fn toggle_select_cell(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    if self.selected_cells.is_empty()
      && let Some(active) = self.active_cell
      && active != cell_id
    {
      self.selected_cells.insert(active);
    }
    if !self.selected_cells.remove(&cell_id) {
      self.selected_cells.insert(cell_id);
    }
    cx.notify();
  }

  /// W2 shift-click: select every cell whose slot falls in the rectangle
  /// spanning the selection anchor (or the cursor) and `(row_ix, column_ix)`.
  /// Empty slots contribute nothing — there is no cell there to select.
  pub fn select_cell_range(&mut self, row_ix: usize, column_ix: usize, cx: &mut Context<Self>) {
    let (anchor_row, anchor_column) = self.selection_anchor.or(self.cursor).unwrap_or((row_ix, column_ix));
    let Some(sheet) = self.active_sheet_ref() else { return };
    let (r0, r1) = (anchor_row.min(row_ix), anchor_row.max(row_ix));
    let (c0, c1) = (anchor_column.min(column_ix), anchor_column.max(column_ix));
    let mut set = HashSet::new();
    for r in r0..=r1 {
      for c in c0..=c1 {
        if let Some(cell) = sheet.slot(r, c) {
          set.insert(cell.id);
        }
      }
    }
    self.selected_cells = set;
    // The extend end becomes the live cursor; the anchor stays put so a
    // further shift-click re-measures from the same corner.
    self.cursor = Some((row_ix, column_ix));
    cx.notify();
  }

  /// Gutter click: select a whole row's cells.
  pub fn select_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else { return };
    let Some(row) = sheet.rows.get(row_ix) else {
      self.selected_cells.clear();
      self.cursor = Some((row_ix, 0));
      cx.notify();
      return;
    };
    self.selected_cells = row
      .cells
      .iter()
      .filter_map(|slot| slot.as_ref().map(|cell| cell.id))
      .collect();
    self.cursor = Some((row_ix, 0));
    cx.notify();
  }

  /// Run one set-op as ONE undo group (W2 law).
  fn grouped<T>(&mut self, count: usize, apply: impl FnOnce(&mut Self) -> T) -> T {
    if count > 1 {
      let _ = self.handle.undo_group_start();
      let result = apply(self);
      let _ = self.handle.undo_group_end();
      result
    } else {
      apply(self)
    }
  }

  pub fn delete_selected(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else {
      return;
    };
    let set = self.operation_set(sheet_id);
    if set.is_empty() {
      return;
    }
    self.grouped(set.len(), |editor| {
      for cell_id in set {
        let _ = editor.apply_intent(&FlowIntent::DeleteCell { sheet_id, cell_id }, cx);
      }
    });
    self.selected_cells.clear();
    self.changed(None, cx);
  }

  pub fn strike_selected(&mut self, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else {
      return;
    };
    let set = self.operation_set(sheet_id);
    if set.is_empty() {
      return;
    }
    // The set converges to one state: struck unless EVERY member already is.
    let struck = !set.iter().all(|cell_id| {
      self
        .active_sheet_ref()
        .and_then(|sheet| sheet.find_cell(*cell_id))
        .is_some_and(|cell| cell.summary.struck)
    });
    let active = self.active_cell;
    self.grouped(set.len(), |editor| {
      for cell_id in set {
        let _ = editor.apply_intent(&FlowIntent::SetCellStruck { sheet_id, cell_id, struck }, cx);
      }
    });
    self.changed(active, cx);
  }

  // ---- row / column verbs (ribbon + gutter + header) ----------------------

  /// Delete the cursor's row (cells go with it — one intent, undoable).
  pub fn delete_cursor_row(&mut self, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else { return };
    let Some((row_ix, _)) = self.cursor else { return };
    let Some(row_id) = self
      .active_sheet_ref()
      .and_then(|sheet| sheet.rows.get(row_ix))
      .map(|row| row.id)
    else {
      return;
    };
    if self
      .apply_intent(
        &FlowIntent::DeleteRows {
          sheet_id,
          row_ids: vec![row_id],
        },
        cx,
      )
      .is_ok()
    {
      self.changed(None, cx);
    }
  }

  /// Append a fresh column at the right edge (label editable via rename).
  pub fn add_column_end(&mut self, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else { return };
    let side = self
      .active_sheet_ref()
      .and_then(|sheet| sheet.columns.last())
      .map(|column| match column.side {
        flowstate_flow::ArgumentSide::One => flowstate_flow::ArgumentSide::Two,
        flowstate_flow::ArgumentSide::Two => flowstate_flow::ArgumentSide::One,
      })
      .unwrap_or(flowstate_flow::ArgumentSide::One);
    let count = self.active_sheet_ref().map_or(0, |sheet| sheet.columns.len());
    if self
      .apply_intent(
        &FlowIntent::AddColumn {
          sheet_id,
          column_id: uuid::Uuid::new_v4(),
          label: format!("Col {}", count + 1),
          side,
          before: None,
        },
        cx,
      )
      .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }

  /// Delete the cursor's column, confirmed (its cells die with it).
  pub fn confirm_delete_cursor_column(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some((label, column_id, sheet_id)) = self.cursor.and_then(|(_, column_ix)| {
      let sheet = self.active_sheet_ref()?;
      let column = sheet.columns.get(column_ix)?;
      Some((column.label.clone(), column.id, sheet.id))
    }) else {
      return;
    };
    if self.active_sheet_ref().is_some_and(|sheet| sheet.columns.len() <= 1) {
      self.refuse("a sheet needs at least one column", None, cx);
      return;
    }
    let detail = format!("\"{label}\" and every card in it will be deleted. Undo can bring it back.");
    let answer = window.prompt(
      gpui::PromptLevel::Warning,
      "Delete this column?",
      Some(&detail),
      &[gpui::PromptButton::ok("Delete"), gpui::PromptButton::cancel("Cancel")],
      cx,
    );
    cx.spawn(async move |editor, cx| {
      if matches!(answer.await, Ok(0)) {
        let _ = editor.update(cx, |editor, cx| {
          if editor
            .apply_intent(&FlowIntent::DeleteColumn { sheet_id, column_id }, cx)
            .is_ok()
          {
            editor.changed(None, cx);
          }
        });
      }
    })
    .detach();
  }

  pub fn can_undo(&self) -> bool {
    self.undo_state.0
  }

  pub fn can_redo(&self) -> bool {
    self.undo_state.1
  }

  pub fn undo(&mut self, cx: &mut Context<Self>) {
    if self.handle.undo().is_ok_and(|changed| changed) {
      self.resync_from_runtime(cx);
      self.dirty = true;
      cx.emit(FlowEditorEvent::Changed);
      cx.notify();
    }
  }

  pub fn redo(&mut self, cx: &mut Context<Self>) {
    if self.handle.redo().is_ok_and(|changed| changed) {
      self.resync_from_runtime(cx);
      self.dirty = true;
      cx.emit(FlowEditorEvent::Changed);
      cx.notify();
    }
  }

  pub(super) fn changed(&mut self, active_cell: Option<CellId>, cx: &mut Context<Self>) {
    self.active_cell = active_cell;
    self.dirty = true;
    cx.emit(FlowEditorEvent::Changed);
    cx.emit(FlowEditorEvent::ActiveCellChanged(active_cell));
    cx.notify();
  }

  pub fn save(&mut self, cx: &mut Context<Self>) -> Task<std::io::Result<()>> {
    let Some(path) = self.path.clone() else {
      return cx
        .background_executor()
        .spawn(async { Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "flow has no save path")) });
    };
    self.save_to_path(path, cx)
  }

  pub fn save_as(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Task<std::io::Result<()>> {
    self.path = Some(path.clone());
    self.save_to_path(path, cx)
  }

  fn save_to_path(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Task<std::io::Result<()>> {
    // The I/O service owns the save: fork under the gate (brief), snapshot
    // export + framing + atomic write off it (spec I-9a).
    let io = self.io.clone();
    cx.spawn(async move |editor, cx| {
      let write_result = cx
        .background_executor()
        .spawn(async move {
          io.save_to(path.clone())
            .await
            .map_err(std::io::Error::other)?;
          // S12: a Dropbox-bound flow mirrors its raw .fl0 after every save.
          if crate::app_settings::load_dropbox_document_binding(&path).is_some() {
            let bytes = io.encode_bytes().await.map_err(std::io::Error::other)?;
            crate::collab::dropbox_checkpoint::sync_bound_flow_file(&path, &io, bytes).await?;
          }
          Ok::<(), std::io::Error>(())
        })
        .await;
      if write_result.is_ok() {
        let _ = editor.update(cx, |editor, cx| {
          editor.dirty = false;
          cx.notify();
        });
      }
      write_result
    })
  }

  /// S10: install (or clear) the crash-recovery target for a pathless
  /// collaboration tab. Writes are debounced off `after_local_change`.
  pub fn set_recovery_path(&mut self, path: Option<PathBuf>, cx: &mut Context<Self>) {
    self.recovery_path = path;
    if self.recovery_path.is_some() {
      self.schedule_recovery_write(cx);
    }
  }

  pub fn recovery_path(&self) -> Option<&PathBuf> {
    self.recovery_path.as_ref()
  }

  fn schedule_recovery_write(&mut self, cx: &mut Context<Self>) {
    if self.recovery_write_pending || self.path.is_some() || self.recovery_path.is_none() {
      return;
    }
    self.recovery_write_pending = true;
    let io = self.io.clone();
    cx.spawn(async move |editor, cx| {
      cx.background_executor()
        .timer(std::time::Duration::from_secs(2))
        .await;
      let Ok(target) = editor.update(cx, |editor, _| {
        editor.recovery_write_pending = false;
        editor
          .path
          .is_none()
          .then(|| editor.recovery_path.clone())
          .flatten()
      }) else {
        return;
      };
      let Some(target) = target else {
        return;
      };
      match io.encode_bytes().await {
        Ok(bytes) => {
          if let Err(error) = std::fs::write(&target, bytes) {
            tracing::warn!(path = %target.display(), %error, "writing flow recovery file failed");
          }
        },
        Err(error) => {
          tracing::warn!(error = %format_args!("{error:#}"), "encoding flow recovery bytes failed");
        },
      }
    })
    .detach();
  }

  pub fn discard_recovery_file(&mut self) {
    if let Some(path) = self.recovery_path.take()
      && let Err(error) = std::fs::remove_file(&path)
      && error.kind() != std::io::ErrorKind::NotFound
    {
      tracing::warn!(path = %path.display(), %error, "discarding flow recovery file failed");
    }
  }

  pub fn resolve_pending(&mut self, _cx: &mut Context<Self>) {}

  // ---- drag plumbing (drop = set address; zero interpretation) -------------

  /// Screen position → board model point (grid-origin relative, zoom 1).
  pub(super) fn model_point(&self, position: gpui::Point<gpui::Pixels>) -> BoardPoint {
    let (origin_x, origin_y) = grid_origin_model();
    BoardPoint {
      x: (position.x.as_f32() - self.viewport_origin.x) / self.board_zoom - origin_x,
      y: (position.y.as_f32() - self.viewport_origin.y) / self.board_zoom - origin_y,
    }
  }

  fn update_cell_drag_target(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
    let target = {
      let point = self.model_point(position);
      let Some(sheet) = self.active_sheet_ref() else { return };
      let layout = GridLayout::compute_to(sheet, &self.cell_measurements, self.ghost_bottom_model());
      layout
        .row_at(point.y)
        .zip(layout.column_at(point.x))
    };
    if self.drag_target != target {
      self.drag_target = target;
      cx.notify();
    }
  }

  fn finish_cell_drop(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    let target = self.drag_target.take();
    self.dragging_cell = None;
    let Some(sheet_id) = self.active_sheet else { return };
    if let Some((row_ix, column_ix)) = target {
      if self.selected_cells.len() > 1 && self.selected_cells.contains(&cell_id) {
        self.move_selection_block(sheet_id, cell_id, row_ix, column_ix, cx);
      } else {
        self.move_cell_to_slot(sheet_id, cell_id, row_ix, column_ix, cx);
      }
    }
    cx.notify();
  }

  fn update_row_drag_gap(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
    let gap = {
      let point = self.model_point(position);
      let Some(sheet) = self.active_sheet_ref() else { return };
      let layout = GridLayout::compute_to(sheet, &self.cell_measurements, self.ghost_bottom_model());
      let row = layout.row_at(point.y).unwrap_or(0).min(layout.real_rows);
      let mid = layout.row_top(row) + layout.row_height(row) / 2.0;
      let gap = if point.y > mid { row + 1 } else { row };
      Some(gap.min(layout.real_rows))
    };
    if self.row_drop_gap != gap {
      self.row_drop_gap = gap;
      cx.notify();
    }
  }

  fn finish_row_drop(&mut self, row_id: RowId, cx: &mut Context<Self>) {
    let gap = self.row_drop_gap.take();
    self.dragging_row = None;
    let Some(sheet_id) = self.active_sheet else { return };
    let Some(gap) = gap else { return };
    let before = self
      .active_sheet_ref()
      .and_then(|sheet| sheet.rows.get(gap))
      .map(|row| row.id);
    if before == Some(row_id) {
      return;
    }
    if self
      .apply_intent(
        &FlowIntent::MoveRows {
          sheet_id,
          row_ids: vec![row_id],
          before,
        },
        cx,
      )
      .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }

  fn update_column_drag_gap(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
    let gap = {
      let point = self.model_point(position);
      let Some(sheet) = self.active_sheet_ref() else { return };
      let layout = GridLayout::compute_to(sheet, &self.cell_measurements, self.ghost_bottom_model());
      let column = layout.column_at(point.x).unwrap_or(0);
      let mid = layout.column_lefts.get(column).copied().unwrap_or(0.0) + layout.column_widths.get(column).copied().unwrap_or(0.0) / 2.0;
      let gap = if point.x > mid { column + 1 } else { column };
      Some(gap.min(sheet.columns.len()))
    };
    if self.column_drop_gap != gap {
      self.column_drop_gap = gap;
      cx.notify();
    }
  }

  fn finish_column_drop(&mut self, column_id: ColumnId, cx: &mut Context<Self>) {
    let gap = self.column_drop_gap.take();
    self.dragging_column = None;
    let Some(sheet_id) = self.active_sheet else { return };
    let Some(gap) = gap else { return };
    let before = self
      .active_sheet_ref()
      .and_then(|sheet| sheet.columns.get(gap))
      .map(|column| column.id);
    if before == Some(column_id) {
      return;
    }
    if self
      .apply_intent(
        &FlowIntent::MoveColumn {
          sheet_id,
          column_id,
          before,
        },
        cx,
      )
      .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }

  fn update_column_resize(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
    let zoom = self.board_zoom;
    let Some(resize) = self.column_resize.as_mut() else { return };
    let x = position.x.as_f32();
    let start_x = *resize.start_x.get_or_insert(x);
    let live = (resize.start_width + (x - start_x) / zoom).max(MIN_COLUMN_WIDTH);
    if (live - resize.live_width).abs() > 0.5 {
      resize.live_width = live;
      cx.notify();
    }
  }

  fn finish_column_resize(&mut self, cx: &mut Context<Self>) {
    let Some(resize) = self.column_resize.take() else { return };
    let Some(sheet_id) = self.active_sheet else { return };
    if resize.start_x.is_none() {
      return; // never moved
    }
    if self
      .apply_intent(
        &FlowIntent::SetColumnWidth {
          sheet_id,
          column_id: resize.column_id,
          width: Some(resize.live_width),
        },
        cx,
      )
      .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }

  fn clear_autofit_width(&mut self, column_id: ColumnId, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else { return };
    if self
      .apply_intent(
        &FlowIntent::SetColumnWidth {
          sheet_id,
          column_id,
          width: None,
        },
        cx,
      )
      .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }

  /// D4: gutter-edge drag on a row → manual height override; double-click
  /// clears back to autofit (wired from the gutter overlay).
  pub fn clear_row_height_override(&mut self, row_id: RowId, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else { return };
    if self
      .apply_intent(
        &FlowIntent::SetRowHeight {
          sheet_id,
          row_id,
          height: None,
        },
        cx,
      )
      .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }

  // ---- the grid render ------------------------------------------------------

  /// The layout for the ACTIVE sheet with any live column-resize preview
  /// applied.
  /// Model-space y the ghost run should reach: one viewport of empty rows
  /// below the current visible bottom, so scrolling always reveals more.
  fn ghost_bottom_model(&self) -> f32 {
    let (_, origin_y) = grid_origin_model();
    let zoom = self.board_zoom.max(0.01);
    let offset = self.board_scroll.offset();
    let viewport_h = self.board_scroll.bounds().size.height.as_f32().max(1.0);
    let view_top = (-offset.y.as_f32()) / zoom - origin_y;
    view_top + 2.0 * viewport_h / zoom
  }

  fn active_layout(&self) -> Option<(GridLayout, &flowstate_flow::Sheet)> {
    let sheet = self.active_sheet_ref()?;
    let mut layout = GridLayout::compute_to(sheet, &self.cell_measurements, self.ghost_bottom_model());
    if let Some(resize) = &self.column_resize
      && let Some(index) = sheet.columns.iter().position(|column| column.id == resize.column_id)
    {
      layout.column_widths[index] = resize.live_width;
      let mut x = layout.column_lefts[index];
      for column_ix in index..layout.column_widths.len() {
        layout.column_lefts[column_ix] = x;
        x += layout.column_widths[column_ix] + grid_layout::COLUMN_GAP;
      }
    }
    Some((layout, sheet))
  }

  fn render_grid(&mut self, cx: &mut Context<Self>) -> AnyElement {
    let Some((layout, sheet)) = self.active_layout() else {
      return div().child("Select a sheet").into_any_element();
    };
    let sheet_id = sheet.id;
    let zoom = self.board_zoom;
    let (origin_x, origin_y) = grid_origin_model();
    let content_width = px((origin_x + layout.total_width() + BOARD_PADDING) * zoom);
    let content_height = px((origin_y + layout.total_height() + BOARD_PADDING * 4.0) * zoom);
    let active = self.active_cell;
    let cursor = self.cursor;
    let weak_editor = cx.entity().downgrade();
    let client_document_theme = load_document_theme();

    // Visible window from the scroll offset (O(log rows)).
    let offset = self.board_scroll.offset();
    let viewport = self.board_scroll.bounds().size;
    let view_top = (-offset.y.as_f32()) / zoom - origin_y;
    let view_bottom = view_top + viewport.height.as_f32().max(1.0) / zoom;
    let visible_rows = layout.visible_rows(view_top, view_bottom);
    let visible_columns = 0..sheet.columns.len();

    // Spreadsheet gridlines: hairlines at every slot boundary (columns run
    // the full grid height; rows are windowed with the slots). Painted UNDER
    // the slots — occupied cells sit 1px inset so the line shows around them.
    // A touch stronger than a hairline so adjacent filled cells stay legibly
    // separate (the gridline is the ONLY separation — cells have no borders).
    let grid_line_color = cx.theme().border.opacity(0.85);
    let line_xs: Vec<f32> = layout
      .column_lefts
      .iter()
      .copied()
      .chain(std::iter::once(layout.total_width()))
      .collect();
    let line_ys: Vec<f32> = visible_rows
      .clone()
      .chain(std::iter::once(visible_rows.end))
      .map(|row_ix| layout.row_top(row_ix))
      .collect();
    let grid_extent = (layout.total_width(), layout.total_height());

    // Ink for this sheet (rigid bodies: anchor slot origin + local points).
    // The anchor is stored CONTENT-RELATIVE (no viewport origin baked in): the
    // paint canvas adds its own live `bounds.origin` each frame, exactly like
    // the gridline canvas. Baking in the cached `viewport_origin` here instead
    // lagged the ink one frame behind the grid during a pan (a visible wobble).
    let strokes: Vec<(gpui::Point<gpui::Pixels>, flowstate_flow::AnnotationStroke)> = if !self.hidden_annotation_sheets.contains(&sheet_id) {
      sheet
        .annotations
        .iter()
        .filter(|stroke| !self.hidden_annotation_originators.contains(&stroke.originator))
        .map(|stroke| {
          let (row_ix, column_ix) = sheet.resolve_anchor(&stroke.anchor);
          let (slot_x, slot_y) = layout.slot_origin(row_ix, column_ix);
          let anchor_offset = point(
            board_content_offset(origin_x, slot_x + stroke.anchor.offset.x, zoom),
            board_content_offset(origin_y, slot_y + stroke.anchor.offset.y, zoom),
          );
          (anchor_offset, stroke.clone())
        })
        .collect()
    } else {
      Vec::new()
    };
    let draft = self.drawing_points.clone();
    // The live draft must paint in the SAME color it will commit as — the
    // right-drag profile color when inking, else the armed marker color — so it
    // never flashes the default amber until release.
    let draft_color = self.active_ink_color.unwrap_or(self.marker_color_rgba);
    // Content-relative too (see `strokes` above): the paint canvas adds its
    // live `bounds.origin`, so the draft tracks the grid frame-for-frame.
    let draft_origin = point(board_content_offset(origin_x, 0.0, zoom), board_content_offset(origin_y, 0.0, zoom));

    // F3: a refused move SHAKES the named card while the toast speaks.
    let shake = self.refusal.as_ref().and_then(|notice| {
      let age = notice.at.elapsed().as_secs_f32();
      (age < 0.45).then_some(())?;
      notice.cell.map(|cell| (cell, 6.0 * (age * 35.0).sin() * (1.0 - age / 0.45)))
    });

    let mut slots: Vec<AnyElement> = Vec::new();
    for row_ix in visible_rows.clone() {
      let row_top = layout.row_top(row_ix);
      let row_height = layout.row_height(row_ix);
      let is_ghost = row_ix >= layout.real_rows;
      for column_ix in visible_columns.clone() {
        let column = &sheet.columns[column_ix];
        let side_palette = flow_side_palette(column.side, cx);
        let side_color = side_palette.base;
        let left = board_content_offset(origin_x, layout.column_lefts[column_ix], zoom);
        let top = board_content_offset(origin_y, row_top, zoom);
        let width = px(layout.column_widths[column_ix] * zoom);
        let height = px(row_height * zoom);
        let occupant = if is_ghost { None } else { sheet.slot(row_ix, column_ix) };
        let is_cursor = cursor == Some((row_ix, column_ix));
        let is_drag_target = self.drag_target == Some((row_ix, column_ix));

        match occupant {
          Some(cell) => {
            let id = cell.id;
            let struck = cell.summary.struck;
            let label: SharedString = cell.summary.summary_text.to_string().into();
            // Idle and editing render the SAME projection + summary mode, so
            // selecting a cell never changes its text size/spacing. (Only a
            // cite cell is naturally a summary — that stays consistent because
            // the editor uses the same flag.)
            let uses_summary_projection = cell.summary.uses_summary_projection;
            let mut rendered_document = if active == Some(id) { None } else { self.cell_document(id) };
            if let Some(document) = rendered_document.as_mut() {
              apply_flow_cell_theme(document, &client_document_theme, side_color, cx.theme().background, zoom);
            }
            let cell_editor = self.cell_editors.get(&id).cloned();
            let fill = crate::flow::cell_theme::flow_cell_fill(
              side_color,
              cx.theme().background,
              cx.theme().foreground,
              cx.theme().is_dark(),
              if active == Some(id) { 0.08 } else { 0.0 },
            );
            // S11: a peer's hand on this cell paints its presence border.
            let presence_ring = self
              .external_presences
              .iter()
              .find(|presence| presence.cell == Some(id))
              .map(|presence| gpui::Hsla::from(rgba((presence.color_rgb << 8) | 0xff)));
            // Excel selection grammar: state is a BORDER on the cell rect —
            // thick accent for the active cell, primary while an occupied slot
            // is a hovering drop's swap partner. Neighbors are separated by the
            // gridline (below), NOT a per-cell border.
            let dragging_this = self.dragging_cell == Some(id);
            // The ring is a CONSTANT 2px border colored per state — and colored
            // as the fill (invisible) when idle — so activating a cell only
            // changes the ring color, never nudges the text inward.
            let ring_color: gpui::Hsla = if dragging_this {
              fill // carried as a faded ghost — no highlight
            } else if is_drag_target && self.dragging_cell.is_some_and(|dragged| dragged != id) {
              cx.theme().primary
            } else if active == Some(id) {
              side_palette.active
            } else if self.selected_cells.contains(&id) {
              side_palette.base.opacity(0.85)
            } else {
              presence_ring.unwrap_or(fill)
            };
            let shake_offset = shake.and_then(|(cell, offset)| (cell == id).then_some(offset));
            let clipped = sheet.rows[row_ix].height_override.is_some()
              && self
                .cell_measurements
                .get(&id)
                .is_some_and(|measurement| measurement.model_height > row_height + 1.0);

            // The cell IS its slot: flat fill inset 1px so the gridline
            // underneath stays visible around it. Content (auto-height, for
            // D4 autofit measurement) and the interaction overlay are
            // separate children so clicks land anywhere in the rect.
            let grip_group = SharedString::from(format!("flow-cell-grip-{}", id.as_u128()));
            slots.push(
              div()
                .absolute()
                .left(left + px(grid_layout::CELL_SLOT_INSET))
                .top(top + px(grid_layout::CELL_SLOT_INSET))
                .w(width - px(grid_layout::CELL_SLOT_INSET))
                .h(height - px(grid_layout::CELL_SLOT_INSET))
                .overflow_hidden()
                .bg(fill)
                .border(px(grid_layout::CELL_BORDER))
                .border_color(ring_color)
                .group(grip_group.clone())
                .when(dragging_this, |this| this.opacity(0.4))
                .when_some(shake_offset, |this, offset| this.ml(px(offset)))
                .on_children_prepainted({
                  let weak = weak_editor.clone();
                  move |bounds, _, cx| {
                    if let Some(card_bounds) = bounds.first().copied() {
                      let _ = weak.update(cx, |editor, cx| editor.set_cell_bounds(id, card_bounds, cx));
                    }
                  }
                })
                .child(
                  div()
                    .relative()
                    .w_full()
                    .min_h(px((grid_layout::MIN_ROW_HEIGHT - 2.0) * zoom))
                    .p(px(grid_layout::CELL_CONTENT_PADDING * zoom))
                    .child(if active == Some(id) {
                      cell_editor.map_or_else(|| div().child(label.clone()).into_any_element(), |editor| editor.into_any_element())
                    } else {
                      rendered_document.map_or_else(
                        || {
                          div()
                            .when(struck, |this| this.line_through())
                            .child(label.clone())
                            .into_any_element()
                        },
                        |document| {
                          RichTextDocumentElement::new(document)
                            .with_invisibility_mode(uses_summary_projection)
                            .into_any_element()
                        },
                      )
                    }),
                )
                .when(active != Some(id), |this| {
                  // Grammar A/C: the card body is the pan + click surface — a
                  // bare drag pans (below-threshold click still selects/edits).
                  // Grammar B: the hover-revealed grip is the ONE place a drag
                  // moves the card instead of panning.
                  let drag_weak = weak_editor.clone();
                  let drag_label = label.clone();
                  let drag_fill = fill;
                  let drag_text_color = side_color;
                  let dot_color = cx.theme().muted_foreground.opacity(0.75);
                  let grip_dot = px(2.0 * zoom);
                  let grip_gap = px(2.0 * zoom);
                  let grip_column = move || {
                    div()
                      .flex()
                      .flex_col()
                      .gap(grip_gap)
                      .children((0..3).map(move |_| div().w(grip_dot).h(grip_dot).rounded_full().bg(dot_color)))
                  };
                  this
                    .child(
                      div()
                        .id(("flow-cell", id.as_u128() as u64))
                        .absolute()
                        .inset_0()
                        .cursor_pointer()
                        .on_mouse_down(
                          MouseButton::Left,
                          cx.listener(|editor, event: &MouseDownEvent, _, cx| {
                            if editor.annotation_tool != AnnotationTool::None {
                              editor.set_annotation_tool(AnnotationTool::None, cx);
                            }
                            editor.begin_pan(event.position, cx);
                            cx.stop_propagation();
                          }),
                        )
                        .on_click(cx.listener(move |editor, event: &gpui::ClickEvent, window, cx| {
                          if editor.suppress_click {
                            editor.suppress_click = false;
                            return; // this "click" was the tail of a pan
                          }
                          let modifiers = event.modifiers();
                          let position = editor.active_sheet_ref().and_then(|sheet| sheet.cell_position(id));
                          // Shift = rectangular range from the anchor; Ctrl/Cmd =
                          // toggle this one cell in the collection.
                          if modifiers.shift {
                            if let Some((row_ix, column_ix)) = position {
                              editor.select_cell_range(row_ix, column_ix, cx);
                            }
                            return;
                          }
                          if modifiers.control || modifiers.platform {
                            editor.toggle_select_cell(id, cx);
                            editor.selection_anchor = position;
                            return;
                          }
                          editor.clear_selection(cx);
                          editor.selection_anchor = position;
                          editor.activate_cell(id, cx);
                          // Enter with the caret at the end so typing appends.
                          editor.focus_active_cell(window, cx);
                        })),
                    )
                    .child(
                      div()
                        .id(("flow-cell-grip", id.as_u128() as u64))
                        .absolute()
                        .top(px(3.0 * zoom))
                        .left(px(3.0 * zoom))
                        .invisible()
                        .group_hover(grip_group.clone(), |this| this.visible())
                        .flex()
                        .gap(grip_gap)
                        .p(px(2.5 * zoom))
                        .rounded(px(3.0 * zoom))
                        .bg(cx.theme().popover)
                        .shadow_sm()
                        .cursor(gpui::CursorStyle::OpenHand)
                        .child(grip_column())
                        .child(grip_column())
                        .on_mouse_down(MouseButton::Left, cx.listener(|_editor, _: &MouseDownEvent, _, cx| cx.stop_propagation()))
                        .on_drag(FlowCellDrag { cell_id: id }, move |drag, _, _, cx| {
                          let _ = drag_weak.update(cx, |editor, cx| {
                            editor.dragging_cell = Some(drag.cell_id);
                            cx.notify();
                          });
                          cx.new(|_| FlowCellDragPreview {
                            label: drag_label.clone(),
                            fill: drag_fill,
                            text_color: drag_text_color,
                          })
                        }),
                    )
                })
                // D4: an overridden row that clips its content says so.
                .when(clipped, |this| {
                  this.child(
                    div()
                      .absolute()
                      .bottom(px(0.0))
                      .right(px(4.0))
                      .text_size(px(10.0 * zoom))
                      .text_color(cx.theme().muted_foreground)
                      .child("⋯"),
                  )
                })
                .into_any_element(),
            );
          },
          None => {
            let drag_live = self.dragging_cell.is_some();
            // Empty in-grid slots share the occupied cell's side wash (idle
            // emphasis) so the sheet reads as one uniform field instead of a
            // lighter grid of holes. The ghost run below stays bare — that
            // transparent aether is the cue for where the sheet actually ends.
            let empty_fill = crate::flow::cell_theme::flow_cell_fill(
              side_color,
              cx.theme().background,
              cx.theme().foreground,
              cx.theme().is_dark(),
              0.0,
            );
            slots.push(
              div()
                .id(("flow-slot", (row_ix as u64) << 16 | column_ix as u64))
                .absolute()
                .left(left + px(1.0))
                .top(top + px(1.0))
                .w(width - px(1.0))
                .h(height - px(1.0))
                .when(!is_ghost, |this| this.bg(empty_fill))
                .when(is_cursor, |this| this.border(px(2.0)).border_color(side_palette.active))
                .when(is_drag_target && drag_live, |this| {
                  this.border(px(2.0)).border_dashed().border_color(cx.theme().primary)
                })
                .on_mouse_down(
                  MouseButton::Left,
                  cx.listener(move |editor, event: &MouseDownEvent, window, cx| {
                    if editor.annotation_tool != AnnotationTool::None {
                      return;
                    }
                    // Modifier-held press builds a selection — leave it to
                    // on_click and don't start a pan.
                    let modifiers = event.modifiers;
                    if modifiers.shift || modifiers.control || modifiers.platform {
                      cx.stop_propagation();
                      return;
                    }
                    // Empty space is the pan surface: a bare drag from here
                    // pans. Cursor placement waits for on_click, so a pan never
                    // leaves the anchor slot selected.
                    editor.focus_handle.focus(window);
                    editor.begin_pan(event.position, cx);
                    cx.stop_propagation();
                  }),
                )
                .on_click(cx.listener(move |editor, event: &gpui::ClickEvent, window, cx| {
                  if editor.suppress_click {
                    editor.suppress_click = false;
                    return; // the tail of a pan, not a selection
                  }
                  let modifiers = event.modifiers();
                  if modifiers.shift {
                    editor.select_cell_range(row_ix, column_ix, cx);
                    return;
                  }
                  if modifiers.control || modifiers.platform {
                    // No cell here to toggle — just re-anchor the range.
                    editor.selection_anchor = Some((row_ix, column_ix));
                    return;
                  }
                  if event.click_count() >= 2 {
                    editor.set_annotation_tool(AnnotationTool::None, cx);
                    if editor
                      .add_cell_at_slot(row_ix, column_ix, flowstate_flow::CellSeed::Empty, cx)
                      .is_some()
                    {
                      editor.focus_active_cell(window, cx);
                    }
                    return;
                  }
                  // Plain single click places the grid cursor on this slot.
                  editor.clear_selection(cx);
                  editor.selection_anchor = Some((row_ix, column_ix));
                  editor.set_cursor(row_ix, column_ix, cx);
                  editor.focus_handle.focus(window);
                }))
                .into_any_element(),
            );
          },
        }
      }
    }

    // Row landing bar (row drag) / column landing bar (column drag).
    let row_bar = self
      .dragging_row
      .is_some()
      .then_some(())
      .and(self.row_drop_gap)
      .map(|gap| {
        let y = if gap >= layout.real_rows && layout.real_rows > 0 {
          layout.row_top(layout.real_rows.saturating_sub(1)) + layout.row_height(layout.real_rows.saturating_sub(1)) + grid_layout::ROW_GAP / 2.0
        } else {
          layout.row_top(gap) - grid_layout::ROW_GAP / 2.0
        };
        div()
          .absolute()
          .left(px(origin_x * zoom))
          .top(px((origin_y + y) * zoom - 2.0))
          .w(px(layout.total_width() * zoom))
          .h(px(4.0))
          .rounded_full()
          .bg(cx.theme().primary)
          .into_any_element()
      });
    let column_bar = self
      .dragging_column
      .is_some()
      .then_some(())
      .and(self.column_drop_gap)
      .map(|gap| {
        let x = if gap >= sheet.columns.len() {
          layout.total_width() + grid_layout::COLUMN_GAP / 2.0
        } else {
          layout.column_lefts[gap] - grid_layout::COLUMN_GAP / 2.0
        };
        div()
          .absolute()
          .left(px((origin_x + x) * zoom - 2.0))
          .top(px(origin_y * zoom))
          .w(px(4.0))
          .h(px(layout.total_height() * zoom))
          .rounded_full()
          .bg(cx.theme().primary)
          .into_any_element()
      });

    div()
      .id("flow-grid-content")
      .relative()
      .w(content_width)
      .h(content_height)
      .min_w_full()
      .min_h_full()
      // Content origin capture: everything (ink, drags, presence chips)
      // measures from here.
      .child(
        canvas(
          {
            let weak = cx.entity().downgrade();
            move |bounds, _, cx| {
              let _ = weak.update(cx, |editor, _| editor.set_viewport_origin(bounds.origin));
            }
          },
          |_, _, _, _| {},
        )
        .absolute()
        .inset_0(),
      )
      // The gridlines, under every slot (screen-constant 1px hairlines).
      .child(
        canvas(|_, _, _| {}, {
          move |bounds, _, window, _| {
            let (extent_x, extent_y) = grid_extent;
            for line_x in &line_xs {
              let x = bounds.origin.x + px((origin_x + line_x) * zoom);
              window.paint_quad(gpui::fill(
                gpui::Bounds::new(
                  point(x, bounds.origin.y + px(origin_y * zoom)),
                  gpui::size(px(1.0), px(extent_y * zoom)),
                ),
                grid_line_color,
              ));
            }
            for line_y in &line_ys {
              let y = bounds.origin.y + px((origin_y + line_y) * zoom);
              window.paint_quad(gpui::fill(
                gpui::Bounds::new(
                  point(bounds.origin.x + px(origin_x * zoom), y),
                  gpui::size(px(extent_x * zoom), px(1.0)),
                ),
                grid_line_color,
              ));
            }
          }
        })
        .absolute()
        .inset_0(),
      )
      // Slot-drop plumbing: the pointer's slot is pure geometry (O(log n)).
      .on_drag_move(cx.listener(|editor, event: &DragMoveEvent<FlowCellDrag>, _, cx| {
        if event.bounds.contains(&event.event.position) {
          editor.update_cell_drag_target(event.event.position, cx);
        }
      }))
      .on_drop(cx.listener(|editor, drag: &FlowCellDrag, _, cx| editor.finish_cell_drop(drag.cell_id, cx)))
      .on_drag_move(cx.listener(|editor, event: &DragMoveEvent<RowDrag>, _, cx| {
        if event.bounds.contains(&event.event.position) {
          editor.update_row_drag_gap(event.event.position, cx);
        }
      }))
      .on_drop(cx.listener(|editor, drag: &RowDrag, _, cx| editor.finish_row_drop(drag.row_id, cx)))
      .on_drag_move(cx.listener(|editor, event: &DragMoveEvent<ColumnDrag>, _, cx| {
        if event.bounds.contains(&event.event.position) {
          editor.update_column_drag_gap(event.event.position, cx);
        }
      }))
      .on_drop(cx.listener(|editor, drag: &ColumnDrag, _, cx| editor.finish_column_drop(drag.column_id, cx)))
      .on_drag_move(cx.listener(|editor, event: &DragMoveEvent<ColumnResizeDrag>, _, cx| {
        editor.update_column_resize(event.event.position, cx);
      }))
      .on_drop(cx.listener(|editor, _: &ColumnResizeDrag, _, cx| editor.finish_column_resize(cx)))
      .children(slots)
      .children(row_bar)
      .children(column_bar)
      // S11: peer name chips ride their focused cells' top-right corner.
      .children(self.external_presences.iter().filter_map(|presence| {
        let cell = presence.cell.filter(|_| presence.sheet == self.active_sheet)?;
        let bounds = self.cell_bounds.get(&cell)?;
        let color = gpui::Hsla::from(rgba((presence.color_rgb << 8) | 0xff));
        let local_x = px(bounds.right().as_f32() - self.viewport_origin.x - 8.0);
        let local_y = px(bounds.top().as_f32() - self.viewport_origin.y - 9.0);
        Some(
          div()
            .absolute()
            .left(local_x)
            .top(local_y)
            .px(px(5.0))
            .rounded_full()
            .bg(color)
            .text_color(gpui::white())
            .text_size(px(10.0))
            .whitespace_nowrap()
            .child(SharedString::from(format!(
              "{}{}",
              presence.name,
              if presence.editing { " ✎" } else { "" }
            ))),
        )
      }))
      .child(
        div()
          .id("flow-annotation-layer")
          .absolute()
          .inset_0()
          .child(
            canvas(
              |_, _, _| {},
              {
                move |bounds, _, window, _| {
                  // Add the canvas's LIVE origin (== content origin this frame)
                  // to each content-relative anchor, so ink stays locked to the
                  // grid while panning instead of trailing by a frame.
                  for (anchor_offset, stroke) in &strokes {
                    let color = gpui::Hsla::from(rgba(stroke.style.color_rgba));
                    paint_stroke(
                      bounds.origin + *anchor_offset,
                      &stroke.points,
                      px(stroke.style.width * zoom),
                      color.opacity(color.a * stroke.style.opacity),
                      zoom,
                      window,
                    );
                  }
                  if !draft.is_empty() {
                    let local: Vec<flowstate_flow::StrokePoint> = draft
                      .iter()
                      .map(|point| flowstate_flow::StrokePoint { x: point.x, y: point.y })
                      .collect();
                    paint_stroke(bounds.origin + draft_origin, &local, px(4.0 * zoom), gpui::Hsla::from(rgba(draft_color)).opacity(0.55), zoom, window);
                  }
                }
              },
            )
            .absolute()
            .size_full(),
          ),
      )
      .into_any_element()
  }

  /// The frozen header strip: column labels + drag/reorder + resize + the
  /// add-column verb. Rides the horizontal scroll only.
  fn render_header_overlay(&mut self, cx: &mut Context<Self>) -> AnyElement {
    let Some((layout, sheet)) = self.active_layout() else {
      return div().into_any_element();
    };
    let zoom = self.board_zoom;
    let offset = self.board_scroll.offset();
    let header_height = px(HEADER_HEIGHT * zoom);
    let mut children: Vec<AnyElement> = Vec::new();
    for (column_ix, column) in sheet.columns.iter().enumerate() {
      let side = flow_side_palette(column.side, cx);
      // The overlay container already sits at the grid's x origin (the
      // gutter edge) — children position in bare column space.
      let left = px(layout.column_lefts[column_ix] * zoom) + offset.x;
      let width = px(layout.column_widths[column_ix] * zoom);
      let column_id = column.id;
      let label: SharedString = column.label.clone().into();
      let weak = cx.entity().downgrade();
      let drag_label = label.clone();
      children.push(
        div()
          .id(("flow-column-header", column_ix))
          .absolute()
          .left(left)
          .top(px(0.0))
          .w(width)
          .h(header_height)
          .flex()
          .items_center()
          .justify_between()
          .px(px(6.0 * zoom))
          .bg(side.base.opacity(0.04))
          .font_weight(gpui::FontWeight::BOLD)
          .text_size(px(13.0 * zoom))
          .text_color(side.base)
          .border_b(px(2.0 * zoom))
          .border_color(side.base)
          .cursor_grab()
          .on_drag(ColumnDrag { column_id }, move |drag, _, _, cx| {
            let _ = weak.update(cx, |editor, cx| {
              editor.dragging_column = Some(drag.column_id);
              cx.notify();
            });
            cx.new(|_| ColumnDragPreview { label: drag_label.clone() })
          })
          .child(label)
          .child(
            Button::new(("flow-column-add-cell", column_ix))
              .with_size(px(18.0 * zoom))
              .ghost()
              .tooltip("Add a card in this column")
              .icon(IconName::Plus)
              .on_click(cx.listener(move |editor, _, window, cx| {
                cx.stop_propagation();
                editor.set_annotation_tool(AnnotationTool::None, cx);
                editor.add_cell_in_column(column_ix, cx);
                editor.focus_active_cell(window, cx);
              })),
          )
          .into_any_element(),
      );
      // The column boundary separator, aligned with the grid's vertical
      // hairline below it.
      children.push(
        div()
          .absolute()
          .left(left + width - px(1.0))
          .top(px(0.0))
          .w(px(1.0))
          .h(header_height)
          .bg(cx.theme().border.opacity(0.55))
          .into_any_element(),
      );
      // The resize grip on the column's right edge; double-click = autofit.
      let start_width = layout.column_widths[column_ix];
      let weak = cx.entity().downgrade();
      children.push(
        div()
          .id(("flow-column-resize", column_ix))
          .absolute()
          .left(left + width - px(3.0))
          .top(px(0.0))
          .w(px(7.0))
          .h(header_height)
          .cursor(gpui::CursorStyle::ResizeLeftRight)
          .on_drag(
            ColumnResizeDrag { column_id, start_width },
            move |drag, _, _, cx| {
              let _ = weak.update(cx, |editor, cx| {
                editor.column_resize = Some(ColumnResizeState {
                  column_id: drag.column_id,
                  start_width: drag.start_width,
                  start_x: None,
                  live_width: drag.start_width,
                });
                cx.notify();
              });
              cx.new(|_| EmptyDragPreview)
            },
          )
          .on_click(cx.listener(move |editor, event: &gpui::ClickEvent, _, cx| {
            if event.click_count() >= 2 {
              editor.clear_autofit_width(column_id, cx);
            }
          }))
          .into_any_element(),
      );
    }
    // Add-column verb at the right edge of the last column.
    let add_left = px((layout.total_width() + 8.0) * zoom) + offset.x;
    children.push(
      Button::new("flow-add-column")
        .with_size(px(22.0 * zoom))
        .ghost()
        .tooltip("Add a column")
        .icon(IconName::Plus)
        .absolute()
        .left(add_left)
        .top(px(4.0))
        .on_click(cx.listener(|editor, _, _, cx| editor.add_column_end(cx)))
        .into_any_element(),
    );

    div()
      .absolute()
      .top(px(0.0))
      .left(px(GUTTER_WIDTH * zoom))
      .right(px(0.0))
      .h(header_height)
      .overflow_hidden()
      .bg(cx.theme().background.opacity(0.96))
      .border_b_1()
      .border_color(cx.theme().border.opacity(0.5))
      .children(children)
      .into_any_element()
  }

  /// The frozen row-number gutter: numbers, row drag handles, row selection.
  fn render_gutter_overlay(&mut self, cx: &mut Context<Self>) -> AnyElement {
    let Some((layout, sheet)) = self.active_layout() else {
      return div().into_any_element();
    };
    let zoom = self.board_zoom;
    let offset = self.board_scroll.offset();
    let (_, origin_y) = grid_origin_model();
    let viewport = self.board_scroll.bounds().size;
    let view_top = (-offset.y.as_f32()) / zoom - origin_y;
    let view_bottom = view_top + viewport.height.as_f32().max(1.0) / zoom;
    let visible = layout.visible_rows(view_top, view_bottom);
    let mut children: Vec<AnyElement> = Vec::new();
    for row_ix in visible {
      let is_ghost = row_ix >= layout.real_rows;
      // The overlay container already sits at the grid's y origin (the
      // header edge) — children position in bare row space.
      let top = px(layout.row_top(row_ix) * zoom) + offset.y;
      let height = px(layout.row_height(row_ix) * zoom);
      let row_id = grid_layout::row_id_at(sheet, row_ix);
      let selected_row = self.cursor.is_some_and(|(cursor_row, _)| cursor_row == row_ix);
      let weak = cx.entity().downgrade();
      children.push(
        div()
          .id(("flow-row-number", row_ix))
          .absolute()
          .left(px(0.0))
          .top(top)
          .w(px(GUTTER_WIDTH * zoom))
          .h(height)
          .flex()
          .items_center()
          .justify_center()
          .border_b(px(1.0))
          .border_color(cx.theme().border.opacity(0.55))
          .text_size(px(10.5 * zoom))
          .text_color(if selected_row {
            cx.theme().foreground
          } else {
            cx.theme().muted_foreground.opacity(if is_ghost { 0.35 } else { 0.8 })
          })
          .when(selected_row, |this| this.bg(cx.theme().primary.opacity(0.12)))
          .hover(|style| style.bg(cx.theme().primary.opacity(0.08)))
          .cursor_grab()
          .child(SharedString::from(format!("{}", row_ix + 1)))
          .when_some(row_id, |this, row_id| {
            this
              .on_drag(RowDrag { row_id }, move |drag, _, _, cx| {
                let _ = weak.update(cx, |editor, cx| {
                  editor.dragging_row = Some(drag.row_id);
                  cx.notify();
                });
                cx.new(|_| RowDragPreview {
                  label: SharedString::from(format!("row {}", row_ix + 1)),
                })
              })
              .on_click(cx.listener(move |editor, event: &gpui::ClickEvent, _, cx| {
                if event.click_count() >= 2 {
                  editor.clear_row_height_override(row_id, cx);
                } else {
                  editor.select_row(row_ix, cx);
                }
              }))
          })
          .into_any_element(),
      );
    }
    div()
      .absolute()
      .top(px(HEADER_HEIGHT * zoom))
      .left(px(0.0))
      .bottom(px(0.0))
      .w(px(GUTTER_WIDTH * zoom))
      .overflow_hidden()
      .bg(cx.theme().background.opacity(0.96))
      .border_r_1()
      .border_color(cx.theme().border.opacity(0.5))
      .children(children)
      .into_any_element()
  }
}

/// The grid's model-space origin inside the content div: room for the frozen
/// gutter + header plus the board padding.
pub(super) fn grid_origin_model() -> (f32, f32) {
  (GUTTER_WIDTH + BOARD_PADDING, HEADER_HEIGHT + BOARD_PADDING)
}

/// Model→screen for one axis of a coordinate inside the scrolled board content:
/// `(pan_origin + model) * zoom`, CONTENT-RELATIVE. The live content-div origin
/// (a canvas `bounds.origin`, or gpui's own placement of an absolutely
/// positioned child) is added on top by the caller. Cells AND annotation ink
/// both map through this one function, so they share a single frame and stay
/// locked together while panning. Baking a cached origin into ink instead lagged
/// it one frame behind the grid (a visible wobble on pan).
pub(super) fn board_content_offset(pan_origin: f32, model: f32, zoom: f32) -> gpui::Pixels {
  px((pan_origin + model) * zoom)
}

impl EventEmitter<FlowEditorEvent> for FlowEditor {}

impl Focusable for FlowEditor {
  fn focus_handle(&self, _: &App) -> FocusHandle {
    self.focus_handle.clone()
  }
}

impl Render for FlowEditor {
  fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    self.apply_pending_camera_center();
    self.refresh_active_cell_theme(cx);
    // Refusal voice: keep frames coming while the toast/shake animates.
    let refusal_toast = self.refusal_toast();
    if self.refusal.is_some() {
      cx.on_next_frame(window, |_, _, cx| cx.notify());
    }
    if !cx.has_active_drag() {
      if self.dragging_cell.is_some() || self.drag_target.is_some() {
        self.dragging_cell = None;
        self.drag_target = None;
      }
      if self.dragging_row.is_some() || self.row_drop_gap.is_some() {
        self.dragging_row = None;
        self.row_drop_gap = None;
      }
      if self.dragging_column.is_some() || self.column_drop_gap.is_some() {
        self.dragging_column = None;
        self.column_drop_gap = None;
      }
      if self.column_resize.is_some() {
        self.finish_column_resize(cx);
      }
    }
    if self.pan_drag.is_some() {
      let editor = cx.entity();
      window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        if phase.bubble() && event.button == MouseButton::Left {
          editor.update(cx, |editor, cx| editor.finish_space_pan(cx));
        }
      });
    }
    let grid_scroll = self.board_scroll.clone();
    let board_zoom = self.board_zoom;
    self.built_scroll_offset.set(self.board_scroll.offset());
    // Part 4: column washes belong to the aether — each side's tint starts
    // under the header and runs to the bottom of the viewport.
    let wash_columns: Vec<(f32, f32, gpui::Hsla)> = if self.scrubber.is_some() {
      Vec::new()
    } else {
      self
        .active_layout()
        .map(|(layout, sheet)| {
          sheet
            .columns
            .iter()
            .enumerate()
            .map(|(column_ix, column)| {
              (
                layout.column_lefts[column_ix],
                layout.column_widths[column_ix],
                flow_side_palette(column.side, cx).base,
              )
            })
            .collect()
        })
        .unwrap_or_default()
    };
    let scroll_sync = {
      let weak = cx.entity().downgrade();
      let handle = self.board_scroll.clone();
      canvas(
        |_, _, _| {},
        move |bounds, _, _, cx| {
          // Frozen chrome + row-window resync: the element tree was built
          // for a different scroll offset or viewport size — rebuild once.
          let _ = weak.update(cx, |editor, cx| {
            let viewport_changed = editor.built_viewport.get() != bounds.size;
            if viewport_changed {
              editor.built_viewport.set(bounds.size);
            }
            if viewport_changed || editor.built_scroll_offset.get() != handle.offset() {
              cx.notify();
            }
          });
        },
      )
      .absolute()
      .inset_0()
    };
    div()
      .id("flow-editor")
      .relative()
      .size_full()
      .track_focus(&self.focus_handle)
      .on_key_down(cx.listener(|editor, event: &KeyDownEvent, window, cx| {
        // Escape disarms the ink tool.
        if event.keystroke.key == "escape" && editor.annotation_tool != AnnotationTool::None {
          editor.set_annotation_tool(AnnotationTool::None, cx);
          cx.stop_propagation();
          return;
        }
        let board_focused = editor.focus_handle.is_focused(window);
        // Escape from a focused cell editor returns to cell-select mode.
        if event.keystroke.key == "escape" && !board_focused && editor.active_cell.is_some() {
          editor.focus_handle.focus(window);
          cx.stop_propagation();
          return;
        }
        if board_focused {
          let modifiers = event.keystroke.modifiers;
          if !modifiers.control && !modifiers.alt && !modifiers.platform {
            match event.keystroke.key.as_str() {
              "up" => {
                editor.navigate(GridDirection::Up, cx);
                cx.stop_propagation();
                return;
              },
              "down" | "enter" => {
                editor.navigate(GridDirection::Down, cx);
                cx.stop_propagation();
                return;
              },
              "left" => {
                editor.navigate(GridDirection::Left, cx);
                cx.stop_propagation();
                return;
              },
              "right" | "tab" => {
                editor.navigate(GridDirection::Right, cx);
                cx.stop_propagation();
                return;
              },
              _ => {},
            }
            // D5: typing on an empty slot IS creation — the keystroke seeds
            // the new cell so nothing is lost.
            if editor.annotation_tool == AnnotationTool::None
              && editor.active_cell.is_none()
              && editor.cursor.is_some()
              && let Some(key_char) = event.keystroke.key_char.clone()
              && !key_char.is_empty()
              && !key_char.chars().any(char::is_control)
            {
              if editor.type_into_cursor(&key_char, cx).is_some() {
                cx.on_next_frame(window, |flow, window, cx| {
                  let Some(editor) = flow
                    .active_cell
                    .and_then(|cell| flow.cell_editors.get(&cell))
                    .cloned()
                  else {
                    return;
                  };
                  editor.update(cx, |editor, cx| editor.move_document_end(cx));
                  editor.read(cx).focus_handle(cx).focus(window);
                });
              }
              cx.stop_propagation();
              return;
            }
          }
        }
        // Space arms panning only when the board itself holds focus.
        if event.keystroke.key == "space" && board_focused {
          editor.space_pan_armed = true;
          cx.stop_propagation();
          cx.notify();
        }
      }))
      .on_key_up(cx.listener(|editor, event: &KeyUpEvent, _, cx| {
        if event.keystroke.key == "space" && editor.space_pan_armed {
          editor.space_pan_armed = false;
          editor.finish_space_pan(cx);
          cx.stop_propagation();
        }
      }))
      .on_mouse_down(
        MouseButton::Left,
        cx.listener(|editor, event: &MouseDownEvent, _, cx| {
          if editor.space_pan_armed {
            editor.begin_pan(event.position, cx);
            cx.stop_propagation();
          }
        }),
      )
      // + : middle-mouse drag pans anywhere — the universal escape hatch that
      // works even when the pointer starts on a card or the chrome.
      .on_mouse_down(
        MouseButton::Middle,
        cx.listener(|editor, event: &MouseDownEvent, _, cx| {
          editor.begin_pan(event.position, cx);
          cx.stop_propagation();
        }),
      )
      .on_mouse_up(
        MouseButton::Middle,
        cx.listener(|editor, _: &MouseUpEvent, _, cx| editor.finish_space_pan(cx)),
      )
      // G : right-button drag inks a freehand stroke in the user's profile
      // color, no tool armed. A right-click with no drag draws nothing.
      .on_mouse_down(
        MouseButton::Right,
        cx.listener(|editor, event: &MouseDownEvent, _, cx| {
          editor.begin_ink(event.position, cx);
          cx.stop_propagation();
        }),
      )
      .on_mouse_up(
        MouseButton::Right,
        cx.listener(|editor, _: &MouseUpEvent, _, cx| editor.finish_annotation(cx)),
      )
      .on_mouse_move(cx.listener(|editor, event: &MouseMoveEvent, window, cx| {
        if editor.right_inking {
          editor.continue_annotation(event.position, cx);
        }
        editor.queue_pan(event.position, window, cx);
      }))
      .on_mouse_up(
        MouseButton::Left,
        cx.listener(|editor, _: &MouseUpEvent, _, cx| editor.finish_space_pan(cx)),
      )
      .child(
        canvas(
          |_, _, _| {},
          move |bounds, _, window, _| {
            // A flat sheet under the grid: only the column washes remain —
            // the dot aether belonged to the board metaphor, not a
            // spreadsheet.
            let offset = grid_scroll.offset();
            let (origin_x, _) = grid_origin_model();
            for (column_left, column_width, side_color) in &wash_columns {
              let left = bounds.origin.x + offset.x + px((origin_x + column_left) * board_zoom);
              let top = bounds.origin.y + px(HEADER_HEIGHT * board_zoom);
              let wash = gpui::Bounds::from_corners(
                point(left.max(bounds.origin.x), top),
                point(
                  (left + px(column_width * board_zoom)).min(bounds.origin.x + bounds.size.width),
                  bounds.origin.y + bounds.size.height,
                ),
              );
              if wash.size.width <= px(0.0) || wash.size.height <= px(0.0) {
                continue;
              }
              window.paint_quad(gpui::fill(wash, side_color.opacity(0.035)));
            }
          },
        )
        .absolute()
        .inset_0(),
      )
      .child(
        div()
          .id("flow-board-scroll")
          .relative()
          .size_full()
          .overflow_scroll()
          .track_scroll(&self.board_scroll)
          .cursor(if self.annotation_tool != AnnotationTool::None {
            gpui::CursorStyle::Crosshair
          } else if self.pan_drag.is_some() {
            gpui::CursorStyle::ClosedHand
          } else {
            gpui::CursorStyle::Arrow
          })
          .on_scroll_wheel(cx.listener(|editor, event: &ScrollWheelEvent, window, cx| {
            if event.modifiers.shift {
              let delta = event.delta.pixel_delta(window.line_height());
              let mut offset = editor.board_scroll.offset();
              offset.x += delta.y + delta.x;
              editor.set_user_scroll_offset(offset);
              cx.stop_propagation();
            } else if !event.modifiers.control {
              editor.camera_center = None;
              cx.on_next_frame(window, |editor, _, _| editor.sync_camera_center_from_scroll());
            }
            // The frozen chrome tracks the offset — rebuild.
            cx.notify();
          }))
          .when(self.annotation_tool != AnnotationTool::None && !self.space_pan_armed, |this| {
            this
              .on_mouse_down(
                MouseButton::Left,
                cx.listener(|editor, event: &MouseDownEvent, _, cx| editor.begin_annotation(event.position, cx)),
              )
              .on_mouse_move(cx.listener(|editor, event: &MouseMoveEvent, _, cx| editor.continue_annotation(event.position, cx)))
              .on_mouse_up(
                MouseButton::Left,
                cx.listener(|editor, _: &MouseUpEvent, _, cx| editor.finish_annotation(cx)),
              )
              .on_mouse_up_out(
                MouseButton::Left,
                cx.listener(|editor, _: &MouseUpEvent, _, cx| editor.finish_annotation(cx)),
              )
          })
          .child(match self.active_sheet {
            // R6: replay mode swaps the live grid for the read-only view.
            Some(_) if self.scrubber.is_some() => self.render_history_view(cx),
            Some(_) => self.render_grid(cx),
            None => div()
              .size_full()
              .flex()
              .items_center()
              .justify_center()
              .text_color(cx.theme().muted_foreground)
              .child("Create a sheet to begin flowing")
              .into_any_element(),
          }),
      )
      // Frozen chrome: header (columns) + gutter (row numbers) + corner.
      .when(self.active_sheet.is_some() && self.scrubber.is_none(), |this| {
        this
          .child(self.render_header_overlay(cx))
          .child(self.render_gutter_overlay(cx))
          .child(
            div()
              .absolute()
              .top(px(0.0))
              .left(px(0.0))
              .w(px(GUTTER_WIDTH * self.board_zoom))
              .h(px(HEADER_HEIGHT * self.board_zoom))
              .bg(cx.theme().background)
              .border_b_1()
              .border_r_1()
              .border_color(cx.theme().border.opacity(0.5)),
          )
      })
      .child(scroll_sync)
      .child(
        div()
          .absolute()
          .inset_0()
          .child(Scrollbar::vertical(&self.board_scroll).scrollbar_show(ScrollbarShow::Always))
          .child(Scrollbar::horizontal(&self.board_scroll).scrollbar_show(ScrollbarShow::Always)),
      )
      // FLOWSTATE_INTENT_LOG: the alpha-calibration overlay.
      .when(Self::intent_log_enabled() && !self.intent_log.is_empty(), |this| {
        this.child(
          div()
            .absolute()
            .top(px(8.0))
            .right(px(16.0))
            .flex()
            .flex_col()
            .gap_0p5()
            .px_2()
            .py_1()
            .rounded(cx.theme().radius)
            .bg(cx.theme().popover.opacity(0.85))
            .border_1()
            .border_color(cx.theme().border)
            .text_size(px(11.0))
            .text_color(cx.theme().muted_foreground)
            .font_family("monospace")
            .children(self.intent_log.iter().cloned()),
        )
      })
      // F3: the refusal SPEAKS — a transient toast naming the reason.
      .when_some(refusal_toast, |this, (message, _, age)| {
        let fade = ((2.4 - age) / 0.5).clamp(0.0, 1.0);
        this.child(
          div()
            .absolute()
            .bottom(px(20.0))
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(
              div()
                .px_3()
                .py_1p5()
                .rounded(cx.theme().radius)
                .bg(cx.theme().popover.opacity(0.92 * fade))
                .border_1()
                .border_color(cx.theme().border.opacity(fade))
                .text_color(cx.theme().popover_foreground.opacity(fade))
                .text_size(px(13.0))
                .child(message),
            ),
        )
      })
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  /// Ink and cells must map identical model coordinates to identical
  /// content-relative screen offsets — the same frame — so once each is added
  /// to the live content origin at paint they stay locked together on pan. The
  /// wobble bug was ink carrying a cached viewport origin that cells didn't;
  /// routing both through `board_content_offset` makes divergence impossible.
  #[test]
  fn ink_shares_the_cell_content_frame_and_is_rigid() {
    // Arbitrary pan origin + fractional zoom (the regime where a stale origin
    // shows): a stroke anchored to a slot with ZERO local offset lands exactly
    // where the cell at that slot lands.
    let (origin_x, origin_y, zoom) = (-137.4_f32, 22.0_f32, 0.85_f32);
    let (slot_x, slot_y) = (280.0_f32, 132.0_f32);

    let cell = point(board_content_offset(origin_x, slot_x, zoom), board_content_offset(origin_y, slot_y, zoom));
    let ink = point(
      board_content_offset(origin_x, slot_x + 0.0, zoom),
      board_content_offset(origin_y, slot_y + 0.0, zoom),
    );
    assert_eq!(cell, ink, "ink at a slot with no local offset must coincide with that slot's cell");

    // A local offset is a rigid displacement: it shifts the ink by exactly the
    // local delta * zoom and nothing else — no viewport term leaking in.
    let ink_shifted = point(
      board_content_offset(origin_x, slot_x + 10.0, zoom),
      board_content_offset(origin_y, slot_y + 4.0, zoom),
    );
    assert!((f32::from(ink_shifted.x - cell.x) - 10.0 * zoom).abs() < 0.01, "x displacement is the local delta * zoom");
    assert!((f32::from(ink_shifted.y - cell.y) - 4.0 * zoom).abs() < 0.01, "y displacement is the local delta * zoom");
  }
}
