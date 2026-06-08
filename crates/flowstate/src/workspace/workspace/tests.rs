#[cfg(test)]
mod workspace_tests {
  use super::*;
  use crate::rich_text_element::{DocumentParagraphInput, DocumentRunInput, RunStyles, document_from_paragraphs};

  #[hotpath::measure]
  fn paragraph(style: ParagraphStyle, text: &str) -> DocumentParagraphInput {
    DocumentParagraphInput {
      style,
      runs: vec![DocumentRunInput {
        text: text.to_string(),
        styles: RunStyles::default(),
      }],
    }
  }

  #[test]
  #[hotpath::measure]
  fn outline_label_normalizes_whitespace_without_full_join() {
    let document = document_from_paragraphs(
      DocumentTheme::default(),
      vec![paragraph(flowstate_document::PARAGRAPH_POCKET, "  alpha\t beta\n\n gamma  ")],
    );

    assert_eq!(outline_paragraph_label(&document, 0), "alpha beta gamma");
  }

  #[test]
  #[hotpath::measure]
  fn active_visible_outline_uses_latest_visible_heading_before_caret() {
    let document = document_from_paragraphs(
      DocumentTheme::default(),
      vec![
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Root"),
        paragraph(flowstate_document::PARAGRAPH_HAT, "Child"),
        paragraph(ParagraphStyle::Normal, "Body"),
        paragraph(flowstate_document::PARAGRAPH_BLOCK, "Grandchild"),
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Next"),
      ],
    );
    let nodes = outline_nodes(&document);
    let mut collapsed = HashSet::new();
    collapsed.insert(0);
    let mut visible = Vec::new();
    collect_visible_outline_paragraphs(&nodes, &collapsed, &mut visible);

    assert_eq!(visible, vec![0, 4]);
    assert_eq!(active_visible_outline_paragraph_from_visible(&visible, 3), Some(0));
    assert_eq!(active_visible_outline_paragraph_from_visible(&visible, 4), Some(4));
  }

  #[test]
  #[hotpath::measure]
  fn outline_signature_ignores_non_outline_text_edits() {
    let before = document_from_paragraphs(
      DocumentTheme::default(),
      vec![
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Root"),
        paragraph(ParagraphStyle::Normal, "Body"),
      ],
    );
    let after = document_from_paragraphs(
      DocumentTheme::default(),
      vec![
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Root"),
        paragraph(ParagraphStyle::Normal, "Body with more plain text"),
      ],
    );

    assert!(outline_signature(&before) == outline_signature(&after));
  }

  #[test]
  #[hotpath::measure]
  fn outline_signature_tracks_outline_labels_and_paragraph_count() {
    let before = document_from_paragraphs(
      DocumentTheme::default(),
      vec![
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Root"),
        paragraph(ParagraphStyle::Normal, "Body"),
      ],
    );
    let renamed = document_from_paragraphs(
      DocumentTheme::default(),
      vec![
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Renamed"),
        paragraph(ParagraphStyle::Normal, "Body"),
      ],
    );
    let appended = document_from_paragraphs(
      DocumentTheme::default(),
      vec![
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Root"),
        paragraph(ParagraphStyle::Normal, "Body"),
        paragraph(ParagraphStyle::Normal, "More body"),
      ],
    );

    assert!(outline_signature(&before) != outline_signature(&renamed));
    assert!(outline_signature(&before) != outline_signature(&appended));
  }

  #[test]
  #[hotpath::measure]
  fn db8_collaboration_source_materializes_workspace_document() {
    let mut document = document_from_paragraphs(
      DocumentTheme::default(),
      vec![
        paragraph(flowstate_document::PARAGRAPH_POCKET, "Root"),
        paragraph(ParagraphStyle::Normal, "Body"),
      ],
    );
    let document_id = CollabDocumentId(Uuid::new_v4());
    let (source, assets) = db8_collaboration_source(&mut document, document_id).unwrap();

    assert_eq!(source.document_id(), document_id);
    assert_eq!(source.format_kind(), FormatKind::Db8);
    assert!(source.is_granular());
    assert_eq!(assets.hashes().len(), document.assets.assets.len());
    let text_id = flowstate_collab::granular_record_id_u128(document.ids.paragraph_ids[0].0);
    source
      .insert_granular_text_utf8(Role::Owner, &text_id, 0, "SYNC ")
      .unwrap();
    match collab_document_to_workspace_document(source).unwrap() {
      JoinedWorkspaceDocument::Document(materialized) => {
        assert_eq!(materialized.paragraphs.len(), document.paragraphs.len());
        assert_eq!(materialized.blocks.len(), document.blocks.len());
        assert!(flowstate_document::document_text_slice(&materialized, paragraph_byte_range(&materialized, 0)).starts_with("SYNC "));
      },
      JoinedWorkspaceDocument::Flow(_) => panic!("DB8 source materialized as FL0"),
    }
  }

