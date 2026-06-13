#[cfg(test)]
mod tests {
  use std::sync::{Arc, Mutex};

  use flowstate_collab::{SessionId, binding::DocBinding, local_apply::LocalApplier, projection, schema};
  use gpui_flowtext::{
    CanonicalOperation, Document, DocumentTheme, InputBlock, InputParagraph, ParagraphStyle, RunStyle, RunStyles, document_from_input_blocks,
    insert_text_at, mutate_runs_in_range, paragraph_text, plain,
  };
  use loro::{ExportMode, LoroDoc, Subscription};

  const MULTIBYTE: &str = "aé🌍\u{2028}x";

  #[test]
  fn two_peers_converge_with_reordered_and_duplicated_update_imports() {
    let session = SessionId::from_bytes([77; 32]);
    let initial = document_from_input_blocks(
      DocumentTheme::default(),
      vec![
        InputBlock::Paragraph(InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain(MULTIBYTE)],
        }),
        InputBlock::Paragraph(InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain("second aé🌍")],
        }),
      ],
    );
    let mut peer_a = Peer::from_initial(session, initial);
    let mut peer_b = Peer::from_snapshot(session, &peer_a.snapshot());

    peer_a.insert_text(0, "a".len(), "A🌍", RunStyles::default().with(RunStyle::Semantic(2)));
    let inserted = "A🌍".len();
    peer_a.set_run_styles(
      0,
      "a".len().."a".len() + inserted,
      RunStyles::default()
        .with(RunStyle::Highlight(4))
        .with_direct_underline(),
    );
    peer_b.insert_text(1, "second ".len(), "Bé", RunStyles::default().with_strikethrough());

    let updates_a = peer_a.updates();
    let updates_b = peer_b.updates();
    assert_eq!(updates_a.len(), 2);
    assert_eq!(updates_b.len(), 1);

    // Import A's second commit before its first commit to exercise Loro's pending-import path,
    // then replay duplicates to assert idempotency.
    import_sequence(&peer_b.loro, [&updates_a[1], &updates_a[1], &updates_a[0], &updates_a[0]]);
    import_sequence(&peer_a.loro, [&updates_b[0], &updates_b[0]]);

    let projected_a = projection::document_from_loro(&peer_a.loro, DocumentTheme::default()).expect("projection A should succeed");
    let projected_b = projection::document_from_loro(&peer_b.loro, DocumentTheme::default()).expect("projection B should succeed");
    assert_documents_eq(&projected_a, &projected_b);
  }

  struct Peer {
    document: Document,
    loro: LoroDoc,
    binding: DocBinding,
    updates: Arc<Mutex<Vec<Vec<u8>>>>,
    _subscription: Subscription,
  }

  impl Peer {
    fn from_initial(session: SessionId, document: Document) -> Self {
      let loro = schema::new_configured_doc();
      projection::populate_from_document(&loro, session, "convergence", &document).expect("populate should succeed");
      let binding = DocBinding::build(&loro, &document).expect("binding should build");
      Self::with_subscription(document, loro, binding)
    }

    fn from_snapshot(session: SessionId, snapshot: &[u8]) -> Self {
      let loro = schema::new_configured_doc();
      loro
        .import(snapshot)
        .expect("snapshot import should succeed");
      projection::verify_lineage(&loro, session).expect("lineage should match");
      let document = projection::document_from_loro(&loro, DocumentTheme::default()).expect("projection should succeed");
      let binding = DocBinding::build(&loro, &document).expect("binding should build");
      Self::with_subscription(document, loro, binding)
    }

    fn with_subscription(document: Document, loro: LoroDoc, binding: DocBinding) -> Self {
      let updates = Arc::new(Mutex::new(Vec::new()));
      let captured = updates.clone();
      let subscription = loro.subscribe_local_update(Box::new(move |bytes| {
        captured
          .lock()
          .expect("update capture lock should not be poisoned")
          .push(bytes.clone());
        true
      }));
      Self {
        document,
        loro,
        binding,
        updates,
        _subscription: subscription,
      }
    }

    fn snapshot(&self) -> Vec<u8> {
      self
        .loro
        .export(ExportMode::Snapshot)
        .expect("snapshot export should succeed")
    }

    fn updates(&self) -> Vec<Vec<u8>> {
      self
        .updates
        .lock()
        .expect("update capture lock should not be poisoned")
        .clone()
    }

    fn insert_text(&mut self, paragraph_ix: usize, byte: usize, text: &str, styles: RunStyles) {
      let paragraph = self.document.ids.paragraph_ids[paragraph_ix];
      insert_text_at(&mut self.document, paragraph_ix, byte, text, styles);
      self.apply(&[CanonicalOperation::InsertText {
        paragraph,
        byte,
        text: text.to_string(),
        styles,
      }]);
    }

    fn set_run_styles(&mut self, paragraph_ix: usize, range: std::ops::Range<usize>, styles: RunStyles) {
      let paragraph = self.document.ids.paragraph_ids[paragraph_ix];
      mutate_runs_in_range(
        &mut self.document,
        gpui_flowtext::DocumentOffset {
          paragraph: paragraph_ix,
          byte: range.start,
        }..gpui_flowtext::DocumentOffset {
          paragraph: paragraph_ix,
          byte: range.end,
        },
        |run_styles| *run_styles = styles,
      );
      self.apply(&[CanonicalOperation::SetRunStyles { paragraph, range, styles }]);
    }

    fn apply(&mut self, ops: &[CanonicalOperation]) {
      LocalApplier {
        doc: &self.loro,
        binding: &mut self.binding,
      }
      .apply(&self.document, ops)
      .expect("local apply should succeed");
    }
  }

  fn import_sequence<'a>(doc: &LoroDoc, updates: impl IntoIterator<Item = &'a Vec<u8>>) {
    for update in updates {
      doc.import(update).expect("update import should succeed");
    }
  }

  fn assert_documents_eq(left: &Document, right: &Document) {
    assert_eq!(left.blocks.len(), right.blocks.len());
    assert_eq!(left.paragraphs.len(), right.paragraphs.len());
    for ix in 0..left.paragraphs.len() {
      assert_eq!(paragraph_text(left, ix), paragraph_text(right, ix));
      assert_eq!(left.paragraphs[ix].style, right.paragraphs[ix].style);
      assert_eq!(left.paragraphs[ix].runs, right.paragraphs[ix].runs);
    }
  }
}
