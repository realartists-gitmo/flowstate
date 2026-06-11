use loro::cursor::Side;

use super::*;

#[test]
fn failing_multi_edit_transaction_does_not_partially_mutate_source() {
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
  let object_id = FlowNodeId::new();
  document
    .create_child_flow_object(
      Role::Owner,
      &document.anchor_in_paragraph_utf8(paragraph, 0, Side::Right).unwrap(),
      object_id,
      b"object",
      FlowId::new(),
      FlowNodeId::new(),
      b"paragraph",
    )
    .unwrap();
  let before = document.materialize().unwrap();

  let result = document.apply_edits(
    Role::Owner,
    &[
      FlowEdit::DeleteObject { object_id },
      FlowEdit::DeleteObject { object_id },
    ],
  );

  assert!(result.is_err());
  assert_eq!(document.materialize().unwrap(), before);
}
