//! Logic-level performance guards: a single-character edit on a large document
//! must produce a Loro update and a remote reconcile proportional to the edit,
//! not to the whole document. These assert structure/size, never wall-clock, so
//! they stay deterministic in CI and guard against regressing the incremental
//! local apply (T2) and delta-driven remote reconcile (T4).

#[cfg(test)]
mod tests {
  use std::sync::{Arc, Mutex};

  use flowstate_collab::{SessionId, binding::DocBinding, local_apply::LocalApplier, patch_apply::apply_patches, projection, schema};
  use gpui_flowtext::{
    CanonicalOperation, CollabPatch, Document, DocumentTheme, InputBlock, InputParagraph, ParagraphStyle, RunStyles, capture_document_span,
    document_from_input_blocks, insert_text_at, paragraph_text, plain,
  };
  use loro::{ExportMode, LoroDoc, event::Subscriber};

  const PARAGRAPHS: usize = 400;

  #[test]
  fn single_char_local_edit_emits_a_delta_proportional_to_the_edit() {
    let session = SessionId::from_bytes([42; 32]);
    let document = large_document();
    let loro = schema::new_configured_doc();
    projection::populate_from_document(&loro, session, "perf-smoke", &document).expect("populate");
    let mut binding = DocBinding::build(&loro, &document).expect("binding");

    let updates = Arc::new(Mutex::new(Vec::new()));
    let captured = updates.clone();
    let _subscription = loro.subscribe_local_update(Box::new(move |bytes| {
      captured.lock().expect("update lock").push(bytes.to_vec());
      true
    }));

    // Edit a paragraph in the middle of the document with a single keystroke,
    // emitted exactly as the editor would: a span replacement of that paragraph.
    let target = PARAGRAPHS / 2;
    let mut edited = document.clone();
    let before = capture_document_span(&edited, target..target + 1);
    insert_text_at(&mut edited, target, 0, "X", RunStyles::default());
    let after = capture_document_span(&edited, target..target + 1);
    let op = CanonicalOperation::ReplaceParagraphSpan {
      start_paragraph: Some(edited.ids.paragraph_ids[target]),
      before,
      after,
    };

    LocalApplier {
      doc: &loro,
      binding: &mut binding,
    }
    .apply(&edited, &[op])
    .expect("local apply");

    let emitted: usize = updates
      .lock()
      .expect("update lock")
      .iter()
      .map(Vec::len)
      .sum();
    let body_bytes = full_body_len(&document);
    // The whole-document body is large; a single-character incremental splice must
    // stay far below it (a full-body rewrite would be >= body_bytes).
    assert!(body_bytes > 4000, "fixture should be large, got {body_bytes} body bytes");
    assert!(
      emitted * 4 < body_bytes,
      "single-char edit emitted {emitted} update bytes for a {body_bytes}-byte body; expected an edit-proportional delta",
    );

    // The edit must still converge: a fresh peer importing the update matches.
    let peer = schema::new_configured_doc();
    let snapshot = loro.export(ExportMode::Snapshot).expect("snapshot");
    let _ = peer.import(&snapshot);
    let projected = projection::document_from_loro(&loro, DocumentTheme::default()).expect("project");
    assert_eq!(paragraph_text(&projected, target), format!("X{}", paragraph_text(&document, target)));
  }

