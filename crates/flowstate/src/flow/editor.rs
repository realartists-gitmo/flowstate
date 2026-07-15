use std::collections::HashSet;
use std::path::PathBuf;
use std::sync::Arc;

use flowstate_collab::flow::{FlowDocHandle, FlowIoHandle, FlowLocalOutcome, FlowRuntime, FlowStreamItem, FlowWriteRejected};
use flowstate_flow::{
  AnnotationOriginator, BoardPoint, CellId, CellPlacement, CellSeed, FlowBoardProjection, FlowDropIntent, FlowIntent, RelativePosition, SheetId,
  VersionVector,
};
use gpui::{
  AnyElement, App, Bounds, Context, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, KeyDownEvent, KeyUpEvent,
  MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render, ScrollHandle, ScrollWheelEvent, SharedString, Subscription, Task, Window,
  canvas, div, point, prelude::*, px, rgba,
};
use gpui_component::ActiveTheme as _;
use gpui_component::PixelsExt as _;
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::scroll::{Scrollbar, ScrollbarShow};
use gpui_component::{Icon, IconName, Sizable as _};

use crate::{
  app_settings::load_document_theme,
  flow::{cell_theme::apply_flow_cell_theme, flow_side_palette},
  rich_text_element::{RichTextDocumentElement, RichTextEditor},
};

mod annotation;
mod cell_editing;
mod connector;
mod drag_drop;
mod grid_nav;
mod layout;
mod telemetry;
mod zoom;

pub use grid_nav::GridDirection;

use annotation::paint_stroke;
use connector::paint_connector_family;
use drag_drop::{DragSession, FlowCellDrag, FlowCellDragPreview, WirePlugDrag, WirePlugPreview};
use layout::{CellMeasurement, sheet_cell_layout};
use zoom::grid_dot_metrics;

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