  #[test]
  #[hotpath::measure]
  fn bounded_pending_collaboration_queue_keeps_source_and_application_entries_distinct() {
    let document_id = CollabDocumentId(Uuid::new_v4());
    let actor_id = ActorId::new();
    let source = CollabDocument::from_projection_source(FormatKind::Db8, document_id, actor_id, b"source", &[]).unwrap();
    let mut queue = VecDeque::new();

    assert!(!push_bounded_pending_collaboration_update(
      &mut queue,
      PendingCollaborationUpdate::Source {
        source,
        application: Some(UpdateApplication::Db8CanonicalOperations(vec![1, 2, 3])),
        hash: Some([7; 32]),
      },
      2,
    ));
    assert!(!push_bounded_pending_collaboration_update(
      &mut queue,
      PendingCollaborationUpdate::Presence {
        cursor: Some("db8:0:4".to_string()),
      },
      2,
    ));

    assert_eq!(queue.len(), 2);
    let Some(PendingCollaborationUpdate::Source { application, hash, .. }) = queue.pop_front() else {
      panic!("expected durable source update first");
    };
    assert!(application.is_some());
    assert_eq!(hash, Some([7; 32]));
    let Some(PendingCollaborationUpdate::Presence { cursor }) = queue.pop_front() else {
      panic!("expected presence update second");
    };
    assert_eq!(cursor, Some("db8:0:4".to_string()));
  }

  #[test]
  #[hotpath::measure]
  fn bounded_pending_collaboration_queue_discards_oldest_when_full() {
    let document_id = CollabDocumentId(Uuid::new_v4());
    let actor_id = ActorId::new();
    let first = CollabDocument::from_projection_source(FormatKind::Db8, document_id, actor_id, b"first", &[]).unwrap();
    let second = CollabDocument::from_projection_source(FormatKind::Db8, document_id, actor_id, b"second", &[]).unwrap();
    let mut queue = VecDeque::new();

    assert!(!push_bounded_pending_collaboration_update(
      &mut queue,
      PendingCollaborationUpdate::Source {
        source: first,
        application: None,
        hash: None,
      },
      2,
    ));
    assert!(!push_bounded_pending_collaboration_update(
      &mut queue,
      PendingCollaborationUpdate::Presence {
        cursor: Some("db8:0:7".to_string()),
      },
      2,
    ));
    assert!(push_bounded_pending_collaboration_update(
      &mut queue,
      PendingCollaborationUpdate::Source {
        source: second,
        application: None,
        hash: None,
      },
      2,
    ));

    assert_eq!(queue.len(), 2);
    let Some(PendingCollaborationUpdate::Presence { cursor }) = queue.pop_front() else {
      panic!("expected oldest source update to be dropped");
    };
    assert_eq!(cursor, Some("db8:0:7".to_string()));
    let Some(PendingCollaborationUpdate::Source { source, .. }) = queue.pop_front() else {
      panic!("expected newest source replacement to remain");
    };
    assert_eq!(source.materialize_projection_cache().unwrap(), b"second");
  }

  #[test]
  #[hotpath::measure]
  fn parse_db8_presence_cursor_accepts_valid_index_byte_payloads() {
    let document = document_from_paragraphs(DocumentTheme::default(), vec![paragraph(ParagraphStyle::Normal, "Body")]);
    assert_eq!(
      resolve_db8_presence_cursor("db8:3:128", &document),
      Some(DocumentOffset { paragraph: 3, byte: 128 })
    );
  }

  #[test]
  #[hotpath::measure]
  fn parse_db8_presence_cursor_rejects_malformed_payloads() {
    let document = document_from_paragraphs(DocumentTheme::default(), vec![paragraph(ParagraphStyle::Normal, "Body")]);
    assert_eq!(resolve_db8_presence_cursor("db8:3", &document), None);
    assert_eq!(resolve_db8_presence_cursor("db8:x:1", &document), None);
    assert_eq!(resolve_db8_presence_cursor("fl0:3:1", &document), None);
    assert_eq!(resolve_db8_presence_cursor("db8:3:1:2", &document), None);
  }

  #[test]
  #[ignore = "target state"]
  fn db8_collaboration_presence_requires_stable_peer_ids() {
    panic!("target state: DB8 presence should be keyed by stable peer/session identifiers, not transient indices and byte payloads");
  }

  #[test]
  #[ignore = "target state"]
  fn db8_collaboration_document_identity_requires_stable_persisted_ids() {
    panic!("target state: collaboration document identity should persist across panel UUID churn");
  }
}
