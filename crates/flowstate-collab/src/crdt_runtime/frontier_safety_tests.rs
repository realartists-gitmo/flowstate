//! Class 4 (runtime core) — a commit against a STALE base frontier must be cleanly
//! rejected WITHOUT losing or corrupting already-committed content.
//!
//! The field logs showed the app "applying locally then rolling back" via
//! `local-commit-abort reason=stale-rebase` (session layer): a local optimistic edit whose
//! base frontier had moved under it (a concurrent remote import landed) is rejected and
//! rebased. The session's rebase is only safe if the RUNTIME guarantees that a stale commit
//! (a) is rejected as `StaleProjectionError` and (b) does not mutate the document — so the
//! concurrent content it raced against survives intact. These tests pin that guarantee; the
//! editor-side edit-loss/rebase half is covered by the gpui reconcile harness.

use anyhow::Result;
use flowstate_document::{DocumentOffset, RunStyles, new_loro_document};
use gpui_flowtext::SemanticEditCommand as EditorSemanticCommand;
use loro::{ExportMode, LoroDoc};

use super::{CrdtRuntime, RuntimeEvent, StaleProjectionError};

fn insert(paragraph: usize, byte: usize, text: &str) -> EditorSemanticCommand {
  EditorSemanticCommand::InsertText {
    at: DocumentOffset { paragraph, byte },
    text: text.to_string(),
    styles: RunStyles::default(),
  }
}

fn local_updates(events: &[RuntimeEvent]) -> Vec<Vec<u8>> {
  events
    .iter()
    .filter_map(|event| match event {
      RuntimeEvent::LocalUpdate { bytes, .. } => Some(bytes.clone()),
      _ => None,
    })
    .collect()
}

/// After a local commit advances the frontier, a second commit carrying the pre-commit
/// (stale) base frontier must be rejected as `StaleProjectionError`, and the first commit's
/// content must survive unclobbered.
#[test]
fn stale_local_base_frontier_is_rejected_without_losing_committed_content() -> Result<()> {
  let mut runtime = CrdtRuntime::from_doc(new_loro_document("stale local")?, None, None)?;
  let base = runtime.projection_snapshot()?;

  runtime.apply_editor_commands(1, &base.frontier, &[insert(0, 0, "AAA")], None)?;
  let after_first = runtime.projection_snapshot()?;
  assert_eq!(flowstate_document::paragraph_text(&after_first, 0), "AAA");
  assert_ne!(after_first.frontier, base.frontier, "the first commit must advance the frontier");

  // Second commit reuses the now-STALE base frontier.
  let stale = runtime.apply_editor_commands(2, &base.frontier, &[insert(0, 0, "BBB")], None);
  let error = stale.expect_err("a commit against a stale base frontier must be rejected");
  assert!(
    error.downcast_ref::<StaleProjectionError>().is_some(),
    "stale commit must fail with StaleProjectionError, got {error:?}"
  );

  let after_reject = runtime.projection_snapshot()?;
  assert_eq!(
    flowstate_document::paragraph_text(&after_reject, 0),
    "AAA",
    "a rejected stale commit must not mutate the document — committed content must survive"
  );
  assert_eq!(after_reject.frontier, after_first.frontier, "a rejected commit must not advance the frontier");
  Ok(())
}

/// A concurrent REMOTE import advances the frontier under a local editor; a local commit
/// still carrying the pre-import base frontier must be rejected, and the remote content must
/// survive (the exact race that produced `reason=stale-rebase` in the field).
#[test]
fn remote_import_makes_local_base_stale_and_remote_content_survives() -> Result<()> {
  let base_doc = new_loro_document("stale remote")?;
  let snapshot = base_doc.export(ExportMode::Snapshot)?;
  let doc_b = LoroDoc::new();
  doc_b.set_peer_id(0x2000)?;
  doc_b.import(&snapshot)?;

  let mut a = CrdtRuntime::from_doc(base_doc, None, None)?;
  let mut b = CrdtRuntime::from_doc(doc_b, None, None)?;

  // Runtime construction commits per-replica registration ops; exchange them so a later
  // edit-update from A does not park as pending (missing dependency) on B.
  for _ in 0..2 {
    let to_b = a.export_updates_for(&b.doc().state_vv())?;
    if !to_b.is_empty() {
      b.import_remote_update(&to_b)?;
    }
    let to_a = b.export_updates_for(&a.doc().state_vv())?;
    if !to_a.is_empty() {
      a.import_remote_update(&to_a)?;
    }
  }

  // B captures its base frontier BEFORE the remote edit arrives.
  let b_base = b.projection_snapshot()?;

  // A edits and B imports it — B's frontier now advances under the still-open local base.
  let a_base = a.projection_snapshot()?;
  let commit = a.apply_editor_commands(1, &a_base.frontier, &[insert(0, 0, "REMOTE")], None)?;
  for update in local_updates(&commit.events) {
    b.import_remote_update(&update)?;
  }
  let b_after_import = b.projection_snapshot()?;
  assert_eq!(flowstate_document::paragraph_text(&b_after_import, 0), "REMOTE");
  assert_ne!(b_after_import.frontier, b_base.frontier, "the remote import must advance B's frontier");

  // B's local commit still uses the stale pre-import base frontier.
  let stale = b.apply_editor_commands(2, &b_base.frontier, &[insert(0, 0, "LOCAL")], None);
  assert!(
    stale.expect_err("stale local commit must be rejected").downcast_ref::<StaleProjectionError>().is_some(),
    "stale local commit racing a remote import must fail with StaleProjectionError"
  );

  let b_final = b.projection_snapshot()?;
  assert_eq!(
    flowstate_document::paragraph_text(&b_final, 0),
    "REMOTE",
    "the remote content must survive a rejected stale local commit"
  );
  Ok(())
}

/// A fresh (non-stale) commit on B after it re-reads the advanced frontier must succeed and
/// preserve the remote content — proving the rejection is recoverable, not a dead end (the
/// session rebases against the fresh snapshot exactly this way).
#[test]
fn recommitting_against_the_fresh_frontier_succeeds_after_a_stale_rejection() -> Result<()> {
  let mut runtime = CrdtRuntime::from_doc(new_loro_document("recommit")?, None, None)?;
  let base = runtime.projection_snapshot()?;
  runtime.apply_editor_commands(1, &base.frontier, &[insert(0, 0, "AAA")], None)?;

  // Stale attempt rejected.
  assert!(runtime.apply_editor_commands(2, &base.frontier, &[insert(0, 0, "BBB")], None).is_err());

  // Re-read the fresh frontier and retry — must succeed and preserve "AAA".
  let fresh = runtime.projection_snapshot()?;
  runtime.apply_editor_commands(3, &fresh.frontier, &[insert(0, 0, "BBB")], None)?;
  let text = flowstate_document::paragraph_text(&runtime.projection_snapshot()?, 0);
  assert_eq!(text, "BBBAAA", "the rebased retry must apply on top of the committed content");
  Ok(())
}