/// A spoken refusal (spec §3.1 F3): message toast + optional cell shake.
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
  collapsed_outline_items: HashSet<uuid::Uuid>,
  annotation_tool: AnnotationTool,
  hidden_annotation_sheets: HashSet<SheetId>,
  hidden_annotation_originators: HashSet<AnnotationOriginator>,
  local_annotation_originator: AnnotationOriginator,
  drawing_points: Vec<BoardPoint>,
  cell_editors: std::collections::HashMap<CellId, Entity<RichTextEditor>>,
  cell_editor_themes: std::collections::HashMap<CellId, (gpui::Hsla, gpui::Hsla, u32)>,
  cell_editor_subscriptions: std::collections::HashMap<CellId, Subscription>,
  pending_cell_drop: Option<FlowDropIntent>,
  dragging_cell: Option<CellId>,
  /// Live drag bookkeeping (frozen slot baseline, pads, fling samples).
  drag: Option<DragSession>,
  /// W2 multi-select: the set every op applies to (always contains the
  /// active cell while non-empty).
  selected_cells: HashSet<CellId>,
  refusal: Option<RefusalNotice>,
  /// G5 wire re-plug: the cell whose plug is being dragged to a new parent.
  replug_cell: Option<CellId>,
  /// `FLOWSTATE_INTENT_LOG` debug overlay: the last few intents + outcomes.
  intent_log: std::collections::VecDeque<SharedString>,
  drag_autoscroll: Option<gpui::Point<gpui::Pixels>>,
  drag_autoscroll_scheduled: bool,
  drag_log: Option<telemetry::DragLogSession>,
  drag_log_counter: u64,
  cell_bounds: std::collections::HashMap<CellId, Bounds<gpui::Pixels>>,
  cell_measurements: std::collections::HashMap<CellId, CellMeasurement>,
  board_scroll: ScrollHandle,
  board_zoom: f32,
  camera_center: Option<BoardPoint>,
  camera_apply_pending: bool,
  viewport_origin: BoardPoint,
  space_pan_armed: bool,
  pan_drag: Option<PanDragState>,
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
    let collapsed_outline_items = board
      .sheets
      .iter()
      .flat_map(|sheet| std::iter::once(sheet.id).chain(sheet.cells.iter().map(|cell| cell.id)))
      .collect();
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
      collapsed_outline_items,
      annotation_tool: AnnotationTool::None,
      hidden_annotation_sheets: HashSet::new(),
      hidden_annotation_originators: HashSet::new(),
      local_annotation_originator: AnnotationOriginator("local".into()),
      drawing_points: Vec::new(),
      cell_editors: std::collections::HashMap::new(),
      cell_editor_themes: std::collections::HashMap::new(),
      cell_editor_subscriptions: std::collections::HashMap::new(),
      pending_cell_drop: None,
      dragging_cell: None,
      drag: None,
      selected_cells: HashSet::new(),
      refusal: None,
      replug_cell: None,
      intent_log: std::collections::VecDeque::new(),
      drag_autoscroll: None,
      drag_autoscroll_scheduled: false,
      drag_log: None,
      drag_log_counter: 0,
      cell_bounds: std::collections::HashMap::new(),
      cell_measurements: std::collections::HashMap::new(),
      board_scroll: ScrollHandle::new(),
      board_zoom: 1.0,
      camera_center: None,
      camera_apply_pending: false,
      viewport_origin: BoardPoint::default(),
      space_pan_armed: false,
      pan_drag: None,
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

  /// The `FLOWSTATE_INTENT_LOG` debug overlay (spec Part 5): alpha field
  /// calibration wants to SEE what each gesture committed.
  fn intent_log_enabled() -> bool {
    static ENABLED: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var("FLOWSTATE_INTENT_LOG").is_ok_and(|value| !matches!(value.trim(), "" | "0" | "false")))
  }

  fn log_intent(&mut self, entry: String) {
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
  fn apply_intent(&mut self, intent: &FlowIntent, cx: &mut Context<Self>) -> Result<FlowLocalOutcome, FlowWriteRejected> {
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

  /// Post-change bookkeeping shared by every mutation path: discard the board
  /// stream copy already integrated, refresh undo state, prune editors for
  /// cells the change removed, and (until a session attaches in S9) sink the
  /// publish queue so a long solo session never accumulates update blobs.
  fn after_local_change(&mut self, cx: &mut Context<Self>) {
    if let Ok(items) = self.handle.drain_board_stream()
      && let Some(FlowStreamItem::Board(board)) = items.into_iter().next_back()
    {
      self.board = *board;
    }
    self.undo_state = self.handle.can_undo().unwrap_or((false, false));
    self.prune_dead_cells(cx);
    self.sink_publish_queue();
  }

  fn sink_publish_queue(&self) {
    use flowstate_collab::local_write::GateHolder;
    if let Ok(mut guard) = self.handle.gate().lock(GateHolder::DocumentService) {
      let _ = guard.take_pending_publish();
    }
  }

  /// Drop editors/caches for cells that no longer exist on the board.
  fn prune_dead_cells(&mut self, _cx: &mut Context<Self>) {
    let live: HashSet<CellId> = self
      .board
      .sheets
      .iter()
      .flat_map(|sheet| sheet.cells.iter().map(|cell| cell.id))
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
  }

  /// Refresh EVERYTHING from the runtime after a change whose cell footprint
  /// is unknown (undo/redo, remote imports): board, all open cell editors,
  /// caches.
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
  /// split lands in S9 — live sessions then import through [`FlowIoHandle`]).
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

  pub fn focus_active_cell(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    cx.on_next_frame(window, |flow, window, cx| {
      let Some(editor) = flow
        .active_cell
        .and_then(|cell| flow.cell_editors.get(&cell))
        .cloned()
      else {
        return;
      };
      editor.update(cx, |editor, cx| editor.move_document_start(cx));
      editor.read(cx).focus_handle(cx).focus(window);
    });
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
      cx.emit(FlowEditorEvent::ActiveSheetChanged(Some(sheet_id)));
      cx.notify();
    }
  }

  pub fn activate_cell(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    let located = self.board.sheets.iter().find_map(|sheet| {
      sheet
        .cells
        .iter()
        .find(|cell| cell.id == cell_id)
        .map(|cell| (sheet.id, cell.summary.uses_summary_projection))
    });
    if let Some((sheet_id, uses_summary_projection)) = located {
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
      self.scroll_cell_into_view(cell_id);
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

  /// Mint + place a new cell in one intent; on success the new cell becomes
  /// active with an editor attached.
  fn add_cell(&mut self, sheet_id: SheetId, placement: CellPlacement, cx: &mut Context<Self>) -> Option<CellId> {
    let cell_id = uuid::Uuid::new_v4();
    self
      .apply_intent(
        &FlowIntent::AddCell {
          sheet_id,
          cell_id,
          placement,
          seed: CellSeed::Empty,
        },
        cx,
      )
      .ok()?;
    self.collapsed_outline_items.insert(cell_id);
    self.ensure_cell_editor(cell_id, cx);
    self.changed(Some(cell_id), cx);
    Some(cell_id)
  }

  pub fn add_response(&mut self, cx: &mut Context<Self>) {
    let Some((sheet, cell)) = self.active_sheet.zip(self.active_cell) else {
      return;
    };
    self.add_cell(sheet, CellPlacement::LastChildOf(cell), cx);
  }

  pub fn add_first_response_to(&mut self, cell: CellId, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    self.add_cell(sheet, CellPlacement::FirstChildOf(cell), cx);
  }

  pub fn active_cell_is_struck(&self) -> bool {
    self
      .active_sheet
      .zip(self.active_cell)
      .and_then(|(sheet, cell)| {
        self
          .board
          .sheets
          .iter()
          .find(|candidate| candidate.id == sheet)?
          .cells
          .iter()
          .find(|candidate| candidate.id == cell)
      })
      .is_some_and(|cell| cell.summary.struck)
  }

  pub fn add_first_argument(&mut self, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    self.add_cell(sheet, CellPlacement::ColumnTop { column_index: 0 }, cx);
  }

  pub fn add_orphan_at_column_top(&mut self, column_index: usize, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    self.add_cell(sheet, CellPlacement::ColumnTop { column_index }, cx);
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
      self.dirty = true;
      cx.emit(FlowEditorEvent::Changed);
      cx.emit(FlowEditorEvent::ActiveSheetChanged(Some(sheet_id)));
      cx.notify();
    }
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
    // MoveSheet is identity-anchored: land immediately before `before`
    // (`None` = end). Moving down must anchor past the displaced neighbor.
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

  fn scroll_cell_into_view(&mut self, cell_id: CellId) {
    let Some(cell) = self.cell_bounds.get(&cell_id) else {
      return;
    };
    let viewport = self.board_scroll.bounds();
    let mut offset = self.board_scroll.offset();
    if cell.left() < viewport.left() {
      offset.x += viewport.left() - cell.left();
    } else if cell.right() > viewport.right() {
      offset.x -= cell.right() - viewport.right();
    }
    if cell.top() < viewport.top() {
      offset.y += viewport.top() - cell.top();
    } else if cell.bottom() > viewport.bottom() {
      offset.y -= cell.bottom() - viewport.bottom();
    }
    self.set_user_scroll_offset(offset);
  }

  fn cell_text_color(&self, cell_id: CellId, cx: &App) -> gpui::Hsla {
    self
      .board
      .sheets
      .iter()
      .find_map(|sheet| {
        let cell = sheet.cells.iter().find(|cell| cell.id == cell_id)?;
        let definition = self.board.format.sheet_type(sheet.sheet_type_id)?;
        let column = definition
          .columns
          .iter()
          .find(|column| column.id == cell.column_id)?;
        Some(flow_side_palette(column.side, cx).base)
      })
      .unwrap_or(cx.theme().foreground)
  }

  fn begin_pan(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
    self.pan_drag = Some(PanDragState {
      pointer_anchor: position,
      scroll_anchor: self.board_scroll.offset(),
      pending_position: position,
      frame_scheduled: false,
    });
    cx.notify();
  }

  fn queue_pan(&mut self, position: gpui::Point<gpui::Pixels>, window: &mut Window, cx: &mut Context<Self>) {
    let Some(pan_drag) = self.pan_drag.as_mut() else {
      return;
    };
    pan_drag.pending_position = position;
    if pan_drag.frame_scheduled {
      return;
    }
    pan_drag.frame_scheduled = true;
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

  pub fn add_sibling(&mut self, position: RelativePosition, cx: &mut Context<Self>) {
    let Some((sheet, cell)) = self.active_sheet.zip(self.active_cell) else {
      return;
    };
    let placement = match position {
      RelativePosition::Before => CellPlacement::Before(cell),
      RelativePosition::After => CellPlacement::After(cell),
    };
    self.add_cell(sheet, placement, cx);
  }

  pub fn active_cell_is_empty(&self) -> bool {
    self
      .active_sheet
      .zip(self.active_cell)
      .and_then(|(sheet_id, cell_id)| {
        self
          .board
          .sheets
          .iter()
          .find(|sheet| sheet.id == sheet_id)?
          .cells
          .iter()
          .find(|cell| cell.id == cell_id)
      })
      .is_some_and(|cell| cell.summary.is_empty)
  }

  /// W2: the set an op applies to — the multi-selection when one exists,
  /// else the active cell alone. Returned in sheet order.
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
          .cells
          .iter()
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

  /// W2 shift-click: toggle a cell in the multi-selection (the active cell
  /// joins the set the first time a second cell is added).
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

  /// W2 double-click on a parent: select its whole thread.
  pub fn select_thread(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    let Some(sheet) = self
      .active_sheet
      .and_then(|sheet_id| self.board.sheets.iter().find(|sheet| sheet.id == sheet_id))
    else {
      return;
    };
    self.selected_cells = flowstate_flow::board_ops::subtree_cell_ids(sheet, cell_id)
      .into_iter()
      .collect();
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

  pub fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else {
      return;
    };
    let set = self.operation_set(sheet_id);
    if set.len() > 1 {
      self.grouped(set.len(), |editor| {
        for cell_id in set {
          let _ = editor.apply_intent(&FlowIntent::DeleteCell { sheet_id, cell_id }, cx);
        }
      });
      self.selected_cells.clear();
      self.changed(None, cx);
      return;
    }
    let Some(cell_id) = self.active_cell else {
      return;
    };
    let fallback = self
      .board
      .sheets
      .iter()
      .find(|sheet| sheet.id == sheet_id)
      .and_then(|sheet| {
        let column_ids: Vec<_> = self
          .board
          .format
          .sheet_type(sheet.sheet_type_id)?
          .columns
          .iter()
          .map(|column| column.id)
          .collect();
        flowstate_flow::board_ops::deletion_fallback(sheet, &column_ids, cell_id)
      });
    if self
      .apply_intent(&FlowIntent::DeleteCell { sheet_id, cell_id }, cx)
      .is_ok()
    {
      if let Some(fallback) = fallback {
        self.ensure_cell_editor(fallback, cx);
        self.changed(Some(fallback), cx);
        if let Some(editor) = self.cell_editors.get(&fallback) {
          editor.read(cx).focus_handle(cx).focus(window);
        }
        self.scroll_cell_into_view(fallback);
      } else {
        self.changed(None, cx);
      }
    }
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
        .board
        .sheets
        .iter()
        .find(|sheet| sheet.id == sheet_id)
        .and_then(|sheet| sheet.cells.iter().find(|cell| cell.id == *cell_id))
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

  fn changed(&mut self, active_cell: Option<CellId>, cx: &mut Context<Self>) {
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
        .spawn(async move { io.save_to(path).await.map_err(std::io::Error::other) })
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

  pub fn discard_recovery_file(&mut self) {}

  pub fn resolve_pending(&mut self, _cx: &mut Context<Self>) {}

  fn render_sheet(&self, sheet_id: SheetId, cx: &mut Context<Self>) -> AnyElement {
    let Some(real_sheet) = self.board.sheets.iter().find(|sheet| sheet.id == sheet_id) else {
      return div().child("Select a sheet").into_any_element();
    };
    // §3.1: the live make-room reflow IS the preview — while a drag has a
    // pending landing, render the board AS IF the move had committed (the
    // dragged thread appears translucent at its previewed slot). Slot CAPTURE
    // runs against the baseline frozen at drag start, so the reflow never
    // moves a drop target out from under the pointer.
    let Some(definition) = self.board.format.sheet_type(real_sheet.sheet_type_id) else {
      return div().child("Invalid sheet type").into_any_element();
    };
    let preview_sheet: Option<flowstate_flow::Sheet> = self
      .dragging_cell
      .zip(self.pending_cell_drop)
      .and_then(|(dragged, intent)| {
        let column_ids: Vec<_> = definition.columns.iter().map(|column| column.id).collect();
        flowstate_flow::board_ops::preview_move_cell_subtree(real_sheet, &column_ids, dragged, intent)
      });
    let sheet = preview_sheet.as_ref().unwrap_or(real_sheet);
    let active = self.active_cell;
    let strokes: Vec<_> = if !self.hidden_annotation_sheets.contains(&sheet_id) {
      sheet
        .annotations
        .iter()
        .filter(|stroke| {
          !self
            .hidden_annotation_originators
            .contains(&stroke.originator)
        })
        .cloned()
        .collect()
    } else {
      Vec::new()
    };
    let draft = self.drawing_points.clone();
    let client_document_theme = load_document_theme();
    let zoom = self.board_zoom;
    let control_size = px(20.0 * zoom);
    let control_icon_size = px(12.0 * zoom);
    let cell_layout = sheet_cell_layout(sheet, &self.cell_measurements, zoom);
    let board_width = px((32.0 + definition.columns.len() as f32 * 280.0 + definition.columns.len().saturating_sub(1) as f32 * 16.0) * zoom);
    let weak_editor = cx.entity().downgrade();
    let weak_connector_editor = weak_editor.clone();
    let mut children_by_parent: std::collections::HashMap<CellId, Vec<CellId>> = std::collections::HashMap::new();
    for cell in &sheet.cells {
      if let Some(parent) = cell.parent_id {
        children_by_parent.entry(parent).or_default().push(cell.id);
      }
    }
    let connector_families = sheet
      .cells
      .iter()
      .filter_map(|parent| {
        children_by_parent
          .remove(&parent.id)
          .map(|children| (parent.id, children))
      })
      .collect::<Vec<_>>();
    div()
      .id("flow-columns")
      .relative()
      .min_w_full()
      .min_h_full()
      .w(board_width)
      .flex()
      .gap(px(16.0 * zoom))
      .p(px(16.0 * zoom))
      // THE slot-capture handler (§3.1): pointer → column (full-height x
      // capture) → frozen-baseline slot → meaning set + pads. One handler,
      // board-wide — per-cell drop zones are gone.
      .on_drag_move(cx.listener(move |editor, event: &DragMoveEvent<FlowCellDrag>, window, cx| {
        let bounds = event.bounds;
        let position = event.event.position;
        if !bounds.contains(&position) {
          return;
        }
        editor.update_drag_autoscroll(position, window, cx);
        editor.update_drag_slot(bounds, position, cx);
      }))
      .on_drop(cx.listener(|editor, drag: &FlowCellDrag, _, cx| editor.finish_cell_drop(drag.cell_id, cx)))
      .children(definition.columns.iter().enumerate().map(|(column_index, column)| {
        let side_palette = flow_side_palette(column.side, cx);
        let side_color = side_palette.base;
        let can_receive_child = column_index + 1 < definition.columns.len();
        let add_editor = cx.entity().clone();
        div()
          .w(px(280.0 * zoom))
          .flex_none()
          .flex_col()
          // Part 4: column wash — the side's identity at ambient strength.
          .bg(side_color.opacity(0.035))
          .rounded(px(9.0 * zoom))
          .child(
            div()
              .flex()
              .items_center()
              .justify_between()
              .font_weight(gpui::FontWeight::BOLD)
              .text_size(px(14.0 * zoom))
              .text_color(side_color)
              .border_b(px(2.0 * zoom))
              .border_color(side_color)
              .child(column.label.clone())
              .child(
                div()
                  .flex()
                  .gap(px(4.0 * zoom))
                  .child(
                    Button::new(("flow-send-to-document", column_index))
                      .with_size(control_size)
                      .ghost()
                      .tooltip("Send to document")
                      .child(Icon::default().path("icons/file-input.svg").with_size(control_icon_size))
                      .on_click(|_, _, _| {}),
                  )
                  .child(
                    Button::new(("flow-add-column-orphan", column_index))
                      .with_size(control_size)
                      .ghost()
                      .tooltip("Add orphan at top")
                      .icon(IconName::Plus)
                      .on_click(move |_, window, cx| {
                        add_editor.update(cx, |editor, cx| {
                          editor.set_annotation_tool(AnnotationTool::None, cx);
                          editor.add_orphan_at_column_top(column_index, cx);
                          editor.focus_active_cell(window, cx);
                        });
                      }),
                  ),
              ),
          )
          .child(div().h(px(12.0 * zoom)).flex_none())
          .children({
            // Render this column's cells in vertical (layout `top`) order, not sheet order. A cell's
            // vertical band comes from the family tree, so after some moves sheet order diverges from
            // top order; iterating in sheet order with push-down-only spacers would stack cells wrong
            // (e.g. a first child's subtree rendering *below* a later sibling's).
            let mut column_cells: Vec<_> = sheet.cells.iter().filter(|cell| cell.column_id == column.id).collect();
            column_cells.sort_by(|a, b| {
              let top = |id| cell_layout.get(id).map(|layout| layout.top).unwrap_or(0.0);
              top(&a.id).partial_cmp(&top(&b.id)).unwrap_or(std::cmp::Ordering::Equal)
            });

            // §3.1 F3: a refused move SHAKES the named card (decaying
            // oscillation) while the toast states the reason.
            let shake = self.refusal.as_ref().and_then(|notice| {
              let age = notice.at.elapsed().as_secs_f32();
              (age < 0.45).then_some(())?;
              notice.cell.map(|cell| (cell, 6.0 * (age * 35.0).sin() * (1.0 - age / 0.45)))
            });
            let mut previous_bottom = 0.0;
            let mut elements: Vec<AnyElement> = column_cells.into_iter().map(|cell| {
            let id = cell.id;
            // The previewed thread renders translucent at its landing slot —
            // the make-room reflow IS the preview (§3.1).
            let is_ghost = self.drag_subtree_contains(id) || self.dragging_cell == Some(id);
            let layout = cell_layout.get(&id).copied().unwrap_or_default();
            let cell_top = layout.top;
            let spacer_height = px((cell_top - previous_bottom).max(0.0));
            previous_bottom = cell_top + layout.height;
            let label: SharedString = cell.summary.summary_text.to_string().into();
            let mut uses_summary_projection = cell.summary.uses_summary_projection;
            let mut rendered_document = self.cell_document(cell.id);
            if let Some(document) = rendered_document.as_mut() {
              if !uses_summary_projection {
                let restyled = {
                  let mut paragraphs = document.paragraphs.make_mut();
                  if let Some(paragraph) = paragraphs.first_mut() {
                    paragraph.style = flowstate_document::PARAGRAPH_TAG;
                    true
                  } else {
                    false
                  }
                };
                if restyled {
                  document.blocks = flowstate_document::BlockSeq::from_vec(flowstate_document::paragraph_blocks_from_paragraphs(&document.paragraphs.to_vec()));
                  uses_summary_projection = true;
                }
              }
              apply_flow_cell_theme(document, &client_document_theme, side_color, cx.theme().background, self.board_zoom);
            }
            let cell_editor = self.cell_editors.get(&id).cloned();
            let reply_editor = cx.entity().clone();
            div()
              .w_full()
              .flex_col()
              .when(spacer_height > px(0.0), |this| this.child(div().h(spacer_height).flex_none()))
              .on_children_prepainted({
                let weak_editor = weak_editor.clone();
                move |bounds, _, cx| {
                  if let Some(card_bounds) = bounds.last().copied() {
                    let _ = weak_editor.update(cx, |editor, cx| editor.set_cell_bounds(id, card_bounds, cx));
                  }
                }
              })
              .child({
              // Part 4 visual system: soft-fill borderless card composited at
              // paint time; contrast-guard hairline; light-theme shadow /
              // dark-theme fill lift. Active/selected read as accent rings.
              let visuals = crate::flow::cell_theme::flow_card_visuals(
                side_color,
                cx.theme().background,
                cx.theme().foreground,
                cx.theme().border,
                cx.theme().is_dark(),
                if active == Some(id) { 0.08 } else { 0.0 },
              );
              let replug_candidate = self.replug_cell.is_some() && self.replug_candidate(id);
              let ring = if replug_candidate {
                // G5: valid re-plug parents HIGHLIGHT while the plug rides.
                Some(cx.theme().primary)
              } else if active == Some(id) {
                Some(side_palette.active)
              } else if self.selected_cells.contains(&id) {
                Some(side_palette.base.opacity(0.7))
              } else {
                visuals.hairline
              };
              // F3: while a drag is live, targets that cannot accept the drop
              // (the dragged thread itself) DIM instead of silently refusing.
              let drag_dimmed = (!is_ghost
                && self.dragging_cell.is_some_and(|dragged| {
                  dragged != id && flowstate_flow::board_ops::is_descendant_of(sheet, id, dragged)
                }))
                || (self.replug_cell.is_some_and(|replug| replug != id) && !replug_candidate);
              let shake_offset = shake.and_then(|(cell, offset)| (cell == id).then_some(offset));
              div()
                .id(("flow-cell", id.as_u128() as u64))
                .relative()
                .w_full()
                .min_h(px(54.0 * zoom))
                .p(px(10.0 * zoom))
                .rounded(px(9.0 * zoom))
                .bg(visuals.fill)
                .when_some(ring, |this, color| this.border(px(1.0)).border_color(color))
                .when(visuals.shadow, |this| {
                  this.shadow(vec![gpui::BoxShadow {
                    color: gpui::hsla(0.0, 0.0, 0.0, 0.12),
                    offset: point(px(0.0), px(2.0)),
                    blur_radius: px(6.0),
                    spread_radius: px(0.0),
                  }])
                })
                .when(drag_dimmed, |this| this.opacity(0.45))
                .when_some(shake_offset, |this, offset| this.ml(px(offset)))
                .hover(|style| style.bg(side_palette.hover.opacity(0.12)))
                .cursor_pointer()
                .when(active != Some(id) && !is_ghost, |this| {
                  // The whole card (except the one being edited) is a drag handle; a plain click still
                  // selects because GPUI only starts a drag past a small movement threshold.
                  let weak = weak_editor.clone();
                  let drag_label = label.clone();
                  this.on_drag(FlowCellDrag { cell_id: id }, move |drag, _, _, cx| {
                    let _ = weak.update(cx, |editor, cx| editor.begin_cell_drag(drag.cell_id, cx));
                    let census = weak.update(cx, |editor, _| editor.drag_census(drag.cell_id)).unwrap_or(0);
                    let preview = cx.new(|_| FlowCellDragPreview {
                      label: drag_label.clone(),
                      census,
                      meaning: SharedString::default(),
                    });
                    let _ = weak.update(cx, |editor, _| editor.set_drag_preview_entity(preview.downgrade()));
                    preview
                  })
                })
                .on_mouse_down(MouseButton::Left, cx.listener(|editor, _, _, cx| {
                  if editor.annotation_tool != AnnotationTool::None {
                    editor.set_annotation_tool(AnnotationTool::None, cx);
                  }
                  cx.stop_propagation();
                }))
                .on_click(cx.listener(move |editor, event: &gpui::ClickEvent, window, cx| {
                  // W2: shift-click grows the multi-selection; double-click a
                  // parent selects its whole thread; a plain click collapses
                  // the set back to a single active cell.
                  if event.modifiers().shift {
                    editor.toggle_select_cell(id, cx);
                    return;
                  }
                  if event.click_count() >= 2 {
                    editor.select_thread(id, cx);
                    editor.activate_cell(id, cx);
                    return;
                  }
                  editor.clear_selection(cx);
                  editor.activate_cell(id, cx);
                  if let Some(cell_editor) = editor.cell_editors.get(&id) {
                    cell_editor.read(cx).focus_handle(cx).focus(window);
                  }
                }))
                // §3.1: cells carry NO drop zones — the board-level slot
                // capture owns landing semantics; the previewed thread renders
                // as a translucent in-slot ghost.
                .when(is_ghost, |this| this.opacity(0.5).border_1().border_dashed().border_color(side_palette.active))
                // G5: the fat plug — a parented card's incoming wire ends in a
                // draggable grip at its left edge; drag it to a highlighted
                // candidate to re-plug the thread.
                .when(cell.parent_id.is_some(), |this| {
                  let weak = weak_editor.clone();
                  this.child(
                    div()
                      .id(("flow-wire-plug", id.as_u128() as u64))
                      .absolute()
                      .left(px(-7.0 * zoom))
                      .top_1_2()
                      .size(px(11.0 * zoom))
                      .rounded_full()
                      .bg(cx.theme().muted_foreground.opacity(if active == Some(id) { 0.9 } else { 0.45 }))
                      .hover(|style| style.bg(cx.theme().primary))
                      .cursor_grab()
                      .on_drag(WirePlugDrag { cell_id: id }, move |drag, _, _, cx| {
                        let _ = weak.update(cx, |editor, cx| editor.begin_wire_replug(drag.cell_id, cx));
                        cx.new(|_| WirePlugPreview)
                      }),
                  )
                })
                .on_drop(cx.listener(move |editor, drag: &WirePlugDrag, _, cx| {
                  editor.finish_wire_replug(drag.cell_id, id, cx);
                  cx.stop_propagation();
                }))
                .child(
                  div()
                    .id(("flow-cell-drag-handle", id.as_u128() as u64))
                    .absolute()
                    .top(px(2.0 * zoom))
                    .right(px(4.0 * zoom))
                    .text_size(px(14.0 * zoom))
                    .cursor_move()
                    .text_color(cx.theme().muted_foreground)
                    .child("⠿")
                    .on_mouse_down(MouseButton::Left, cx.listener(move |editor, _, _, cx| editor.activate_cell(id, cx)))
                    .on_drag(FlowCellDrag { cell_id: id }, {
                      let weak = weak_editor.clone();
                      let drag_label = label.clone();
                      move |drag, _, _, cx| {
                        let _ = weak.update(cx, |editor, cx| editor.begin_cell_drag(drag.cell_id, cx));
                        let census = weak.update(cx, |editor, _| editor.drag_census(drag.cell_id)).unwrap_or(0);
                        let preview = cx.new(|_| FlowCellDragPreview {
                          label: drag_label.clone(),
                          census,
                          meaning: SharedString::default(),
                        });
                        let _ = weak.update(cx, |editor, _| editor.set_drag_preview_entity(preview.downgrade()));
                        preview
                      }
                    }),
                )
                .when(can_receive_child, |this| {
                  this.child(
                    Button::new(("flow-cell-reply", id.as_u128() as u64))
                      .absolute()
                      .top(px(24.0 * zoom))
                      .right(px(2.0 * zoom))
                      .with_size(control_size)
                      .ghost()
                      .tooltip("Add first reply")
                      .child(Icon::default().path("icons/message-square-reply.svg").with_size(control_icon_size))
                      .on_click(move |_, window, cx| {
                        cx.stop_propagation();
                        reply_editor.update(cx, |editor, cx| {
                          editor.set_annotation_tool(AnnotationTool::None, cx);
                          editor.add_first_response_to(id, cx);
                          editor.focus_active_cell(window, cx);
                        });
                      }),
                  )
                })
                .child(if active == Some(id) {
                  cell_editor.map_or_else(|| div().child(label).into_any_element(), |editor| editor.into_any_element())
                } else {
                  rendered_document.map_or_else(
                    || div().child(label).into_any_element(),
                    |document| RichTextDocumentElement::new(document).with_invisibility_mode(uses_summary_projection).into_any_element(),
                  )
                })
            })
            .into_any_element()
          }).collect::<Vec<AnyElement>>();
            elements
          })
      }))
      .child(
        canvas(
          |_, _, _| {},
          move |_canvas_bounds, _, window, cx| {
            let Some(editor) = weak_connector_editor.upgrade() else {
              return;
            };
            let editor_read = editor.read(cx);
            let connector_bounds = &editor_read.cell_bounds;
            let focused_cell = editor_read.active_cell;
            for (parent_id, child_ids) in &connector_families {
              let Some(parent) = connector_bounds.get(parent_id) else {
                continue;
              };
              let children = child_ids
                .iter()
                .filter_map(|id| connector_bounds.get(id).copied())
                .collect::<Vec<_>>();
              // Part 4 wires: always-on muted (~40%); the focused cell's
              // family thickens with an accent (spec G5 hover/focus).
              let focused = focused_cell.is_some_and(|cell| cell == *parent_id || child_ids.contains(&cell));
              let (color, emphasis) = if focused {
                (cx.theme().primary.opacity(0.75), 1.15)
              } else {
                (cx.theme().muted_foreground.opacity(0.4), 0.55)
              };
              paint_connector_family(*parent, &children, color, emphasis, window);
            }
          },
        )
        .absolute()
        .inset_0(),
      )
      // R1/R5 landing pads: unfold at the gap when a spot plausibly means
      // more than one parentage. gpui always paints the cursor ghost last, so
      // the row is OFFSET below the grab point (and the ghost is translucent)
      // to honor the never-obscure-the-options law.
      .when_some(
        self
          .drag
          .as_ref()
          .filter(|drag| !drag.pads.is_empty())
          .and_then(|drag| drag.pad_origin.map(|origin| (drag.pads.clone(), origin, drag.hovered_pad))),
        |this, (pads, origin, hovered)| {
          let local_x = px(origin.x.as_f32() - self.viewport_origin.x);
          let local_y = px(origin.y.as_f32() - self.viewport_origin.y + 34.0);
          this.child(
            div()
              .absolute()
              .left(local_x)
              .top(local_y)
              .flex()
              .gap_1()
              .children(pads.into_iter().enumerate().map(|(pad_ix, pad)| {
                let selected = hovered == Some(pad_ix);
                div()
                  .id(("flow-pad", pad_ix))
                  .px_2()
                  .py_1()
                  .rounded_full()
                  .bg(if selected {
                    cx.theme().primary
                  } else {
                    cx.theme().popover.opacity(0.95)
                  })
                  .text_color(if selected {
                    cx.theme().primary_foreground
                  } else {
                    cx.theme().popover_foreground
                  })
                  .border_1()
                  .border_color(cx.theme().primary.opacity(0.6))
                  .text_size(px(12.0))
                  .whitespace_nowrap()
                  .child(pad.label.clone())
                  .on_drag_move(cx.listener(move |editor, event: &DragMoveEvent<FlowCellDrag>, _, cx| {
                    if event.bounds.contains(&event.event.position) {
                      editor.hover_pad(pad_ix, event.bounds, cx);
                    }
                  }))
                  .on_drop(cx.listener(|editor, drag: &FlowCellDrag, _, cx| {
                    editor.finish_cell_drop(drag.cell_id, cx);
                    cx.stop_propagation();
                  }))
              })),
          )
        },
      )
      .child(
        div()
          .id("flow-annotation-layer")
          .absolute()
          .inset_0()
          .child(
            canvas(
              |_, _, _| {},
              move |bounds, _, window, _| {
                for stroke in &strokes {
                  let color = gpui::Hsla::from(rgba(stroke.style.color_rgba));
                  paint_stroke(
                    bounds.origin,
                    &stroke.points,
                    px(stroke.style.width * zoom),
                    color.opacity(color.a * stroke.style.opacity),
                    zoom,
                    window,
                  );
                }
                if !draft.is_empty() {
                  paint_stroke(bounds.origin, &draft, px(4.0 * zoom), gpui::Hsla::from(rgba(0xf59e_0bff)).opacity(0.55), zoom, window);
                }
              },
            )
            .absolute()
            .size_full(),
          ),
      )
      .into_any_element()
  }
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
    if !cx.has_active_drag() && self.replug_cell.is_some() {
      self.replug_cell = None;
    }
    if !cx.has_active_drag() && (self.pending_cell_drop.is_some() || self.dragging_cell.is_some()) {
      self.pending_cell_drop = None;
      self.dragging_cell = None;
      self.drag = None;
      self.drag_autoscroll = None;
      // A drag that ended without an accepted drop (released over empty space) still flushes, tagged
      // as an uncommitted attempt.
      self.finish_drag_log(None, false);
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
    div()
      .id("flow-editor")
      .relative()
      .size_full()
      .track_focus(&self.focus_handle)
      .on_key_down(cx.listener(|editor, event: &KeyDownEvent, window, cx| {
        // Only arm panning when the board itself holds focus. Otherwise a space typed inside a focused
        // cell editor would silently arm a pan (and a later click would pan instead of act).
        if event.keystroke.key == "space" && editor.focus_handle.is_focused(window) {
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
      .on_mouse_move(cx.listener(|editor, event: &MouseMoveEvent, window, cx| editor.queue_pan(event.position, window, cx)))
      .on_mouse_up(
        MouseButton::Left,
        cx.listener(|editor, _: &MouseUpEvent, _, cx| editor.finish_space_pan(cx)),
      )
      .child(
        canvas(
          |_, _, _| {},
          move |bounds, _, window, cx| {
            let spacing = px(24.0 * board_zoom);
            let scale = window.scale_factor();
            let (dot_size, dot_opacity) = grid_dot_metrics(board_zoom, scale);
            let offset = grid_scroll.offset();
            let mut x = offset.x % spacing;
            let color = cx.theme().border.opacity(0.56 * dot_opacity);
            let snap = |value: gpui::Pixels| px((value.as_f32() * scale).round() / scale);
            while x < bounds.size.width {
              let mut y = offset.y % spacing;
              while y < bounds.size.height {
                window.paint_quad(gpui::fill(
                  gpui::Bounds::new(bounds.origin + point(snap(x), snap(y)), gpui::size(dot_size, dot_size)),
                  color,
                ));
                y += spacing;
              }
              x += spacing;
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
          .cursor(if self.pan_drag.is_some() {
            gpui::CursorStyle::ClosedHand
          } else {
            gpui::CursorStyle::OpenHand
          })
          .when(self.annotation_tool == AnnotationTool::None && !self.space_pan_armed, |this| {
            this.on_mouse_down(
              MouseButton::Left,
              cx.listener(|editor, event: &MouseDownEvent, _, cx| {
                editor.begin_pan(event.position, cx);
                cx.stop_propagation();
              }),
            )
          })
          .on_scroll_wheel(cx.listener(|editor, event: &ScrollWheelEvent, window, cx| {
            if event.modifiers.shift {
              let delta = event.delta.pixel_delta(window.line_height());
              let mut offset = editor.board_scroll.offset();
              offset.x += delta.y + delta.x;
              editor.set_user_scroll_offset(offset);
              cx.stop_propagation();
              cx.notify();
            } else if !event.modifiers.control {
              editor.camera_center = None;
              cx.on_next_frame(window, |editor, _, _| editor.sync_camera_center_from_scroll());
            }
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
            .size_full(),
          )
          .child(match self.active_sheet {
            Some(sheet) => self.render_sheet(sheet, cx).into_any_element(),
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
      .child(
        div()
          .absolute()
          .inset_0()
          .child(Scrollbar::vertical(&self.board_scroll).scrollbar_show(ScrollbarShow::Always))
          .child(Scrollbar::horizontal(&self.board_scroll).scrollbar_show(ScrollbarShow::Always)),
      )
      // FLOWSTATE_INTENT_LOG: the alpha-calibration overlay — every gesture's
      // committed intent, newest last.
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
      // §3.1 F3: the refusal SPEAKS — a transient toast naming the reason,
      // fading out over its last half-second.
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
