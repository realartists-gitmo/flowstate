use super::*;
use crate::{
  AuthoritativeEditController, AuthoritativeSourceEditRequest, AuthoritativeSourceOperation, AuthoritativeSourcePosition,
  AuthoritativeSourceSelection, DocumentTheme, document_from_paragraphs,
};

fn document_with_text(text: &str) -> Document {
  document_from_paragraphs(
    DocumentTheme::default(),
    vec![DocumentParagraphInput {
      style: ParagraphStyle::Normal,
      runs: vec![DocumentRunInput {
        text: text.to_string(),
        styles: RunStyles::default(),
      }],
    }],
  )
}

#[test]
fn typing_burst_groups_while_structural_edits_remain_isolated() {
  let mut controller =
    Db8DocumentController::from_document(&document_with_text(""), ActorId::new(), ReplicaId::new()).unwrap();
  let paragraph = controller.projection().ids.paragraph_ids[0];
  for (byte, text) in ["a", "b", "c"].into_iter().enumerate() {
    controller
      .apply_intent(
        Role::Owner,
        Db8EditIntent::InsertText {
          at: Db8SourcePosition {
            paragraph_id: paragraph,
            byte,
          },
          text: text.to_string(),
          styles: RunStyles::default(),
        },
      )
      .unwrap();
  }
  let second = ParagraphId(uuid::Uuid::new_v4().as_u128());
  controller
    .apply_intent(
      Role::Owner,
      Db8EditIntent::SplitParagraph {
        at: Db8SourcePosition {
          paragraph_id: paragraph,
          byte: 3,
        },
        new_paragraph_id: second,
        style: ParagraphStyle::Normal,
      },
    )
    .unwrap();

  controller.undo(Role::Owner).unwrap().unwrap();
  assert_eq!(controller.projection().paragraphs.len(), 1);
  assert_eq!(paragraph_text(controller.projection(), 0), "abc");

  controller.undo(Role::Owner).unwrap().unwrap();
  assert_eq!(paragraph_text(controller.projection(), 0), "");
}

#[test]
fn typing_burst_breaks_when_the_cursor_jumps() {
  let mut controller =
    Db8DocumentController::from_document(&document_with_text(""), ActorId::new(), ReplicaId::new()).unwrap();
  let paragraph = controller.projection().ids.paragraph_ids[0];
  controller
    .apply_intent(
      Role::Owner,
      Db8EditIntent::InsertText {
        at: Db8SourcePosition {
          paragraph_id: paragraph,
          byte: 0,
        },
        text: "a".to_string(),
        styles: RunStyles::default(),
      },
    )
    .unwrap();
  controller
    .apply_intent(
      Role::Owner,
      Db8EditIntent::InsertText {
        at: Db8SourcePosition {
          paragraph_id: paragraph,
          byte: 0,
        },
        text: "X".to_string(),
        styles: RunStyles::default(),
      },
    )
    .unwrap();

  controller.undo(Role::Owner).unwrap().unwrap();
  assert_eq!(paragraph_text(controller.projection(), 0), "a");
  controller.undo(Role::Owner).unwrap().unwrap();
  assert_eq!(paragraph_text(controller.projection(), 0), "");
}

#[test]
fn authoritative_undo_and_redo_restore_anchored_selections() {
  let controller =
    Db8DocumentController::from_document(&document_with_text("abcd"), ActorId::new(), ReplicaId::new()).unwrap();
  let paragraph = controller.projection().ids.paragraph_ids[0];
  let mut authority = Db8EditorAuthority::new(controller, Role::Owner);
  let before = collapsed_selection(paragraph, 2);
  let after = collapsed_selection(paragraph, 3);

  let response = authority.apply_source(AuthoritativeSourceEditRequest {
    selection_before: before,
    planned_selection: after,
    operations: vec![AuthoritativeSourceOperation::InsertText {
      at: before.head,
      text: "X".to_string(),
      styles: RunStyles::default(),
    }],
  });
  assert_eq!(response.projection.selection.unwrap().head, DocumentOffset { paragraph: 0, byte: 3 });

  let response = authority.undo(after);
  assert_eq!(paragraph_text(authority.controller().projection(), 0), "abcd");
  assert_eq!(response.projection.selection.unwrap().head, DocumentOffset { paragraph: 0, byte: 2 });

  let response = authority.redo(before);
  assert_eq!(paragraph_text(authority.controller().projection(), 0), "abXcd");
  assert_eq!(response.projection.selection.unwrap().head, DocumentOffset { paragraph: 0, byte: 3 });
}

