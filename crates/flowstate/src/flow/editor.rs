use std::path::PathBuf;
use std::collections::HashSet;

use flowstate_flow::{
  AnnotationOriginator, BoardPoint, BoardRect, CellId, FlowDocument, RelativePosition, SheetId, VersionVector,
};
use gpui::{
  AnyElement, App, Bounds, Context, DragMoveEvent, Entity, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, MouseDownEvent,
  KeyDownEvent, KeyUpEvent, MouseMoveEvent, MouseUpEvent, PathBuilder, Render, SharedString, Subscription, Task, Window, canvas, div, point,
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
mod drag_drop;
mod layout;

use drag_drop::{CellDropDestination, FlowCellDrag, FlowCellDragPreview};
use layout::sheet_cell_layout;

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
  cell_editor_themes: std::collections::HashMap<CellId, (gpui::Hsla, gpui::Hsla)>,
  cell_editor_subscriptions: std::collections::HashMap<CellId, Subscription>,
  pending_cell_drop: Option<CellDropDestination>,
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
      cell_editor_themes: std::collections::HashMap::new(),
      cell_editor_subscriptions: std::collections::HashMap::new(),
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
    let client_document_theme = load_document_theme();
    let cell_layout = sheet_cell_layout(sheet, &self.cell_bounds);
    let board_width = px(32.0 + definition.columns.len() as f32 * 280.0 + definition.columns.len().saturating_sub(1) as f32 * 16.0);
    let weak_editor = cx.entity().downgrade();
    let weak_connector_editor = weak_editor.clone();
    let connector_families = sheet
      .cells
      .iter()
      .filter(|cell| sheet.cells.iter().any(|child| child.parent_id == Some(cell.id)))
      .map(|parent| {
        (
          parent.id,
          sheet.cells.iter().filter(|child| child.parent_id == Some(parent.id)).map(|child| child.id).collect::<Vec<_>>(),
        )
      })
      .collect::<Vec<_>>();
    div()
      .id("flow-columns")
      .relative()
      .min_w_full()
      .min_h_full()
      .w(board_width)
      .flex()
      .gap(px(16.0))
      .p(px(16.0))
      .children(definition.columns.iter().enumerate().map(|(column_index, column)| {
        let side_palette = flow_side_palette(column.side, cx);
        let side_color = side_palette.base;
        let can_receive_child = column_index + 1 < definition.columns.len();
        let add_editor = cx.entity().clone();
        div()
          .w(px(280.0))
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
              .text_color(side_color)
              .border_b_2()
              .border_color(side_color)
              .child(column.label.clone())
              .child(
                div()
                  .flex()
                  .gap_1()
                  .child(
                    Button::new(("flow-send-to-document", column_index))
                      .xsmall()
                      .ghost()
                      .tooltip("Send to document")
                      .child(Icon::default().path("icons/file-input.svg").xsmall())
                      .on_click(|_, _, _| {}),
                  )
                  .child(
                    Button::new(("flow-add-column-orphan", column_index))
                      .xsmall()
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
          .child(div().h(px(12.0)).flex_none())
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
              apply_flow_cell_theme(document, &client_document_theme, side_color, cx.theme().background);
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
                .min_h(px(54.0))
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
                .on_mouse_down(MouseButton::Left, cx.listener(|editor, _, _, cx| {
                  if editor.annotation_tool != AnnotationTool::None {
                    editor.set_annotation_tool(AnnotationTool::None, cx);
                    cx.stop_propagation();
                  }
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
                .when(is_drop_target, |this| this.border_2().border_color(side_palette.active))
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
                .when(can_receive_child, |this| {
                  this.child(
                    Button::new(("flow-cell-reply", id.as_u128() as u64))
                      .absolute()
                      .top(px(24.0))
                      .right(px(2.0))
                      .xsmall()
                      .ghost()
                      .tooltip("Add first reply")
                      .child(Icon::default().path("icons/message-square-reply.svg").xsmall())
                      .on_click(move |_, window, cx| reply_editor.update(cx, |editor, cx| {
                        editor.set_annotation_tool(AnnotationTool::None, cx);
                        editor.add_first_response_to(id, cx);
                        editor.focus_active_cell(window, cx);
                      })),
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
              let children = child_ids.iter().filter_map(|id| connector_bounds.get(id)).collect::<Vec<_>>();
              let Some(first_child) = children.first() else {
                continue;
              };
              let start = point(parent.right(), parent.center().y);
              let midpoint = start.x + (first_child.left() - start.x) / 2.0;
              let lowest_y = children.iter().map(|child| child.center().y).max().unwrap_or(start.y);
              let mut path = PathBuilder::stroke(px(1.5));
              path.move_to(start);
              path.line_to(point(midpoint, start.y));
              path.line_to(point(midpoint, lowest_y));
              for child in children {
                let end = point(child.left(), child.center().y);
                path.move_to(point(midpoint, end.y));
                path.line_to(end);
              }
              if let Ok(path) = path.build() {
                window.paint_path(path, cx.theme().secondary);
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
                  let color = gpui::Hsla::from(rgba(stroke.style.color_rgba));
                  paint_stroke(
                    bounds.origin,
                    &stroke.points,
                    px(stroke.style.width),
                    color.opacity(color.a * stroke.style.opacity),
                    window,
                  );
                }
                if !draft.is_empty() {
                  paint_stroke(bounds.origin, &draft, px(4.0), gpui::Hsla::from(rgba(0xf59e_0bff)).opacity(0.55), window);
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
    self.refresh_active_cell_theme(cx);
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
          editor.begin_space_pan(event.position, cx);
          cx.stop_propagation();
        }
      }))
      .on_mouse_move(cx.listener(|editor, event: &MouseMoveEvent, _, _| editor.continue_space_pan(event.position)))
      .on_mouse_up(MouseButton::Left, cx.listener(|editor, _: &MouseUpEvent, _, cx| editor.finish_space_pan(cx)))
      .child(
        canvas(
          |_, _, _| {},
          move |bounds, _, window, cx| {
            let spacing = px(24.0 * board_zoom);
            let offset = grid_scroll.offset();
            let mut x = offset.x % spacing;
            let color = cx.theme().border.opacity(0.56);
            while x < bounds.size.width {
              let mut y = offset.y % spacing;
              while y < bounds.size.height {
                window.paint_quad(gpui::fill(
                  gpui::Bounds::new(bounds.origin + point(x, y), gpui::size(px(1.5), px(1.5))),
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
          .on_scroll_wheel(cx.listener(|editor, event: &ScrollWheelEvent, window, cx| {
            if event.modifiers.shift {
              let delta = event.delta.pixel_delta(window.line_height());
              let mut offset = editor.board_scroll.offset();
              offset.x += delta.y + delta.x;
              editor.board_scroll.set_offset(offset);
              cx.stop_propagation();
              cx.notify();
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
