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

// C-S5: the unread lifecycle — a comment created while the rail is closed
// raises the badge; opening the panel shows the per-thread dot and marks the
// thread seen, killing the badge.
#[gpui::test]
fn unread_badge_raises_while_closed_and_clears_on_view(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.wait_until(cx, "document runtime attach", |ws| {
    ws.active_document_id
      .is_some_and(|id| ws.document_runtimes.contains_key(&id))
  });

  let selection = h.update(cx, |ws, _window, cx| {
    let editor = ws.active_editor.clone().expect("active editor");
    editor.update(cx, |editor, cx| {
      editor.insert_text_command("Unread threads must announce themselves.", cx);
      editor.select_all(cx);
      editor.selection().clone()
    })
  });
  let io = h.read(cx, |ws| {
    let id = ws.active_document_id.expect("active document");
    ws.document_runtimes.get(&id).cloned().expect("document runtime")
  });
  cx.executor().allow_parking();
  cx.executor()
    .block_test(io.create_comment(Some(selection), "new activity".into(), 7, "Tester".into()))
    .expect("comment creation succeeds");

  // Recount with the rail closed: the badge must raise.
  h.update(cx, |ws, _window, cx| ws.schedule_comment_unread_refresh(cx));
  cx.executor().advance_clock(std::time::Duration::from_millis(450));
  for _ in 0..500 {
    cx.run_until_parked();
    if h.read(cx, |ws| ws.unread_comment_count) == 1 {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(10));
  }
  assert_eq!(h.read(cx, |ws| ws.unread_comment_count), 1, "badge raises while the rail is closed");

  // Open the panel: the thread shows its dot and the badge clears.
  h.update(cx, |ws, window, cx| ws.open_comments_panel(window, cx));
  for _ in 0..500 {
    cx.run_until_parked();
    let cleared = h.read(cx, |ws| ws.unread_comment_count) == 0;
    let dotted = h.update(cx, |ws, _window, cx| {
      ws.comments_panel
        .as_ref()
        .is_some_and(|panel| panel.read(cx).unread_thread_count() == 1)
    });
    if cleared && dotted {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(10));
  }
  assert_eq!(h.read(cx, |ws| ws.unread_comment_count), 0, "viewing the panel marks threads seen");
  let dotted = h.update(cx, |ws, _window, cx| {
    ws.comments_panel
      .as_ref()
      .is_some_and(|panel| panel.read(cx).unread_thread_count() == 1)
  });
  assert!(dotted, "the viewed thread carries its new-activity dot for this viewing session");
  assert!(
    h.read(cx, |ws| !ws.comment_last_seen.is_empty()),
    "the read-state store records the thread"
  );
}
