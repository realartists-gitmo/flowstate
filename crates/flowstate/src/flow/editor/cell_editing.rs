use flowstate_flow::CellId;
use gpui::{AppContext as _, Context};
use gpui_component::ActiveTheme as _;

use crate::{
  app_settings::load_document_theme,
  flow::cell_theme::apply_flow_cell_theme,
  rich_text_element::{EditorEvent, RichTextEditor},
};

use super::{FlowEditor, FlowEditorEvent};

impl FlowEditor {
  /// Attach a real write authority to the cell's editor (spec S8): the cell's
  /// `RichTextEditor` commits straight through `FlowCellAuthority` into the
  /// gated flow runtime — the v1 observer/re-encode loop is gone. The editor's
  /// projection advances through its own authority stream; this host only
  /// tracks dirtiness and the board copy.
  pub(super) fn ensure_cell_editor(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    if self.cell_editors.contains_key(&cell_id) {
      return;
    }
    let Some(uses_summary_projection) = self.board.sheets.iter().find_map(|sheet| {
      sheet
        .cells
        .iter()
        .find(|cell| cell.id == cell_id)
        .map(|cell| cell.summary.uses_summary_projection)
    }) else {
      return;
    };
    let Ok(mut document) = self.handle.cell_projection(cell_id) else {
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
    let authority = self.handle.cell_authority(cell_id);
    let editor = cx.new(|cx| {
      // Seed with the themed projection FIRST: `set_write_authority` keeps the
      // editor's existing local theme when installing the canonical
      // projection, so the flow cell theme must already be in place.
      let mut editor = RichTextEditor::new_with_path(document.clone(), None, cx);
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
      editor.set_write_authority(authority, document, cx);
      editor
    });
    // Content commits already happened through the authority by the time this
    // event fires; the host's jobs are the board copy (summaries), the render
    // cache, and dirtiness.
    let subscription = cx.subscribe(&editor, move |flow, _editor, event: &EditorEvent, cx| {
      if let EditorEvent::Changed { .. } = event {
        flow.cell_documents.borrow_mut().remove(&cell_id);
        flow.after_local_change(cx);
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

  /// Re-theme the active cell's editor when the palette inputs changed (theme
  /// switch, zoom). The authority remains attached; only the canonical
  /// projection is re-installed with fresh theme composition.
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
    let Ok(mut document) = self.handle.cell_projection(cell_id) else {
      return;
    };
    apply_flow_cell_theme(
      &mut document,
      &load_document_theme(),
      text_color,
      cx.theme().background,
      self.board_zoom(),
    );
    editor.update(cx, |editor, cx| editor.install_canonical_projection(document, cx));
    self.cell_editor_themes.insert(cell_id, signature);
  }
}