  #[test]
  fn remote_single_char_insert_reconciles_one_paragraph() {
    let session = SessionId::from_bytes([43; 32]);
    let document = large_document();

    let source_loro = schema::new_configured_doc();
    projection::populate_from_document(&source_loro, session, "perf-smoke", &document).expect("populate");
    let mut source_binding = DocBinding::build(&source_loro, &document).expect("binding");
    let updates = Arc::new(Mutex::new(Vec::new()));
    let captured = updates.clone();
    let _subscription = source_loro.subscribe_local_update(Box::new(move |bytes| {
      captured.lock().expect("update lock").push(bytes.to_vec());
      true
    }));
    let snapshot = source_loro.export(ExportMode::Snapshot).expect("snapshot");

    let target_loro = schema::new_configured_doc();
    target_loro.import(&snapshot).expect("import snapshot");
    let mut target_document = projection::document_from_loro(&target_loro, DocumentTheme::default()).expect("project");
    let mut target_binding = DocBinding::build(&target_loro, &target_document).expect("binding");

    // Source makes a single-character insert in the middle paragraph.
    let target = PARAGRAPHS / 2;
    let mut edited = document.clone();
    let before = capture_document_span(&edited, target..target + 1);
    insert_text_at(&mut edited, target, 0, "X", RunStyles::default());
    let after = capture_document_span(&edited, target..target + 1);
    LocalApplier {
      doc: &source_loro,
      binding: &mut source_binding,
    }
    .apply(
      &edited,
      &[CanonicalOperation::ReplaceParagraphSpan {
        start_paragraph: Some(edited.ids.paragraph_ids[target]),
        before,
        after,
      }],
    )
    .expect("local apply");
    let update_batches = std::mem::take(&mut *updates.lock().expect("update lock"));

    let patches = import_updates(&target_loro, &mut target_document, &mut target_binding, &update_batches);

    // The remote reconcile must touch exactly the one changed paragraph row, not
    // re-emit a patch per paragraph in the document.
    assert_eq!(patches.len(), 1, "expected one reconcile patch, got {}: {patches:?}", patches.len());
    assert!(
      matches!(patches.as_slice(), [CollabPatch::ParagraphText { row, .. }] if *row == target),
      "expected a single ParagraphText patch for row {target}, got {patches:?}",
    );

    // And the imported document converges to the source projection.
    let source_projection = projection::document_from_loro(&source_loro, DocumentTheme::default()).expect("project");
    assert_eq!(paragraph_text(&target_document, target), paragraph_text(&source_projection, target));
    assert_eq!(target_document.paragraphs.len(), source_projection.paragraphs.len());
  }

  fn large_document() -> Document {
    let blocks = (0..PARAGRAPHS)
      .map(|ix| {
        InputBlock::Paragraph(InputParagraph {
          style: ParagraphStyle::Normal,
          runs: vec![plain(&format!("paragraph number {ix} body text"))],
        })
      })
      .collect();
    document_from_input_blocks(DocumentTheme::default(), blocks)
  }

  fn full_body_len(document: &Document) -> usize {
    (0..document.paragraphs.len())
      .map(|ix| paragraph_text(document, ix).len() + 1)
      .sum()
  }

  fn import_updates(loro: &LoroDoc, document: &mut Document, binding: &mut DocBinding, updates: &[Vec<u8>]) -> Vec<CollabPatch> {
    let mut all = Vec::new();
    for update in updates {
      let snapshot_document = Arc::new(document.clone());
      let shared_binding = Arc::new(Mutex::new(std::mem::take(binding)));
      let patches = Arc::new(Mutex::new(Vec::new()));
      let doc = loro.clone();
      let callback: Subscriber = Arc::new({
        let shared_binding = shared_binding.clone();
        let patches = patches.clone();
        move |event| {
          let produced = {
            let mut guard = shared_binding.lock().expect("binding lock");
            flowstate_collab::remote_apply::RemoteApplier {
              doc: &doc,
              binding: &mut guard,
            }
            .apply_event(&snapshot_document, &event)
            .expect("remote apply")
          };
          patches.lock().expect("patch lock").extend(produced);
        }
      });
      let subscription = loro.subscribe_root(callback);
      loro.import_with(update, "remote").expect("import");
      drop(subscription);
      *binding = Arc::try_unwrap(shared_binding)
        .expect("binding owner")
        .into_inner()
        .expect("binding lock");
      let produced = Arc::try_unwrap(patches)
        .expect("patch owner")
        .into_inner()
        .expect("patch lock");
      apply_patches(document, binding, loro, &produced).expect("apply patches");
      all.extend(produced);
    }
    all
  }
}
