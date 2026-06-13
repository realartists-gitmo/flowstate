use std::path::PathBuf;
use std::collections::HashSet;

use flowstate_flow::{
  AnnotationOriginator, AnnotationStroke, BoardPoint, BoardRect, CellId, FlowDocument, RelativePosition, SheetId, StrokeStyle, VersionVector,
};
use gpui::{
  AnyElement, App, Bounds, Context, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent,
  KeyDownEvent, KeyUpEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Render, SharedString, Subscription, Task, Window, canvas, div, point,
  prelude::*, px, rgba, ScrollHandle,
};
use gpui_component::ActiveTheme as _;
use gpui_component::PixelsExt as _;
use gpui_component::scroll::{Scrollbar, ScrollbarShow};

use crate::{flow::flow_side_palette, rich_text_element::RichTextEditor};

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

#[derive(Clone)]
struct FlowCellDrag {
  cell_id: CellId,
}

struct FlowCellDragPreview;

impl Render for FlowCellDragPreview {
  fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .px_2()
      .py_1()
      .rounded(cx.theme().radius)
      .bg(cx.theme().popover)
      .border_1()
      .border_color(cx.theme().border)
      .child("Move argument")
  }
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
  cell_editor_subscriptions: Vec<Subscription>,
  pending_cell_drop: Option<(CellId, RelativePosition)>,
  cell_bounds: std::collections::HashMap<CellId, Bounds<gpui::Pixels>>,
  board_scroll: ScrollHandle,
  board_zoom: f32,
  viewport_origin: BoardPoint,
  space_pan_armed: bool,
  pan_drag_anchor: Option<(gpui::Point<gpui::Pixels>, gpui::Point<gpui::Pixels>)>,
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
      cell_editor_subscriptions: Vec::new(),
      pending_cell_drop: None,
      cell_bounds: std::collections::HashMap::new(),
      board_scroll: ScrollHandle::new(),
      board_zoom: 1.0,
      viewport_origin: BoardPoint::default(),
      space_pan_armed: false,
      pan_drag_anchor: None,
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

  pub fn annotation_tool(&self) -> AnnotationTool {
    self.annotation_tool
  }

  pub fn annotations_visible(&self) -> bool {
    self.active_sheet.is_some_and(|sheet| !self.hidden_annotation_sheets.contains(&sheet))
  }

  pub fn board_zoom(&self) -> f32 {
    self.board_zoom
  }

  /// suggestion: wire future zoom controls here. Annotation and board-space
  /// transforms already consume this value even though no zoom UI exists yet.
  pub fn set_board_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
    self.board_zoom = zoom.clamp(0.25, 4.0);
    cx.notify();
  }

  pub fn visible_board_rect(&self) -> BoardRect {
    let bounds = self.board_scroll.bounds();
    let offset = self.board_scroll.offset();
    let scroll_x = -offset.x.as_f32();
    let scroll_y = -offset.y.as_f32();
    BoardRect {
      min: BoardPoint {
        x: scroll_x,
        y: scroll_y,
      },
      max: BoardPoint {
        x: scroll_x + bounds.size.width.as_f32() / self.board_zoom,
        y: scroll_y + bounds.size.height.as_f32() / self.board_zoom,
      },
    }
  }

  pub fn set_annotation_tool(&mut self, tool: AnnotationTool, cx: &mut Context<Self>) {
    self.annotation_tool = tool;
    cx.notify();
  }

  pub fn toggle_annotations_visible(&mut self, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    if !self.hidden_annotation_sheets.remove(&sheet) {
      self.hidden_annotation_sheets.insert(sheet);
    }
    cx.notify();
  }

  pub fn set_originator_annotations_hidden(&mut self, originator: AnnotationOriginator, hidden: bool, cx: &mut Context<Self>) {
    if hidden {
      self.hidden_annotation_originators.insert(originator);
    } else {
      self.hidden_annotation_originators.remove(&originator);
    }
    cx.notify();
  }

  pub fn clear_annotations(&mut self, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    if self.document.clear_annotations(sheet, &self.local_annotation_originator).is_ok() {
      self.changed(self.active_cell, cx);
    }
  }

  fn annotation_point(&self, position: gpui::Point<gpui::Pixels>) -> BoardPoint {
    let offset = self.board_scroll.offset();
    BoardPoint {
      x: (position.x.as_f32() - self.viewport_origin.x) / self.board_zoom - offset.x.as_f32(),
      y: (position.y.as_f32() - self.viewport_origin.y) / self.board_zoom - offset.y.as_f32(),
    }
  }

  fn begin_annotation(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
    match self.annotation_tool {
      AnnotationTool::Marker => {
        self.drawing_points.clear();
        self.drawing_points.push(self.annotation_point(position));
      },
      AnnotationTool::Eraser => self.erase_at(self.annotation_point(position), cx),
      AnnotationTool::None => {},
    }
    cx.notify();
  }

  fn continue_annotation(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
    if self.annotation_tool != AnnotationTool::Marker || self.drawing_points.is_empty() {
      return;
    }
    let point = self.annotation_point(position);
    let should_append = self.drawing_points.last().is_none_or(|last| {
      let dx = point.x - last.x;
      let dy = point.y - last.y;
      dx * dx + dy * dy >= 4.0
    });
    if should_append {
      self.drawing_points.push(point);
      cx.notify();
    }
  }

  fn finish_annotation(&mut self, cx: &mut Context<Self>) {
    let Some(sheet_id) = self.active_sheet else {
      self.drawing_points.clear();
      return;
    };
    if self.drawing_points.len() < 2 {
      self.drawing_points.clear();
      return;
    }
    let points = simplify_stroke(&std::mem::take(&mut self.drawing_points), 1.5);
    let bbox = stroke_bbox(&points);
    let stroke = AnnotationStroke {
      id: uuid::Uuid::new_v4(),
      sheet_id,
      originator: self.local_annotation_originator.clone(),
      points,
      style: StrokeStyle {
        color_rgba: 0xfff5_9e0b,
        width: 4.0,
        opacity: 0.55,
      },
      bbox,
    };
    if self.document.add_annotation(sheet_id, stroke).is_ok() {
      self.changed(self.active_cell, cx);
    }
  }

  fn erase_at(&mut self, point: BoardPoint, cx: &mut Context<Self>) {
    let Some(sheet) = self
      .active_sheet
      .and_then(|sheet_id| self.document.projection().sheets.iter().find(|sheet| sheet.id == sheet_id))
    else {
      return;
    };
    let radius = 10.0;
    let touched = sheet
      .annotations
      .iter()
      .find(|stroke| {
        stroke.originator == self.local_annotation_originator
          && point.x >= stroke.bbox.min.x - radius
          && point.x <= stroke.bbox.max.x + radius
          && point.y >= stroke.bbox.min.y - radius
          && point.y <= stroke.bbox.max.y + radius
          && stroke.points.windows(2).any(|segment| segment_distance(point, segment[0], segment[1]) <= radius)
      })
      .map(|stroke| stroke.id);
    if let Some(stroke_id) = touched
      && self
        .document
        .delete_annotation(sheet.id, stroke_id, &self.local_annotation_originator)
        .is_ok_and(|removed| removed)
    {
      self.changed(self.active_cell, cx);
    }
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

  pub fn add_first_argument(&mut self, cx: &mut Context<Self>) {
    let Some(sheet) = self.active_sheet else {
      return;
    };
    if let Ok(id) = self.document.add_plain_cell(sheet, 0, None, None) {
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

  fn update_cell_drop(&mut self, target: CellId, position: RelativePosition, cx: &mut Context<Self>) {
    self.pending_cell_drop = Some((target, position));
    cx.notify();
  }

  fn finish_cell_drop(&mut self, dragged: CellId, cx: &mut Context<Self>) {
    let Some((target, position)) = self.pending_cell_drop.take() else {
      return;
    };
    if dragged == target {
      return;
    }
    let Some(sheet_id) = self.active_sheet else {
      return;
    };
    let Some(sheet) = self.document.projection().sheets.iter().find(|sheet| sheet.id == sheet_id) else {
      return;
    };
    let Some(target_index) = sheet.cells.iter().position(|cell| cell.id == target) else {
      return;
    };
    let target_cell = &sheet.cells[target_index];
    let Some(definition) = self.document.projection().format.sheet_type(sheet.sheet_type_id) else {
      return;
    };
    let Some(column_index) = definition.columns.iter().position(|column| column.id == target_cell.column_id) else {
      return;
    };
    let insertion_index = match position {
      RelativePosition::Before => target_index,
      RelativePosition::After => target_index + 1,
    };
    if self
      .document
      .move_cell(sheet_id, dragged, column_index, insertion_index, target_cell.parent_id)
      .is_ok()
    {
      self.changed(Some(dragged), cx);
    }
  }

  fn set_cell_bounds(&mut self, cell_id: CellId, bounds: Bounds<gpui::Pixels>, cx: &mut Context<Self>) {
    if self.cell_bounds.get(&cell_id) != Some(&bounds) {
      self.cell_bounds.insert(cell_id, bounds);
      cx.notify();
    }
  }

  fn scroll_cell_into_view(&self, cell_id: CellId) {
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
    self.board_scroll.set_offset(offset);
  }

  fn set_viewport_origin(&mut self, origin: gpui::Point<gpui::Pixels>) {
    self.viewport_origin = BoardPoint {
      x: origin.x.as_f32(),
      y: origin.y.as_f32(),
    };
  }

  fn begin_space_pan(&mut self, position: gpui::Point<gpui::Pixels>, cx: &mut Context<Self>) {
    if self.space_pan_armed {
      self.pan_drag_anchor = Some((position, self.board_scroll.offset()));
      cx.notify();
    }
  }

  fn continue_space_pan(&mut self, position: gpui::Point<gpui::Pixels>) {
    if let Some((start, offset)) = self.pan_drag_anchor {
      self.board_scroll.set_offset(offset + (position - start));
    }
  }

  fn finish_space_pan(&mut self, cx: &mut Context<Self>) {
    self.pan_drag_anchor = None;
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

  fn ensure_cell_editor(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    if self.cell_editors.contains_key(&cell_id) {
      return;
    }
    let Some((sheet_id, document, uses_summary_projection)) = self.document.projection().sheets.iter().find_map(|sheet| {
      sheet
        .cells
        .iter()
        .find(|cell| cell.id == cell_id)
        .and_then(|cell| cell.document().ok().map(|document| (document, cell.uses_summary_projection().unwrap_or(false))))
        .map(|(document, uses_summary_projection)| (sheet.id, document, uses_summary_projection))
    }) else {
      return;
    };
    let editor = cx.new(|cx| {
      let mut editor = RichTextEditor::new_with_path(document, None, cx);
      editor.set_invisibility_mode(uses_summary_projection, cx);
      editor.update_config(|config| config.allow_paragraph_breaks = false, cx);
      editor
    });
    let subscription = cx.observe(&editor, move |flow, editor, cx| {
      let document = editor.read(cx).document().clone();
      let Ok(bytes) = flowstate_document::db8_bytes(&document) else {
        return;
      };
      let unchanged = flow
        .document
        .projection()
        .sheets
        .iter()
        .find(|sheet| sheet.id == sheet_id)
        .and_then(|sheet| sheet.cells.iter().find(|cell| cell.id == cell_id))
        .is_some_and(|cell| cell.document_bytes == bytes);
      if unchanged {
        return;
      }
      if flow.document.replace_cell_document(sheet_id, cell_id, &document).is_ok() {
        flow.dirty = true;
        cx.emit(FlowEditorEvent::Changed);
        cx.notify();
      }
    });
    self.cell_editors.insert(cell_id, editor);
    self.cell_editor_subscriptions.push(subscription);
  }

  fn sync_cell_editors(&mut self, cx: &mut Context<Self>) {
    let cells: std::collections::HashMap<_, _> = self
      .document
      .projection()
      .sheets
      .iter()
      .flat_map(|sheet| sheet.cells.iter().filter_map(|cell| cell.document().ok().map(|document| (cell.id, document))))
      .collect();
    self.cell_editors.retain(|id, _| cells.contains_key(id));
    for (cell_id, editor) in &self.cell_editors {
      if let Some(document) = cells.get(cell_id) {
        let current = flowstate_document::db8_bytes(editor.read(cx).document()).ok();
        let desired = flowstate_document::db8_bytes(document).ok();
        if current != desired {
          let document = document.clone();
          editor.update(cx, |editor, cx| editor.replace_document_from_collaboration(document, cx));
        }
      }
    }
    if let Some(active) = self.active_cell
      && cells.contains_key(&active)
    {
      self.ensure_cell_editor(active, cx);
    }
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
    let visible_rect = self.visible_board_rect();
    let inflation = 32.0 / self.board_zoom;
    let strokes: Vec<_> = if !self.hidden_annotation_sheets.contains(&sheet_id) {
      sheet
        .annotations
        .iter()
        .filter(|stroke| {
          !self.hidden_annotation_originators.contains(&stroke.originator)
            && stroke.bbox.max.x >= visible_rect.min.x - inflation
            && stroke.bbox.min.x <= visible_rect.max.x + inflation
            && stroke.bbox.max.y >= visible_rect.min.y - inflation
            && stroke.bbox.min.y <= visible_rect.max.y + inflation
        })
        .cloned()
        .collect()
    } else {
      Vec::new()
    };
    let draft = self.drawing_points.clone();
    let annotation_active = self.annotation_tool != AnnotationTool::None && !self.space_pan_armed;
    let connector_bounds = self.cell_bounds.clone();
    let connector_edges: Vec<_> = sheet
      .cells
      .iter()
      .filter_map(|cell| {
        let parent = cell.parent_id?;
        let side = definition.columns.iter().find(|column| column.id == cell.column_id)?.side;
        Some((parent, cell.id, side))
      })
      .collect();
    let weak_editor = cx.entity().downgrade();
    div()
      .id("flow-columns")
      .relative()
      .flex()
      .gap(px(12.0))
      .p(px(16.0))
      .children(definition.columns.iter().map(|column| {
        let side_palette = flow_side_palette(column.side, cx);
        let side_color = side_palette.base;
        div()
          .w(px(280.0))
          .flex_none()
          .flex_col()
          .gap(px(8.0))
          .child(
            div()
              .font_weight(gpui::FontWeight::BOLD)
              .text_color(side_color)
              .border_b_2()
              .border_color(side_color)
              .child(column.label.clone()),
          )
          .children(sheet.cells.iter().filter(|cell| cell.column_id == column.id).map(|cell| {
            let id = cell.id;
            let label: SharedString = cell
              .summary_text()
              .unwrap_or_else(|_| "Invalid rich text".into())
              .into();
            let estimated_lines = label
              .lines()
              .map(|line| line.chars().count().div_ceil(34).max(1))
              .sum::<usize>()
              .max(1);
            let cell_height = px(32.0 + 22.0 * estimated_lines as f32);
            let cell_editor = self.cell_editors.get(&id).cloned();
            let is_drop_target = self.pending_cell_drop.is_some_and(|(target, _)| target == id);
            div()
              .id(("flow-cell", id.as_u128() as u64))
              .relative()
              .h(cell_height)
              .p(px(10.0))
              .rounded(px(6.0))
              .border_1()
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
              .on_click(cx.listener(move |editor, _, _, cx| editor.activate_cell(id, cx)))
              .on_drag_move(cx.listener(move |editor, event: &DragMoveEvent<FlowCellDrag>, _, cx| {
                let position = if event.event.position.y < event.bounds.top() + event.bounds.size.height / 2.0 {
                  RelativePosition::Before
                } else {
                  RelativePosition::After
                };
                editor.update_cell_drop(id, position, cx);
              }))
              .child(
                div()
                  .absolute()
                  .inset_0()
                  .invisible()
                  .group_drag_over::<FlowCellDrag>("", |this| this.visible().border_2().border_color(side_palette.hover))
                  .on_drop(cx.listener(|editor, drag: &FlowCellDrag, _, cx| editor.finish_cell_drop(drag.cell_id, cx))),
              )
              .when(is_drop_target, |this| this.border_2().border_color(side_palette.active))
              .child(
                canvas(
                  {
                    let weak_editor = weak_editor.clone();
                    move |bounds, _, cx| {
                      let _ = weak_editor.update(cx, |editor, cx| editor.set_cell_bounds(id, bounds, cx));
                    }
                  },
                  |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
              )
              .child(
                div()
                  .id(("flow-cell-drag-handle", id.as_u128() as u64))
                  .absolute()
                  .top(px(2.0))
                  .right(px(4.0))
                  .cursor_move()
                  .text_color(cx.theme().muted_foreground)
                  .child("⠿")
                  .on_mouse_down(MouseButton::Left, cx.listener(move |editor, _, _, cx| editor.activate_cell(id, cx)))
                  .on_drag(FlowCellDrag { cell_id: id }, |_, _, _, cx| cx.new(|_| FlowCellDragPreview)),
              )
              .child(if active == Some(id) {
                cell_editor.map_or_else(|| div().child(label).into_any_element(), |editor| editor.into_any_element())
              } else {
                div().child(label).into_any_element()
              })
          }))
      }))
      .child(
        canvas(
          |_, _, _| {},
          move |bounds, _, window, cx| {
            for (parent_id, child_id, side) in &connector_edges {
              let Some(parent) = connector_bounds.get(parent_id) else {
                continue;
              };
              let Some(child) = connector_bounds.get(child_id) else {
                continue;
              };
              let color = flow_side_palette(*side, cx).base;
              let start = point(parent.right() - bounds.origin.x, parent.center().y - bounds.origin.y);
              let end = point(child.left() - bounds.origin.x, child.center().y - bounds.origin.y);
              let midpoint = start.x + (end.x - start.x) / 2.0;
              let mut path = PathBuilder::stroke(px(1.5));
              path.move_to(start);
              path.line_to(point(midpoint, start.y));
              path.line_to(point(midpoint, end.y));
              path.line_to(end);
              if let Ok(path) = path.build() {
                window.paint_path(path, color.opacity(0.68));
              }
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
                  paint_stroke(
                    bounds.origin,
                    &stroke.points,
                    px(stroke.style.width),
                    gpui::Hsla::from(rgba(stroke.style.color_rgba)).opacity(stroke.style.opacity),
                    window,
                  );
                }
                if !draft.is_empty() {
                  paint_stroke(bounds.origin, &draft, px(4.0), gpui::Hsla::from(rgba(0xfff5_9e0b)).opacity(0.55), window);
                }
              },
            )
            .absolute()
            .size_full(),
          )
          .when(annotation_active, |this| {
            this
              .on_mouse_down(MouseButton::Left, cx.listener(|editor, event: &MouseDownEvent, _, cx| {
                editor.begin_annotation(event.position, cx);
                cx.stop_propagation();
              }))
              .on_mouse_move(cx.listener(|editor, event: &MouseMoveEvent, _, cx| editor.continue_annotation(event.position, cx)))
              .on_mouse_up(MouseButton::Left, cx.listener(|editor, _: &MouseUpEvent, _, cx| editor.finish_annotation(cx)))
          }),
      )
      .into_any_element()
  }
}

fn stroke_bbox(points: &[BoardPoint]) -> BoardRect {
  let mut min = points[0];
  let mut max = points[0];
  for point in &points[1..] {
    min.x = min.x.min(point.x);
    min.y = min.y.min(point.y);
    max.x = max.x.max(point.x);
    max.y = max.y.max(point.y);
  }
  BoardRect { min, max }
}

fn simplify_stroke(points: &[BoardPoint], minimum_distance: f32) -> Vec<BoardPoint> {
  let Some(first) = points.first().copied() else {
    return Vec::new();
  };
  let mut simplified = Vec::with_capacity(points.len());
  simplified.push(first);
  for window in points.windows(3) {
    let smoothed = BoardPoint {
      x: (window[0].x + window[1].x + window[2].x) / 3.0,
      y: (window[0].y + window[1].y + window[2].y) / 3.0,
    };
    if simplified
      .last()
      .is_none_or(|previous| (smoothed.x - previous.x).hypot(smoothed.y - previous.y) >= minimum_distance)
    {
      simplified.push(smoothed);
    }
  }
  if let Some(last) = points.last().copied()
    && simplified.last() != Some(&last)
  {
    simplified.push(last);
  }
  simplified
}

fn segment_distance(point: BoardPoint, start: BoardPoint, end: BoardPoint) -> f32 {
  let dx = end.x - start.x;
  let dy = end.y - start.y;
  let length_squared = dx * dx + dy * dy;
  let t = if length_squared == 0.0 {
    0.0
  } else {
    (((point.x - start.x) * dx + (point.y - start.y) * dy) / length_squared).clamp(0.0, 1.0)
  };
  let nearest_x = start.x + t * dx;
  let nearest_y = start.y + t * dy;
  (point.x - nearest_x).hypot(point.y - nearest_y)
}

fn paint_stroke(origin: gpui::Point<gpui::Pixels>, points: &[BoardPoint], width: gpui::Pixels, color: gpui::Hsla, window: &mut Window) {
  let Some(first) = points.first() else {
    return;
  };
  let mut path = PathBuilder::stroke(width);
  path.move_to(point(origin.x + px(first.x), origin.y + px(first.y)));
  for point_value in &points[1..] {
    path.line_to(point(origin.x + px(point_value.x), origin.y + px(point_value.y)));
  }
  if let Ok(path) = path.build() {
    window.paint_path(path, color);
  }
}

impl EventEmitter<FlowEditorEvent> for FlowEditor {}

impl Focusable for FlowEditor {
  fn focus_handle(&self, _: &App) -> FocusHandle {
    self.focus_handle.clone()
  }
}

impl Render for FlowEditor {
  fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
    div()
      .id("flow-editor")
      .relative()
      .size_full()
      .overflow_scroll()
      .track_scroll(&self.board_scroll)
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
          editor.begin_space_pan(event.position, cx);
          cx.stop_propagation();
        }
      }))
      .on_mouse_move(cx.listener(|editor, event: &MouseMoveEvent, _, _| editor.continue_space_pan(event.position)))
      .on_mouse_up(MouseButton::Left, cx.listener(|editor, _: &MouseUpEvent, _, cx| editor.finish_space_pan(cx)))
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
      })
      .child(
        div()
          .absolute()
          .inset_0()
          .child(Scrollbar::horizontal(&self.board_scroll).scrollbar_show(ScrollbarShow::Scrolling)),
      )
  }
}
