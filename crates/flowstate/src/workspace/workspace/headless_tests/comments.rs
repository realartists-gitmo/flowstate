//! C-S4: the review-mark lifecycle under the real workspace wiring. The
//! comments panel owns the editor's annotation overlay — armed while the rail
//! shows the panel, re-armed on reopen, cleared the moment review mode ends.

use gpui::TestAppContext;

use super::super::ToolkitTool;
use super::support;

fn wait_for_marks(h: &support::WorkspaceHarness, cx: &mut TestAppContext, want_armed: bool, what: &str) {
  for _ in 0..500 {
    cx.run_until_parked();
    let armed = h.update(cx, |ws, _window, cx| {
      ws.active_editor
        .as_ref()
        .is_some_and(|editor| !editor.read(cx).annotation_selections().is_empty())
    });
    if armed == want_armed {
      return;
    }
    std::thread::sleep(std::time::Duration::from_millis(10));
  }
  panic!("timed out waiting for: {what}");
}

#[gpui::test]
fn comments_panel_owns_review_marks_across_open_close_reopen(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.wait_until(cx, "document runtime attach", |ws| {
    ws.active_document_id
      .is_some_and(|id| ws.document_runtimes.contains_key(&id))
  });

  // Give the document text and an anchored, unresolved comment. The insert
  // and the comment ride the same per-document FIFO, so ordering holds.
  let selection = h.update(cx, |ws, _window, cx| {
    let editor = ws.active_editor.clone().expect("active editor");
    editor.update(cx, |editor, cx| {
      editor.insert_text_command("The panel marks this text while review mode is on.", cx);
      editor.select_all(cx);
      editor.selection().clone()
    })
  });
  let io = h.read(cx, |ws| {
    let id = ws.active_document_id.expect("active document");
    ws.document_runtimes.get(&id).cloned().expect("document runtime")
  });
  // The document service replies from an OS thread outside the test
  // dispatcher, so blocking must be allowed to park.
  cx.executor().allow_parking();
  cx.executor()
    .block_test(io.create_comment(Some(selection), "anchored note".into(), 7, "Tester".into()))
    .expect("comment creation succeeds");

  // Open: the rail render creates the panel, its reload arms the marks.
  h.update(cx, |ws, window, cx| ws.open_comments_panel(window, cx));
  wait_for_marks(&h, cx, true, "review marks armed on open");

  // Close (switch review mode off): marks clear synchronously via detach.
  h.update(cx, |ws, _window, cx| ws.toggle_toolkit_tool(ToolkitTool::Comments, cx));
  wait_for_marks(&h, cx, false, "review marks cleared on close");

  // Reopen with the same document: the cached-thread reopen path must re-arm.
  h.update(cx, |ws, window, cx| ws.open_comments_panel(window, cx));
  wait_for_marks(&h, cx, true, "review marks re-armed on reopen");
}
