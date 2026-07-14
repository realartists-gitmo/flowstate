//! Tab navigation, pinning, ribbon, speech-document routing, and the
//! full-screen overlays (settings, styles, file search) — each test forces a
//! real headless render of the surface it opens.

use gpui::TestAppContext;

use crate::commands::CommandId;

use super::support;

fn command(h: &support::WorkspaceHarness, cx: &mut TestAppContext, id: CommandId) -> bool {
  let handled = h.update(cx, |ws, window, cx| ws.handle_window_keybinding(id, window, cx));
  cx.run_until_parked();
  handled
}

#[gpui::test]
fn next_and_previous_tab_cycle_through_documents(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  for _ in 0..3 {
    h.new_document(cx);
  }
  let order: Vec<_> = h.read(cx, |ws| {
    ws.document_panels.iter().map(|p| p.entity_id()).collect()
  });
  assert_eq!(order.len(), 3);

  let start = h.read(cx, |ws| ws.active_document_id).expect("active document");
  assert!(command(&h, cx, CommandId::NextTab));
  let next = h.read(cx, |ws| ws.active_document_id).expect("active document");
  assert_ne!(start, next, "NextTab must move activation");
  assert!(command(&h, cx, CommandId::PreviousTab));
  assert_eq!(h.read(cx, |ws| ws.active_document_id), Some(start), "PreviousTab must move back");
}

#[gpui::test]
fn switch_to_tab_shortcuts_activate_by_index(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  h.new_document(cx);
  assert!(command(&h, cx, CommandId::SwitchToTab1));
  let on_first = h.read(cx, |ws| ws.active_document_id);
  assert!(command(&h, cx, CommandId::SwitchToTab2));
  let on_second = h.read(cx, |ws| ws.active_document_id);
  assert!(on_first.is_some() && on_second.is_some());
  assert_ne!(on_first, on_second);
  // Out-of-range index must be harmless.
  assert!(command(&h, cx, CommandId::SwitchToTab9));
}

#[gpui::test]
fn toggle_pin_tab_round_trips(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  let id = h.read(cx, |ws| ws.active_document_id).expect("active document");
  assert!(command(&h, cx, CommandId::TogglePinTab));
  assert!(h.read(cx, |ws| ws.pinned_document_ids.contains(&id)));
  assert!(command(&h, cx, CommandId::TogglePinTab));
  assert!(!h.read(cx, |ws| ws.pinned_document_ids.contains(&id)));
}

#[gpui::test]
fn toggle_ribbon_flips_and_renders(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  let before = h.read(cx, |ws| ws.ribbon_collapsed);
  assert!(command(&h, cx, CommandId::ToggleRibbon));
  assert_ne!(h.read(cx, |ws| ws.ribbon_collapsed), before);
  assert!(command(&h, cx, CommandId::ToggleRibbon));
  assert_eq!(h.read(cx, |ws| ws.ribbon_collapsed), before);
}

#[gpui::test]
fn speech_document_toggle_marks_and_clears(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  let id = h.read(cx, |ws| ws.active_document_id).expect("active document");
  assert!(command(&h, cx, CommandId::ToggleSpeechDocument));
  assert_eq!(h.read(cx, |ws| ws.speech_document_id), Some(id));
  assert!(command(&h, cx, CommandId::ToggleSpeechDocument));
  assert_eq!(h.read(cx, |ws| ws.speech_document_id), None);
}

#[gpui::test]
fn settings_and_styles_overlays_render_headlessly(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.new_document(cx);
  for overlay in [
    super::super::WorkspaceSettingsOverlay::Settings,
    super::super::WorkspaceSettingsOverlay::Styles,
  ] {
    h.update(cx, |ws, _, cx| {
      ws.settings_overlay = Some(overlay);
      cx.notify();
    });
    // run_until_parked draws the frame — a panic in the overlay render
    // (the layer with zero coverage until now) fails the test here.
    cx.run_until_parked();
    h.update(cx, |ws, _, cx| {
      ws.settings_overlay = None;
      cx.notify();
    });
    cx.run_until_parked();
  }
}

#[gpui::test]
fn file_search_overlay_opens_and_closes(cx: &mut TestAppContext) {
  let h = support::open_workspace(cx);
  h.update(cx, |ws, window, cx| ws.open_file_search_overlay(window, cx));
  cx.run_until_parked();
  assert!(h.read(cx, |ws| ws.file_search_overlay.is_some()));
  h.update(cx, |ws, _, cx| ws.close_file_search_overlay(cx));
  cx.run_until_parked();
  assert!(h.read(cx, |ws| ws.file_search_overlay.is_none()));
}
