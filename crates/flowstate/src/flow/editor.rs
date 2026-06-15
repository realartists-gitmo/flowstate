use std::path::PathBuf;
use std::collections::HashSet;

use flowstate_flow::{AnnotationOriginator, BoardPoint, CellId, FlowDocument, RelativePosition, SheetId, VersionVector};
use gpui::{
  AnyElement, App, Bounds, Context, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent,
  KeyDownEvent, KeyUpEvent, MouseMoveEvent, MouseUpEvent, Render, SharedString, Subscription, Task, Window, canvas, div, point,
  prelude::*, px, rgba, ScrollHandle, ScrollWheelEvent,
};
use gpui_component::{Icon, IconName, Sizable as _};
use gpui_component::button::{Button, ButtonVariants as _};
use gpui_component::ActiveTheme as _;
use gpui_component::PixelsExt as _;
use gpui_component::scroll::{Scrollbar, ScrollbarShow};

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
mod zoom;

use annotation::paint_stroke;
use connector::paint_connector_family;
use drag_drop::{CellDropDestination, FlowCellDrag, FlowCellDragPreview};
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
  document: FlowDocument,
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
  pending_cell_drop: Option<CellDropDestination>,
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
  pub fn new_with_path(document: FlowDocument, path: Option<PathBuf>, _window: &mut Window, cx: &mut Context<Self>) -> Self {
    let active_sheet = document.projection().sheets.first().map(|sheet| sheet.id);
    let collapsed_outline_items = document
      .projection()
      .sheets
      .iter()
      .flat_map(|sheet| std::iter::once(sheet.id).chain(sheet.cells.iter().map(|cell| cell.id)))
      .collect();
    Self {
      document,
      path,
      dirty: false,
      active_sheet,
      active_cell: None,
      collapsed_outline_items,
      annotation_tool: AnnotationTool::None,
      hidden_annotation_sheets: HashSet::new(),
      hidden_annotation_originators: HashSet::new(),
      // suggestion: replace this opaque local value with the identity source
      // selected by the document-collaboration implementation.
      local_annotation_originator: AnnotationOriginator("local".into()),
      drawing_points: Vec::new(),
      cell_editors: std::collections::HashMap::new(),
      cell_editor_themes: std::collections::HashMap::new(),
      cell_editor_subscriptions: std::collections::HashMap::new(),
      pending_cell_drop: None,
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
    Self::new_with_path(FlowDocument::new(), None, window, cx)
  }

  pub fn document(&self) -> &FlowDocument {
    &self.document
  }

  pub fn version_vector(&self) -> VersionVector {
    self.document.version_vector()
  }

  pub fn collaboration_updates_since(&self, version: &VersionVector) -> anyhow::Result<Vec<u8>> {
    self.document.updates_since(version)
  }

  pub fn import_collaboration_updates(&mut self, bytes: &[u8], cx: &mut Context<Self>) -> anyhow::Result<()> {
    self.document.import_updates(bytes)?;
    self.sync_cell_editors(cx);
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
      let Some(editor) = flow.active_cell.and_then(|cell| flow.cell_editors.get(&cell)).cloned() else {
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
    self.active_sheet.is_some_and(|sheet| !self.hidden_annotation_sheets.contains(&sheet))
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
    if self.document.projection().sheets.iter().any(|sheet| sheet.id == sheet_id) {
      self.active_sheet = Some(sheet_id);
      self.active_cell = None;
      cx.emit(FlowEditorEvent::ActiveSheetChanged(Some(sheet_id)));
      cx.notify();
    }
  }

  pub fn activate_cell(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    let sheet_id = self
      .document
      .projection()
      .sheets
      .iter()
      .find(|sheet| sheet.cells.iter().any(|cell| cell.id == cell_id))
      .map(|sheet| sheet.id);
    if let Some(sheet_id) = sheet_id {
      if self.document.ensure_cell_editable_projection(sheet_id, cell_id).is_ok_and(|changed| changed) {
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

  pub fn add_response(&mut self, cx: &mut Context<Self>) {
    let Some((sheet, cell)) = self.active_sheet.zip(self.active_cell) else {
      return;
    };
    if let Ok(id) = self.document.add_response(sheet, cell) {
      self.collapsed_outline_items.insert(id);
      self.ensure_cell_editor(id, cx);
      self.changed(Some(id), cx);
    }
  }

  pub fn add_first_response_to(&mut self, cell: CellId, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    if let Ok(id) = self.document.add_first_response(sheet, cell) {
      self.collapsed_outline_items.insert(id);
      self.ensure_cell_editor(id, cx);
      self.changed(Some(id), cx);
    }
  }

  pub fn active_cell_is_struck(&self) -> bool {
    self
      .active_sheet
      .zip(self.active_cell)
      .and_then(|(sheet, cell)| self.document.projection().sheets.iter().find(|candidate| candidate.id == sheet)?.cells.iter().find(|candidate| candidate.id == cell))
      .and_then(|cell| cell.document().ok())
      .is_some_and(|document| document.paragraphs.iter().flat_map(|paragraph| &paragraph.runs).all(|run| run.styles.strikethrough))
  }

  pub fn add_first_argument(&mut self, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    if let Ok(id) = self.document.add_orphan_at_column_top(sheet, 0) {
      self.collapsed_outline_items.insert(id);
      self.ensure_cell_editor(id, cx);
      self.changed(Some(id), cx);
    }
  }

  pub fn add_orphan_at_column_top(&mut self, column_index: usize, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    if let Ok(id) = self.document.add_orphan_at_column_top(sheet, column_index) {
      self.collapsed_outline_items.insert(id);
      self.ensure_cell_editor(id, cx);
      self.changed(Some(id), cx);
    }
  }

  pub fn create_sheet(&mut self, cx: &mut Context<Self>) {
    self.create_sheet_of_type(0, cx);
  }

  pub fn create_sheet_of_type(&mut self, sheet_type_index: usize, cx: &mut Context<Self>) {
    let Some(sheet_type) = self.document.projection().format.sheet_types.get(sheet_type_index) else {
      return;
    };
    let name = format!("Sheet {}", self.document.projection().sheets.len() + 1);
    if let Ok(id) = self.document.create_sheet(name, sheet_type.id) {
      self.collapsed_outline_items.insert(id);
      self.active_sheet = Some(id);
      self.active_cell = None;
      self.dirty = true;
      cx.emit(FlowEditorEvent::Changed);
      cx.emit(FlowEditorEvent::ActiveSheetChanged(Some(id)));
      cx.notify();
    }
  }

  pub fn rename_active_sheet(&mut self, name: impl Into<String>, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    if self.document.rename_sheet(sheet, name).is_ok() {
      self.changed(self.active_cell, cx);
    }
  }

  pub fn delete_active_sheet(&mut self, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    if self.document.delete_sheet(sheet).is_ok() {
      self.collapsed_outline_items.remove(&sheet);
      self.active_sheet = self.document.projection().sheets.first().map(|sheet| sheet.id);
      self.active_cell = None;
      self.dirty = true;
      cx.emit(FlowEditorEvent::Changed);
      cx.emit(FlowEditorEvent::ActiveSheetChanged(self.active_sheet));
      cx.emit(FlowEditorEvent::ActiveCellChanged(None));
      cx.notify();
    }
  }

  pub fn move_active_sheet(&mut self, direction: isize, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    let Some(index) = self.document.projection().sheets.iter().position(|candidate| candidate.id == sheet) else {
      return;
    };
    let target = index.saturating_add_signed(direction).min(self.document.projection().sheets.len().saturating_sub(1));
    if target != index && self.document.move_sheet(sheet, target).is_ok() {
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
      .document
      .projection()
      .sheets
      .iter()
      .find_map(|sheet| {
        let cell = sheet.cells.iter().find(|cell| cell.id == cell_id)?;
        let definition = self.document.projection().format.sheet_type(sheet.sheet_type_id)?;
        let column = definition.columns.iter().find(|column| column.id == cell.column_id)?;
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
    if let Ok(id) = self.document.add_sibling(sheet, cell, position) {
      self.collapsed_outline_items.insert(id);
      self.ensure_cell_editor(id, cx);
      self.changed(Some(id), cx);
    }
  }

  pub fn active_cell_is_empty(&self) -> bool {
    self
      .active_sheet
      .zip(self.active_cell)
      .and_then(|(sheet_id, cell_id)| {
        self
          .document
          .projection()
          .sheets
          .iter()
          .find(|sheet| sheet.id == sheet_id)?
          .cells
          .iter()
          .find(|cell| cell.id == cell_id)
      })
      .is_some_and(|cell| cell.is_empty().unwrap_or(false))
  }

  pub fn delete_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
    let Some((sheet, cell)) = self.active_sheet.zip(self.active_cell) else {
      return;
    };
    let fallback = self.document.deletion_fallback(sheet, cell);
    if self.document.delete_cell(sheet, cell).is_ok() {
      self.cell_editors.remove(&cell);
      self.cell_editor_themes.remove(&cell);
      self.cell_editor_subscriptions.remove(&cell);
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
    if self.document.strike_cell(sheet, cell).is_ok() {
      self.sync_cell_editors(cx);
      self.changed(Some(cell), cx);
    }
  }

  pub fn can_undo(&self) -> bool {
    self.document.can_undo()
  }

  pub fn can_redo(&self) -> bool {
    self.document.can_redo()
  }

  pub fn undo(&mut self, cx: &mut Context<Self>) {
    if self.document.undo().is_ok_and(|changed| changed) {
      self.sync_cell_editors(cx);
      self.dirty = true;
      cx.emit(FlowEditorEvent::Changed);
      cx.notify();
    }
  }

  pub fn redo(&mut self, cx: &mut Context<Self>) {
    if self.document.redo().is_ok_and(|changed| changed) {
      self.sync_cell_editors(cx);
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
    let document = self.document.clone();
    cx.spawn(async move |editor, cx| {
      let write_result = cx
        .background_executor()
        .spawn(async move { flowstate_flow::save_flow_document(&path, &document).map_err(std::io::Error::other) })
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
    let sheet = self.document.projection().sheets.iter().find(|sheet| sheet.id == sheet_id);
    let Some(sheet) = sheet else {
      return div().child("Select a sheet").into_any_element();
    };
    let Some(definition) = self.document.projection().format.sheet_type(sheet.sheet_type_id) else {
      return div().child("Invalid sheet type").into_any_element();
    };
    let active = self.active_cell;
    let strokes: Vec<_> = if !self.hidden_annotation_sheets.contains(&sheet_id) {
      sheet
        .annotations
        .iter()
        .filter(|stroke| !self.hidden_annotation_originators.contains(&stroke.originator))
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
      .filter_map(|parent| children_by_parent.remove(&parent.id).map(|children| (parent.id, children)))
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
      .children(definition.columns.iter().enumerate().map(|(column_index, column)| {
        let side_palette = flow_side_palette(column.side, cx);
        let side_color = side_palette.base;
        let can_receive_child = column_index + 1 < definition.columns.len();
        let add_editor = cx.entity().clone();
        div()
          .w(px(280.0 * zoom))
          .flex_none()
          .flex_col()
          .on_drag_move(cx.listener(move |editor, event: &DragMoveEvent<FlowCellDrag>, _, cx| {
            editor.update_column_drop(column_index, event.event.position.y, cx);
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
            let mut previous_bottom = 0.0;
            sheet.cells.iter().filter(|cell| cell.column_id == column.id).map(|cell| {
            let id = cell.id;
            let layout = cell_layout.get(&id).copied().unwrap_or_default();
            let spacer_height = px((layout.top - previous_bottom).max(0.0));
            previous_bottom = layout.top + layout.height;
            let label: SharedString = cell
              .summary_text()
              .unwrap_or_else(|_| "Invalid rich text".into())
              .into();
            let mut uses_summary_projection = cell.uses_summary_projection().unwrap_or(false);
            let mut rendered_document = cell.document().ok();
            if let Some(document) = rendered_document.as_mut() {
              if !uses_summary_projection
                && let Some(paragraph) = std::sync::Arc::make_mut(&mut document.paragraphs).first_mut()
              {
                paragraph.style = flowstate_document::PARAGRAPH_TAG;
                document.blocks = std::sync::Arc::new(flowstate_document::paragraph_blocks_from_paragraphs(&document.paragraphs));
                uses_summary_projection = true;
              }
              apply_flow_cell_theme(document, &client_document_theme, side_color, cx.theme().background, self.board_zoom);
            }
            let cell_editor = self.cell_editors.get(&id).cloned();
            let reply_editor = cx.entity().clone();
            let is_drop_target = self.pending_cell_drop.is_some_and(|destination| {
              matches!(destination, CellDropDestination::Relative(target, _) | CellDropDestination::ChildOf(target) if target == id)
            });
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
                .on_drag_move(cx.listener(move |editor, event: &DragMoveEvent<FlowCellDrag>, _, cx| {
                  let right_zone = event.event.position.x >= event.bounds.left() + event.bounds.size.width * 0.72;
                  let destination = if right_zone && can_receive_child {
                    CellDropDestination::ChildOf(id)
                  } else if event.event.position.y < event.bounds.top() + event.bounds.size.height / 2.0 {
                    CellDropDestination::Relative(id, RelativePosition::Before)
                  } else {
                    CellDropDestination::Relative(id, RelativePosition::After)
                  };
                  editor.update_cell_drop(destination, cx);
                  cx.stop_propagation();
                }))
                .on_drop(cx.listener(|editor, drag: &FlowCellDrag, _, cx| {
                  editor.finish_cell_drop(drag.cell_id, cx);
                  cx.stop_propagation();
                }))
                .when(is_drop_target, |this| this.border(px(2.0 * zoom)).border_color(side_palette.active))
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
                    .on_drag(FlowCellDrag { cell_id: id }, |_, _, _, cx| cx.new(|_| FlowCellDragPreview)),
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
          }).collect::<Vec<_>>()})
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
      .on_key_down(cx.listener(|editor, event: &KeyDownEvent, _, cx| {
        if event.keystroke.key == "space" {
          editor.space_pan_armed = true;
          cx.stop_propagation();
          cx.notify();
        }
      }))
      .on_key_up(cx.listener(|editor, event: &KeyUpEvent, _, cx| {
        if event.keystroke.key == "space" {
          editor.space_pan_armed = false;
          editor.finish_space_pan(cx);
          cx.stop_propagation();
        }
      }))
      .on_mouse_down(MouseButton::Left, cx.listener(|editor, event: &MouseDownEvent, _, cx| {
        if editor.space_pan_armed {
          editor.begin_pan(event.position, cx);
          cx.stop_propagation();
        }
      }))
      .on_mouse_move(cx.listener(|editor, event: &MouseMoveEvent, window, cx| editor.queue_pan(event.position, window, cx)))
      .on_mouse_up(MouseButton::Left, cx.listener(|editor, _: &MouseUpEvent, _, cx| editor.finish_space_pan(cx)))
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
            this.on_mouse_down(MouseButton::Left, cx.listener(|editor, event: &MouseDownEvent, _, cx| {
              editor.begin_pan(event.position, cx);
              cx.stop_propagation();
            }))
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
              .on_mouse_down(MouseButton::Left, cx.listener(|editor, event: &MouseDownEvent, _, cx| editor.begin_annotation(event.position, cx)))
              .on_mouse_move(cx.listener(|editor, event: &MouseMoveEvent, _, cx| editor.continue_annotation(event.position, cx)))
              .on_mouse_up(MouseButton::Left, cx.listener(|editor, _: &MouseUpEvent, _, cx| editor.finish_annotation(cx)))
              .on_mouse_up_out(MouseButton::Left, cx.listener(|editor, _: &MouseUpEvent, _, cx| editor.finish_annotation(cx)))
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
