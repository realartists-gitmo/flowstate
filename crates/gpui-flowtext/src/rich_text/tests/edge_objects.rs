// B-S1: select-all + copy must keep DOCUMENT-EDGE objects. Text selections
// live in paragraph space, so a leading/trailing image or equation could never
// satisfy the old strictly-between fragment test — copy/cut silently dropped
// them.

#[gpui::test]
fn select_all_copy_keeps_edge_objects(cx: &mut gpui::TestAppContext) {
  let mut document = document_from_input(
    DocumentTheme::default(),
    vec![InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![InputRun {
        text: "middle paragraph".into(),
        styles: RunStyles::default(),
      }],
    }],
  );
  fn equation(source: &str) -> Block {
    Block::Equation(EquationBlock {
      source: source.to_string().into(),
      syntax: EquationSyntax::Latex,
      display: EquationDisplay::Display,
      version: 0,
    })
  }
  let paragraph_block = document.blocks.get(0).expect("paragraph block").clone();
  document.blocks = BlockSeq::from_vec(vec![equation("lead"), paragraph_block, equation("tail")]);
  document.ids = document_ids_for_shape(1, 3);
  rebuild_document_sections(&mut document);

  cx.update(gpui_component::init);
  let handle = cx.add_window(|_window, cx| RichTextEditor::new_with_path(document, None, cx));
  let fragment = handle
    .update(cx, |editor, _window, cx| {
      editor.select_all(cx);
      editor.copy(cx);
      cx.read_from_clipboard()
        .and_then(|item| item.metadata().cloned())
        .and_then(|metadata| serde_json::from_str::<RichClipboardFragment>(&metadata).ok())
    })
    .expect("window update")
    .expect("copy must write a rich fragment when objects are in range");

  let equations = fragment
    .blocks
    .iter()
    .filter(|block| matches!(block, InputBlock::Equation(_)))
    .count();
  assert_eq!(equations, 2, "both document-edge equations must survive select-all copy");
  assert!(
    fragment
      .blocks
      .iter()
      .any(|block| matches!(block, InputBlock::Paragraph(_))),
    "the paragraph travels too"
  );
}
