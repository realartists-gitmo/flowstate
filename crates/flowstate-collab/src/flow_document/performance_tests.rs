use loro::cursor::Side;

use super::*;

#[test]
fn repeated_typed_text_edits_do_not_parse_the_document_flow() {
  let mut document = FlowDocument::new(
    DocumentId::new(),
    ActorId::new(),
    ReplicaId::new(),
    b"paragraph",
  )
  .unwrap();
  let paragraph = document.materialize_flow(document.root_flow_id()).unwrap().nodes[0]
    .record()
    .id;
  let before = document.parse_flow_call_count();

  for byte in 0..64 {
    let at = document.anchor_in_paragraph_utf8(paragraph, byte, Side::Right).unwrap();
    document
      .apply_edits(
        Role::Owner,
        &[FlowEdit::InsertText {
          at,
          text: "x".to_string(),
          marks: Vec::new(),
        }],
      )
      .unwrap();
  }

  assert_eq!(document.parse_flow_call_count(), before);
}

#[test]
fn common_structural_style_and_undo_edits_do_not_parse_the_document_flow() {
  let mut document = FlowDocument::new(
    DocumentId::new(),
    ActorId::new(),
    ReplicaId::new(),
    b"paragraph",
  )
  .unwrap();
  let paragraph = document.materialize_flow(document.root_flow_id()).unwrap().nodes[0]
    .record()
    .id;
  let mut undo = document.new_undo_manager();
  document
    .apply_edits(
      Role::Owner,
      &[FlowEdit::InsertText {
        at: document.anchor_in_paragraph_utf8(paragraph, 0, Side::Right).unwrap(),
        text: "abcd".to_string(),
        marks: Vec::new(),
      }],
    )
    .unwrap();
  let before = document.parse_flow_call_count();

  document
    .apply_edits(
      Role::Owner,
      &[FlowEdit::SetTextMarks {
        start: document.anchor_in_paragraph_utf8(paragraph, 1, Side::Right).unwrap(),
        end: document.anchor_in_paragraph_utf8(paragraph, 3, Side::Left).unwrap(),
        clear_keys: Vec::new(),
        marks: vec![("bold".to_string(), FlowMarkValue::Bool(true))],
      }],
    )
    .unwrap();
  assert_eq!(document.parse_flow_call_count(), before);

  let second = FlowNodeId::new();
  document
    .apply_edits(
      Role::Owner,
      &[FlowEdit::SplitParagraph {
        at: document.anchor_in_paragraph_utf8(paragraph, 2, Side::Right).unwrap(),
        new_paragraph_id: second,
        metadata: b"paragraph".to_vec(),
      }],
    )
    .unwrap();
  assert_eq!(document.parse_flow_call_count(), before);

  document
    .apply_edits(
      Role::Owner,
      &[FlowEdit::JoinParagraph {
        second_paragraph_id: second,
      }],
    )
    .unwrap();
  assert_eq!(document.parse_flow_call_count(), before);

  document.undo(Role::Owner, &mut undo).unwrap().unwrap();
  assert_eq!(document.parse_flow_call_count(), before);
}

#[test]
fn id_targeted_replacement_and_object_deletion_do_not_parse_the_document_flow() {
  let mut document = FlowDocument::new(
    DocumentId::new(),
    ActorId::new(),
    ReplicaId::new(),
    b"paragraph",
  )
  .unwrap();
  let paragraph = document.materialize_flow(document.root_flow_id()).unwrap().nodes[0]
    .record()
    .id;
  document
    .apply_edits(
      Role::Owner,
      &[FlowEdit::InsertText {
        at: document.anchor_in_paragraph_utf8(paragraph, 0, Side::Right).unwrap(),
        text: "old".to_string(),
        marks: Vec::new(),
      }],
    )
    .unwrap();
  let before_replace = document.parse_flow_call_count();
  document
    .apply_edits(
      Role::Owner,
      &[FlowEdit::ReplaceParagraphText {
        paragraph_id: paragraph,
        text: "new".to_string(),
        marks: Vec::new(),
      }],
    )
    .unwrap();
  assert_eq!(document.parse_flow_call_count(), before_replace);

  let object_id = FlowNodeId::new();
  document
    .create_child_flow_object(
      Role::Owner,
      &document.anchor_in_paragraph_utf8(paragraph, 3, Side::Right).unwrap(),
      object_id,
      b"object",
      FlowId::new(),
      FlowNodeId::new(),
      b"paragraph",
    )
    .unwrap();
  let before_delete = document.parse_flow_call_count();
  document
    .apply_edits(Role::Owner, &[FlowEdit::DeleteObject { object_id }])
    .unwrap();
  assert_eq!(document.parse_flow_call_count(), before_delete);
}
