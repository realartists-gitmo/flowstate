//! Per-cell editor wiring (flow spec Part C, editor rewiring).
//!
//! Each open cell's `RichTextEditor` is driven by a real
//! [`flowstate_collab::flow::FlowCellAuthority`] through
//! `RichTextEditor::set_write_authority` — the identical injection the .db8
//! body editor uses (invariant 5). The record-era observer loop (re-encode
//! whole cell → `replace_cell_document` on every editor change) is GONE:
//! keystrokes commit through the gated intent path, and this flow editor
//! only reacts to the editor's notifications to refresh its board copy.

use flowstate_flow::CellId;
use gpui::{AppContext as _, Context};
use gpui_component::ActiveTheme as _;

use crate::{app_settings::load_document_theme, flow::cell_theme::apply_flow_cell_theme, rich_text_element::RichTextEditor};

use super::FlowEditor;

impl FlowEditor {
  pub(super) fn ensure_cell_editor(&mut self, cell_id: CellId, cx: &mut Context<Self>) {
    if self.cell_editors.contains_key(&cell_id) {
      return;
    }
    let Ok(mut document) = self.handle.open_cell(cell_id) else {
      return;
    };
    let uses_summary_projection = self
      .board
      .cell(cell_id)
      .is_some_and(|(_, cell)| cell.summary.uses_summary_projection);
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
    // The editor commits through its authority; this subscription only keeps
    // the BOARD side current (summary refreshes ride the board stream) and
    // the dirty flag honest — caret-only notifies are ignored.
    let subscription = cx.subscribe(&editor, move |flow, _editor, event: &crate::rich_text_element::EditorEvent, cx| {
      if matches!(event, crate::rich_text_element::EditorEvent::Changed { .. }) {
        flow.sync_board_from_handle(cx);
        if !flow.dirty {
          flow.dirty = true;
          cx.emit(super::FlowEditorEvent::Changed);
        }
        cx.notify();
      }
    });
    self.cell_editors.insert(cell_id, editor);
    self
      .cell_editor_themes
      .insert(cell_id, (text_color, cx.theme().background, self.board_zoom().to_bits()));
    self.cell_editor_subscriptions.insert(cell_id, subscription);
  }

  /// Pump remote projection changes into every open cell editor (the
  /// collaboration session calls this after imports; solo tabs never need it).
  pub fn sync_cell_editors_from_authority(&mut self, cx: &mut Context<Self>) {
    for editor in self.cell_editors.values() {
      editor.update(cx, |editor, cx| editor.sync_projection_from_authority(cx));
    }
  }

  pub(super) fn refresh_active_cell_theme(&mut self, cx: &mut Context<Self>) {
    let Some(cell_id) = self.active_cell else {
      return;
    };
    let Some(editor) = self.cell_editors.get(&cell_id).cloned() else {
      return;
    };
    let text_color = self.cell_text_color(cell_id, cx);
    let signature = (text_color, cx.theme().background, self.board_zoom().to_bits());
    if self.cell_editor_themes.get(&cell_id) == Some(&signature) {
      return;
    }
    let Ok(mut document) = self.handle.cell_preview(cell_id) else {
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
