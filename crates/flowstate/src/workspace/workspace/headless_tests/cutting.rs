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
