//! Document panel lifecycle: creation, activation, close, and the sandboxed
//! open-tabs session file.

use gpui::TestAppContext;

use super::support;

#[gpui::test]
fn new_document_activates_panel(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  assert!(h.read(cx, |ws| ws.document_panels.is_empty() && ws.active_document_id.is_none()));
  h.new_document(cx);
  h.read(cx, |ws| {
    assert_eq!(ws.document_panels.len(), 1);
    assert!(ws.active_document_id.is_some());
    assert!(ws.active_editor.is_some());
  });
}

#[gpui::test]
fn second_document_takes_focus_and_activation_switches_back(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  let first = h
    .read(cx, |ws| ws.active_document_id)
    .expect("first panel active");
  h.new_document(cx);
  let second = h
    .read(cx, |ws| ws.active_document_id)
    .expect("second panel active");
  assert_ne!(first, second, "new document must become active");

  h.update(cx, |ws, _, cx| {
    let (id, editor) = {
      let panel = ws
        .document_panels
        .iter()
        .find(|panel| panel.read(cx).id() == first)
        .expect("first panel still open");
      (panel.read(cx).id(), panel.read(cx).editor())
    };
    ws.set_active_document(id, editor, cx);
  });
  cx.run_until_parked();
  assert_eq!(h.read(cx, |ws| ws.active_document_id), Some(first));
}

#[gpui::test]
fn close_active_document_removes_panel(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.update(cx, |ws, window, cx| ws.close_active_document(window, cx));
  cx.run_until_parked();
  // A blank untitled document may count as dirty; if the save prompt fired,
  // decline saving and let the close proceed.
  if cx.has_pending_prompt() {
    cx.simulate_prompt_answer("Don't Save");
    cx.run_until_parked();
  }
  h.wait_until(cx, "panel closed", |ws| ws.document_panels.is_empty());
  assert_eq!(h.read(cx, |ws| ws.active_document_id), None);
  assert!(h.read(cx, |ws| ws.active_editor.is_none()));
}

#[gpui::test]
fn temporary_session_file_lands_in_sandbox(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  // Persist through the workspace path (debounced spawn), then verify the
  // direct write, so both prove the FLOWSTATE_CONFIG_DIR reroute: a leak to
  // the real std::env::temp_dir() would clobber the user's real tab session.
  h.update(cx, |ws, _, cx| ws.persist_temporary_workspace_session(cx));
  cx.run_until_parked();

  super::super::persist_temporary_workspace_session_to_disk(super::super::TemporaryWorkspaceSession {
    entries: vec![],
    active_index: None,
    ribbon_collapsed: false,
    outline_collapsed: false,
    pinned_entry_indices: vec![],
    speech_entry_index: None,
    left_nav_tub: false,
    nav_width: None,
    tub_tool_open: false,
    toolkit_filter: None,
    tub_expanded_dirs: vec![],
    comment_last_seen: vec![],
  });
  let sandbox_session = support::sandbox_config_dir().join("flowstate-open-tabs-session.json");
  let path = super::super::temporary_workspace_session_path();
  assert_eq!(path, sandbox_session, "session file must resolve into the sandbox");
}

// B-S8: the equation composer replaces the in-document source strip. Insert
// opens it (compose-new), commit lands one InsertObject through the intent
// path, Enter on the selected equation REOPENS it (edit mode), and a second
// commit rewrites the block's source through ReplaceEquationSourceRange.
#[gpui::test]
fn equation_composer_inserts_and_reopens_through_the_intent_path(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.wait_until(cx, "document runtime attach", |ws| {
    ws.active_document_id
      .is_some_and(|id| ws.document_runtimes.contains_key(&id))
  });

  // Insert opens the composer in compose-new mode — no placeholder block.
  h.update(cx, |ws, window, cx| {
    let editor = ws.active_editor.clone().expect("active editor");
    editor.update(cx, |editor, cx| {
      editor.dispatch_window_command(crate::rich_text_element::RichTextEditorCommand::InsertEquation, window, cx);
    });
  });
  cx.run_until_parked();
  let composer = h.read(cx, |ws| ws.equation_composer.clone()).expect("composer opens on insert");
  let block_count = h.update(cx, |ws, _, cx| {
    let editor = ws.active_editor.clone().expect("active editor");
    editor.read(cx).document().blocks.len()
  });
  assert_eq!(block_count, 1, "no placeholder block lands before the composer commits");
  cx.read(|cx| assert_eq!(composer.read(cx).target_equation(), None, "insert composes NEW"));

  // Commit: exactly one equation block with the composed source.
  h.update(cx, |_, window, cx| {
    composer.update(cx, |composer, cx| {
      composer.set_source("e = mc^2", window, cx);
      composer.commit(window, cx);
    });
  });
  cx.run_until_parked();
  assert!(h.read(cx, |ws| ws.equation_composer.is_none()), "commit dismisses the composer");
  let source = h.update(cx, |ws, _, cx| {
    let editor = ws.active_editor.clone().expect("active editor");
    editor
      .read(cx)
      .document()
      .blocks
      .iter()
      .find_map(|block| match block {
        crate::rich_text_element::Block::Equation(equation) => Some(equation.source.to_string()),
        _ => None,
      })
  });
  assert_eq!(source.as_deref(), Some("e = mc^2"), "the composed source lands as the block");

  // Arrow onto the block, Enter reopens the composer in EDIT mode.
  h.update(cx, |ws, window, cx| {
    let editor = ws.active_editor.clone().expect("active editor");
    editor.update(cx, |editor, cx| {
      editor.dispatch_window_command(crate::rich_text_element::RichTextEditorCommand::MoveLeft, window, cx);
      editor.dispatch_window_command(crate::rich_text_element::RichTextEditorCommand::InsertNewline, window, cx);
    });
  });
  cx.run_until_parked();
  let composer = h
    .read(cx, |ws| ws.equation_composer.clone())
    .expect("Enter on the selected equation reopens the composer");
  cx.read(|cx| {
    assert!(
      composer.read(cx).target_equation().is_some(),
      "reopen targets the existing block"
    );
  });

  // Second commit REWRITES the same block (no second block appears).
  h.update(cx, |_, window, cx| {
    composer.update(cx, |composer, cx| {
      composer.set_source("a^2 + b^2 = c^2", window, cx);
      composer.commit(window, cx);
    });
  });
  cx.run_until_parked();
  let (count, source) = h.update(cx, |ws, _, cx| {
    let editor = ws.active_editor.clone().expect("active editor");
    let document = editor.read(cx).document();
    let sources: Vec<String> = document
      .blocks
      .iter()
      .filter_map(|block| match block {
        crate::rich_text_element::Block::Equation(equation) => Some(equation.source.to_string()),
        _ => None,
      })
      .collect();
    (sources.len(), sources.first().cloned())
  });
  assert_eq!(count, 1, "editing must not mint a second block");
  assert_eq!(source.as_deref(), Some("a^2 + b^2 = c^2"), "commit rewrote the source");
}
