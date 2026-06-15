use flowstate_flow::CellId;
use gpui::{AppContext as _, Context};
use gpui_component::ActiveTheme as _;

use crate::{app_settings::load_document_theme, flow::cell_theme::apply_flow_cell_theme, rich_text_element::RichTextEditor};

use super::{FlowEditor, FlowEditorEvent};

impl FlowEditor {
  pub(super) fn ensure_cell_editor(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    if self.cell_editors.contains_key(&cell_id) {
      return;
    }
    let Some((sheet_id, mut document, uses_summary_projection)) = self.document.projection().sheets.iter().find_map(|sheet| {
      sheet
        .cells
        .iter()
        .find(|cell| cell.id == cell_id)
        .and_then(|cell| cell.document().ok().map(|document| (document, cell.uses_summary_projection().unwrap_or(false))))
        .map(|(document, uses_summary_projection)| (sheet.id, document, uses_summary_projection))
    }) else {
      return;
    };
    let text_color = self.cell_text_color(cell_id, cx);
    apply_flow_cell_theme(&mut document, &load_document_theme(), text_color, cx.theme().background, self.board_zoom());
    let editor = cx.new(|cx| {
      let mut editor = RichTextEditor::new_with_path(document, None, cx);
      editor.set_invisibility_mode(uses_summary_projection, cx);
      editor.update_config(
        |config| {
          config.allow_paragraph_breaks = false;
          config.flow_cell_surface = true;
          config.show_section_collapse_controls = false;
          config.caret_color = Some(text_color);
        },
        cx,
      );
      editor
    });
    let subscription = cx.observe(&editor, move |flow, editor, cx| {
      let mut document = editor.read(cx).document().clone();
      let source_theme = flow
        .document
        .projection()
        .sheets
        .iter()
        .find(|sheet| sheet.id == sheet_id)
        .and_then(|sheet| sheet.cells.iter().find(|cell| cell.id == cell_id))
        .and_then(|cell| cell.document().ok())
        .map(|document| document.theme);
      if let Some(source_theme) = source_theme {
        document.theme = source_theme;
      }
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
    self.cell_editor_themes.insert(cell_id, (text_color, cx.theme().background, self.board_zoom().to_bits()));
    self.cell_editor_subscriptions.insert(cell_id, subscription);
  }

  pub(super) fn sync_cell_editors(&mut self, cx: &mut Context<Self>) {
    let client_theme = load_document_theme();
    let cells: std::collections::HashMap<_, _> = self
      .document
      .projection()
      .sheets
      .iter()
      .flat_map(|sheet| sheet.cells.iter().filter_map(|cell| cell.document().ok().map(|document| (cell.id, document))))
      .collect();
    self.cell_editors.retain(|id, _| cells.contains_key(id));
    self.cell_editor_themes.retain(|id, _| cells.contains_key(id));
    self.cell_editor_subscriptions.retain(|id, _| cells.contains_key(id));
    self.cell_bounds.retain(|id, _| cells.contains_key(id));
    self.cell_measurements.retain(|id, _| cells.contains_key(id));
    for (cell_id, editor) in &self.cell_editors {
      if let Some(document) = cells.get(cell_id) {
        let current = flowstate_document::db8_bytes(editor.read(cx).document()).ok();
        let mut themed_document = document.clone();
        let text_color = self.cell_text_color(*cell_id, cx);
        apply_flow_cell_theme(&mut themed_document, &client_theme, text_color, cx.theme().background, self.board_zoom());
        let desired = flowstate_document::db8_bytes(&themed_document).ok();
        if current != desired {
          editor.update(cx, |editor, cx| editor.replace_document_from_collaboration(themed_document, cx));
          self.cell_editor_themes.insert(*cell_id, (text_color, cx.theme().background, self.board_zoom().to_bits()));
        }
      }
    }
    if let Some(active) = self.active_cell
      && cells.contains_key(&active)
    {
      self.ensure_cell_editor(active, cx);
    }
  }

  pub(super) fn refresh_active_cell_theme(&mut self, cx: &mut Context<Self>) {
    let Some(cell_id) = self.active_cell else {
      return;
    };
    let Some(editor) = self.cell_editors.get(&cell_id) else {
      return;
    };
    let text_color = self.cell_text_color(cell_id, cx);
    let signature = (text_color, cx.theme().background, self.board_zoom().to_bits());
    if self.cell_editor_themes.get(&cell_id) == Some(&signature) {
      return;
    }
    let Some(mut document) = self
      .document
      .projection()
      .sheets
      .iter()
      .flat_map(|sheet| &sheet.cells)
      .find(|cell| cell.id == cell_id)
      .and_then(|cell| cell.document().ok())
    else {
      return;
    };
    apply_flow_cell_theme(&mut document, &load_document_theme(), text_color, cx.theme().background, self.board_zoom());
    editor.update(cx, |editor, cx| editor.replace_document_from_collaboration(document, cx));
    self.cell_editor_themes.insert(cell_id, signature);
  }
}
