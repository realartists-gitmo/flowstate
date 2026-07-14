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
    let Some((sheet_id, uses_summary_projection)) = self.document.projection().sheets.iter().find_map(|sheet| {
      sheet
        .cells
        .iter()
        .find(|cell| cell.id == cell_id)
        .map(|cell| (sheet.id, cell.summary.uses_summary_projection))
    }) else {
      return;
    };
    let Ok(mut document) = self.document.cell_document(cell_id) else {
      return;
    };
    let text_color = self.cell_text_color(cell_id, cx);
    apply_flow_cell_theme(
      &mut document,
      &load_document_theme(),
      text_color,
      cx.theme().background,
      self.board_zoom(),
    );
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
      let document = editor.read(cx).document().clone();
      let unchanged = flow
        .document
        .cell_document(cell_id)
        .is_ok_and(|current| cell_signature(&current) == cell_signature(&document));
      if unchanged {
        return;
      }
      if flow
        .document
        .replace_cell_document(sheet_id, cell_id, &document)
        .is_ok()
      {
        flow.dirty = true;
        cx.emit(FlowEditorEvent::Changed);
        cx.notify();
      }
    });
    self.cell_editors.insert(cell_id, editor);
    self
      .cell_editor_themes
      .insert(cell_id, (text_color, cx.theme().background, self.board_zoom().to_bits()));
    self.cell_editor_subscriptions.insert(cell_id, subscription);
  }

  pub(super) fn sync_cell_editors(&mut self, cx: &mut Context<Self>) {
    let client_theme = load_document_theme();
    let cell_ids: Vec<CellId> = self
      .document
      .projection()
      .sheets
      .iter()
      .flat_map(|sheet| sheet.cells.iter().map(|cell| cell.id))
      .collect();
    let cells: std::collections::HashMap<_, _> = cell_ids
      .iter()
      .filter_map(|cell_id| {
        self
          .document
          .cell_document(*cell_id)
          .ok()
          .map(|document| (*cell_id, document))
      })
      .collect();
    self.cell_editors.retain(|id, _| cells.contains_key(id));
    self
      .cell_editor_themes
      .retain(|id, _| cells.contains_key(id));
    self
      .cell_editor_subscriptions
      .retain(|id, _| cells.contains_key(id));
    self.cell_bounds.retain(|id, _| cells.contains_key(id));
    self
      .cell_measurements
      .retain(|id, _| cells.contains_key(id));
    for (cell_id, editor) in &self.cell_editors {
      if let Some(document) = cells.get(cell_id) {
        let current = cell_signature(editor.read(cx).document());
        let mut themed_document = document.clone();
        let text_color = self.cell_text_color(*cell_id, cx);
        apply_flow_cell_theme(&mut themed_document, &client_theme, text_color, cx.theme().background, self.board_zoom());
        let desired = cell_signature(&themed_document);
        if current != desired {
          editor.update(cx, |editor, cx| editor.replace_document_projection(themed_document, cx));
          self
            .cell_editor_themes
            .insert(*cell_id, (text_color, cx.theme().background, self.board_zoom().to_bits()));
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
    let Ok(mut document) = self.document.cell_document(cell_id) else {
      return;
    };
    apply_flow_cell_theme(
      &mut document,
      &load_document_theme(),
      text_color,
      cx.theme().background,
      self.board_zoom(),
    );
    editor.update(cx, |editor, cx| editor.replace_document_projection(document, cx));
    self.cell_editor_themes.insert(cell_id, signature);
  }
}

/// Theme-independent content signature for skip-if-unchanged checks: the
/// rope text plus every paragraph's (style, runs). Replaces the v1 whole-blob
/// byte comparison.
fn cell_signature(
  document: &flowstate_document::DocumentProjection,
) -> (String, Vec<(flowstate_document::ParagraphStyle, Vec<flowstate_document::TextRun>)>) {
  (
    document.text.to_string(),
    document
      .paragraphs
      .iter()
      .map(|paragraph| (paragraph.style, paragraph.runs.clone()))
      .collect(),
  )
}
