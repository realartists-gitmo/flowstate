use std::{cell::RefCell, rc::Rc};

use gpui::AppContext as _;

struct SelectionEventRecorder {
  selections: Rc<RefCell<Vec<EditorSelection>>>,
  _subscription: gpui::Subscription,
}

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
    let [SemanticEditCommand::InsertText { at, text, .. }] = edit.semantic_commands.as_slice() else {
      panic!("expected one semantic InsertText command, got {:?}", edit.semantic_commands);
    };
    assert_eq!(text, expected_text);
    assert_eq!(
      *at,
      DocumentOffset {
        paragraph: 0,
        byte: expected_byte,
      }
    );
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
  assert!(matches!(
    paste_edits[0].semantic_commands.as_slice(),
    [SemanticEditCommand::ReplaceParagraphSpan { .. }]
  ));

  let edits_after_paste_undo = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.undo(cx);
      editor.take_pending_collab_edits()
    })
  });
  assert!(edits_after_paste_undo.is_empty());
}

#[gpui::test]
fn applying_collab_patches_does_not_arm_local_caret_scroll(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      assert!(!editor.pending_scroll_head_after_layout_for_test());
      let before_generation = editor.edit_generation();
      editor.apply_collab_patches(
        &[CollabPatch::ParagraphText {
          row: 0,
          new: InputParagraph {
            style: ParagraphStyle::Normal,
            runs: vec![plain("remote")],
          },
          delta_utf8: vec![CollabTextDelta::Insert("remote".len())],
        }],
        cx,
      );
      assert!(!editor.pending_scroll_head_after_layout_for_test());
      assert!(editor.edit_generation() > before_generation);
    });
  });
}

#[gpui::test]
fn own_collaboration_caret_color_can_be_toggled_off(cx: &mut gpui::TestAppContext) {
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(blank_document(), None, cx)));

  cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_own_collaboration_caret_color(Some(0x3b82f6), cx);
      assert_eq!(editor.local_caret_color_rgb(), Some(0x3b82f6));
      editor.set_show_own_collaboration_caret_color(false, cx);
      assert_eq!(editor.local_caret_color_rgb(), None);
      editor.set_show_own_collaboration_caret_color(true, cx);
      assert_eq!(editor.local_caret_color_rgb(), Some(0x3b82f6));
    });
  });
}

#[gpui::test]
fn text_entry_in_selected_equation_updates_equation_only(cx: &mut gpui::TestAppContext) {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("body")],
    }],
  );
  document.blocks = std::sync::Arc::new(vec![
    Block::Paragraph(document.paragraphs[0].clone()),
    Block::Equation(EquationBlock {
      source: "x".into(),
      syntax: EquationSyntax::Latex,
      display: EquationDisplay::Display,
      version: 0,
    }),
  ]);
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));

  let (document, edits) = cx.update(|cx| {
    editor.update(cx, |editor, cx| {
      editor.set_collab_capture(true);
      editor.select_equation_block_for_test(1, cx);
      editor.insert_plain_text_from_toolkit("+1", cx);
      editor.replace_selected_text_from_platform_for_test("+2", cx);
      (editor.document().clone(), editor.take_pending_collab_edits())
    })
  });

  assert_eq!(paragraph_text(&document, 0), "body");
  let Block::Equation(equation) = &document.blocks[1] else {
    panic!("expected equation block after toolkit text insert");
  };
  assert_eq!(equation.source.as_ref(), "x+1+2");
  assert_eq!(edits.len(), 2);
  for edit in edits {
    assert!(matches!(
      edit.semantic_commands.as_slice(),
      [SemanticEditCommand::ReplaceBlock { .. }]
    ));
  }
}

#[gpui::test]
fn select_all_emits_selection_changed_once(cx: &mut gpui::TestAppContext) {
  let document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![plain("alpha beta")],
    }],
  );
  let editor = cx.update(|cx| cx.new(|cx| RichTextEditor::new_with_path(document, None, cx)));
  let selections = Rc::new(RefCell::new(Vec::new()));
  let recorder_selections = selections.clone();
  let _recorder = cx.update(|cx| {
    let editor = editor.clone();
    cx.new(|cx| SelectionEventRecorder {
      selections: recorder_selections,
      _subscription: cx.subscribe(&editor, |recorder: &mut SelectionEventRecorder, _, event: &EditorEvent, _| {
        if let EditorEvent::SelectionChanged { selection } = event {
          recorder.selections.borrow_mut().push(selection.clone());
        }
      }),
    })
  });

  cx.update(|cx| editor.update(cx, |editor, cx| editor.select_all(cx)));
  let first_events = selections.borrow();
  assert_eq!(first_events.len(), 1);
  assert_eq!(
    first_events[0].normalized(),
    DocumentOffset { paragraph: 0, byte: 0 }..DocumentOffset {
      paragraph: 0,
      byte: "alpha beta".len(),
    }
  );
  drop(first_events);

  cx.update(|cx| editor.update(cx, |editor, cx| editor.select_all(cx)));
  assert_eq!(selections.borrow().len(), 1);
}
