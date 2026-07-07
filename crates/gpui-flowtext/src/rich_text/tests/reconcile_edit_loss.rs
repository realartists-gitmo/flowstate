// Class 4 (editor reconcile half) — when optimistic local edits cannot be replayed onto
// the authoritative canonical projection during a stale-rebase, the editor must SURFACE the
// dropped batches (via `reconciliation_recoveries`) rather than losing content silently.
//
// The field logs showed the app "applying locally then rolling back": a `local-commit-abort
// reason=stale-rebase` re-reads the canonical snapshot and replays the pending optimistic
// edits onto it; any batch that no longer applies is dropped (a content-shrink `rejected`
// count in `rebuild_visible_from_committed`). These tests pin that (a) an un-replayable
// optimistic edit is counted as a recovery (detectable, not silent), and (b) a replayable
// edit is NOT dropped (no false loss on a normal rebase).

use super::*;

fn multi_paragraph_editor(cx: &mut gpui::TestAppContext, paragraphs: usize) -> gpui::Entity<RichTextEditor> {
  let document = document_from_input(
    DocumentTheme::default(),
    (0..paragraphs)
      .map(|ix| InputParagraph {
        style: ParagraphStyle::Normal,
        runs: vec![plain(&format!("paragraph {ix}"))],
      })
      .collect(),
  );
  cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)))
}

/// An optimistic edit whose target paragraph no longer exists in the canonical projection
/// must be dropped AND surfaced as a reconciliation recovery — edit loss is detected.
#[gpui::test]
fn unreplayable_optimistic_edit_is_surfaced_as_a_recovery(cx: &mut gpui::TestAppContext) {
  let editor = multi_paragraph_editor(cx, 4);
  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_session_capture(true);
      // Type into the last paragraph — the captured optimistic edit targets paragraph 3.
      editor.selection = EditorSelection::collapsed(DocumentOffset { paragraph: 3, byte: 0 });
      assert!(editor.insert_single_grapheme_fast_path("z", cx));
      assert!(!editor.take_pending_semantic_edits().is_empty(), "the edit must be captured as pending");
      // Re-capture it (take_pending drained it): redo so a pending batch exists to replay.
      editor.selection = EditorSelection::collapsed(DocumentOffset { paragraph: 3, byte: 0 });
      assert!(editor.insert_single_grapheme_fast_path("z", cx));

      let recoveries_before = editor.reconciliation_recoveries();
      // Rebase onto a canonical projection with only ONE paragraph — the pending edit's
      // paragraph 3 no longer exists, so its replay must fail and be surfaced.
      let canonical = document_from_input(
        DocumentTheme::default(),
        vec![InputParagraph { style: ParagraphStyle::Normal, runs: vec![plain("only paragraph")] }],
      );
      editor.replace_document_projection_replaying_pending(canonical, Vec::new(), None, cx);

      assert!(
        editor.reconciliation_recoveries() > recoveries_before,
        "an un-replayable optimistic edit must be surfaced as a reconciliation recovery, not lost silently"
      );
    });
  });
}

/// A stale-rebase onto a compatible canonical projection must NOT drop a replayable
/// optimistic edit — no false edit loss, and the typed content survives.
#[gpui::test]
fn replayable_optimistic_edit_survives_a_rebase_without_loss(cx: &mut gpui::TestAppContext) {
  let editor = multi_paragraph_editor(cx, 3);
  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_session_capture(true);
      editor.selection = EditorSelection::collapsed(DocumentOffset { paragraph: 0, byte: 0 });
      assert!(editor.insert_single_grapheme_fast_path("Q", cx));

      let recoveries_before = editor.reconciliation_recoveries();
      // Rebase onto a canonical projection with the SAME shape (a concurrent remote edit to
      // a different paragraph) — the pending edit still applies to paragraph 0.
      let canonical = document_from_input(
        DocumentTheme::default(),
        vec![
          InputParagraph { style: ParagraphStyle::Normal, runs: vec![plain("paragraph 0")] },
          InputParagraph { style: ParagraphStyle::Normal, runs: vec![plain("paragraph 1 edited remotely")] },
          InputParagraph { style: ParagraphStyle::Normal, runs: vec![plain("paragraph 2")] },
        ],
      );
      editor.replace_document_projection_replaying_pending(canonical, Vec::new(), None, cx);

      assert_eq!(
        editor.reconciliation_recoveries(),
        recoveries_before,
        "a replayable optimistic edit must not be dropped on a compatible rebase"
      );
      assert!(
        paragraph_text(&editor.document, 0).starts_with('Q'),
        "the optimistic edit must survive the rebase (paragraph 0 = {:?})",
        paragraph_text(&editor.document, 0),
      );
    });
  });
}
