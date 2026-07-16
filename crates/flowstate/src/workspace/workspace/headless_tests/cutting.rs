//! CT-S1 — the cutting-toolkit floor: send refusals speak, the flow-speech
//! trap is dead, and Mark Card never silently no-ops.

use gpui::TestAppContext;

use super::support;

#[gpui::test]
fn send_without_speech_document_refuses_loudly(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);

  let sent = h.update(cx, |ws, window, cx| ws.send_selection_to_speech_document(window, cx));
  assert!(!sent, "send must refuse with no speech document set");
  assert!(
    h.read(cx, |ws| ws.activity_event.is_some()),
    "the refusal must land in the activity zone — the old silent no-op is the defect"
  );
}

#[gpui::test]
fn flow_cannot_be_designated_speech_document(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.new_flow(window, cx));
  cx.run_until_parked();
  let flow_id = h.read(cx, |ws| ws.active_document_id).expect("active flow");

  h.update(cx, |ws, _, cx| ws.toggle_speech_document(flow_id, cx));

  assert_eq!(
    h.read(cx, |ws| ws.speech_document_id),
    None,
    "a flow must never become the speech document (the badge lied and sends silently no-oped)"
  );
  assert!(h.read(cx, |ws| ws.activity_event.is_some()), "the refusal must speak");
}

#[gpui::test]
fn self_send_refuses_loudly(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  let doc_id = h.read(cx, |ws| ws.active_document_id).expect("active doc");
  h.update(cx, |ws, _, cx| ws.toggle_speech_document(doc_id, cx));
  assert_eq!(h.read(cx, |ws| ws.speech_document_id), Some(doc_id));

  let sent = h.update(cx, |ws, window, cx| ws.send_selection_to_speech_document(window, cx));
  assert!(!sent, "sending the speech document to itself must refuse");
  assert!(h.read(cx, |ws| ws.activity_event.is_some()), "the refusal must speak");
}

// CT-S3: designation writes the doc's replicated self-marker, and the
// cross-doc reconcile follows the newest designation while clearing the
// loser's marker — one team speech doc, never two.
#[gpui::test]
fn speech_designation_marks_the_doc_and_newest_wins_across_docs(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.wait_until(cx, "first document runtime attach", |ws| {
    ws.active_document_id
      .is_some_and(|id| ws.document_runtimes.contains_key(&id))
  });
  let first_id = h.read(cx, |ws| ws.active_document_id).expect("first doc");
  let first_io = h.read(cx, |ws| ws.document_runtimes.get(&first_id).cloned().expect("runtime"));

  h.update(cx, |ws, _, cx| ws.toggle_speech_document(first_id, cx));
  assert_eq!(h.read(cx, |ws| ws.speech_document_id), Some(first_id));

  // The marker write is a detached async task — wait for it to land.
  cx.executor().allow_parking();
  let mut marker = None;
  for _ in 0..500 {
    cx.run_until_parked();
    marker = cx.executor().block_test(first_io.speech_target()).expect("marker read");
    if marker.as_ref().is_some_and(|marker| marker.active) {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(10));
  }
  assert!(
    marker.is_some_and(|marker| marker.active),
    "designation must write the doc's active self-marker"
  );

  // A second, NEWER designation on another doc: the local designation moves
  // and the reconcile clears the first doc's marker (the loser).
  h.new_document(cx);
  h.wait_until(cx, "second document runtime attach", |ws| {
    ws.active_document_id
      .is_some_and(|id| id != first_id && ws.document_runtimes.contains_key(&id))
  });
  let second_id = h.read(cx, |ws| ws.active_document_id).expect("second doc");
  h.update(cx, |ws, _, cx| ws.toggle_speech_document(second_id, cx));
  assert_eq!(h.read(cx, |ws| ws.speech_document_id), Some(second_id));

  let mut first_marker = None;
  for _ in 0..500 {
    cx.run_until_parked();
    // Editor activity drives the debounced reconcile; nudge it directly so
    // the loser-clear pass runs without waiting on incidental notifies.
    h.update(cx, |ws, _, cx| ws.schedule_speech_target_reconcile(cx));
    cx.executor().advance_clock(std::time::Duration::from_millis(500));
    cx.run_until_parked();
    first_marker = cx.executor().block_test(first_io.speech_target()).expect("marker read");
    if first_marker.as_ref().is_some_and(|marker| !marker.active) {
      break;
    }
    std::thread::sleep(std::time::Duration::from_millis(10));
  }
  assert!(
    first_marker.is_some_and(|marker| !marker.active),
    "the losing doc's marker must be cleared — one speech doc per session"
  );
  assert_eq!(
    h.read(cx, |ws| ws.speech_document_id),
    Some(second_id),
    "the newest designation stays the winner"
  );
}
