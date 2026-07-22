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
use gpui_component::{IconName, Sizable as _, WindowExt as _};

use crate::{
  app_settings::load_document_theme,
  flow::{cell_theme::apply_flow_cell_theme, resolve_flow_theme},
  rich_text_element::{RichTextDocumentElement, RichTextEditor},
};

mod annotation;
mod cell_editing;
mod clipboard;
mod grid_layout;
mod grid_nav;
mod preview;
mod zoom;

pub(crate) use preview::{FlowPreview, preview_cell_ids, render_flow_board_preview, theme_flow_preview};

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
  /// F4/Q-22: "Send column to new document" — the workspace opens a fresh
  /// .db8 and types the column's cards in, one paragraph per cell.
  SendColumnToDocument { text: String },
  /// G2: the slot cursor / marquee / live-ink state changed in a way only
  /// presence cares about — the session refreshes its published hand.
  PresenceShifted,
  /// Q-21/F2: "Open source card" — the workspace opens the evidence document
  /// this cell was flowed from.
  OpenCellSource { path: String },
}

/// C11: the pen's width/opacity preset — color always comes from the user's
/// collab identity (Q-8: peer-color-only pens).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum PenPreset {
  Fine,
  #[default]
  Marker,
  Highlighter,
}

impl PenPreset {
  pub fn width(self) -> f32 {
    match self {
      Self::Fine => 2.0,
      Self::Marker => 4.0,
      Self::Highlighter => 9.0,
    }
  }

  pub fn opacity(self) -> f32 {
    match self {
      Self::Fine => 0.9,
      Self::Marker => 0.55,
      Self::Highlighter => 0.35,
    }
  }

  pub fn label(self) -> &'static str {
    match self {
      Self::Fine => "Fine",
      Self::Marker => "Marker",
      Self::Highlighter => "Wide",
    }
  }
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
  /// G2: the slot cursor (real even on empty slots).
  pub slot: Option<(usize, usize)>,
  /// G2: the selection rectangle (r0, c0, r1, c1).
  pub selection_rect: Option<(usize, usize, usize, usize)>,
  /// G2: the live ink draft, board-space rounded.
  pub ink_preview: Vec<(i32, i32)>,
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
  /// G2: the peer's slot — real even over EMPTY slots.
  pub slot: Option<(usize, usize)>,
  /// G2: the peer's selection rectangle (r0, c0, r1, c1).
  pub selection_rect: Option<(usize, usize, usize, usize)>,
  /// G2: the peer's in-flight ink draft, board-space.
  pub ink_preview: Vec<(i32, i32)>,
}

/// A spoken refusal (F3): message toast + optional cell shake.
struct RefusalNotice {
  message: String,
  cell: Option<CellId>,
  at: std::time::Instant,
  /// P2/C6: the floating toast for this refusal has been pushed (render
  /// pushes exactly once; the notice itself keeps driving the shake).
  toasted: bool,
}

/// P2/C6: notification id namespace — rapid refusals REPLACE the previous
/// refusal toast instead of stacking into spam.
struct FlowRefusalToast;

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

/// Drag payload for the selection's fill handle (the source is the current
/// selection, so it carries no data).
#[derive(Clone)]
pub(super) struct FillHandleDrag;

pub(super) struct FlowCellDragPreview {
  label: SharedString,
  fill: gpui::Hsla,
  text_color: gpui::Hsla,
}

