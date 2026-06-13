use gpui::AppContext as _;

#[gpui::test]
fn collab_capture_fast_path_emits_single_grapheme_deltas(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  let edits = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_collab_capture(true);
      assert!(editor.insert_single_grapheme_fast_path("a", cx));
      assert!(editor.insert_single_grapheme_fast_path("b", cx));
      assert!(editor.insert_single_grapheme_fast_path("c", cx));
      editor.take_pending_collab_edits()
    })
  });

  assert_eq!(edits.len(), 3);
  for (edit, (expected_text, expected_byte)) in edits.iter().zip([("a", 0), ("b", 1), ("c", 2)]) {
    let [CanonicalOperation::InsertText { byte, text, .. }] = edit.operations.as_slice() else {
      panic!("expected one InsertText op, got {:?}", edit.operations);
    };
    assert_eq!(text, expected_text);
    assert_eq!(*byte, expected_byte);
  }

  let edits_after_undo = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.undo(cx);
      editor.take_pending_collab_edits()
    })
  });
  assert!(edits_after_undo.is_empty());

  let paste_edits = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      let fragment = RichClipboardFragment {
        format: RICH_TEXT_CLIPBOARD_FORMAT.to_string(),
        paragraphs: vec![InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain("paste")],
        }],
        blocks: Vec::new(),
        assets: Vec::new(),
      };
      assert!(editor.insert_rich_fragment_paste_at_caret(&fragment, cx));
      editor.take_pending_collab_edits()
    })
  });
  assert_eq!(paste_edits.len(), 1);
  assert!(matches!(paste_edits[0].operations.as_slice(), [CanonicalOperation::ReplaceParagraphSpan { .. }]));

  let edits_after_paste_undo = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.undo(cx);
      editor.take_pending_collab_edits()
    })
  });
  assert!(edits_after_paste_undo.is_empty());
}
