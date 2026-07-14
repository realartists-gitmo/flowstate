//! Comment thread lifecycle against the runtime API: the thread author is
//! recorded at creation and gates deletion, message edits are author-gated,
//! and bodies are sanitized before they reach the CRDT.

use flowstate_collab::crdt_runtime::CrdtRuntime;
use flowstate_collab::local_write::{GateHolder, InsertTextIntent, LocalDocHandle, LocalWriteConfig, TextAnchor};
use gpui_flowtext::{DocumentOffset, EditorSelection};

fn commented_doc() -> (LocalDocHandle, std::sync::Arc<flowstate_collab::local_write::WriteGate<CrdtRuntime>>) {
  let core = CrdtRuntime::new_empty("comment-threads").expect("runtime");
  let (handle, gate) = LocalDocHandle::new(core, LocalWriteConfig::default());
  let projection = handle.projection().expect("projection");
  let paragraph = projection.ids.paragraph_ids[0];
  handle
    .insert_text(InsertTextIntent {
      at: TextAnchor::new(paragraph, 0),
      text: "hello comment anchors".into(),
      style_override: None,
    })
    .expect("insert text");
  (handle, gate)
}

fn selection(start: usize, end: usize) -> EditorSelection {
  let mut selection = EditorSelection::collapsed(DocumentOffset { paragraph: 0, byte: start });
  selection.head = DocumentOffset { paragraph: 0, byte: end };
  selection
}

#[test]
fn thread_author_is_stored_and_gates_deletion() {
  let (_handle, gate) = commented_doc();
  let mut guard = gate.lock(GateHolder::DocumentService).expect("gate");
  let comment_id = guard
    .create_comment(&selection(0, 5), "first!", 7, "Alex")
    .expect("create comment");

  let threads = guard.comments();
  assert_eq!(threads.len(), 1);
  assert_eq!(threads[0].comment_id, comment_id);
  assert_eq!(threads[0].messages[0].author_user_id, 7);
  assert_eq!(threads[0].quoted_text, "hello");

  // A replying non-author must not be able to delete the thread — even
  // though their message now exists (and could win a timestamp race).
  guard
    .reply_to_comment(comment_id, "me too", 9, "Blair")
    .expect("reply");
  let error = guard
    .delete_comment(comment_id, 9)
    .expect_err("non-author delete");
  assert!(error.to_string().contains("thread author"), "unexpected error: {error}");
  assert_eq!(guard.comments().len(), 1);

  guard.delete_comment(comment_id, 7).expect("author delete");
  assert!(guard.comments().is_empty());
}

#[test]
fn message_edits_are_author_gated_and_bodies_sanitized() {
  let (_handle, gate) = commented_doc();
  let mut guard = gate.lock(GateHolder::DocumentService).expect("gate");
  let comment_id = guard
    .create_comment(&selection(0, 5), "line one\r\nline\u{7} two", 7, "Alex")
    .expect("create comment");

  let threads = guard.comments();
  assert_eq!(threads[0].messages[0].body, "line one\nline two");

  let message_id = threads[0].messages[0].message_id;
  let error = guard
    .edit_comment_message(comment_id, message_id, "hijacked", 9)
    .expect_err("non-author edit");
  assert!(error.to_string().contains("author"), "unexpected error: {error}");

  guard
    .edit_comment_message(comment_id, message_id, "revised", 7)
    .expect("author edit");
  assert_eq!(guard.comments()[0].messages[0].body, "revised");

  assert!(
    guard
      .create_comment(&selection(2, 2), "empty selection", 7, "Alex")
      .is_err()
  );
  assert!(
    guard
      .create_comment(&selection(0, 5), " \r\u{0} ", 7, "Alex")
      .is_err()
  );
}