#[test]
fn grouped_typing_restores_the_burst_boundary_selections() {
  let controller =
    Db8DocumentController::from_document(&document_with_text(""), ActorId::new(), ReplicaId::new()).unwrap();
  let paragraph = controller.projection().ids.paragraph_ids[0];
  let mut authority = Db8EditorAuthority::new(controller, Role::Owner);
  for (byte, text) in ["a", "b", "c"].into_iter().enumerate() {
    authority.apply_source(AuthoritativeSourceEditRequest {
      selection_before: collapsed_selection(paragraph, byte),
      planned_selection: collapsed_selection(paragraph, byte + 1),
      operations: vec![AuthoritativeSourceOperation::InsertText {
        at: AuthoritativeSourcePosition { paragraph, byte },
        text: text.to_string(),
        styles: RunStyles::default(),
      }],
    });
  }

  let response = authority.undo(collapsed_selection(paragraph, 3));
  assert_eq!(paragraph_text(authority.controller().projection(), 0), "");
  assert_eq!(response.projection.selection.unwrap().head, DocumentOffset { paragraph: 0, byte: 0 });

  let response = authority.redo(collapsed_selection(paragraph, 0));
  assert_eq!(paragraph_text(authority.controller().projection(), 0), "abc");
  assert_eq!(response.projection.selection.unwrap().head, DocumentOffset { paragraph: 0, byte: 3 });
}

#[test]
fn undo_selection_cursors_track_concurrent_remote_edits() {
  let left =
    Db8DocumentController::from_document(&document_with_text("abcd"), ActorId::new(), ReplicaId::new()).unwrap();
  let paragraph = left.projection().ids.paragraph_ids[0];
  let document_id = left.source().document_id();
  let snapshot = left.source().export_snapshot().unwrap();
  let right_source = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  let mut right = Db8DocumentController::from_source(right_source, AssetStore::default()).unwrap();
  let mut authority = Db8EditorAuthority::new(left, Role::Owner);

  authority.apply_source(AuthoritativeSourceEditRequest {
    selection_before: collapsed_selection(paragraph, 2),
    planned_selection: collapsed_selection(paragraph, 3),
    operations: vec![AuthoritativeSourceOperation::InsertText {
      at: AuthoritativeSourcePosition { paragraph, byte: 2 },
      text: "X".to_string(),
      styles: RunStyles::default(),
    }],
  });
  let remote = right
    .apply_intent(
      Role::Owner,
      Db8EditIntent::InsertText {
        at: Db8SourcePosition {
          paragraph_id: paragraph,
          byte: 0,
        },
        text: "R".to_string(),
        styles: RunStyles::default(),
      },
    )
    .unwrap();
  authority
    .apply_remote_update(right.source().peer_id(), &remote.source.update)
    .unwrap();

  let response = authority.undo(collapsed_selection(paragraph, 4));
  assert_eq!(paragraph_text(authority.controller().projection(), 0), "Rabcd");
  assert_eq!(response.projection.selection.unwrap().head, DocumentOffset { paragraph: 0, byte: 3 });
}

fn collapsed_selection(paragraph: ParagraphId, byte: usize) -> AuthoritativeSourceSelection {
  let position = AuthoritativeSourcePosition { paragraph, byte };
  AuthoritativeSourceSelection {
    anchor: position,
    head: position,
  }
}