impl Render for FlowCellDragPreview {
  fn render(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
    // A faithful mini-card: the cell's own fill and text color, lifted off the
    // sheet with a soft shadow so it reads as "picked up," not a foreign chip.
    // C7: composed from FlowTheme slots — the preview hovers OVER the board,
    // but it depicts a piece OF the board.
    let flow_theme = resolve_flow_theme();
    div()
      .px(px(8.0))
      .py(px(5.0))
      .rounded(px(4.0))
      .bg(self.fill)
      .border_1()
      .border_color(flow_theme.selection)
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
  fn render(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
    // C7: FlowTheme slots — see FlowCellDragPreview.
    let flow_theme = resolve_flow_theme();
    div()
      .px_2()
      .py_0p5()
      .rounded(px(4.0))
      .bg(flow_theme.header_bg.opacity(0.95))
      .border_1()
      .border_color(flow_theme.chrome_border)
      .text_size(px(11.0))
      .text_color(flow_theme.muted_text)
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
  fn render(&mut self, _: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
    // C7: FlowTheme slots — see FlowCellDragPreview.
    let flow_theme = resolve_flow_theme();
    div()
      .px_3()
      .py_1()
      .rounded(px(4.0))
      .bg(flow_theme.header_bg.opacity(0.95))
      .border_1()
      .border_color(flow_theme.chrome_border)
      .text_size(px(12.0))
      .text_color(flow_theme.text)
      .child(self.label.clone())
  }
}

#[derive(Clone)]
pub(super) struct ColumnResizeDrag {
  pub column_id: ColumnId,
  pub start_width: f32,
}

#[derive(Clone)]
pub(super) struct RowResizeDrag {
  pub row_id: RowId,
  pub start_height: f32,
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

struct RowResizeState {
  row_id: RowId,
  start_height: f32,
  start_y: Option<f32>,
  live_height: f32,
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
  /// E12: ONE app-global ink-visibility switch (persisted in app settings) —
  /// per-sheet hiding was a lie the tooltip told.
  ink_visible: bool,
  hidden_annotation_originators: HashSet<AnnotationOriginator>,
  local_annotation_originator: AnnotationOriginator,
  /// I-S2: the pen's current color (theme-derived swatches on the marker
  /// chip). Flowing tradition is color-coded pens.
  marker_color_rgba: u32,
  /// C11: the armed pen's width/opacity preset.
  pen_preset: PenPreset,
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
  row_resize: Option<RowResizeState>,
  /// W2 multi-select: the set every op applies to (always contains the
  /// active cell while non-empty).
  selected_cells: HashSet<CellId>,
  /// W2 range grammar: the (row, column) a shift-extend measures its rectangle
  /// from. Set on plain click / ctrl-toggle; untouched by a shift-extend.
  selection_anchor: Option<(usize, usize)>,
  /// Excel tab-anchor: the column a run of Tabs started in, so Enter after the
  /// run drops one row and returns to it. Set on the first Tab of a run,
  /// consumed by Enter, cleared by any other navigation.
  tab_anchor_column: Option<usize>,
  /// Shift-drag marquee: the slot the rubber-band rectangle is anchored at.
  /// `Some` while a shift-drag is in progress; the moving corner tracks the
  /// pointer. Bare drag stays panning.
  marquee_anchor: Option<(usize, usize)>,
  /// Ctrl+X source cells — WITH their home sheet — to delete when the cut is
  /// pasted (Excel move). The sheet rides along so a cross-sheet paste deletes
  /// the sources where they actually live. Cleared on a fresh copy or after
  /// the paste consumes it. The instant feeds the C4 marquee pulse.
  cut_pending: Option<(flowstate_flow::SheetId, Vec<CellId>, std::time::Instant)>,
  /// C4: a slow repaint tick is scheduled while the cut marquee pulses.
  cut_pulse_scheduled: bool,
  /// A6: while `Some`, gate rejections inside a bulk gesture are counted here
  /// instead of toasting one-by-one; the gesture's end refuses once, with
  /// words. `None` = solo intents toast directly (A7).
  bulk_failures: Option<usize>,
  /// The slot the fill handle is being dragged over (the fill's far corner).
  fill_target: Option<(usize, usize)>,
  /// A4/Q-23: cells to flash briefly — remote structural changes and bump-down
  /// landings paint a fading wash so the grid never rearranges silently.
  cell_flash: std::collections::HashMap<CellId, std::time::Instant>,
  /// C3: the slot rectangle the current selection was formed from (range /
  /// marquee / row / column / all). `None` for irregular ctrl-click sets —
  /// those keep per-cell rings. `(r0, c0, r1, c1)`, inclusive.
  selection_rect: Option<(usize, usize, usize, usize)>,
  /// B7: a whole-row band selected from the gutter — Delete and row drags
  /// operate on the ROWS, not just their cells. `(first, last)`, inclusive.
  selected_row_band: Option<(usize, usize)>,
  /// B7: the column twin, from header selection.
  selected_column_band: Option<(usize, usize)>,
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
  /// G6: an in-flight rename of the selected tape mark — `(mark id, input)`.
  tape_rename: Option<(u128, Entity<gpui_component::input::InputState>)>,
  tape_rename_subscription: Option<gpui::Subscription>,
  /// B4: an open right-click menu — (window position, menu view).
  context_menu: Option<(gpui::Point<gpui::Pixels>, Entity<gpui_component::menu::PopupMenu>)>,
  /// D5: inline column rename — (column id, anchor position, input).
  column_rename: Option<(ColumnId, gpui::Point<gpui::Pixels>, Entity<gpui_component::input::InputState>)>,
  column_rename_subscription: Option<gpui::Subscription>,
  /// D8/Q-17: numeric column-width entry — (column id, anchor, input).
  column_width_entry: Option<(ColumnId, gpui::Point<gpui::Pixels>, Entity<gpui_component::input::InputState>)>,
  column_width_subscription: Option<gpui::Subscription>,
  /// E10: the round-metadata form (opened from the ribbon's Round chip).
  round_form: Option<Vec<(flowstate_flow::RoundField, Entity<gpui_component::input::InputState>)>>,
  round_form_subscriptions: Vec<gpui::Subscription>,
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
  /// B2: Esc cancelled the gesture in flight — the release's drop event (which
  /// gpui will still deliver) must be swallowed, not applied.
  suppress_drop: bool,
  /// B8: Excel's Enter-mode — true while the focused cell editor was entered
  /// by TYPE-OVER (a printable key on the slot), so bare arrows commit and
  /// move instead of walking the caret. F2/click entry clears it.
  typeover_mode: bool,
  /// B1: live edge-autoscroll vector while a drag/marquee hugs the viewport
  /// edge — applied per frame in render until the gesture ends.
  autoscroll: Option<(f32, f32)>,
  /// D11: the rich twin of the TSV clipboard — the same copy's cells as
  /// paragraph seeds so an intra-app paste keeps styles/highlights. The TSV
  /// string is the fingerprint: if the system clipboard stops matching it, an
  /// external copy replaced it and the rich payload is stale.
  internal_clipboard: Option<(String, Vec<Vec<Option<Vec<flowstate_document::InputParagraph>>>>)>,
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
      ink_visible: crate::app_settings::load_flow_ink_visible(),
      hidden_annotation_originators: HashSet::new(),
      // I-S1: strokes author under the durable user identity.
      local_annotation_originator: AnnotationOriginator(crate::app_settings::load_local_user_identity().0.to_string()),
      // C10/Q-8: the pen IS your collab identity color — no palette.
      marker_color_rgba: {
        let color_rgb = crate::app_settings::load_local_user_profile().color_rgb & 0x00ff_ffff;
        (color_rgb << 8) | 0xff
      },
      pen_preset: PenPreset::default(),
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
      row_resize: None,
      selected_cells: HashSet::new(),
      selection_anchor: None,
      tab_anchor_column: None,
      marquee_anchor: None,
      cut_pending: None,
      cut_pulse_scheduled: false,
      bulk_failures: None,
      fill_target: None,
      cell_flash: std::collections::HashMap::new(),
      selection_rect: None,
      selected_row_band: None,
      selected_column_band: None,
      refusal: None,
      intent_log: std::collections::VecDeque::new(),
      session_attached: false,
      recovery_path: None,
      recovery_write_pending: false,
      external_presences: Vec::new(),
      scrubber: None,
      tape_rename: None,
      tape_rename_subscription: None,
      context_menu: None,
      column_rename: None,
      column_rename_subscription: None,
      column_width_entry: None,
      column_width_subscription: None,
      round_form: None,
      round_form_subscriptions: Vec::new(),
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
      suppress_drop: false,
      typeover_mode: false,
      autoscroll: None,
      internal_clipboard: None,
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
    // P4 (hybrid ink law): before a LOCAL structural change, capture where the
    // user's own strokes sit in absolute board space; after it commits,
    // re-anchor any that would have moved — your own edits never move your
    // ink. (Remote ops keep translating via the anchors, which is correct.)
    let ink_snapshot = self.ink_compensation_snapshot(intent);
    let wrap_group = ink_snapshot.as_ref().is_some_and(|snapshot| !snapshot.is_empty()) && self.bulk_failures.is_none();
    if wrap_group {
      // The structural op + its ink re-anchors are ONE undo step.
      self.begin_bulk();
    }
    let class = intent.class();
    let outcome = self.handle.apply(intent);
    self.log_intent(match &outcome {
      Ok(_) => format!("{class} ✓"),
      Err(error) => format!("{class} ✗ {error}"),
    });
    // A7: a rejection that slips past the UI pre-checks (a race against a
    // concurrent import, usually) must not die in the debug overlay — it
    // speaks. Inside a bulk gesture it is counted instead and the gesture
    // refuses once at the end (A6).
    if let Err(error) = &outcome {
      match self.bulk_failures.as_mut() {
        Some(count) => *count += 1,
        None => self.refuse(format!("{error}"), None, cx),
      }
    }
    let outcome = match outcome {
      Ok(outcome) => outcome,
      Err(error) => {
        if wrap_group {
          self.end_bulk("edit", cx);
        }
        return Err(error);
      },
    };
    self.board = outcome.board.clone();
    for cell in &outcome.content_cells {
      self.cell_documents.borrow_mut().remove(cell);
      if let Some(editor) = self.cell_editors.get(cell).cloned() {
        editor.update(cx, |editor, cx| editor.sync_projection_from_authority(cx));
      }
    }
    self.after_local_change(cx);
    if let Some(snapshot) = ink_snapshot {
      self.compensate_local_ink(snapshot, cx);
    }
    if wrap_group {
      self.end_bulk("edit", cx);
    }
    Ok(outcome)
  }

  /// P4: the strokes to hold still across this intent — the LOCAL user's ink
  /// on the active sheet with its current absolute board position. `None`
  /// when the intent isn't structural (nothing can move).
  fn ink_compensation_snapshot(&self, intent: &FlowIntent) -> Option<Vec<(flowstate_flow::AnnotationStroke, BoardPoint)>> {
    let structural = matches!(
      intent,
      FlowIntent::InsertRows { .. }
        | FlowIntent::MoveRows { .. }
        | FlowIntent::DeleteRows { .. }
        | FlowIntent::AddColumn { .. }
        | FlowIntent::MoveColumn { .. }
        | FlowIntent::DeleteColumn { .. }
        | FlowIntent::SetColumnWidth { .. }
        | FlowIntent::SetRowHeight { .. }
    );
    if !structural {
      return None;
    }
    let (layout, sheet) = self.active_layout()?;
    Some(
      sheet
        .annotations
        .iter()
        .filter(|stroke| stroke.originator == self.local_annotation_originator)
        .map(|stroke| {
          let (row_ix, column_ix) = sheet.resolve_anchor(&stroke.anchor);
          let (slot_x, slot_y) = layout.slot_origin(row_ix, column_ix);
          (
            stroke.clone(),
            BoardPoint {
              x: slot_x + stroke.anchor.offset.x,
              y: slot_y + stroke.anchor.offset.y,
            },
          )
        })
        .collect(),
    )
  }

  /// P4: re-anchor any of the captured strokes whose resolved position moved,
  /// pinning each back to its pre-change absolute spot (the re-anchor targets
  /// the real slot geometrically under that spot). AddAnnotation with the
  /// SAME id overwrites the record — write-once stays true per key.
  fn compensate_local_ink(&mut self, snapshot: Vec<(flowstate_flow::AnnotationStroke, BoardPoint)>, cx: &mut Context<Self>) {
    if snapshot.is_empty() {
      return;
    }
    let updates: Vec<flowstate_flow::AnnotationStroke> = {
      let Some((layout, sheet)) = self.active_layout() else {
        return;
      };
      if sheet.rows.is_empty() || sheet.columns.is_empty() {
        return;
      }
      snapshot
        .into_iter()
        .filter_map(|(stroke, absolute)| {
          if sheet.id != stroke.sheet_id || sheet.annotations.iter().all(|live| live.id != stroke.id) {
            return None; // deleted (e.g. its sheet-sweep) — nothing to hold
          }
          let (row_ix, column_ix) = sheet.resolve_anchor(&stroke.anchor);
          let (slot_x, slot_y) = layout.slot_origin(row_ix, column_ix);
          let drifted = (slot_x + stroke.anchor.offset.x - absolute.x).abs() > 0.5
            || (slot_y + stroke.anchor.offset.y - absolute.y).abs() > 0.5;
          if !drifted {
            return None;
          }
          let new_row = (0..sheet.rows.len())
            .rev()
            .find(|&ix| layout.row_top(ix) <= absolute.y)
            .unwrap_or(0);
          let new_column = (0..sheet.columns.len())
            .rev()
            .find(|&ix| layout.column_lefts[ix] <= absolute.x)
            .unwrap_or(0);
          let (anchor_x, anchor_y) = layout.slot_origin(new_row, new_column);
          let mut updated = stroke;
          updated.anchor = flowstate_flow::GridAnchor {
            row_id: sheet.rows[new_row].id,
            column_id: sheet.columns[new_column].id,
            offset: flowstate_flow::StrokePoint {
              x: absolute.x - anchor_x,
              y: absolute.y - anchor_y,
            },
          };
          Some(updated)
        })
        .collect()
    };
    for stroke in updates {
      let _ = self.apply_intent(&FlowIntent::AddAnnotation { stroke }, cx);
    }
  }

  /// Post-change bookkeeping shared by every mutation path.
  fn after_local_change(&mut self, cx: &mut Context<Self>) {
    if let Ok(items) = self.handle.drain_board_stream() {
      // Local gestures don't flash — the user just did it themselves.
      self.ingest_stream_events(items, false, cx);
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
    // G2: the in-flight ink draft rides presence (rounded board units,
    // capped) so peers see a stroke WHILE it's drawn.
    let ink_preview: Vec<(i32, i32)> = if self.annotation_tool == AnnotationTool::Marker || self.right_inking {
      self
        .drawing_points
        .iter()
        .rev()
        .take(96)
        .rev()
        .map(|point| (point.x.round() as i32, point.y.round() as i32))
        .collect()
    } else {
      Vec::new()
    };
    FlowPresenceSnapshot {
      sheet: self.active_sheet,
      cell: self.active_cell,
      editing,
      caret,
      slot: self.cursor,
      selection_rect: self.selection_rect,
      ink_preview,
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
  /// G6: rename the selected tape mark inline — the input replaces the tape
  /// label; Enter/blur commits through the flow I/O service.
  fn begin_tape_rename(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(scrubber) = &self.scrubber else { return };
    let Some(mark_id) = scrubber.selected_mark else {
      self.refuse("select a checkpoint mark on the tape first", None, cx);
      return;
    };
    let current = scrubber
      .marks
      .iter()
      .find(|mark| mark.id == mark_id)
      .map(|mark| mark.title.to_string())
      .unwrap_or_default();
    let input = cx.new(|cx| {
      let mut state = gpui_component::input::InputState::new(window, cx).placeholder("Checkpoint name");
      state.set_value(current, window, cx);
      state
    });
    input.focus_handle(cx).focus(window);
    self.tape_rename_subscription = Some(cx.subscribe_in(
      &input,
      window,
      move |editor: &mut Self, input, event: &gpui_component::input::InputEvent, _window, cx| {
        if matches!(
          event,
          gpui_component::input::InputEvent::PressEnter { .. } | gpui_component::input::InputEvent::Blur
        ) {
          let Some((mark_id, _)) = editor.tape_rename.take() else { return };
          editor.tape_rename_subscription = None;
          let title = input.read(cx).value().trim().to_string();
          if title.is_empty() {
            cx.notify();
            return;
          }
          let io = editor.io.clone();
          cx.spawn(async move |editor, cx| {
            let result = io.rename_checkpoint(mark_id, title).await;
            let _ = editor.update(cx, |editor, cx| {
              match result {
                Ok(()) => editor.reload_tape_marks(cx),
                Err(error) => editor.refuse(format!("renaming the checkpoint failed: {error:#}"), None, cx),
              }
              cx.notify();
            });
          })
          .detach();
        }
      },
    ));
    self.tape_rename = Some((mark_id, input));
    cx.notify();
  }

  /// G6: refresh the scrubber's marks after a checkpoint rename lands.
  fn reload_tape_marks(&mut self, cx: &mut Context<Self>) {
    let (marks, mark_frontiers) = self.load_tape_marks();
    if let Some(scrubber) = self.scrubber.as_mut() {
      scrubber.marks = marks;
      scrubber.mark_frontiers = mark_frontiers;
    }
    cx.notify();
  }

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
    let flow_theme = resolve_flow_theme();
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
    // C16: replay renders the REAL grid — header band, gridlines, positioned
    // cells, historical ink — read-only, so the past speaks the same visual
    // language as the sheet being replayed.
    let grid: AnyElement = sheet
      .map(|sheet| {
        let layout = GridLayout::compute_to(sheet, &self.cell_measurements, 0.0);
        let header_h = HEADER_HEIGHT * zoom;
        let content_w = px((layout.total_width() + 40.0) * zoom);
        let content_h = px(layout.total_height() * zoom) + px(header_h + 40.0);
        let mut children: Vec<AnyElement> = Vec::new();
        // Header band.
        for (column_ix, column) in sheet.columns.iter().enumerate() {
          let side = flow_theme.side(column.side);
          children.push(
            div()
              .absolute()
              .left(px(layout.column_lefts[column_ix] * zoom))
              .top(px(0.0))
              .w(px(layout.column_widths[column_ix] * zoom))
              .h(px(header_h))
              .flex()
              .items_center()
              .px(px(6.0 * zoom))
              .bg(side.base.opacity(0.14))
              .font_weight(gpui::FontWeight::BOLD)
              .text_size(px(13.0 * zoom))
              .text_color(side.base)
              .border_b(px(2.0 * zoom))
              .border_color(side.base)
              .overflow_hidden()
              .child(SharedString::from(column.label.clone()))
              .into_any_element(),
          );
        }
        // Gridlines (real rows only — no ghost run in the past).
        let total_w = px(layout.total_width() * zoom);
        for x in layout
          .column_lefts
          .iter()
          .copied()
          .chain(std::iter::once(layout.total_width()))
        {
          children.push(
            div()
              .absolute()
              .left(px(x * zoom))
              .top(px(header_h))
              .w(px(1.0))
              .h(px(layout.row_top(sheet.rows.len()) * zoom))
              .bg(flow_theme.gridline)
              .into_any_element(),
          );
        }
        for row_ix in 0..=sheet.rows.len() {
          children.push(
            div()
              .absolute()
              .left(px(0.0))
              .top(px(header_h) + px(layout.row_top(row_ix) * zoom))
              .w(total_w)
              .h(px(1.0))
              .bg(flow_theme.gridline)
              .into_any_element(),
          );
        }
        // Cells at their real addresses.
        for (row_ix, row) in sheet.rows.iter().enumerate() {
          for (column_ix, slot) in row.cells.iter().enumerate() {
            let Some(cell) = slot else { continue };
            let side = flow_theme.side(sheet.columns[column_ix].side);
            children.push(
              div()
                .absolute()
                .left(px(layout.column_lefts[column_ix] * zoom) + px(grid_layout::CELL_SLOT_INSET))
                .top(px(header_h) + px(layout.row_top(row_ix) * zoom) + px(grid_layout::CELL_SLOT_INSET))
                .w(px(layout.column_widths[column_ix] * zoom) - px(grid_layout::CELL_SLOT_INSET))
                .h(px(layout.row_height(row_ix) * zoom) - px(grid_layout::CELL_SLOT_INSET))
                .overflow_hidden()
                .bg(crate::flow::cell_theme::flow_cell_fill(&flow_theme, side.base, 0.0))
                .p(px(grid_layout::CELL_CONTENT_PADDING * zoom))
                .text_size(px(12.0 * zoom))
                .text_color(flow_theme.text.opacity(if cell.summary.struck { 0.45 } else { 0.9 }))
                .when(cell.summary.struck, |this| this.line_through())
                .child(SharedString::from(cell.summary.summary_text.to_string()))
                .into_any_element(),
            );
          }
        }
        // Historical ink, through the same rigid-body anchors.
        let strokes: Vec<(gpui::Point<gpui::Pixels>, flowstate_flow::AnnotationStroke)> = sheet
          .annotations
          .iter()
          .map(|stroke| {
            let (row_ix, column_ix) = sheet.resolve_anchor(&stroke.anchor);
            let (slot_x, slot_y) = layout.slot_origin(row_ix, column_ix);
            let origin = point(
              px((slot_x + stroke.anchor.offset.x) * zoom),
              px(header_h) + px((slot_y + stroke.anchor.offset.y) * zoom),
            );
            (origin, stroke.clone())
          })
          .collect();
        div()
          .id("flow-history-grid")
          .relative()
          .w(content_w)
          .h(content_h)
          .bg(flow_theme.surface)
          .children(children)
          .child(
            canvas(
              |_, _, _| {},
              move |bounds, _, window, _| {
                for (origin, stroke) in &strokes {
                  let color = gpui::Hsla::from(rgba(stroke.style.color_rgba));
                  paint_stroke(
                    bounds.origin + *origin,
                    &stroke.points,
                    px(stroke.style.width * zoom),
                    color.opacity(color.a * stroke.style.opacity),
                    zoom,
                    window,
                  );
                }
              },
            )
            .absolute()
            .inset_0(),
          )
          .into_any_element()
      })
      .unwrap_or_else(|| div().into_any_element());
    div()
      .size_full()
      .flex()
      .flex_col()
      .child(
        div()
          .id("flow-history-scroll")
          .flex_1()
          .overflow_scroll()
          .p(px(16.0 * zoom))
          .child(grid),
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
          .child(match self.tape_rename.as_ref() {
            // G6: the inline rename input takes the label's slot while open.
            Some((_, input)) => div()
              .w(px(180.0))
              .child(gpui_component::input::Input::new(input).xsmall().w_full())
              .into_any_element(),
            None => div()
              .text_size(px(11.0))
              .text_color(cx.theme().muted_foreground)
              .whitespace_nowrap()
              .child(label)
              .into_any_element(),
          })
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
          .child({
            use gpui_component::Disableable as _;
            // G6: pins stop being named-forever-at-creation.
            gpui_component::button::Button::new("flow-history-rename")
              .xsmall()
              .label("Rename pin")
              .tooltip("Rename the selected checkpoint mark")
              .disabled(selected_mark.is_none())
              .on_click(cx.listener(|editor, _, window, cx| editor.begin_tape_rename(window, cx)))
          })
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

  // ---- B4: context menus (cell / slot / header / gutter) -------------------

  /// One menu item that routes back into the editor and closes the menu.
  fn menu_item(
    &self,
    cx: &Context<Self>,
    label: impl Into<gpui::SharedString>,
    action: impl Fn(&mut Self, &mut Window, &mut Context<Self>) + 'static,
  ) -> gpui_component::menu::PopupMenuItem {
    let weak = cx.entity().downgrade();
    gpui_component::menu::PopupMenuItem::new(label).on_click(move |_, window, cx| {
      let _ = weak.update(cx, |editor, cx| {
        editor.context_menu = None;
        action(editor, window, cx);
        cx.notify();
      });
    })
  }

  /// B4: right-click on a slot — the cell menu (occupied) or slot menu (empty).
  fn show_slot_context_menu(&mut self, position: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
    let Some((row_ix, column_ix)) = self.slot_at_position(position) else {
      return;
    };
    // The menu acts on the slot under the pointer — park the cursor there.
    self.set_cursor(row_ix, column_ix, cx);
    let occupied = self
      .active_sheet_ref()
      .and_then(|sheet| sheet.slot(row_ix, column_ix))
      .is_some();
    let struck = self
      .active_sheet_ref()
      .and_then(|sheet| sheet.slot(row_ix, column_ix))
      .is_some_and(|cell| cell.summary.struck);
    let real_row = self.active_sheet_ref().is_some_and(|sheet| row_ix < sheet.rows.len());
    let occupant_id = self
      .active_sheet_ref()
      .and_then(|sheet| sheet.slot(row_ix, column_ix))
      .map(|cell| cell.id);
    let items: Vec<gpui_component::menu::PopupMenuItem> = if occupied {
      let mut items = vec![
        self.menu_item(cx, if struck { "Unstrike" } else { "Strike" }, |editor, _, cx| {
          editor.strike_selected(cx)
        }),
        self.menu_item(cx, "Cut", |editor, _, cx| editor.cut_selection(cx)),
        self.menu_item(cx, "Copy", |editor, _, cx| editor.copy_selection(cx)),
        self.menu_item(cx, "Paste", |editor, _, cx| editor.paste(cx)),
        self.menu_item(cx, "Delete card", |editor, window, cx| {
          editor.delete_selected(window, cx)
        }),
      ];
      // Q-21/F2: jump back to the evidence this card was flowed from.
      if let Some(source_path) = self
        .active_sheet_ref()
        .and_then(|sheet| sheet.slot(row_ix, column_ix))
        .and_then(|cell| cell.source.as_ref())
        .map(|source| source.path.clone())
      {
        items.push(self.menu_item(cx, "Open source card", move |editor, _, cx| {
          cx.emit(FlowEditorEvent::OpenCellSource {
            path: source_path.clone(),
          });
          let _ = editor;
        }));
      }
      // F5: lossless cross-sheet move — one item per other sheet.
      if let Some(cell_id) = occupant_id {
        for (target, name) in self
          .board
          .sheets
          .iter()
          .filter(|sheet| Some(sheet.id) != self.active_sheet)
          .map(|sheet| {
            let name = if sheet.name.trim().is_empty() { "Untitled".to_string() } else { sheet.name.clone() };
            (sheet.id, name)
          })
          .collect::<Vec<_>>()
        {
          items.push(self.menu_item(cx, format!("Send to “{name}”"), move |editor, _, cx| {
            editor.send_cell_to_sheet(cell_id, target, cx)
          }));
        }
      }
      items
    } else {
      vec![
        self.menu_item(cx, "New card here", |editor, window, cx| {
          if let Some((row_ix, column_ix)) = editor.cursor
            && editor
              .add_cell_at_slot(row_ix, column_ix, flowstate_flow::CellSeed::Empty, cx)
              .is_some()
          {
            editor.focus_active_cell(window, cx);
          }
        }),
        self.menu_item(cx, "Paste", |editor, _, cx| editor.paste(cx)),
      ]
    };
    let mut trailing: Vec<gpui_component::menu::PopupMenuItem> = vec![
      self.menu_item(cx, "Insert row above", |editor, window, cx| {
        editor.add_sibling(RelativePosition::Before, cx);
        editor.focus_active_cell(window, cx);
      }),
      self.menu_item(cx, "Insert row below", |editor, window, cx| {
        editor.add_sibling(RelativePosition::After, cx);
        editor.focus_active_cell(window, cx);
      }),
    ];
    if real_row {
      trailing.push(self.menu_item(cx, "Delete row", |editor, _, cx| editor.delete_cursor_row(cx)));
    }
    let menu = gpui_component::menu::PopupMenu::build(window, cx, move |mut menu, _, _| {
      for item in items {
        menu = menu.item(item);
      }
      menu = menu.item(gpui_component::menu::PopupMenuItem::separator());
      for item in trailing {
        menu = menu.item(item);
      }
      menu
    });
    self.context_menu = Some((position, menu));
    cx.notify();
  }

  /// B4: right-click on a column header.
  pub(super) fn show_header_context_menu(
    &mut self,
    column_ix: usize,
    position: gpui::Point<gpui::Pixels>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    let Some((column_id, side)) = self
      .active_sheet_ref()
      .and_then(|sheet| sheet.columns.get(column_ix))
      .map(|column| (column.id, column.side))
    else {
      return;
    };
    let sheet_id = self.active_sheet;
    let other_side = match side {
      flowstate_flow::ArgumentSide::One => flowstate_flow::ArgumentSide::Two,
      flowstate_flow::ArgumentSide::Two => flowstate_flow::ArgumentSide::One,
    };
    let items: Vec<gpui_component::menu::PopupMenuItem> = vec![
      self.menu_item(cx, "Rename column…", move |editor, window, cx| {
        editor.begin_column_rename(column_id, position, window, cx)
      }),
      self.menu_item(cx, "Column width…", move |editor, window, cx| {
        editor.begin_column_width_entry(column_id, position, window, cx)
      }),
      self.menu_item(cx, "Autofit width", move |editor, window, cx| {
        editor.autofit_column(column_id, column_ix, window, cx)
      }),
      // E3: the alternation guess finally has an eraser.
      self.menu_item(cx, "Switch side (aff/neg)", move |editor, _, cx| {
        if let Some(sheet_id) = sheet_id
          && editor
            .apply_intent(
              &FlowIntent::SetColumnSide {
                sheet_id,
                column_id,
                side: other_side,
              },
              cx,
            )
            .is_ok()
        {
          editor.changed(editor.active_cell, cx);
        }
      }),
      self.menu_item(cx, "Insert column before", move |editor, cx_window, cx| {
        let _ = cx_window;
        editor.set_cursor(editor.cursor.map_or(0, |(row, _)| row), column_ix, cx);
        editor.insert_column_at_cursor(cx);
      }),
      self.menu_item(cx, "Delete column", move |editor, _, cx| {
        editor.set_cursor(editor.cursor.map_or(0, |(row, _)| row), column_ix, cx);
        editor.delete_cursor_column(cx);
      }),
      // F4/Q-22: the rebuttal is spoken off the flow.
      self.menu_item(cx, "Send column to new document", move |editor, _, cx| {
        editor.send_column_to_document(column_ix, cx)
      }),
    ];
    let menu = gpui_component::menu::PopupMenu::build(window, cx, move |mut menu, _, _| {
      for item in items {
        menu = menu.item(item);
      }
      menu
    });
    self.context_menu = Some((position, menu));
    cx.notify();
  }

  /// B4: right-click on a gutter row number.
  pub(super) fn show_gutter_context_menu(
    &mut self,
    row_ix: usize,
    position: gpui::Point<gpui::Pixels>,
    window: &mut Window,
    cx: &mut Context<Self>,
  ) {
    let real = self.active_sheet_ref().is_some_and(|sheet| row_ix < sheet.rows.len());
    let has_override = self
      .active_sheet_ref()
      .and_then(|sheet| sheet.rows.get(row_ix))
      .is_some_and(|row| row.height_override.is_some());
    let row_id = self
      .active_sheet_ref()
      .and_then(|sheet| sheet.rows.get(row_ix))
      .map(|row| row.id);
    let mut items: Vec<gpui_component::menu::PopupMenuItem> = vec![
      self.menu_item(cx, "Select row", move |editor, _, cx| editor.select_row(row_ix, cx)),
      self.menu_item(cx, "Insert row above", move |editor, window, cx| {
        editor.set_cursor(row_ix, editor.cursor.map_or(0, |(_, col)| col), cx);
        editor.add_sibling(RelativePosition::Before, cx);
        editor.focus_active_cell(window, cx);
      }),
      self.menu_item(cx, "Insert row below", move |editor, window, cx| {
        editor.set_cursor(row_ix, editor.cursor.map_or(0, |(_, col)| col), cx);
        editor.add_sibling(RelativePosition::After, cx);
        editor.focus_active_cell(window, cx);
      }),
    ];
    if real {
      items.push(self.menu_item(cx, "Delete row", move |editor, _, cx| {
        editor.set_cursor(row_ix, editor.cursor.map_or(0, |(_, col)| col), cx);
        editor.delete_cursor_row(cx);
      }));
    }
    if has_override && let Some(row_id) = row_id {
      items.push(self.menu_item(cx, "Autofit height", move |editor, _, cx| {
        editor.clear_row_height_override(row_id, cx)
      }));
    }
    let menu = gpui_component::menu::PopupMenu::build(window, cx, move |mut menu, _, _| {
      for item in items {
        menu = menu.item(item);
      }
      menu
    });
    self.context_menu = Some((position, menu));
    cx.notify();
  }

  /// F1/Q-21: a toolkit card dropped on a slot — the cell is seeded with the
  /// card's FULL paragraphs (summary-mode rendering shows tag/cite) and the
  /// provenance blob links back to the evidence.
  pub(super) fn drop_toolkit_card(&mut self, drag: &crate::rich_text_element::ToolkitTextDrag, cx: &mut Context<Self>) {
    let Some((row_ix, column_ix)) = self.drag_target.take() else {
      return;
    };
    let Some(sheet_id) = self.active_sheet else { return };
    if self
      .active_sheet_ref()
      .and_then(|sheet| sheet.slot(row_ix, column_ix))
      .is_some()
    {
      self.refuse("that slot is occupied — drop the card on an empty slot", None, cx);
      return;
    }
    if drag.paragraphs.is_empty() && drag.text.trim().is_empty() {
      self.refuse("that card has no content to flow", None, cx);
      return;
    }
    self.begin_bulk();
    let rows = self.active_sheet_ref().map_or(0, |sheet| sheet.rows.len());
    if row_ix >= rows {
      self.materialize_rows(sheet_id, row_ix + 1 - rows, cx);
    }
    let ids = {
      let sheet = self.active_sheet_ref();
      let column_id = sheet.and_then(|sheet| sheet.columns.get(column_ix)).map(|column| column.id);
      let row_id = sheet.and_then(|sheet| sheet.rows.get(row_ix)).map(|row| row.id);
      column_id.zip(row_id)
    };
    if let Some((column_id, row_id)) = ids {
      let seed = if drag.paragraphs.is_empty() {
        flowstate_flow::CellSeed::Paragraphs(vec![flowstate_document::InputParagraph {
          style: flowstate_document::PARAGRAPH_TAG,
          runs: vec![flowstate_document::InputRun {
            text: drag.text.clone(),
            styles: flowstate_document::RunStyles::default(),
          }],
        }])
      } else {
        flowstate_flow::CellSeed::Paragraphs(drag.paragraphs.clone())
      };
      let cell_id: CellId = uuid::Uuid::new_v4();
      let added = self
        .apply_intent(
          &FlowIntent::AddCell {
            sheet_id,
            cell_id,
            row_id,
            column_id,
            seed,
          },
          cx,
        )
        .is_ok();
      if added {
        if let Some(path) = drag.source_path.clone() {
          let _ = self.apply_intent(
            &FlowIntent::SetCellSource {
              sheet_id,
              cell_id,
              source: Some(flowstate_flow::CellSource {
                path,
                unit: drag.source_unit.clone(),
                cursor: None,
              }),
            },
            cx,
          );
        }
        self.set_cursor(row_ix, column_ix, cx);
      }
    }
    self.end_bulk("drop card", cx);
    self.changed(None, cx);
  }

  /// F5: send a cell to another sheet, landing at the SAME (row, column)
  /// indexes — spatial honesty across sheets, identity preserved (comments
  /// and history ride along). Missing target rows materialize; a missing
  /// column or an occupied slot refuses with words.
  fn send_cell_to_sheet(&mut self, cell_id: CellId, target_sheet: SheetId, cx: &mut Context<Self>) {
    let Some(from_sheet) = self.active_sheet else { return };
    let Some((row_ix, column_ix)) = self.active_sheet_ref().and_then(|sheet| sheet.cell_position(cell_id)) else {
      return;
    };
    let Some(target) = self.board.sheets.iter().find(|sheet| sheet.id == target_sheet) else {
      return;
    };
    let Some(column_id) = target.columns.get(column_ix).map(|column| column.id) else {
      self.refuse("the target sheet has fewer speech columns", Some(cell_id), cx);
      return;
    };
    if target.slot(row_ix, column_ix).is_some() {
      self.refuse("that slot is occupied on the target sheet", Some(cell_id), cx);
      return;
    }
    let target_rows = target.rows.len();
    self.begin_bulk();
    if row_ix >= target_rows {
      self.materialize_rows(target_sheet, row_ix + 1 - target_rows, cx);
    }
    let row_id = self
      .board
      .sheets
      .iter()
      .find(|sheet| sheet.id == target_sheet)
      .and_then(|sheet| sheet.rows.get(row_ix))
      .map(|row| row.id);
    if let Some(row_id) = row_id {
      let _ = self.apply_intent(
        &FlowIntent::MoveCellToSheet {
          from_sheet,
          cell_id,
          to_sheet: target_sheet,
          row_id,
          column_id,
        },
        cx,
      );
    }
    self.end_bulk("send to sheet", cx);
    self.changed(None, cx);
  }

  /// D5: inline column rename, anchored where the menu was.
  fn begin_column_rename(&mut self, column_id: ColumnId, position: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
    let current = self
      .active_sheet_ref()
      .and_then(|sheet| sheet.columns.iter().find(|column| column.id == column_id))
      .map(|column| column.label.clone())
      .unwrap_or_default();
    let input = cx.new(|cx| {
      let mut state = gpui_component::input::InputState::new(window, cx).placeholder("Column name");
      state.set_value(current, window, cx);
      state
    });
    input.focus_handle(cx).focus(window);
    self.column_rename_subscription = Some(cx.subscribe_in(
      &input,
      window,
      move |editor: &mut Self, input, event: &gpui_component::input::InputEvent, _window, cx| {
        if matches!(
          event,
          gpui_component::input::InputEvent::PressEnter { .. } | gpui_component::input::InputEvent::Blur
        ) {
          let Some((column_id, _, _)) = editor.column_rename.take() else { return };
          editor.column_rename_subscription = None;
          let label = input.read(cx).value().trim().to_string();
          if label.is_empty() {
            cx.notify();
            return;
          }
          if let Some(sheet_id) = editor.active_sheet
            && editor
              .apply_intent(&FlowIntent::RenameColumn { sheet_id, column_id, label }, cx)
              .is_ok()
          {
            editor.changed(editor.active_cell, cx);
          }
          cx.notify();
        }
      },
    ));
    self.column_rename = Some((column_id, position, input));
    cx.notify();
  }

  /// D8/Q-17: numeric width entry, anchored where the menu was.
  fn begin_column_width_entry(&mut self, column_id: ColumnId, position: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
    let current = self
      .active_sheet_ref()
      .and_then(|sheet| sheet.columns.iter().position(|column| column.id == column_id))
      .and_then(|ix| self.active_layout().map(|(layout, _)| layout.column_widths[ix]))
      .unwrap_or(grid_layout::DEFAULT_COLUMN_WIDTH);
    let input = cx.new(|cx| {
      let mut state = gpui_component::input::InputState::new(window, cx).placeholder("Width (px)");
      state.set_value(format!("{current:.0}"), window, cx);
      state
    });
    input.focus_handle(cx).focus(window);
    self.column_width_subscription = Some(cx.subscribe_in(
      &input,
      window,
      move |editor: &mut Self, input, event: &gpui_component::input::InputEvent, _window, cx| {
        if matches!(
          event,
          gpui_component::input::InputEvent::PressEnter { .. } | gpui_component::input::InputEvent::Blur
        ) {
          let Some((column_id, _, _)) = editor.column_width_entry.take() else { return };
          editor.column_width_subscription = None;
          let raw = input.read(cx).value().trim().to_string();
          let Ok(width) = raw.parse::<f32>() else {
            if !raw.is_empty() {
              editor.refuse(format!("\"{raw}\" is not a width in pixels"), None, cx);
            }
            cx.notify();
            return;
          };
          let width = width.max(MIN_COLUMN_WIDTH);
          if let Some(sheet_id) = editor.active_sheet
            && editor
              .apply_intent(
                &FlowIntent::SetColumnWidth {
                  sheet_id,
                  column_id,
                  width: Some(width),
                },
                cx,
              )
              .is_ok()
          {
            editor.changed(editor.active_cell, cx);
          }
          cx.notify();
        }
      },
    ));
    self.column_width_entry = Some((column_id, position, input));
    cx.notify();
  }

  /// E10: toggle the round-metadata form. Each field commits its own LWW
  /// write on Enter/blur, so teammates can fill different fields at once.
  pub fn toggle_round_form(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    if self.round_form.take().is_some() {
      self.round_form_subscriptions.clear();
      cx.notify();
      return;
    }
    let round = self.board.round.clone();
    let mut fields = Vec::new();
    let mut subscriptions = Vec::new();
    for field in flowstate_flow::RoundField::ALL {
      let current = match field {
        flowstate_flow::RoundField::Tournament => round.tournament.clone(),
        flowstate_flow::RoundField::Round => round.round.clone(),
        flowstate_flow::RoundField::Opponent => round.opponent.clone(),
        flowstate_flow::RoundField::Judge => round.judge.clone(),
        flowstate_flow::RoundField::Side => round.side.clone(),
        flowstate_flow::RoundField::Result => round.result.clone(),
      };
      let input = cx.new(|cx| {
        let mut state = gpui_component::input::InputState::new(window, cx).placeholder(field.label());
        state.set_value(current, window, cx);
        state
      });
      subscriptions.push(cx.subscribe_in(
        &input,
        window,
        move |editor: &mut Self, input, event: &gpui_component::input::InputEvent, _window, cx| {
          if matches!(
            event,
            gpui_component::input::InputEvent::PressEnter { .. } | gpui_component::input::InputEvent::Blur
          ) {
            let value = input.read(cx).value().to_string();
            let _ = editor.apply_intent(&FlowIntent::SetRoundField { field, value }, cx);
          }
        },
      ));
      fields.push((field, input));
    }
    self.round_form = Some(fields);
    self.round_form_subscriptions = subscriptions;
    cx.notify();
  }

  pub fn round_form_open(&self) -> bool {
    self.round_form.is_some()
  }

  /// F4/Q-22: a column becomes a speech skeleton — one paragraph per card, in
  /// row order. The workspace handles the event by opening a new document.
  fn send_column_to_document(&mut self, column_ix: usize, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else { return };
    let text: String = sheet
      .rows
      .iter()
      .filter_map(|row| row.cells.get(column_ix).and_then(|slot| slot.as_ref()))
      .map(|cell| cell.summary.summary_text.trim().to_string())
      .filter(|line| !line.is_empty())
      .collect::<Vec<_>>()
      .join("\n");
    if text.is_empty() {
      self.refuse("this column has no cards to send", None, cx);
      return;
    }
    cx.emit(FlowEditorEvent::SendColumnToDocument { text });
  }

  /// B2: Esc aborts whatever gesture is in flight — drags lose their drop,
  /// resize previews revert, marquee/pan state clears. Returns whether
  /// anything was actually cancelled (so Esc falls through otherwise).
  fn cancel_active_gesture(&mut self, cx: &mut Context<Self>) -> bool {
    let mut cancelled = false;
    let dropped_drag = self.dragging_cell.take().is_some()
      | self.drag_target.take().is_some()
      | self.dragging_row.take().is_some()
      | self.row_drop_gap.take().is_some()
      | self.dragging_column.take().is_some()
      | self.column_drop_gap.take().is_some()
      | self.column_resize.take().is_some()
      | self.row_resize.take().is_some()
      | self.fill_target.take().is_some();
    if dropped_drag {
      self.suppress_drop = true;
      cancelled = true;
    }
    if self.marquee_anchor.take().is_some() {
      cancelled = true;
    }
    if self.pan_drag.take().is_some() {
      cancelled = true;
    }
    if cancelled {
      cx.notify();
    }
    cancelled
  }

  /// One-shot check the drop handlers run first: was this drop cancelled?
  fn take_suppressed_drop(&mut self) -> bool {
    std::mem::take(&mut self.suppress_drop)
  }

  /// B1: while a drag hugs a viewport edge, set the per-frame pan vector —
  /// render applies it until the gesture ends, so you can drop beyond the
  /// visible screen.
  pub(super) fn update_autoscroll(&mut self, position: gpui::Point<gpui::Pixels>) {
    const EDGE: f32 = 28.0;
    const MAX_STEP: f32 = 14.0;
    let bounds = self.board_scroll.bounds();
    if bounds.size.width <= px(1.0) || bounds.size.height <= px(1.0) {
      self.autoscroll = None;
      return;
    }
    let left = (position.x - bounds.origin.x).as_f32();
    let right = (bounds.origin.x + bounds.size.width - position.x).as_f32();
    let top = (position.y - bounds.origin.y).as_f32();
    let bottom = (bounds.origin.y + bounds.size.height - position.y).as_f32();
    let mut vx = 0.0;
    let mut vy = 0.0;
    if left < EDGE {
      vx = MAX_STEP * (1.0 - (left / EDGE).clamp(0.0, 1.0));
    } else if right < EDGE {
      vx = -MAX_STEP * (1.0 - (right / EDGE).clamp(0.0, 1.0));
    }
    if top < EDGE {
      vy = MAX_STEP * (1.0 - (top / EDGE).clamp(0.0, 1.0));
    } else if bottom < EDGE {
      vy = -MAX_STEP * (1.0 - (bottom / EDGE).clamp(0.0, 1.0));
    }
    self.autoscroll = (vx != 0.0 || vy != 0.0).then_some((vx, vy));
  }

  /// Consume a drained board stream: adopt the last `Board`, flash `Delta`
  /// footprints (when `flash` — the remote/undo paths), and voice `Defects`
  /// (A4: a silently rearranged grid is a defect of its own).
  fn ingest_stream_events(&mut self, items: Vec<FlowStreamItem>, flash: bool, cx: &mut Context<Self>) {
    let mut board = None;
    for item in items {
      match item {
        FlowStreamItem::Board(next) => board = Some(next),
        FlowStreamItem::Delta(delta) => {
          if flash {
            let now = std::time::Instant::now();
            for cell in delta.moved_cells.iter().chain(delta.inserted_cells.iter()) {
              self.cell_flash.insert(*cell, now);
            }
          }
        },
        FlowStreamItem::Defects(defects) => self.surface_defects(&defects, cx),
      }
    }
    if let Some(board) = board {
      self.board = *board;
    }
  }

  /// A4: normalizer/repair defects reach the user — a bump-down toast plus a
  /// flash on every bumped cell, so a merge never rearranges the grid without
  /// words.
  fn surface_defects(&mut self, defects: &[flowstate_flow::FlowDefect], cx: &mut Context<Self>) {
    use flowstate_flow::FlowDefect;
    if defects.is_empty() {
      return;
    }
    let now = std::time::Instant::now();
    let mut bumped = 0usize;
    for defect in defects {
      if let FlowDefect::SlotCollisionBumped { cell, .. } = defect {
        bumped += 1;
        self.cell_flash.insert(*cell, now);
      }
    }
    let message = if bumped > 0 {
      let plural = if bumped == 1 { "card" } else { "cards" };
      format!("merging changes put {bumped} {plural} on an occupied slot — moved to a fresh row below (flashed)")
    } else {
      let count = defects.len();
      let plural = if count == 1 { "inconsistency" } else { "inconsistencies" };
      format!("the grid repaired {count} {plural} while merging changes")
    };
    self.refuse(message, None, cx);
  }

  /// The fading wash alpha for a flashed cell, `None` once expired.
  fn cell_flash_alpha(&self, id: CellId) -> Option<f32> {
    const FLASH_SECS: f32 = 1.1;
    let age = self.cell_flash.get(&id)?.elapsed().as_secs_f32();
    (age < FLASH_SECS).then(|| 0.30 * (1.0 - age / FLASH_SECS))
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
    if let Ok(items) = self.handle.drain_board_stream() {
      // Remote/undo footprints flash their landing cells (Q-23) and voice any
      // repair defects (A4) instead of vanishing into a silent rearrange.
      self.ingest_stream_events(items, true, cx);
    }
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

  /// E12: ink visibility is one app-global view state, not per-sheet.
  pub fn annotations_visible(&self) -> bool {
    self.ink_visible
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

  /// B6: Ctrl+PageUp/Down — step to the previous/next sheet tab, wrapping.
  pub fn cycle_sheet(&mut self, forward: bool, cx: &mut Context<Self>) {
    let count = self.board.sheets.len();
    if count == 0 {
      self.refuse("no sheets to switch between", None, cx);
      return;
    }
    let current = self
      .active_sheet
      .and_then(|id| self.board.sheets.iter().position(|sheet| sheet.id == id))
      .unwrap_or(0);
    let next = if forward { (current + 1) % count } else { (current + count - 1) % count };
    let id = self.board.sheets[next].id;
    self.activate_sheet(id, cx);
  }

  pub fn activate_cell(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    // B8: any non-typeover activation (click, F2, programmatic) leaves
    // Enter-mode; the typeover path re-arms it right after activating.
    self.typeover_mode = false;
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
  /// Delete the active sheet — cells and ink go with it. No confirmation:
  /// undo is the guard (P3).
  pub fn confirm_delete_active_sheet(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
    if self.active_sheet.is_none() {
      return;
    }
    self.delete_active_sheet(cx);
  }

  /// Session restore — reopen on the sheet you were flowing. (E12: ink
  /// visibility became an app-global setting; the per-sheet list in old
  /// session files is ignored.)
  pub fn restore_ui_state(&mut self, active_sheet: Option<uuid::Uuid>, _hidden_ink_sheets: &[uuid::Uuid], cx: &mut Context<Self>) {
    if let Some(sheet_id) = active_sheet
      && self.board.sheets.iter().any(|sheet| sheet.id == sheet_id)
    {
      self.active_sheet = Some(sheet_id);
      cx.emit(FlowEditorEvent::ActiveSheetChanged(Some(sheet_id)));
    }
    cx.notify();
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
  /// A9: whether any cell verb (strike/delete/cut/copy) has something to act
  /// on — the ribbon gates on this, not on `active_cell`, because header and
  /// gutter selections fill the set while `active_cell` stays `None`.
  pub fn has_operation_targets(&self) -> bool {
    self
      .active_sheet
      .is_some_and(|sheet_id| !self.operation_set(sheet_id).is_empty())
  }

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

  /// C3/B7: an irregular or cleared selection has no rectangle and no bands.
  pub(super) fn clear_selection_shape(&mut self) {
    self.selection_rect = None;
    self.selected_row_band = None;
    self.selected_column_band = None;
  }

  pub fn clear_selection(&mut self, cx: &mut Context<Self>) {
    self.clear_selection_shape();
    if !self.selected_cells.is_empty() {
      self.selected_cells.clear();
      cx.notify();
    }
  }

  /// W2 shift-click: toggle a cell in the multi-selection.
  pub fn toggle_select_cell(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    self.clear_selection_shape();
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
    // C3: the range IS a rectangle — render one fill, not scattered rings.
    self.selection_rect = Some((r0, c0, r1, c1));
    self.selected_row_band = None;
    self.selected_column_band = None;
    // The extend end becomes the live cursor; the anchor stays put so a
    // further shift-click re-measures from the same corner.
    self.cursor = Some((row_ix, column_ix));
    // G2: peers see the marquee grow.
    cx.emit(FlowEditorEvent::PresenceShifted);
    cx.notify();
  }

  /// Gutter click: select a whole row's cells.
  pub fn select_row(&mut self, row_ix: usize, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet_ref() else { return };
    let columns = sheet.columns.len();
    let Some(row) = sheet.rows.get(row_ix) else {
      self.selected_cells.clear();
      self.clear_selection_shape();
      self.cursor = Some((row_ix, 0));
      cx.notify();
      return;
    };
    self.selected_cells = row
      .cells
      .iter()
      .filter_map(|slot| slot.as_ref().map(|cell| cell.id))
      .collect();
    self.selection_rect = (columns > 0).then_some((row_ix, 0, row_ix, columns - 1));
    self.selected_row_band = Some((row_ix, row_ix));
    self.selected_column_band = None;
    self.cursor = Some((row_ix, 0));
    self.selection_anchor = Some((row_ix, 0));
    cx.notify();
  }

  /// Open a bulk gesture: one undo group (W2 law) + rejection collection (A6).
  /// Always paired with `end_bulk`.
  pub(super) fn begin_bulk(&mut self) {
    let _ = self.handle.undo_group_start();
    self.bulk_failures = Some(0);
  }

  /// Close a bulk gesture; if any intent inside was rejected, refuse ONCE with
  /// words instead of having failures vanish per-op.
  pub(super) fn end_bulk(&mut self, label: &str, cx: &mut Context<Self>) {
    let failures = self.bulk_failures.take().unwrap_or(0);
    let _ = self.handle.undo_group_end();
    if failures > 0 {
      let plural = if failures == 1 { "" } else { "s" };
      self.refuse(format!("{label}: {failures} step{plural} refused by the document"), None, cx);
    }
  }

  pub fn delete_selected(&mut self, _window: &mut Window, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else {
      return;
    };
    // B7: a gutter band deletes the ROWS themselves — no empty husks left.
    if let Some((r0, r1)) = self.selected_row_band {
      let row_ids: Vec<RowId> = self
        .active_sheet_ref()
        .map(|sheet| {
          let last = sheet.rows.len().saturating_sub(1);
          if sheet.rows.is_empty() {
            Vec::new()
          } else {
            sheet.rows[r0.min(last)..=r1.min(last)].iter().map(|row| row.id).collect()
          }
        })
        .unwrap_or_default();
      if !row_ids.is_empty() {
        if self
          .apply_intent(&FlowIntent::DeleteRows { sheet_id, row_ids }, cx)
          .is_ok()
        {
          self.selected_cells.clear();
          self.clear_selection_shape();
          self.changed(None, cx);
        }
        return;
      }
    }
    // B7: a header band deletes the COLUMNS (a sheet keeps at least one).
    if let Some((c0, c1)) = self.selected_column_band {
      let (column_ids, total) = self
        .active_sheet_ref()
        .map(|sheet| {
          let last = sheet.columns.len().saturating_sub(1);
          let ids: Vec<ColumnId> = if sheet.columns.is_empty() {
            Vec::new()
          } else {
            sheet.columns[c0.min(last)..=c1.min(last)].iter().map(|column| column.id).collect()
          };
          (ids, sheet.columns.len())
        })
        .unwrap_or_default();
      if !column_ids.is_empty() {
        if column_ids.len() >= total {
          self.refuse("a sheet needs at least one column", None, cx);
          return;
        }
        let count = column_ids.len();
        if count > 1 {
          self.begin_bulk();
        }
        for column_id in column_ids {
          let _ = self.apply_intent(&FlowIntent::DeleteColumn { sheet_id, column_id }, cx);
        }
        if count > 1 {
          self.end_bulk("delete columns", cx);
        }
        self.selected_cells.clear();
        self.clear_selection_shape();
        self.changed(None, cx);
        return;
      }
    }
    let set = self.operation_set(sheet_id);
    if set.is_empty() {
      return;
    }
    let count = set.len();
    if count > 1 {
      self.begin_bulk();
    }
    for cell_id in set {
      let _ = self.apply_intent(&FlowIntent::DeleteCell { sheet_id, cell_id }, cx);
    }
    if count > 1 {
      self.end_bulk("delete", cx);
    }
    self.selected_cells.clear();
    self.clear_selection_shape();
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
    let count = set.len();
    if count > 1 {
      self.begin_bulk();
    }
    for cell_id in set {
      let _ = self.apply_intent(&FlowIntent::SetCellStruck { sheet_id, cell_id, struck }, cx);
    }
    if count > 1 {
      self.end_bulk("strike", cx);
    }
    self.changed(active, cx);
  }

  // ---- row / column verbs (ribbon + gutter + header) ----------------------

  /// Delete the cursor's row (cells go with it — one intent, undoable).
  pub fn delete_cursor_row(&mut self, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else { return };
    let Some((row_ix, _)) = self.cursor else {
      // A5: a dead keypress must speak.
      self.refuse("place the cursor on a row to delete it", None, cx);
      return;
    };
    let Some(row_id) = self
      .active_sheet_ref()
      .and_then(|sheet| sheet.rows.get(row_ix))
      .map(|row| row.id)
    else {
      // A5: the cursor sits in the ghost run — there is no real row here yet.
      self.refuse("no row here to delete", None, cx);
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

  /// Insert a blank column immediately BEFORE the cursor's column (Excel's
  /// "insert column"), inheriting that column's side. With no cursor it falls
  /// back to appending at the end.
  pub fn insert_column_at_cursor(&mut self, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else { return };
    let Some((_, column_ix)) = self.cursor else {
      self.add_column_end(cx);
      return;
    };
    let anchor = self.active_sheet_ref().and_then(|sheet| sheet.columns.get(column_ix)).map(|column| (column.id, column.side));
    let Some((before, side)) = anchor else {
      self.add_column_end(cx);
      return;
    };
    let count = self.active_sheet_ref().map_or(0, |sheet| sheet.columns.len());
    if self
      .apply_intent(
        &FlowIntent::AddColumn {
          sheet_id,
          column_id: uuid::Uuid::new_v4(),
          label: format!("Col {}", count + 1),
          side,
          before: Some(before),
        },
        cx,
      )
      .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }

  /// Delete the cursor's column (its cells die with it). No confirmation —
  /// undo is the guard (P3).
  pub fn delete_cursor_column(&mut self, cx: &mut Context<Self>) {
    let Some((column_id, sheet_id)) = self.cursor.and_then(|(_, column_ix)| {
      let sheet = self.active_sheet_ref()?;
      let column = sheet.columns.get(column_ix)?;
      Some((column.id, sheet.id))
    }) else {
      self.refuse("no column under the cursor to delete", None, cx);
      return;
    };
    if self.active_sheet_ref().is_some_and(|sheet| sheet.columns.len() <= 1) {
      self.refuse("a sheet needs at least one column", None, cx);
      return;
    }
    if self
      .apply_intent(&FlowIntent::DeleteColumn { sheet_id, column_id }, cx)
      .is_ok()
    {
      self.changed(None, cx);
    }
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
    self.save_to_path(path, flowstate_document::RevisionKind::Session, cx)
  }

  /// G5: the autosave twin — same write, an `Auto`-tier checkpoint.
  pub fn save_auto(&mut self, cx: &mut Context<Self>) -> Task<std::io::Result<()>> {
    let Some(path) = self.path.clone() else {
      return cx
        .background_executor()
        .spawn(async { Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, "flow has no save path")) });
    };
    self.save_to_path(path, flowstate_document::RevisionKind::Auto, cx)
  }

  pub fn save_as(&mut self, path: PathBuf, cx: &mut Context<Self>) -> Task<std::io::Result<()>> {
    self.path = Some(path.clone());
    self.save_to_path(path, flowstate_document::RevisionKind::Session, cx)
  }

  fn save_to_path(
    &mut self,
    path: PathBuf,
    checkpoint_kind: flowstate_document::RevisionKind,
    cx: &mut Context<Self>,
  ) -> Task<std::io::Result<()>> {
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
          // G5: every save mints its tier's checkpoint, exactly like .db8
          // revisions — the history tape stops being empty-unless-pinned.
          if let Err(error) = io.create_checkpoint(None, checkpoint_kind).await {
            tracing::warn!(error = %format_args!("{error:#}"), "minting save checkpoint failed");
          }
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

  /// Map a pointer position to the (row, column) slot under it.
  fn slot_at_position(&self, position: gpui::Point<gpui::Pixels>) -> Option<(usize, usize)> {
    let point = self.model_point(position);
    let sheet = self.active_sheet_ref()?;
    let layout = GridLayout::compute_to(sheet, &self.cell_measurements, self.ghost_bottom_model());
    layout.row_at(point.y).zip(layout.column_at(point.x))
  }

  fn update_cell_drag_target(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
    let target = self.slot_at_position(position);
    if self.drag_target != target {
      self.drag_target = target;
      cx.notify();
    }
  }

  /// Shift-press on the grid starts a rubber-band selection anchored at the
  /// slot under the pointer (bare press pans instead).
  fn begin_marquee(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
    if let Some(slot) = self.slot_at_position(position) {
      self.marquee_anchor = Some(slot);
      self.selection_anchor = Some(slot);
      self.tab_anchor_column = None;
      self.select_cell_range(slot.0, slot.1, cx);
    }
  }

  /// While shift-dragging: grow the rectangle from the marquee anchor to the
  /// slot under the pointer.
  fn update_marquee(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
    if self.marquee_anchor.is_some() {
      // B1: a marquee dragged to the edge pans the board too.
      self.update_autoscroll(position);
      if let Some((row, column)) = self.slot_at_position(position) {
        self.select_cell_range(row, column, cx);
      }
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
    // B7: dragging a row that belongs to a gutter-selected band moves the
    // WHOLE band (Excel drags the selection, not the grabbed row).
    let row_ids: Vec<RowId> = self
      .banded_row_ids(row_id)
      .unwrap_or_else(|| vec![row_id]);
    if before.is_some_and(|anchor| row_ids.contains(&anchor)) {
      return;
    }
    if self
      .apply_intent(
        &FlowIntent::MoveRows {
          sheet_id,
          row_ids,
          before,
        },
        cx,
      )
      .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }

  /// B7: if `grabbed` is inside the selected gutter band, the band's row ids
  /// (in grid order); `None` otherwise.
  fn banded_row_ids(&self, grabbed: RowId) -> Option<Vec<RowId>> {
    let (r0, r1) = self.selected_row_band?;
    let sheet = self.active_sheet_ref()?;
    let last = sheet.rows.len().checked_sub(1)?;
    let (r0, r1) = (r0.min(last), r1.min(last));
    let ids: Vec<RowId> = sheet.rows[r0..=r1].iter().map(|row| row.id).collect();
    ids.contains(&grabbed).then_some(ids)
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
    // B7: dragging a header-banded column moves the whole band, in order.
    let column_ids: Vec<ColumnId> = self
      .banded_column_ids(column_id)
      .unwrap_or_else(|| vec![column_id]);
    if before.is_some_and(|anchor| column_ids.contains(&anchor)) {
      return;
    }
    let count = column_ids.len();
    if count > 1 {
      self.begin_bulk();
    }
    for id in column_ids {
      let _ = self.apply_intent(
        &FlowIntent::MoveColumn {
          sheet_id,
          column_id: id,
          before,
        },
        cx,
      );
    }
    if count > 1 {
      self.end_bulk("move columns", cx);
    }
    self.changed(self.active_cell, cx);
  }

  /// B7: if `grabbed` is inside the selected header band, the band's column
  /// ids (in grid order); `None` otherwise.
  fn banded_column_ids(&self, grabbed: ColumnId) -> Option<Vec<ColumnId>> {
    let (c0, c1) = self.selected_column_band?;
    let sheet = self.active_sheet_ref()?;
    let last = sheet.columns.len().checked_sub(1)?;
    let (c0, c1) = (c0.min(last), c1.min(last));
    let ids: Vec<ColumnId> = sheet.columns[c0..=c1].iter().map(|column| column.id).collect();
    ids.contains(&grabbed).then_some(ids)
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

  fn update_row_resize(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
    let zoom = self.board_zoom;
    let Some(resize) = self.row_resize.as_mut() else { return };
    let y = position.y.as_f32();
    let start_y = *resize.start_y.get_or_insert(y);
    let live = (resize.start_height + (y - start_y) / zoom).max(grid_layout::MIN_ROW_HEIGHT);
    if (live - resize.live_height).abs() > 0.5 {
      resize.live_height = live;
      cx.notify();
    }
  }

  fn finish_row_resize(&mut self, cx: &mut Context<Self>) {
    let Some(resize) = self.row_resize.take() else { return };
    let Some(sheet_id) = self.active_sheet else { return };
    if resize.start_y.is_none() {
      return; // a click with no drag: leave the row on autofit
    }
    if self
      .apply_intent(
        &FlowIntent::SetRowHeight {
          sheet_id,
          row_id: resize.row_id,
          height: Some(resize.live_height),
        },
        cx,
      )
      .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }

  /// Double-click the column edge = fit width to content (Excel autofit).
  /// Width is measured from the widest single line across the column's cells
  /// and its header label, using the same char-per-line model the row-height
  /// estimator uses (cells are rich text, so this is a content-aware estimate,
  /// not a pixel-exact layout pass). Clamped to a sane min/max.
  fn autofit_column(&mut self, column_id: ColumnId, column_ix: usize, window: &mut Window, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else { return };
    let width = self
      .autofit_column_width(column_ix, window)
      .unwrap_or(grid_layout::DEFAULT_COLUMN_WIDTH);
    if self
      .apply_intent(
        &FlowIntent::SetColumnWidth {
          sheet_id,
          column_id,
          width: Some(width),
        },
        cx,
      )
      .is_ok()
    {
      self.changed(self.active_cell, cx);
    }
  }

  /// B12: the content-fit width for a column, measured by SHAPING every line
  /// of the header label and each occupied cell with the real document font —
  /// not the old chars×average estimate — at zoom-1 model units. No arbitrary
  /// 3× ceiling: the widest real line wins.
  fn autofit_column_width(&self, column_ix: usize, window: &mut Window) -> Option<f32> {
    let sheet = self.active_sheet_ref()?;
    let theme = load_document_theme();
    let font = gpui::font(theme.default_font_family.clone());
    let font_size = theme.body_font_size;
    let text_system = window.text_system().clone();
    let measure = |text: &str| -> f32 {
      text
        .lines()
        .map(|line| {
          if line.is_empty() {
            return 0.0;
          }
          let run = gpui::TextRun {
            len: line.len(),
            font: font.clone(),
            color: gpui::black(),
            background_color: None,
            underline: None,
            strikethrough: None,
          };
          text_system
            .shape_line(SharedString::from(line.to_string()), font_size, &[run], None)
            .width
            .as_f32()
        })
        .fold(0.0_f32, f32::max)
    };
    let header = sheet.columns.get(column_ix).map_or(0.0, |column| measure(&column.label));
    let content = sheet
      .rows
      .iter()
      .filter_map(|row| row.cells.get(column_ix).and_then(|slot| slot.as_ref()))
      .map(|cell| measure(&cell.summary.summary_text))
      .fold(0.0_f32, f32::max);
    let padding = grid_layout::CELL_CONTENT_PADDING * 2.0 + 12.0;
    Some((header.max(content) + padding).max(grid_layout::MIN_COLUMN_WIDTH))
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
    // Live row-resize preview: shift every row top below the dragged row by the
    // height delta so the grid grows/shrinks under the pointer in real time.
    if let Some(resize) = &self.row_resize
      && let Some(index) = sheet.rows.iter().position(|row| row.id == resize.row_id)
    {
      let delta = resize.live_height - layout.row_height(index);
      for top in layout.row_tops.iter_mut().skip(index + 1) {
        *top += delta;
      }
    }
    Some((layout, sheet))
  }

  fn render_grid(&mut self, cx: &mut Context<Self>) -> AnyElement {
    let Some((layout, sheet)) = self.active_layout() else {
      return div().child("Select a sheet").into_any_element();
    };
    let zoom = self.board_zoom;
    let (origin_x, origin_y) = grid_origin_model();
    let content_width = px((origin_x + layout.total_width() + BOARD_PADDING) * zoom);
    let content_height = px((origin_y + layout.total_height() + BOARD_PADDING * 4.0) * zoom);
    let active = self.active_cell;
    let cursor = self.cursor;
    let weak_editor = cx.entity().downgrade();
    let client_document_theme = load_document_theme();
    let flow_theme = resolve_flow_theme();

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
    // The gridline is the ONLY separation between cells — its color is an
    // explicit slot in the flow's own palette, not derived from the app theme.
    let grid_line_color = flow_theme.gridline;
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
    let strokes: Vec<(gpui::Point<gpui::Pixels>, flowstate_flow::AnnotationStroke)> = if self.ink_visible {
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
    let draft_pen = self.pen_preset;
    // G2: peers' in-flight strokes paint live in their identity colors.
    let peer_drafts: Vec<(u32, Vec<flowstate_flow::StrokePoint>)> = self
      .external_presences
      .iter()
      .filter(|presence| presence.sheet == self.active_sheet && presence.ink_preview.len() >= 2)
      .map(|presence| {
        let points = presence
          .ink_preview
          .iter()
          .map(|(x, y)| flowstate_flow::StrokePoint {
            x: *x as f32,
            y: *y as f32,
          })
          .collect();
        (presence.color_rgb, points)
      })
      .collect();
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
        let side_palette = flow_theme.side(column.side);
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
              apply_flow_cell_theme(document, &client_document_theme, flow_theme.text, flow_theme.surface, zoom);
            }
            let cell_editor = self.cell_editors.get(&id).cloned();
            let fill = crate::flow::cell_theme::flow_cell_fill(
              &flow_theme,
              side_color,
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
              flow_theme.selection
            } else if active == Some(id) {
              side_palette.active
            } else if self.selection_rect.is_none() && self.selected_cells.contains(&id) {
              // C3: rectangle selections render ONE overlay rect instead of
              // scattered per-cell rings — the ring survives only for
              // irregular (ctrl-click) selections.
              side_palette.base.opacity(0.85)
            } else {
              presence_ring.unwrap_or(fill)
            };
            let shake_offset = shake.and_then(|(cell, offset)| (cell == id).then_some(offset));
            // Excel cut marquee: a cell armed by Ctrl+X wears a dashed border
            // until the paste consumes it (or Esc clears it), so a cut is
            // visibly distinct from a copy and the user can see what will move.
            // C4: the cut marquee pulses in the selection accent — a slow
            // breathe standing in for marching ants.
            let cut_alpha = self.cut_pending.as_ref().and_then(|(_, cells, since)| {
              cells.contains(&id).then(|| {
                let t = since.elapsed().as_secs_f32();
                0.55 + 0.35 * (t * 4.0).sin()
              })
            });
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
                // C5: the refusal shake rides the absolute LEFT so nothing
                // about the content box changes mid-shake.
                .left(left + px(grid_layout::CELL_SLOT_INSET) + px(shake_offset.unwrap_or(0.0)))
                .top(top + px(grid_layout::CELL_SLOT_INSET))
                .w(width - px(grid_layout::CELL_SLOT_INSET))
                .h(height - px(grid_layout::CELL_SLOT_INSET))
                .overflow_hidden()
                .bg(fill)
                .border(px(grid_layout::CELL_BORDER))
                .border_color(ring_color)
                // A4/Q-23: a remote landing or bump-down paints a fading wash.
                .when_some(self.cell_flash_alpha(id), |this, alpha| {
                  this.child(
                    div()
                      .absolute()
                      .inset_0()
                      .bg(flow_theme.selection.opacity(alpha)),
                  )
                })
                .when_some(cut_alpha, |this, alpha| {
                  this.border_dashed().border_color(flow_theme.selection.opacity(alpha))
                })
                .group(grip_group.clone())
                .when(dragging_this, |this| this.opacity(0.4))
                // C1 (Q-5): the hover wash — a faint selection tint on the slot
                // under the pointer, on top of the fill, under the content.
                .child(
                  div()
                    .absolute()
                    .inset_0()
                    .invisible()
                    .group_hover(grip_group.clone(), |this| this.visible())
                    .bg(flow_theme.selection.opacity(0.06)),
                )
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
                  let drag_text_color = flow_theme.text;
                  let dot_color = flow_theme.muted_text.opacity(0.75);
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
                            // Shift-press starts a rubber-band marquee; Ctrl/Cmd-press
                            // is a toggle (handled by on_click). Neither pans — a pan's
                            // 4px threshold would otherwise swallow the click.
                            let modifiers = event.modifiers;
                            if modifiers.shift {
                              editor.begin_marquee(event.position, cx);
                              cx.stop_propagation();
                              return;
                            }
                            if modifiers.control || modifiers.platform {
                              cx.stop_propagation();
                              return;
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
                          editor.tab_anchor_column = None;
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
                // D4/C15: an overridden row that clips its content says so —
                // and the indicator explains itself and FIXES it on click.
                .when(clipped, |this| {
                  let clipped_row_id = sheet.rows[row_ix].id;
                  let clip_weak = weak_editor.clone();
                  this.child(
                    div()
                      .id(("flow-clipped-row", id.as_u128() as u64))
                      .absolute()
                      .bottom(px(0.0))
                      .right(px(4.0))
                      .text_size(px(10.0 * zoom))
                      .text_color(flow_theme.muted_text)
                      .cursor_pointer()
                      .hover(|this| this.text_color(flow_theme.text))
                      .tooltip(move |window, cx| {
                        gpui_component::tooltip::Tooltip::new(
                          "Row height is set by hand and clips this card — click to autofit",
                        )
                        .build(window, cx)
                      })
                      .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                      .on_click(move |_, _, cx| {
                        let _ = clip_weak.update(cx, |editor, cx| {
                          editor.clear_row_height_override(clipped_row_id, cx);
                        });
                        cx.stop_propagation();
                      })
                      .child("⋯"),
                  )
                })
                .into_any_element(),
            );
          },
          None => {
            // Every empty slot — real rows AND the ghost run below — shares the
            // occupied cell's idle side wash, so the whole grid reads as one
            // uniform field (Excel-style, effectively endless) with no darker
            // band appearing right under the last row of content.
            let empty_fill = crate::flow::cell_theme::flow_cell_fill(&flow_theme, side_color, 0.0);
            slots.push(
              div()
                .id(("flow-slot", (row_ix as u64) << 16 | column_ix as u64))
                .absolute()
                // C19: same named inset as occupied cells — one constant.
                .left(left + px(grid_layout::CELL_SLOT_INSET))
                .top(top + px(grid_layout::CELL_SLOT_INSET))
                .w(width - px(grid_layout::CELL_SLOT_INSET))
                .h(height - px(grid_layout::CELL_SLOT_INSET))
                .bg(empty_fill)
                // C1 (Q-5): hover wash on empty slots too — the grid answers
                // the pointer everywhere.
                .hover(|this| this.bg(flow_theme.selection.opacity(0.06)))
                .when(is_cursor, |this| this.border(px(2.0)).border_color(side_palette.active))
                // G2: a peer parked on this EMPTY slot rings it in their color
                // (a cell id could never say this).
                .when_some(
                  self
                    .external_presences
                    .iter()
                    .find(|presence| {
                      presence.sheet == self.active_sheet
                        && presence.cell.is_none()
                        && presence.slot == Some((row_ix, column_ix))
                    })
                    .map(|presence| gpui::Hsla::from(rgba((presence.color_rgb << 8) | 0xff))),
                  |this, color| this.border(px(2.0)).border_color(color.opacity(0.8)),
                )
                // (F1: drag_target is also set by toolkit-card drags, so the
                // landing cue no longer requires a cell drag to be live.)
                .when(is_drag_target, |this| {
                  this.border(px(2.0)).border_dashed().border_color(flow_theme.selection)
                })
                .on_mouse_down(
                  MouseButton::Left,
                  cx.listener(move |editor, event: &MouseDownEvent, window, cx| {
                    if editor.annotation_tool != AnnotationTool::None {
                      return;
                    }
                    // Shift-press starts a marquee; Ctrl/Cmd-press is left to
                    // on_click. Neither pans.
                    let modifiers = event.modifiers;
                    if modifiers.shift {
                      editor.begin_marquee(event.position, cx);
                      cx.stop_propagation();
                      return;
                    }
                    if modifiers.control || modifiers.platform {
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
                  editor.tab_anchor_column = None;
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
          .bg(flow_theme.selection)
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
          .bg(flow_theme.selection)
          .into_any_element()
      });

    // Fill handle: the small square at the selection's bottom-right corner
    // (Excel). Dragging it tiles the selection's content into the swept range.
    let fill_handle: Option<AnyElement> = {
      let bottom_right = if !self.selected_cells.is_empty() {
        self
          .selected_cells
          .iter()
          .filter_map(|id| sheet.cell_position(*id))
          .reduce(|a, b| (a.0.max(b.0), a.1.max(b.1)))
      } else {
        self.cursor.filter(|&(row, column)| sheet.slot(row, column).is_some())
      };
      bottom_right.map(|(row_ix, column_ix)| {
        let x = board_content_offset(origin_x, layout.column_lefts[column_ix] + layout.column_widths[column_ix], zoom);
        let y = board_content_offset(origin_y, layout.row_top(row_ix) + layout.row_height(row_ix), zoom);
        // B10: a forgiving 21px hit area (the visible square stays 9px) and
        // Excel's thin-cross cursor instead of a grab hand.
        div()
          .id("flow-fill-handle")
          .absolute()
          .left(x - px(11.0))
          .top(y - px(11.0))
          .w(px(21.0))
          .h(px(21.0))
          .flex()
          .items_center()
          .justify_center()
          .cursor(gpui::CursorStyle::Crosshair)
          .on_drag(FillHandleDrag, |_, _, _, cx| cx.new(|_| EmptyDragPreview))
          .child(
            div()
              .w(px(9.0))
              .h(px(9.0))
              .bg(flow_theme.selection)
              .border_1()
              .border_color(flow_theme.surface),
          )
          .into_any_element()
      })
    };

    // Fill preview: while the handle is being dragged, outline the range the
    // fill will land in (Excel's marching-ants) so the user isn't dragging
    // blind. Mirrors `fill_handle_drop`'s dominant-axis rule.
    let fill_preview: Option<AnyElement> = self.fill_target.and_then(|(target_row, target_col)| {
      let selection = (|| {
        let positions: Vec<(usize, usize)> = if !self.selected_cells.is_empty() {
          self.selected_cells.iter().filter_map(|id| sheet.cell_position(*id)).collect()
        } else {
          self.cursor.filter(|&(r, c)| sheet.slot(r, c).is_some()).into_iter().collect()
        };
        let r0 = positions.iter().map(|(r, _)| *r).min()?;
        let c0 = positions.iter().map(|(_, c)| *c).min()?;
        let r1 = positions.iter().map(|(r, _)| *r).max()?;
        let c1 = positions.iter().map(|(_, c)| *c).max()?;
        Some((r0, c0, r1, c1))
      })();
      let (r0, c0, r1, c1) = selection?;
      let down = target_row > r1;
      let right = target_col > c1;
      let (end_row, end_col) = if down && (target_row - r1) >= target_col.saturating_sub(c1) {
        (target_row, c1)
      } else if right {
        (r1, target_col)
      } else {
        return None; // up/left drag fills nothing — no preview
      };
      let last_row = layout.total_rows().saturating_sub(1);
      let last_col = layout.column_widths.len().saturating_sub(1);
      let end_row = end_row.min(last_row);
      let end_col = end_col.min(last_col);
      let x0 = board_content_offset(origin_x, layout.column_lefts[c0], zoom);
      let y0 = board_content_offset(origin_y, layout.row_top(r0), zoom);
      let x1 = board_content_offset(origin_x, layout.column_lefts[end_col] + layout.column_widths[end_col], zoom);
      let y1 = board_content_offset(origin_y, layout.row_top(end_row) + layout.row_height(end_row), zoom);
      // B10: show the value the far corner will receive ("3AC"), so a series
      // fill isn't blind until the drop.
      let preview_value: Option<String> = if end_row > r1 {
        let sources: Vec<String> = (r0..=r1).map(|row| self.cell_text_at(row, c0).unwrap_or_default()).collect();
        clipboard::series_or_tile(&sources, end_row - r1).last().cloned()
      } else if end_col > c1 {
        let sources: Vec<String> = (c0..=c1).map(|column| self.cell_text_at(r0, column).unwrap_or_default()).collect();
        clipboard::series_or_tile(&sources, end_col - c1).last().cloned()
      } else {
        None
      }
      .filter(|value| !value.is_empty())
      .map(|value| value.chars().take(28).collect());
      Some(
        div()
          .absolute()
          .left(x0)
          .top(y0)
          .w(x1 - x0)
          .h(y1 - y0)
          .border(px(1.5))
          .border_dashed()
          .border_color(flow_theme.selection)
          .when_some(preview_value, |this, value| {
            this.child(
              div()
                .absolute()
                .bottom(px(2.0))
                .right(px(2.0))
                .px(px(4.0))
                .rounded(px(3.0))
                .bg(flow_theme.header_bg)
                .border_1()
                .border_color(flow_theme.chrome_border)
                .text_size(px(10.0))
                .text_color(flow_theme.text)
                .whitespace_nowrap()
                .child(value),
            )
          })
          .into_any_element(),
      )
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
        if !editor.suppress_drop && event.bounds.contains(&event.event.position) {
          editor.update_autoscroll(event.event.position);
          editor.update_cell_drag_target(event.event.position, cx);
        }
      }))
      .on_drop(cx.listener(|editor, drag: &FlowCellDrag, _, cx| {
        if editor.take_suppressed_drop() {
          return; // B2: the gesture was Esc-cancelled
        }
        editor.finish_cell_drop(drag.cell_id, cx)
      }))
      .on_drag_move(cx.listener(|editor, event: &DragMoveEvent<RowDrag>, _, cx| {
        if !editor.suppress_drop && event.bounds.contains(&event.event.position) {
          editor.update_autoscroll(event.event.position);
          editor.update_row_drag_gap(event.event.position, cx);
        }
      }))
      .on_drop(cx.listener(|editor, drag: &RowDrag, _, cx| {
        if editor.take_suppressed_drop() {
          return; // B2
        }
        editor.finish_row_drop(drag.row_id, cx)
      }))
      .on_drag_move(cx.listener(|editor, event: &DragMoveEvent<ColumnDrag>, _, cx| {
        if !editor.suppress_drop && event.bounds.contains(&event.event.position) {
          editor.update_autoscroll(event.event.position);
          editor.update_column_drag_gap(event.event.position, cx);
        }
      }))
      .on_drop(cx.listener(|editor, drag: &ColumnDrag, _, cx| {
        if editor.take_suppressed_drop() {
          return; // B2
        }
        editor.finish_column_drop(drag.column_id, cx)
      }))
      .on_drag_move(cx.listener(|editor, event: &DragMoveEvent<ColumnResizeDrag>, _, cx| {
        editor.update_column_resize(event.event.position, cx);
      }))
      .on_drop(cx.listener(|editor, _: &ColumnResizeDrag, _, cx| editor.finish_column_resize(cx)))
      .on_drag_move(cx.listener(|editor, event: &DragMoveEvent<RowResizeDrag>, _, cx| {
        editor.update_row_resize(event.event.position, cx);
      }))
      .on_drop(cx.listener(|editor, _: &RowResizeDrag, _, cx| editor.finish_row_resize(cx)))
      // F1/Q-21: a card dragged from the tub (or any toolkit surface) lands
      // in a slot — full content seeded, provenance recorded.
      .on_drag_move(cx.listener(|editor, event: &DragMoveEvent<crate::rich_text_element::ToolkitTextDrag>, _, cx| {
        if event.bounds.contains(&event.event.position) {
          editor.update_autoscroll(event.event.position);
          editor.update_cell_drag_target(event.event.position, cx);
        }
      }))
      .on_drop(cx.listener(|editor, drag: &crate::rich_text_element::ToolkitTextDrag, _, cx| {
        editor.drop_toolkit_card(drag, cx);
      }))
      .on_drag_move(cx.listener(|editor, event: &DragMoveEvent<FillHandleDrag>, _, cx| {
        if !editor.suppress_drop && event.bounds.contains(&event.event.position) {
          editor.update_autoscroll(event.event.position);
          let target = editor.slot_at_position(event.event.position);
          if editor.fill_target != target {
            editor.fill_target = target;
            cx.notify();
          }
        }
      }))
      .on_drop(cx.listener(|editor, _: &FillHandleDrag, _, cx| {
        if let Some((row, column)) = editor.fill_target.take() {
          editor.fill_handle_drop(row, column, cx);
        }
      }))
      .children(slots)
      // C2/C3: rectangle selections (range / marquee / row / column / all)
      // paint ONE translucent fill spanning the whole rect — empty slots
      // included, so a live marquee is visible over blank regions — plus a
      // single perimeter border. Non-occluding: clicks pass through.
      .children(self.selection_rect.and_then(|(r0, c0, r1, c1)| {
        let last_column = sheet.columns.len().checked_sub(1)?;
        let (c0, c1) = (c0.min(last_column), c1.min(last_column));
        let last_row = layout.total_rows().checked_sub(1)?;
        let (r0, r1) = (r0.min(last_row), r1.min(last_row));
        let left = board_content_offset(origin_x, layout.column_lefts[c0], zoom);
        let right = board_content_offset(origin_x, layout.column_lefts[c1] + layout.column_widths[c1], zoom);
        let top = board_content_offset(origin_y, layout.row_top(r0), zoom);
        let bottom = board_content_offset(origin_y, layout.row_top(r1) + layout.row_height(r1), zoom);
        Some(
          div()
            .absolute()
            .left(left)
            .top(top)
            .w(right - left)
            .h(bottom - top)
            .bg(flow_theme.selection.opacity(0.10))
            .border(px(1.5))
            .border_color(flow_theme.selection)
            .into_any_element(),
        )
      }))
      // G2: peers' selection rectangles, faint in their identity colors.
      .children(
        self
          .external_presences
          .iter()
          .filter(|presence| presence.sheet == self.active_sheet)
          .filter_map(|presence| {
            let (r0, c0, r1, c1) = presence.selection_rect?;
            if r0 == r1 && c0 == c1 {
              return None; // single slot: the ring already covers it
            }
            let last_column = sheet.columns.len().checked_sub(1)?;
            let (c0, c1) = (c0.min(last_column), c1.min(last_column));
            let last_row = layout.total_rows().checked_sub(1)?;
            let (r0, r1) = (r0.min(last_row), r1.min(last_row));
            let color = gpui::Hsla::from(rgba((presence.color_rgb << 8) | 0xff));
            let left = board_content_offset(origin_x, layout.column_lefts[c0], zoom);
            let right = board_content_offset(origin_x, layout.column_lefts[c1] + layout.column_widths[c1], zoom);
            let top = board_content_offset(origin_y, layout.row_top(r0), zoom);
            let bottom = board_content_offset(origin_y, layout.row_top(r1) + layout.row_height(r1), zoom);
            Some(
              div()
                .absolute()
                .left(left)
                .top(top)
                .w(right - left)
                .h(bottom - top)
                .bg(color.opacity(0.07))
                .border_1()
                .border_color(color.opacity(0.6))
                .into_any_element(),
            )
          })
          .collect::<Vec<_>>(),
      )
      .children(row_bar)
      .children(column_bar)
      .children(fill_preview)
      .children(fill_handle)
      // B11: resize feedback is a PHANTOM GUIDE at the live edge — no numeric
      // tooltip chatter; numbers live in the header context menu (D8).
      .children(self.column_resize.as_ref().and_then(|resize| {
        let ix = sheet.columns.iter().position(|column| column.id == resize.column_id)?;
        let x = board_content_offset(origin_x, layout.column_lefts[ix] + layout.column_widths[ix], zoom);
        Some(
          div()
            .absolute()
            .left(x - px(1.0))
            .top(px(0.0))
            .w(px(2.0))
            .h(content_height)
            .bg(flow_theme.selection.opacity(0.8))
            .into_any_element(),
        )
      }))
      .children(self.row_resize.as_ref().and_then(|resize| {
        let ix = sheet.rows.iter().position(|row| row.id == resize.row_id)?;
        let y = board_content_offset(origin_y, layout.row_top(ix) + resize.live_height, zoom);
        Some(
          div()
            .absolute()
            .left(px(0.0))
            .top(y - px(1.0))
            .w(content_width)
            .h(px(2.0))
            .bg(flow_theme.selection.opacity(0.8))
            .into_any_element(),
        )
      }))
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
                    paint_stroke(
                      bounds.origin + draft_origin,
                      &local,
                      px(draft_pen.width() * zoom),
                      gpui::Hsla::from(rgba(draft_color)).opacity(draft_pen.opacity()),
                      zoom,
                      window,
                    );
                  }
                  // G2: peers' in-flight strokes, in their identity colors.
                  for (color_rgb, points) in &peer_drafts {
                    paint_stroke(
                      bounds.origin + draft_origin,
                      points,
                      px(4.0 * zoom),
                      gpui::Hsla::from(rgba((color_rgb << 8) | 0xff)).opacity(0.45),
                      zoom,
                      window,
                    );
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
    let flow_theme = resolve_flow_theme();
    let mut children: Vec<AnyElement> = Vec::new();
    for (column_ix, column) in sheet.columns.iter().enumerate() {
      let side = flow_theme.side(column.side);
      // The overlay container already sits at the grid's x origin (the
      // gutter edge) — children position in bare column space.
      let left = px(layout.column_lefts[column_ix] * zoom) + offset.x;
      let width = px(layout.column_widths[column_ix] * zoom);
      let column_id = column.id;
      let label: SharedString = column.label.clone().into();
      let weak = cx.entity().downgrade();
      let drag_label = label.clone();
      // The header lights up when the cursor is in this column OR any of its
      // cells is selected (Excel's column-header highlight) — a
      // select-column/span otherwise left the header completely inert.
      let selected_column = self.cursor.is_some_and(|(_, cursor_column)| cursor_column == column_ix)
        || (!self.selected_cells.is_empty()
          && sheet
            .rows
            .iter()
            .filter_map(|row| row.cells.get(column_ix).and_then(|slot| slot.as_ref()))
            .any(|cell| self.selected_cells.contains(&cell.id)));
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
          // Cells are neutral (C2), so the header band carries the side
          // identity: a stronger tint plus the accent label and underline.
          // A selected column deepens that tint.
          .bg(side.base.opacity(if selected_column { 0.30 } else { 0.14 }))
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
          // B4: right-click opens the column menu (rename / width / side / …).
          .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |editor, event: &MouseDownEvent, window, cx| {
              editor.show_header_context_menu(column_ix, event.position, window, cx);
              cx.stop_propagation();
            }),
          )
          // Click a header to select the whole column; shift = column span,
          // ctrl/cmd = add the column to the selection.
          .on_click(cx.listener(move |editor, event: &gpui::ClickEvent, window, cx| {
            let m = event.modifiers();
            if m.shift {
              let anchor = editor.selection_anchor.map(|(_, column)| column).unwrap_or(column_ix);
              editor.select_column_span(anchor, column_ix, cx);
            } else if m.control || m.platform {
              editor.add_column_to_selection(column_ix, cx);
            } else {
              editor.select_column(column_ix, cx);
            }
            editor.focus_handle.focus(window);
          }))
          // C13: the label truncates instead of colliding with the "+".
          .child(
            div()
              .flex_1()
              .min_w(px(0.0))
              .overflow_hidden()
              .text_ellipsis()
              .whitespace_nowrap()
              .child(label),
          )
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
          .bg(flow_theme.chrome_border)
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
          .on_click(cx.listener(move |editor, event: &gpui::ClickEvent, window, cx| {
            if event.click_count() >= 2 {
              editor.autofit_column(column_id, column_ix, window, cx);
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
        // C13: vertically centered at every zoom, not a fixed 4px.
        .top((header_height - px(22.0 * zoom)) / 2.0)
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
      .bg(flow_theme.header_bg)
      .border_b_1()
      .border_color(flow_theme.chrome_border)
      .children(children)
      .into_any_element()
  }

  /// The frozen row-number gutter: numbers, row drag handles, row selection.
  fn render_gutter_overlay(&mut self, cx: &mut Context<Self>) -> AnyElement {
    let Some((layout, sheet)) = self.active_layout() else {
      return div().into_any_element();
    };
    let zoom = self.board_zoom;
    let flow_theme = resolve_flow_theme();
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
      // The gutter number highlights when the cursor is on this row OR any cell
      // in it is selected — so a whole-row / multi-row / column selection lights
      // up its row headers (Excel's primary "where am I" cue), not just the
      // single cursor row.
      let selected_row = self.cursor.is_some_and(|(cursor_row, _)| cursor_row == row_ix)
        || (!self.selected_cells.is_empty()
          && sheet
            .rows
            .get(row_ix)
            .is_some_and(|row| row.cells.iter().flatten().any(|cell| self.selected_cells.contains(&cell.id))));
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
          .border_color(flow_theme.chrome_border)
          .text_size(px(10.5 * zoom))
          .text_color(if selected_row {
            flow_theme.text
          } else {
            flow_theme.muted_text.opacity(if is_ghost { 0.45 } else { 0.9 })
          })
          .when(selected_row, |this| this.bg(flow_theme.selection.opacity(0.12)))
          .hover(|style| style.bg(flow_theme.selection.opacity(0.08)))
          .cursor_grab()
          .child(SharedString::from(format!("{}", row_ix + 1)))
          // B4: right-click opens the row menu.
          .on_mouse_down(
            MouseButton::Right,
            cx.listener(move |editor, event: &MouseDownEvent, window, cx| {
              editor.show_gutter_context_menu(row_ix, event.position, window, cx);
              cx.stop_propagation();
            }),
          )
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
                  return;
                }
                // Shift = row span, ctrl/cmd = add the row to the selection.
                let m = event.modifiers();
                if m.shift {
                  let anchor = editor.selection_anchor.map(|(row, _)| row).unwrap_or(row_ix);
                  editor.select_row_span(anchor, row_ix, cx);
                } else if m.control || m.platform {
                  editor.add_row_to_selection(row_ix, cx);
                } else {
                  editor.select_row(row_ix, cx);
                }
              }))
          })
          .into_any_element(),
      );
      // D4: the bottom-edge drag grip that sets a manual row height (Excel row
      // resize); double-click clears the override back to autofit. Real rows
      // only — ghost rows have no durable id to override.
      if !is_ghost
        && let Some(row_id) = row_id
      {
        let start_height = layout.row_height(row_ix);
        let resize_weak = cx.entity().downgrade();
        children.push(
          div()
            .id(("flow-row-resize", row_ix))
            .absolute()
            .left(px(0.0))
            .top(top + height - px(3.0))
            .w(px(GUTTER_WIDTH * zoom))
            .h(px(7.0))
            .cursor(gpui::CursorStyle::ResizeUpDown)
            .on_drag(RowResizeDrag { row_id, start_height }, move |drag, _, _, cx| {
              let _ = resize_weak.update(cx, |editor, cx| {
                editor.row_resize = Some(RowResizeState {
                  row_id: drag.row_id,
                  start_height: drag.start_height,
                  start_y: None,
                  live_height: drag.start_height,
                });
                cx.notify();
              });
              cx.new(|_| EmptyDragPreview)
            })
            .on_click(cx.listener(move |editor, event: &gpui::ClickEvent, _, cx| {
              if event.click_count() >= 2 {
                editor.clear_row_height_override(row_id, cx);
              }
            }))
            .into_any_element(),
        );
      }
    }
    div()
      .absolute()
      .top(px(HEADER_HEIGHT * zoom))
      .left(px(0.0))
      .bottom(px(0.0))
      .w(px(GUTTER_WIDTH * zoom))
      .overflow_hidden()
      .bg(flow_theme.gutter_bg)
      .border_r_1()
      .border_color(flow_theme.chrome_border)
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
    // gpui's native scroll accumulates the wheel delta UNCLAMPED and only clamps
    // it at paint — but this render reads `board_scroll.offset()` during element
    // construction (frozen gutter/header, grid window), i.e. before that clamp.
    // Scrolling up at the top would otherwise let every reader observe a
    // transient overscrolled offset, desyncing the frozen chrome and perturbing
    // row-0 cell measurement (the gutter "1" ballooning until prefetch resettles
    // ~1-2s later). Re-clamp up front so this frame's readers see an in-range
    // offset.
    self.clamp_board_scroll_offset();
    self.refresh_active_cell_theme(cx);
    let flow_theme = resolve_flow_theme();
    // Refusal voice (P2/C6): the words float as an app-level notification —
    // pushed exactly once per refusal, REPLACING the previous one instead of
    // stacking — while the notice itself keeps driving the cell shake.
    let _ = self.refusal_toast(); // ages the notice out
    if let Some(notice) = self.refusal.as_mut()
      && !notice.toasted
    {
      notice.toasted = true;
      let message = notice.message.clone();
      window.push_notification(
        gpui_component::notification::Notification::warning(message).id1::<FlowRefusalToast>("flow-refusal"),
        cx,
      );
    }
    // A4/Q-23: expired flashes drop out; live ones keep animating.
    self.cell_flash.retain(|_, at| at.elapsed().as_secs_f32() < 1.2);
    if self.refusal.is_some() || !self.cell_flash.is_empty() {
      cx.on_next_frame(window, |_, _, cx| cx.notify());
    }
    // C4: the cut marquee breathes at ~8fps — a timer, not a full-rate frame
    // loop, since a cut can stay armed a long time.
    if self.cut_pending.is_some() && !self.cut_pulse_scheduled {
      self.cut_pulse_scheduled = true;
      cx.spawn(async move |editor, cx| {
        cx.background_executor()
          .timer(std::time::Duration::from_millis(120))
          .await;
        let _ = editor.update(cx, |editor, cx| {
          editor.cut_pulse_scheduled = false;
          if editor.cut_pending.is_some() {
            cx.notify();
          }
        });
      })
      .detach();
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
      // A8: the row twin — a row-resize drag that evaporated outside the grid
      // used to leave its preview state wedged forever.
      if self.row_resize.is_some() {
        self.finish_row_resize(cx);
      }
      // B2: an Esc-cancelled drag whose release never produced a drop must not
      // leave the flag armed to eat the NEXT gesture's drop.
      self.suppress_drop = false;
    }
    // B1: pump the edge-autoscroll while a drag or marquee is live; stop the
    // instant the gesture ends.
    if let Some((vx, vy)) = self.autoscroll {
      if cx.has_active_drag() || self.marquee_anchor.is_some() {
        let mut offset = self.board_scroll.offset();
        offset.x += px(vx);
        offset.y += px(vy);
        self.set_user_scroll_offset(offset);
        cx.on_next_frame(window, |_, _, cx| cx.notify());
      } else {
        self.autoscroll = None;
      }
    }
    if self.pan_drag.is_some() {
      let editor = cx.entity();
      window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
        // A8: middle-button pans end here too — releasing off-editor used to
        // leave the pan armed until the next left click.
        if phase.bubble() && matches!(event.button, MouseButton::Left | MouseButton::Middle) {
          editor.update(cx, |editor, cx| editor.finish_space_pan(cx));
        }
      });
    }
    let grid_scroll = self.board_scroll.clone();
    let board_zoom = self.board_zoom;
    self.built_scroll_offset.set(self.board_scroll.offset());
    // Part 4: column washes belong to the aether — each side's tint starts
    // under the header and runs to the bottom of the viewport.
    // The column washes only exist when the palette opts into a side tint
    // (`cell_wash > 0`); C2's neutral grid paints no band.
    let wash_columns: Vec<(f32, f32, gpui::Hsla)> = if self.scrubber.is_some() || flow_theme.cell_wash <= 0.0 {
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
                flow_theme.side(column.side).base,
              )
            })
            .collect()
        })
        .unwrap_or_default()
    };
    // The band is a fraction of the cell wash (historically 3.5% vs the 8%
    // cell wash → ~0.44), so it stays subtler than the cells themselves.
    let column_wash_alpha = flow_theme.cell_wash * 0.44;
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
        // B2: Esc first aborts any gesture in flight (drag / resize / marquee /
        // pan) — Excel muscle memory. Only then does the escape ladder run.
        if event.keystroke.key == "escape" && editor.cancel_active_gesture(cx) {
          cx.stop_propagation();
          return;
        }
        // Escape disarms the ink tool.
        if event.keystroke.key == "escape" && editor.annotation_tool != AnnotationTool::None {
          editor.set_annotation_tool(AnnotationTool::None, cx);
          cx.stop_propagation();
          return;
        }
        let board_focused = editor.focus_handle.is_focused(window);
        let key = event.keystroke.key.as_str();
        let m = event.keystroke.modifiers;
        // Cmd is treated like Ctrl for grid shortcuts. Alt is reserved (never a
        // grid verb) so alt-letter accented input still reaches the type path.
        let ctrl = m.control || m.platform;
        // Escape from a focused cell editor returns to cell-select mode.
        if key == "escape" && !board_focused && editor.active_cell.is_some() {
          editor.focus_handle.focus(window);
          cx.stop_propagation();
          return;
        }
        // Escape on the board clears a pending cut (Excel: the marching-ants
        // marquee goes away and the armed move is cancelled).
        if key == "escape" && board_focused && editor.cut_pending.take().is_some() {
          cx.notify();
          cx.stop_propagation();
          return;
        }
        // B3: Escape collapses a multi-cell selection to the cursor (Excel).
        if key == "escape" && board_focused && !editor.selected_cells.is_empty() {
          editor.selected_cells.clear();
          editor.clear_selection_shape();
          cx.notify();
          cx.stop_propagation();
          return;
        }
        // Enter/Tab WHILE EDITING a cell commit (cells commit live) and advance
        // the grid cursor, then hand focus back to the board. Excel: Enter =
        // down, Tab = right, Shift+Tab = left. Shift+Enter and Alt+Enter fall
        // through to the cell editor (soft line break / in-cell newline).
        if !board_focused && editor.active_cell.is_some() && !ctrl && !m.alt {
          let advanced = match key {
            "enter" if !m.shift => {
              editor.focus_handle.focus(window);
              editor.enter_navigate(false, cx);
              true
            },
            "tab" => {
              editor.focus_handle.focus(window);
              editor.tab_navigate(m.shift, cx);
              true
            },
            // B8: Excel Enter-mode — a cell entered by TYPE-OVER commits on a
            // bare arrow and the cursor moves on; F2/click entry keeps arrows
            // for the caret.
            "up" | "down" | "left" | "right" if editor.typeover_mode && !m.shift => {
              let dir = match key {
                "up" => GridDirection::Up,
                "down" => GridDirection::Down,
                "left" => GridDirection::Left,
                _ => GridDirection::Right,
              };
              editor.focus_handle.focus(window);
              editor.navigate(dir, cx);
              true
            },
            _ => false,
          };
          if advanced {
            cx.stop_propagation();
            return;
          }
        }
        if !board_focused {
          return;
        }
        if !m.alt {
          match key {
            "up" | "down" | "left" | "right" => {
              editor.tab_anchor_column = None; // any arrow breaks a Tab run
              let dir = match key {
                "up" => GridDirection::Up,
                "down" => GridDirection::Down,
                "left" => GridDirection::Left,
                _ => GridDirection::Right,
              };
              if ctrl {
                editor.jump_to_edge(dir, m.shift, cx); // Ctrl(+Shift) = jump to data edge
              } else if m.shift {
                editor.extend_selection(dir, cx);
              } else {
                editor.navigate(dir, cx);
              }
              cx.stop_propagation();
              return;
            },
            // Excel: Enter = down (returning to the Tab-run column), Shift+Enter
            // = up, Ctrl+Enter = new family.
            "enter" => {
              if ctrl {
                editor.add_new_family(cx);
              } else if !editor.cycle_within_selection(!m.shift, cx) {
                editor.enter_navigate(m.shift, cx);
              }
              cx.stop_propagation();
              return;
            },
            // Excel: Tab = right, Shift+Tab = left (remembers the run's column).
            "tab" => {
              if !editor.cycle_within_selection(!m.shift, cx) {
                editor.tab_navigate(m.shift, cx);
              }
              cx.stop_propagation();
              return;
            },
            "f2" => {
              editor.edit_cursor_cell(window, cx);
              cx.stop_propagation();
              return;
            },
            "delete" | "backspace" => {
              editor.delete_selected(window, cx);
              cx.stop_propagation();
              return;
            },
            "home" | "end" => {
              editor.tab_anchor_column = None;
              editor.cursor_to_extreme(key, ctrl, cx);
              cx.stop_propagation();
              return;
            },
            "pageup" | "pagedown" => {
              editor.tab_anchor_column = None;
              if ctrl {
                // B6: Ctrl+PageUp/Down cycles sheets (Excel).
                editor.cycle_sheet(key == "pagedown", cx);
              } else {
                editor.page(key == "pagedown", cx);
              }
              cx.stop_propagation();
              return;
            },
            // B16: the wedge keys — insert a row above/below the cursor with a
            // fresh card (Verbatim's F5 family; mid-speech speed feature).
            "f5" => {
              if m.shift {
                editor.add_sibling(RelativePosition::After, cx);
              } else {
                editor.add_sibling(RelativePosition::Before, cx);
              }
              editor.focus_active_cell(window, cx);
              cx.stop_propagation();
              return;
            },
            // B16: Ctrl+Minus deletes the cursor's row (Excel's delete key).
            "-" if ctrl => {
              editor.delete_cursor_row(cx);
              cx.stop_propagation();
              return;
            },
            // E9: mid-1NC sheet spawning — Ctrl+Shift+N mints another sheet
            // of the ACTIVE sheet's type without touching the mouse.
            "n" if ctrl && m.shift => {
              editor.create_sheet_matching_active(cx);
              cx.stop_propagation();
              return;
            },
            // B17: jump-to-speech — Ctrl+digit parks the cursor at the top of
            // that speech column (Verbatim's Switch Speech, keyboard-first).
            "1" | "2" | "3" | "4" | "5" | "6" | "7" | "8" | "9" if ctrl => {
              let column_ix = key.parse::<usize>().unwrap_or(1) - 1;
              let columns = editor.active_sheet_ref().map_or(0, |sheet| sheet.columns.len());
              if column_ix < columns {
                editor.set_cursor(0, column_ix, cx);
                editor.scroll_cursor_into_view();
              } else {
                editor.refuse(format!("this sheet has no speech {}", column_ix + 1), None, cx);
              }
              cx.stop_propagation();
              return;
            },
            "a" if ctrl => {
              editor.select_all(cx);
              cx.stop_propagation();
              return;
            },
            "c" if ctrl => {
              editor.copy_selection(cx);
              cx.stop_propagation();
              return;
            },
            "x" if ctrl => {
              editor.cut_selection(cx);
              cx.stop_propagation();
              return;
            },
            "v" if ctrl => {
              editor.paste(cx);
              cx.stop_propagation();
              return;
            },
            "d" if ctrl => {
              editor.fill_down(cx);
              cx.stop_propagation();
              return;
            },
            "r" if ctrl => {
              editor.fill_right(cx);
              cx.stop_propagation();
              return;
            },
            // Ctrl+Shift++ inserts a column before the cursor (Excel insert).
            "=" | "+" if ctrl && m.shift => {
              editor.insert_column_at_cursor(cx);
              cx.stop_propagation();
              return;
            },
            // Shift+Space = select row, Ctrl+Space = select column, bare = pan-arm.
            "space" => {
              if m.shift {
                if let Some((row, _)) = editor.cursor {
                  editor.select_row(row, cx);
                }
              } else if ctrl {
                if let Some((_, column)) = editor.cursor {
                  editor.select_column(column, cx);
                }
              } else {
                editor.space_pan_armed = true;
                cx.notify();
              }
              cx.stop_propagation();
              return;
            },
            _ => {},
          }
        }
        // D5: a printable key on the cursor's slot creates (empty) or overwrites
        // (occupied), seeded with the keystroke, then drops focus into the cell.
        if !ctrl
          && editor.annotation_tool == AnnotationTool::None
          && editor.cursor.is_some()
          && let Some(key_char) = event.keystroke.key_char.clone()
          && !key_char.is_empty()
          && !key_char.chars().any(char::is_control)
        {
          if let Some(new_cell) = editor.overwrite_cursor(&key_char, cx) {
            // B8: type-over entry arms Excel Enter-mode — the next bare arrow
            // commits and moves the grid cursor. (activate_cell cleared it.)
            editor.typeover_mode = true;
            // Hand focus to the new cell's editor SYNCHRONOUSLY, caret at end.
            // Deferring this to the next frame (the old path) left the board
            // focused for ~16ms, so a second keystroke arriving mid-frame
            // re-entered `overwrite_cursor`, saw the slot now occupied, and
            // deleted+recreated the cell seeded with only the 2nd char —
            // fast-typing "ab" into an empty slot yielded "b". Synchronous
            // focus routes the very next key into the cell, in order.
            // (`overwrite_cursor` → `add_cell_at_slot` already set `active_cell`
            // to `new_cell` via `changed`, so Escape/Enter stay wired.)
            if let Some(cell_editor) = editor.cell_editors.get(&new_cell).cloned() {
              cell_editor.update(cx, |cell_editor, cx| cell_editor.move_document_end(cx));
              cell_editor.read(cx).focus_handle(cx).focus(window);
            }
          }
          cx.stop_propagation();
          return;
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
      // color, no tool armed. B4: a right CLICK (no drag) opens the context
      // menu for the slot under the pointer instead of drawing nothing.
      .on_mouse_down(
        MouseButton::Right,
        cx.listener(|editor, event: &MouseDownEvent, _, cx| {
          editor.begin_ink(event.position, cx);
          cx.stop_propagation();
        }),
      )
      .on_mouse_up(
        MouseButton::Right,
        cx.listener(|editor, event: &MouseUpEvent, window, cx| {
          if editor.drawing_points.len() >= 2 {
            editor.finish_annotation(cx);
          } else {
            // The dot-stroke guard would reject this anyway — reclaim the
            // click as the context-menu gesture.
            editor.right_inking = false;
            editor.active_ink_color = None;
            editor.drawing_points.clear();
            editor.show_slot_context_menu(event.position, window, cx);
          }
        }),
      )
      .on_mouse_move(cx.listener(|editor, event: &MouseMoveEvent, window, cx| {
        if editor.right_inking {
          editor.continue_annotation(event.position, cx);
        }
        if editor.marquee_anchor.is_some() {
          editor.update_marquee(event.position, cx);
          return;
        }
        editor.queue_pan(event.position, window, cx);
      }))
      .on_mouse_up(
        MouseButton::Left,
        cx.listener(|editor, _: &MouseUpEvent, _, cx| {
          editor.marquee_anchor = None;
          editor.finish_space_pan(cx);
        }),
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
              window.paint_quad(gpui::fill(wash, side_color.opacity(column_wash_alpha)));
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
            } else if event.modifiers.control {
              // B5: Ctrl+wheel zooms at the pointer instead of falling through
              // to native scroll.
              let up = event.delta.pixel_delta(window.line_height()).y > px(0.0);
              editor.zoom_wheel(event.position, up, cx);
              cx.stop_propagation();
            } else {
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
            None => {
              // C14: a real empty state — the verbs live right here instead of
              // a bare sentence pointing at chrome elsewhere.
              let sheet_types: Vec<String> = self
                .board
                .format
                .sheet_types
                .iter()
                .map(|sheet_type| sheet_type.name.clone())
                .collect();
              div()
                .size_full()
                .flex()
                .flex_col()
                .items_center()
                .justify_center()
                .gap(px(10.0))
                .text_color(cx.theme().muted_foreground)
                .child("This flow has no sheets yet")
                .child(div().flex().flex_row().gap(px(8.0)).children(sheet_types.into_iter().enumerate().map(
                  |(index, name)| {
                    Button::new(("flow-empty-create-sheet", index))
                      .label(format!("New {name} sheet"))
                      .on_click(cx.listener(move |editor, _, window, cx| {
                        editor.create_sheet_of_type(index, cx);
                        editor.focus_handle.focus(window);
                      }))
                  },
                )))
                .child(
                  div()
                    .text_size(px(11.0))
                    .child("Sheets also live on the strip below — one per side, more per off-case"),
                )
                .into_any_element()
            },
          }),
      )
      // Frozen chrome: header (columns) + gutter (row numbers) + corner.
      .when(self.active_sheet.is_some() && self.scrubber.is_none(), |this| {
        this
          .child(self.render_header_overlay(cx))
          .child(self.render_gutter_overlay(cx))
          .child(
            div()
              .id("flow-corner-select-all")
              .absolute()
              .top(px(0.0))
              .left(px(0.0))
              .w(px(GUTTER_WIDTH * self.board_zoom))
              .h(px(HEADER_HEIGHT * self.board_zoom))
              .bg(flow_theme.gutter_bg)
              .border_b_1()
              .border_r_1()
              .border_color(flow_theme.chrome_border)
              .cursor_pointer()
              // Excel: the corner box selects the whole sheet.
              .on_click(cx.listener(|editor, _: &gpui::ClickEvent, window, cx| {
                editor.select_all(cx);
                editor.focus_handle.focus(window);
              })),
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
      // (P2/C6: the refusal's WORDS now float via the notification layer —
      // pushed above — so no inline toast occupies board space here.)
      // B4: the context menu, anchored at the right-click, over a click-away
      // scrim (the workspace outline menu's exact pattern).
      .when_some(self.context_menu.clone(), |this, (position, menu)| {
        let weak = cx.entity().downgrade();
        let close_weak = weak.clone();
        this.child(
          gpui::deferred(
            gpui::anchored().child(
              div()
                .w(window.bounds().size.width)
                .h(window.bounds().size.height)
                .occlude()
                .on_mouse_down(MouseButton::Left, move |_, _, cx| {
                  let _ = weak.update(cx, |editor, cx| {
                    editor.context_menu = None;
                    cx.notify();
                  });
                })
                .on_mouse_down(MouseButton::Right, move |_, _, cx| {
                  let _ = close_weak.update(cx, |editor, cx| {
                    editor.context_menu = None;
                    cx.notify();
                  });
                })
                .child(
                  gpui::anchored()
                    .position(position)
                    .snap_to_window_with_margin(px(8.0))
                    .anchor(gpui::Corner::TopLeft)
                    .child(menu),
                ),
            ),
          )
          .with_priority(1),
        )
      })
      // E10: the round-metadata form — a floating card, top-right.
      .when_some(self.round_form.clone(), |this, fields| {
        this.child(
          gpui::deferred(
            div()
              .absolute()
              .top(px(12.0))
              .right(px(12.0))
              .w(px(260.0))
              .p_3()
              .rounded(cx.theme().radius)
              .bg(cx.theme().popover)
              .border_1()
              .border_color(cx.theme().border)
              .shadow_lg()
              .flex()
              .flex_col()
              .gap_2()
              .child(
                div()
                  .text_size(px(11.0))
                  .font_weight(gpui::FontWeight::BOLD)
                  .text_color(cx.theme().muted_foreground)
                  .child("Round"),
              )
              .children(fields.into_iter().map(|(field, input)| {
                div()
                  .flex()
                  .flex_col()
                  .gap_0p5()
                  .child(
                    div()
                      .text_size(px(9.5))
                      .text_color(cx.theme().muted_foreground)
                      .child(field.label()),
                  )
                  .child(gpui_component::input::Input::new(&input).xsmall().w_full())
              })),
          )
          .with_priority(1),
        )
      })
      // D5/D8: the inline column rename / width inputs, anchored likewise.
      .when_some(
        self
          .column_rename
          .as_ref()
          .map(|(_, position, input)| (*position, input.clone(), "Rename column"))
          .or_else(|| {
            self
              .column_width_entry
              .as_ref()
              .map(|(_, position, input)| (*position, input.clone(), "Column width (px)"))
          }),
        |this, (position, input, caption)| {
          this.child(
            gpui::deferred(
              gpui::anchored()
                .position(position)
                .snap_to_window_with_margin(px(8.0))
                .anchor(gpui::Corner::TopLeft)
                .child(
                  div()
                    .p_2()
                    .w(px(200.0))
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().popover)
                    .border_1()
                    .border_color(cx.theme().border)
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                      div()
                        .text_size(px(10.0))
                        .text_color(cx.theme().muted_foreground)
                        .child(caption),
                    )
                    .child(gpui_component::input::Input::new(&input).xsmall().w_full()),
                ),
            )
            .with_priority(1),
          )
        },
      )
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
