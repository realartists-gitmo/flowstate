use std::collections::HashSet;
use std::path::PathBuf;

use std::sync::Arc;

use flowstate_collab::flow::{FlowDocHandle, FlowStreamItem, FlowUndoMeta};
use flowstate_flow::intents::{CellPlacement, CellSeed};
use flowstate_flow::{
  AnnotationOriginator, BoardPoint, CellId, FlowBoardProjection, FlowDropIntent, FlowIntent, RelativePosition, SheetId, board_ops,
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
mod layout;
mod telemetry;
mod zoom;

use annotation::paint_stroke;
use connector::paint_connector_family;
use drag_drop::{DropEdge, FlowCellDrag, FlowCellDragPreview};
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

struct PanDragState {
  pointer_anchor: gpui::Point<gpui::Pixels>,
  scroll_anchor: gpui::Point<gpui::Pixels>,
  pending_position: gpui::Point<gpui::Pixels>,
  frame_scheduled: bool,
}

pub struct FlowEditor {
  /// The ONE local write path (invariant 5): identical handle for solo and
  /// collaborative tabs; the board copy advances only via the ordered stream.
  handle: Arc<FlowDocHandle>,
  board: FlowBoardProjection,
  /// Render cache for INACTIVE cells' rich-text previews (materialized on
  /// demand, dropped on any board change / remote sync).
  cell_documents: std::collections::HashMap<CellId, flowstate_document::DocumentProjection>,
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
  pub fn new_with_path(handle: Arc<FlowDocHandle>, path: Option<PathBuf>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
    let board = handle.board_projection().unwrap_or(FlowBoardProjection {
      format: flowstate_flow::FlowFormat::policy_debate(),
      sheets: Vec::new(),
    });
    let active_sheet = board.sheets.first().map(|sheet| sheet.id);
    let collapsed_outline_items = board
      .sheets
      .iter()
      .flat_map(|sheet| std::iter::once(sheet.id).chain(sheet.cells.iter().map(|cell| cell.id)))
      .collect();
    Self {
      handle,
      board,
      cell_documents: std::collections::HashMap::new(),
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
    let (handle, _gate) =
      FlowDocHandle::new_document(&flowstate_flow::FlowFormat::policy_debate()).expect("fresh flow document");
    Self::new_with_path(handle, None, window, cx)
  }

  pub fn handle(&self) -> &Arc<FlowDocHandle> {
    &self.handle
  }

  pub fn board(&self) -> &FlowBoardProjection {
    &self.board
  }

  /// Drain the ordered board stream and adopt the latest Replace — the ONE
  /// way this editor's board copy advances. Local intents call it right after
  /// `apply`; the collaboration session calls it as a pump when remote
  /// batches land; solo tabs need no pump.
  pub fn sync_board_from_handle(&mut self, cx: &mut Context<Self>) {
    let Ok(items) = self.handle.drain_board_stream() else {
      return;
    };
    let Some(FlowStreamItem::Board(board)) = items.into_iter().next_back() else {
      return;
    };
    self.board = *board;
    self.cell_documents.clear();
    self.prune_dead_cell_state(cx);
    if let Some(active) = self.active_sheet
      && self.board.sheet(active).is_none()
    {
      self.active_sheet = self.board.sheets.first().map(|sheet| sheet.id);
      cx.emit(FlowEditorEvent::ActiveSheetChanged(self.active_sheet));
    }
    if let Some(active) = self.active_cell
      && self.board.cell(active).is_none()
    {
      self.active_cell = None;
      cx.emit(FlowEditorEvent::ActiveCellChanged(None));
    }
    cx.notify();
  }

  /// Apply one flow intent through the handle and integrate the outcome.
  fn apply_intent(&mut self, intent: FlowIntent, cx: &mut Context<Self>) -> bool {
    match self.handle.apply(intent) {
      Ok(outcome) => {
        self.sync_board_from_handle(cx);
        if outcome.changed {
          self.dirty = true;
          cx.emit(FlowEditorEvent::Changed);
          cx.notify();
        }
        outcome.changed
      },
      Err(rejection) => {
        tracing::warn!(%rejection, "flow intent rejected");
        false
      },
    }
  }

  /// Update the undo context riding future commits (focus restoration).
  fn refresh_undo_context(&self) {
    self.handle.set_undo_context(FlowUndoMeta {
      active_sheet: self.active_sheet,
      focused_cell: self.active_cell,
      head_cursor: None,
      anchor_cursor: None,
    });
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
    if self.board.sheet(sheet_id).is_some() {
      self.active_sheet = Some(sheet_id);
      self.active_cell = None;
      self.refresh_undo_context();
      cx.emit(FlowEditorEvent::ActiveSheetChanged(Some(sheet_id)));
      cx.notify();
    }
  }

  pub fn activate_cell(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    let sheet_id = self.board.cell(cell_id).map(|(sheet, _)| sheet.id);
    if let Some(sheet_id) = sheet_id {
      self.apply_intent(
        FlowIntent::EnsureCellEditable {
          sheet_id,
          cell_id,
        },
        cx,
      );
      self.ensure_cell_editor(cell_id, cx);
      self.active_sheet = Some(sheet_id);
      self.active_cell = Some(cell_id);
      self.refresh_undo_context();
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

  pub fn add_response(&mut self, cx: &mut Context<Self>) {
    let Some((sheet, cell)) = self.active_sheet.zip(self.active_cell) else {
      return;
    };
    self.add_cell_with_placement(sheet, CellPlacement::ResponseTo { parent: cell }, cx);
  }

  pub fn add_first_response_to(&mut self, cell: CellId, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    self.add_cell_with_placement(sheet, CellPlacement::FirstResponseTo { parent: cell }, cx);
  }

  /// The one AddCell path behind every add_* entry point.
  fn add_cell_with_placement(&mut self, sheet_id: SheetId, placement: CellPlacement, cx: &mut Context<Self>) {
    let cell_id = uuid::Uuid::new_v4();
    if self.apply_intent(
      FlowIntent::AddCell {
        sheet_id,
        cell_id,
        placement,
        seed: CellSeed::Empty,
      },
      cx,
    ) {
      self.collapsed_outline_items.insert(cell_id);
      self.ensure_cell_editor(cell_id, cx);
      self.changed(Some(cell_id), cx);
    }
  }

  pub fn active_cell_is_struck(&self) -> bool {
    self
      .active_cell
      .and_then(|cell| self.board.cell(cell))
      .is_some_and(|(_, cell)| cell.summary.struck)
  }

  pub fn add_first_argument(&mut self, cx: &mut Context<Self>) {
    self.add_orphan_at_column_top(0, cx);
  }

  pub fn add_orphan_at_column_top(&mut self, column_index: usize, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    self.add_cell_with_placement(sheet, CellPlacement::ColumnTop { column_index }, cx);
  }

  pub fn create_sheet(&mut self, cx: &mut Context<Self>) {
    self.create_sheet_of_type(0, cx);
  }

  pub fn create_sheet_of_type(&mut self, sheet_type_index: usize, cx: &mut Context<Self>) {
    let Some(sheet_type) = self.board.format.sheet_types.get(sheet_type_index) else {
      return;
    };
    let sheet_type_id = sheet_type.id;
    let name = format!("Sheet {}", self.board.sheets.len() + 1);
    let sheet_id = uuid::Uuid::new_v4();
    if self.apply_intent(
      FlowIntent::CreateSheet {
        sheet_id,
        name,
        sheet_type_id,
      },
      cx,
    ) {
      self.collapsed_outline_items.insert(sheet_id);
      self.active_sheet = Some(sheet_id);
      self.active_cell = None;
      self.refresh_undo_context();
      cx.emit(FlowEditorEvent::ActiveSheetChanged(Some(sheet_id)));
      cx.notify();
    }
  }

  pub fn rename_active_sheet(&mut self, name: impl Into<String>, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    if self.apply_intent(
      FlowIntent::RenameSheet {
        sheet_id: sheet,
        name: name.into(),
      },
      cx,
    ) {
      self.changed(self.active_cell, cx);
    }
  }

  pub fn delete_active_sheet(&mut self, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    if self.apply_intent(FlowIntent::DeleteSheet { sheet_id: sheet }, cx) {
      self.collapsed_outline_items.remove(&sheet);
      self.active_sheet = self.board.sheets.first().map(|sheet| sheet.id);
      self.active_cell = None;
      self.refresh_undo_context();
      cx.emit(FlowEditorEvent::ActiveSheetChanged(self.active_sheet));
      cx.emit(FlowEditorEvent::ActiveCellChanged(None));
      cx.notify();
    }
  }

  pub fn move_active_sheet(&mut self, direction: isize, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    let Some(index) = self.board.sheets.iter().position(|candidate| candidate.id == sheet) else {
      return;
    };
    let target = index
      .saturating_add_signed(direction)
      .min(self.board.sheets.len().saturating_sub(1));
    if target != index
      && self.apply_intent(
        FlowIntent::MoveSheet {
          sheet_id: sheet,
          target_index: target,
        },
        cx,
      )
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
      .cell(cell_id)
      .and_then(|(sheet, cell)| {
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
    self.add_cell_with_placement(sheet, CellPlacement::Sibling { of: cell, position }, cx);
  }

  pub fn active_cell_is_empty(&self) -> bool {
    self
      .active_cell
      .and_then(|cell| self.board.cell(cell))
      .is_some_and(|(_, cell)| cell.summary.is_empty)
  }

  pub fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some((sheet, cell)) = self.active_sheet.zip(self.active_cell) else {
      return;
    };
    let fallback = board_ops::deletion_fallback(&self.board, sheet, cell);
    if self.apply_intent(
      FlowIntent::DeleteCell {
        sheet_id: sheet,
        cell_id: cell,
      },
      cx,
    ) {
      self.drop_cell_editor(cell);
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
    let Some((sheet, cell)) = self.active_sheet.zip(self.active_cell) else {
      return;
    };
    let struck = self
      .board
      .cell(cell)
      .is_some_and(|(_, cell)| cell.summary.struck);
    if self.apply_intent(
      FlowIntent::SetCellStruck {
        sheet_id: sheet,
        cell_id: cell,
        struck: !struck,
      },
      cx,
    ) {
      self.changed(Some(cell), cx);
    }
  }

  pub fn can_undo(&self) -> bool {
    self.handle.can_undo().unwrap_or(false)
  }

  pub fn can_redo(&self) -> bool {
    self.handle.can_redo().unwrap_or(false)
  }

  pub fn undo(&mut self, cx: &mut Context<Self>) {
    if let Ok(outcome) = self.handle.undo()
      && outcome.applied
    {
      self.sync_board_from_handle(cx);
      self.restore_undo_focus(outcome.meta, cx);
      self.dirty = true;
      cx.emit(FlowEditorEvent::Changed);
      cx.notify();
    }
  }

  pub fn redo(&mut self, cx: &mut Context<Self>) {
    if let Ok(outcome) = self.handle.redo()
      && outcome.applied
    {
      self.sync_board_from_handle(cx);
      self.restore_undo_focus(outcome.meta, cx);
      self.dirty = true;
      cx.emit(FlowEditorEvent::Changed);
      cx.notify();
    }
  }

  /// `FlowUndoMeta` focus restoration: bring back the sheet/cell the undone
  /// commit was made under (cell-text selection restores through the cell's
  /// own authority).
  fn restore_undo_focus(&mut self, meta: Option<FlowUndoMeta>, cx: &mut Context<Self>) {
    let Some(meta) = meta else {
      return;
    };
    if let Some(sheet) = meta.active_sheet
      && self.board.sheet(sheet).is_some()
      && self.active_sheet != Some(sheet)
    {
      self.active_sheet = Some(sheet);
      cx.emit(FlowEditorEvent::ActiveSheetChanged(Some(sheet)));
    }
    if let Some(cell) = meta.focused_cell
      && self.board.cell(cell).is_some()
      && self.active_cell != Some(cell)
    {
      self.ensure_cell_editor(cell, cx);
      self.active_cell = Some(cell);
      cx.emit(FlowEditorEvent::ActiveCellChanged(Some(cell)));
    }
  }

  /// Drop every per-cell UI artifact for cells no longer on the board, and
  /// release their runtime streams.
  fn prune_dead_cell_state(&mut self, cx: &mut Context<Self>) {
    let live: HashSet<CellId> = self
      .board
      .sheets
      .iter()
      .flat_map(|sheet| sheet.cells.iter().map(|cell| cell.id))
      .collect();
    let dead: Vec<CellId> = self
      .cell_editors
      .keys()
      .copied()
      .filter(|id| !live.contains(id))
      .collect();
    for id in dead {
      self.drop_cell_editor(id);
    }
    self.cell_bounds.retain(|id, _| live.contains(id));
    self.cell_measurements.retain(|id, _| live.contains(id));
    self.cell_documents.retain(|id, _| live.contains(id));
    let _ = cx;
  }

  fn drop_cell_editor(&mut self, cell: CellId) {
    self.cell_editors.remove(&cell);
    self.cell_editor_themes.remove(&cell);
    self.cell_editor_subscriptions.remove(&cell);
    let _ = self.handle.close_cell(cell);
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
    // Fork under the gate (brief), encode + write on the background executor
    // (the I-9a long-export rule at editor scale).
    let handle = self.handle.clone();
    cx.spawn(async move |editor, cx| {
      let write_result = cx
        .background_executor()
        .spawn(async move {
          let snapshot = handle.snapshot().map_err(std::io::Error::other)?;
          flowstate_flow::write_fl0(&path, &snapshot).map_err(std::io::Error::other)
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

  pub fn discard_recovery_file(&mut self) {}

  pub fn resolve_pending(&mut self, _cx: &mut Context<Self>) {}

  fn render_sheet(&mut self, sheet_id: SheetId, cx: &mut Context<Self>) -> AnyElement {
    // Prefill the inactive-cell rich-text preview cache OUTSIDE the render
    // borrow (materialize-on-miss; dropped on any board change).
    let missing: Vec<CellId> = self
      .board
      .sheet(sheet_id)
      .map(|sheet| {
        sheet
          .cells
          .iter()
          .map(|cell| cell.id)
          .filter(|id| !self.cell_documents.contains_key(id))
          .collect()
      })
      .unwrap_or_default();
    for id in missing {
      if let Ok(document) = self.handle.cell_preview(id) {
        self.cell_documents.insert(id, document);
      }
    }
    let Some(real_sheet) = self.board.sheet(sheet_id) else {
      return div().child("Select a sheet").into_any_element();
    };
    // During a drag the layout stays stable (no reflow) so drop locations don't move under the pointer:
    // the dragged cell holds its slot as a faded placeholder, its children stay put, and the landing is
    // shown by a directional accent on the target cell. The real subtree move still happens on drop.
    let sheet = real_sheet;
    let drop_target = self.drag_drop_target(real_sheet);
    let Some(definition) = self.board.format.sheet_type(sheet.sheet_type_id) else {
      return div().child("Invalid sheet type").into_any_element();
    };
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
    // The drop preview marks the target with a thin, non-displacing landing bar. A full-card-height
    // gap used to be inserted here, which shoved the target — and everything below it — down by a
    // whole card; for a Before landing that slid the very cell you were aiming at out from under the
    // pointer (making fine placement feel like chasing a moving target, and silently handing the drop
    // off to the column handler at family boundaries). A slim bar keeps the pointer over its target.
    // Tuple: (column index, top in layout space, bar height).
    let column_gap: Option<(usize, f32, f32)> = drop_target.and_then(|(target, edge)| {
      let target_cell = sheet.cells.iter().find(|cell| cell.id == target)?;
      let target_layout = cell_layout.get(&target)?;
      let target_column = definition
        .columns
        .iter()
        .position(|column| column.id == target_cell.column_id)?;
      // Keep well under a card's half-height (min card is 54px) so opening the bar never displaces the
      // target far enough for the pointer to fall off it.
      let height = 5.0 * zoom;
      Some(match edge {
        DropEdge::Before => (target_column, target_layout.top, height),
        DropEdge::After => (target_column, target_layout.top + target_layout.height, height),
        DropEdge::Child => (target_column + 1, target_layout.top, height),
      })
    });
    let board_width = px((32.0 + definition.columns.len() as f32 * 280.0 + definition.columns.len().saturating_sub(1) as f32 * 16.0) * zoom);
    let column_count = definition.columns.len();
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
      // Board-level fallback so the landing preview never freezes in a dead zone. The per-column
      // handlers only act inside a column div, but the 16px inter-column flex gaps and the outer
      // padding lie outside every column, so a pointer crossing between columns would otherwise stop
      // updating the drop. This runs first (capture phase, parent-before-child), so a real column or
      // cell handler still overrides it whenever the pointer is genuinely inside one; it only "wins"
      // in the strips no column covers.
      .on_drag_move(cx.listener(move |editor, event: &DragMoveEvent<FlowCellDrag>, window, cx| {
        let bounds = event.bounds;
        let position = event.event.position;
        if !bounds.contains(&position) {
          return;
        }
        editor.update_drag_autoscroll(position, window, cx);
        if editor.cursor_over_live_cell(position) {
          return;
        }
        let zoom = editor.board_zoom;
        let column_width = 280.0 * zoom;
        let stride = (280.0 + 16.0) * zoom;
        let relative = (position.x - bounds.left()).as_f32() - 16.0 * zoom;
        if relative < 0.0 {
          editor.update_column_drop(0, position.y, cx);
          return;
        }
        let raw_index = (relative / stride).floor();
        let within = relative - raw_index * stride;
        let index = raw_index as usize;
        if within <= column_width && index < column_count {
          // Inside a real column — its own handler already covers this point, so don't fight it.
          return;
        }
        // In the gap after `index` (or the trailing padding) — snap to the nearest real column.
        editor.update_column_drop(index.min(column_count.saturating_sub(1)), position.y, cx);
      }))
      .children(definition.columns.iter().enumerate().map(|(column_index, column)| {
        let side_palette = flow_side_palette(column.side, cx);
        let side_color = side_palette.base;
        let can_receive_child = column_index + 1 < definition.columns.len();
        let add_editor = cx.entity().clone();
        div()
          .w(px(280.0 * zoom))
          .flex_none()
          .flex_col()
          .on_drag_move(cx.listener(move |editor, event: &DragMoveEvent<FlowCellDrag>, window, cx| {
            // Same as the cell handler: `on_drag_move` is not hit-tested, so gate on this column's own
            // bounds, and defer to the cell handler whenever the pointer is actually over a live cell.
            if !event.bounds.contains(&event.event.position) {
              return;
            }
            editor.update_drag_autoscroll(event.event.position, window, cx);
            if editor.cursor_over_live_cell(event.event.position) {
              return;
            }
            editor.update_column_drop(column_index, event.event.position.y, cx);
            if let Some(intent) = editor.pending_cell_drop {
              editor.log_drag_over_column(column_index, event.event.position, intent);
            }
          }))
          .on_drop(cx.listener(|editor, drag: &FlowCellDrag, _, cx| editor.finish_cell_drop(drag.cell_id, cx)))
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
            // Open the landing gap (if it belongs to this column) before the first cell at/after its
            // top, shifting the rest of the column down by the gap's height. Everything else stays put.
            let mut gap_pending = column_gap.filter(|(gap_column, _, _)| *gap_column == column_index).map(|(_, top, height)| (top, height));
            let mut previous_bottom = 0.0;
            let mut shift = 0.0;
            let mut elements: Vec<AnyElement> = column_cells.into_iter().map(|cell| {
            let id = cell.id;
            let is_ghost = self.dragging_cell == Some(id);
            let layout = cell_layout.get(&id).copied().unwrap_or_default();
            let gap_slot = if let Some((gap_top, gap_height)) = gap_pending
              && layout.top >= gap_top - 0.5
            {
              let gap_spacer = px((gap_top - previous_bottom).max(0.0));
              previous_bottom = gap_top + gap_height;
              shift = gap_height;
              gap_pending = None;
              Some((gap_spacer, px(gap_height)))
            } else {
              None
            };
            let cell_top = layout.top + shift;
            let spacer_height = px((cell_top - previous_bottom).max(0.0));
            previous_bottom = cell_top + layout.height;
            let label: SharedString = cell.summary.summary_text.to_string().into();
            let mut uses_summary_projection = cell.summary.uses_summary_projection;
            let mut rendered_document = self.cell_documents.get(&id).cloned();
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
              .when_some(gap_slot, |this, (gap_spacer, gap_height)| {
                this.child(div().h(gap_spacer).flex_none()).child(
                  div()
                    .w_full()
                    .h(gap_height)
                    .rounded(px(2.0 * zoom))
                    .bg(side_palette.active),
                )
              })
              .when(spacer_height > px(0.0), |this| this.child(div().h(spacer_height).flex_none()))
              .on_children_prepainted({
                let weak_editor = weak_editor.clone();
                move |bounds, _, cx| {
                  if let Some(card_bounds) = bounds.last().copied() {
                    let _ = weak_editor.update(cx, |editor, cx| editor.set_cell_bounds(id, card_bounds, cx));
                  }
                }
              })
              .child(
              div()
                .id(("flow-cell", id.as_u128() as u64))
                .relative()
                .w_full()
                .min_h(px(54.0 * zoom))
                .p(px(10.0 * zoom))
                .rounded(px(6.0 * zoom))
                .border(px(1.0 * zoom))
                .border_color(if active == Some(id) {
                  side_palette.active
                } else {
                  side_color.opacity(0.56)
                })
                .bg(if active == Some(id) {
                  side_palette.active.opacity(0.14)
                } else {
                  cx.theme().background
                })
                .hover(|style| style.bg(side_palette.hover.opacity(0.12)))
                .cursor_pointer()
                .when(active != Some(id) && !is_ghost, |this| {
                  // The whole card (except the one being edited) is a drag handle; a plain click still
                  // selects because GPUI only starts a drag past a small movement threshold.
                  let weak = weak_editor.clone();
                  let drag_label = label.clone();
                  this.on_drag(FlowCellDrag { cell_id: id }, move |drag, _, _, cx| {
                    let _ = weak.update(cx, |editor, cx| editor.begin_cell_drag(drag.cell_id, cx));
                    cx.new(|_| FlowCellDragPreview { label: drag_label.clone() })
                  })
                })
                .on_mouse_down(MouseButton::Left, cx.listener(|editor, _, _, cx| {
                  if editor.annotation_tool != AnnotationTool::None {
                    editor.set_annotation_tool(AnnotationTool::None, cx);
                  }
                  cx.stop_propagation();
                }))
                .on_click(cx.listener(move |editor, _, window, cx| {
                  editor.activate_cell(id, cx);
                  if let Some(cell_editor) = editor.cell_editors.get(&id) {
                    cell_editor.read(cx).focus_handle(cx).focus(window);
                  }
                }))
                // Ghost cells (the dragged subtree drawn in its previewed drop position) must not be drop
                // targets. Otherwise the ghost sits under the cursor, captures the drag, resolves to a
                // self-referential intent, and the placement oscillates and pins near the source/parent.
                .when(!is_ghost, |this| {
                  this
                    .on_drag_move(cx.listener(move |editor, event: &DragMoveEvent<FlowCellDrag>, window, cx| {
                      // `on_drag_move` is dispatched in the CAPTURE phase to every registered element
                      // (not hit-tested), parent-before-child — so the column handler above has already
                      // run this frame. Each handler hit-tests itself against its own bounds; the actual
                      // column-vs-cell arbitration is the `cursor_over_live_cell` guard in the column
                      // handler, NOT the `stop_propagation` below (which fires too late to gate the
                      // already-executed column handler and only suppresses later-painted siblings).
                      if !event.bounds.contains(&event.event.position) {
                        return;
                      }
                      // Left 60% reorders siblings (before/after by vertical half); right 40% nests as a
                      // child (first/last by vertical half). The wider child zone is far easier to hit
                      // than the old 28% strip, and top/bottom finally makes "first child" reachable.
                      let in_child_zone = event.event.position.x >= event.bounds.left() + event.bounds.size.width * 0.6;
                      let upper_half = event.event.position.y < event.bounds.top() + event.bounds.size.height / 2.0;
                      let destination = if in_child_zone && can_receive_child {
                        if upper_half {
                          FlowDropIntent::FirstChildOf(id)
                        } else {
                          FlowDropIntent::LastChildOf(id)
                        }
                      } else if upper_half {
                        FlowDropIntent::BeforeSibling(id)
                      } else {
                        FlowDropIntent::AfterSibling(id)
                      };
                      editor.update_cell_drop(destination, cx);
                      editor.update_drag_autoscroll(event.event.position, window, cx);
                      editor.log_drag_over_cell(id, event.event.position, event.bounds, destination);
                      cx.stop_propagation();
                    }))
                    .on_drop(cx.listener(|editor, drag: &FlowCellDrag, _, cx| {
                      editor.finish_cell_drop(drag.cell_id, cx);
                      cx.stop_propagation();
                    }))
                })
                .when(is_ghost, |this| this.opacity(0.5).border_dashed().border_color(side_palette.active))
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
                        cx.new(|_| FlowCellDragPreview { label: drag_label.clone() })
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
                }),
            )
            .into_any_element()
          }).collect();
            if let Some((gap_top, gap_height)) = gap_pending {
              let gap_spacer = px((gap_top - previous_bottom).max(0.0));
              elements.push(
                div()
                  .w_full()
                  .flex_col()
                  .child(div().h(gap_spacer).flex_none())
                  .child(
                    div()
                      .w_full()
                      .h(px(gap_height))
                      .rounded(px(2.0 * zoom))
                      .bg(side_palette.active),
                  )
                  .into_any_element(),
              );
            }
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
            let connector_bounds = &editor.read(cx).cell_bounds;
            for (parent_id, child_ids) in &connector_families {
              let Some(parent) = connector_bounds.get(parent_id) else {
                continue;
              };
              let children = child_ids
                .iter()
                .filter_map(|id| connector_bounds.get(id).copied())
                .collect::<Vec<_>>();
              paint_connector_family(*parent, &children, cx.theme().foreground, window);
            }
          },
        )
        .absolute()
        .inset_0(),
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
    if !cx.has_active_drag() && (self.pending_cell_drop.is_some() || self.dragging_cell.is_some()) {
      self.pending_cell_drop = None;
      self.dragging_cell = None;
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
  }
}
