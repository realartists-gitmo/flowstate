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
    pane_layout: None,
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

// R3-A + R5-B: an orphaned `.recovery` sibling of a recent document lands on
// the home shelf at startup, and opening it is PATHLESS with provenance —
// autosave must never adopt the .recovery path (the docx-clobber law).
#[gpui::test]
fn recovered_work_shelf_scans_and_opens_pathless(cx: &mut TestAppContext) {
  let root = support::sandbox_config_dir();
  let doc_path = root.join("crashed-cards.db8");
  let recovery_path = root.join("crashed-cards.db8.recovery");
  std::fs::write(&doc_path, b"placeholder").expect("seed doc file");
  // A REAL recovery package (the shelf opener decodes it).
  let imported =
    flowstate_document::import_document_projection(crate::rich_text_element::demo_document(), "Recovered fixture").expect("import");
  let mut runtime = flowstate_collab::crdt_runtime::CrdtRuntime::from_imported_document(imported).expect("runtime");
  runtime
    .checkpoint_package("Recovery snapshot", Some(recovery_path.clone()), &flowstate_document::RevisionStamp::session())
    .expect("write recovery package");
  crate::app_settings::save_recent_documents(vec![doc_path.clone()]).expect("seed recents");

  let h = support::open_workspace(cx);
  let entry = h
    .read(cx, |ws| ws.recovered_work.first().cloned())
    .expect("the startup scan finds the orphaned recovery snapshot");
  assert_eq!(entry.source_path, doc_path);
  assert_eq!(entry.recovery_path, recovery_path);

  h.update(cx, |ws, window, cx| ws.open_recovered_work(&entry, window, cx));
  cx.run_until_parked();
  h.update(cx, |ws, _, cx| {
    let panel = ws.document_panels.first().expect("recovered panel opens").read(cx);
    assert!(
      panel.title_text().starts_with("Recovered — "),
      "provenance title, got {:?}",
      panel.title_text()
    );
    assert_eq!(panel.path(), None, "recovered work opens PATHLESS — never adopts .recovery");
    assert!(ws.recovered_work.is_empty(), "the shelf entry is consumed on open");
  });

  // Cleanup so other sandboxed tests don't inherit the seeded recents.
  let _ = std::fs::remove_file(&recovery_path);
  let _ = crate::app_settings::save_recent_documents(Vec::new());
}

// B-S11b: the drop-target half of drag-the-block — dropping an object at
// the caret's placement point lands ONE MoveBlock through the intent path
// and the block re-selects at its new home; the own-footprint drop no-ops.
#[gpui::test]
fn block_drag_drop_moves_through_the_intent_path(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.wait_until(cx, "document runtime attach", |ws| {
    ws.active_document_id
      .is_some_and(|id| ws.document_runtimes.contains_key(&id))
  });
  let editor = h.read(cx, |ws| ws.active_editor.clone()).expect("editor");

  // [alpha ¶, beta ¶, equation] — then drop the equation before beta.
  h.update(cx, |_, window, cx| {
    editor.update(cx, |editor, cx| {
      editor.insert_text_command("alpha", cx);
      editor.dispatch_window_command(crate::rich_text_element::RichTextEditorCommand::InsertNewline, window, cx);
      editor.insert_text_command("beta", cx);
      editor.insert_equation("x^2", cx);
    });
  });
  cx.run_until_parked();
  let (equation_ix, equation_id) = h.update(cx, |_, _, cx| {
    let editor = editor.read(cx);
    let document = editor.document();
    let ix = document
      .blocks
      .iter()
      .position(|block| matches!(block, crate::rich_text_element::Block::Equation(_)))
      .expect("equation block");
    (ix, document.ids.block_ids[ix])
  });

  // Caret to document start → placement inserts after the first block.
  h.update(cx, |_, window, cx| {
    editor.update(cx, |editor, cx| {
      editor.dispatch_window_command(crate::rich_text_element::RichTextEditorCommand::MoveDocumentStart, window, cx);
      editor.on_block_drop(
        &crate::rich_text_element::BlockDrag {
          block_ix: equation_ix,
          block_id: equation_id,
          label: "Moving equation".into(),
        },
        window,
        cx,
      );
    });
  });
  cx.run_until_parked();
  h.update(cx, |_, _, cx| {
    let editor = editor.read(cx);
    let document = editor.document();
    assert!(
      matches!(document.blocks.get(1), Some(crate::rich_text_element::Block::Equation(_))),
      "the equation moved up beside the first paragraph"
    );
    assert!(
      matches!(editor.selected_block_kind(), Some("equation")),
      "the moved block stays selected"
    );
  });
}
