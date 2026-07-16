//! H-S3: history takeover lifecycle under the real workspace wiring.

use gpui::TestAppContext;

use super::support;

#[gpui::test]
fn history_takeover_opens_loads_the_ledger_and_toggles_closed(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.wait_until(cx, "document runtime attach", |ws| {
    ws.active_document_id
      .is_some_and(|id| ws.document_runtimes.contains_key(&id))
  });

  // Give the document content and a first save so revision records exist.
  h.update(cx, |ws, _window, cx| {
    let editor = ws.active_editor.clone().expect("active editor");
    editor.update(cx, |editor, cx| editor.insert_text_command("history under test", cx));
  });
  let io = h.read(cx, |ws| {
    let id = ws.active_document_id.expect("active document");
    ws.document_runtimes.get(&id).cloned().expect("document runtime")
  });
  cx.executor().allow_parking();
  cx.executor()
    .block_test(io.checkpoint_package("Doc".into(), None, flowstate_document::RevisionStamp::session()))
    .expect("first save");
  cx.executor()
    .block_test(io.create_named_pin("Key moment".into()))
    .expect("named pin");

  // Open: the takeover commandeers the viewport and its ledger loads.
  h.update(cx, |ws, window, cx| ws.open_history_takeover(window, cx));
  cx.run_until_parked();
  assert!(h.read(cx, |ws| ws.history_takeover.is_some()), "takeover opens");
  for _ in 0..500 {
    cx.run_until_parked();
    let loaded = h.update(cx, |ws, _window, cx| {
      ws.history_takeover
        .as_ref()
        .is_some_and(|takeover| takeover.read(cx).revision_count() >= 2)
    });
    if loaded {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(10));
  }
  let count = h.update(cx, |ws, _window, cx| {
    ws.history_takeover
      .as_ref()
      .map_or(0, |takeover| takeover.read(cx).revision_count())
  });
  assert!(count >= 2, "the ledger loads the save + the named pin (got {count})");

  // Toggle: the same command exits history mode.
  h.update(cx, |ws, window, cx| ws.open_history_takeover(window, cx));
  cx.run_until_parked();
  assert!(h.read(cx, |ws| ws.history_takeover.is_none()), "toggle closes the takeover");
}
