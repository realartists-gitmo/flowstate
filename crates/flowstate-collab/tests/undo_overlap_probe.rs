//! §A14.3.2 frequency probe: is the slow-path undo hit by MARK-overlap
//! (rewritable, worth fixing) or only by destructive CONTENT/structural
//! overlap (a genuine Loro checkout-diff floor)?

mod tests {
  use flowstate_collab::crdt_runtime::CrdtRuntime;
  use flowstate_collab::local_write::{GateHolder, InsertTextIntent, LocalDocHandle, LocalWriteConfig, SetParagraphStylesIntent, TextAnchor};
  use flowstate_document::{ParagraphStyle, loro_schema::body_text};
  use loro::LoroDoc;

  fn seeded() -> (
    LocalDocHandle,
    std::sync::Arc<flowstate_collab::local_write::WriteGate<CrdtRuntime>>,
    LoroDoc,
  ) {
    let core = CrdtRuntime::new_empty("undo overlap").expect("runtime");
    let (handle, gate) = LocalDocHandle::new(core, LocalWriteConfig::default());
    let para = handle.projection().expect("projection").ids.paragraph_ids[0];
    for i in 0..200 {
      handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(para, usize::MAX),
          text: format!("word{i} "),
          style_override: None,
        })
        .expect("seed");
    }
    let peer = {
      let g = gate.lock(GateHolder::ExportUpdates).expect("gate");
      let doc = LoroDoc::new();
      doc
        .import(
          &g.doc()
            .export(loro::ExportMode::updates(&loro::VersionVector::default()))
            .expect("export"),
        )
        .expect("peer boot");
      doc
    };
    (handle, gate, peer)
  }

  fn push_peer(gate: &std::sync::Arc<flowstate_collab::local_write::WriteGate<CrdtRuntime>>, peer: &LoroDoc, from: &loro::VersionVector) {
    let update = peer
      .export(loro::ExportMode::updates(from))
      .expect("peer export");
    gate
      .lock(GateHolder::ImportChunk)
      .expect("gate")
      .import_remote_update(&update)
      .expect("import");
  }

  #[test]
  fn mark_overlap_undo_timing() {
    // A restyles a paragraph range; B applies a DIFFERENT mark to an
    // overlapping range; A undoes. Is A's undo fast (rebasable marks) or slow?
    let (handle, gate, peer) = seeded();
    let ids = handle
      .projection()
      .expect("projection")
      .ids
      .paragraph_ids
      .clone();
    handle
      .set_paragraph_styles(SetParagraphStylesIntent {
        paragraphs: ids.to_vec(),
        style: ParagraphStyle::Custom(3),
      })
      .expect("A restyle");
    // B: a mark on the body (bold-ish via a run style) overlapping A's range.
    let from = peer.oplog_vv();
    let body = body_text(&peer);
    let _ = body.mark(0..50, "bold", loro::LoroValue::Bool(true));
    peer.commit();
    push_peer(&gate, &peer, &from);
    let t = std::time::Instant::now();
    let outcome = handle.apply_undo().expect("undo");
    eprintln!("[undo-overlap] mark_overlap undo: {:?} applied={}", t.elapsed(), outcome.applied);
  }

  #[test]
  fn content_overlap_undo_timing() {
    // Baseline: content insert overlapping A's restyled range (the bench shape).
    let (handle, gate, peer) = seeded();
    let ids = handle
      .projection()
      .expect("projection")
      .ids
      .paragraph_ids
      .clone();
    handle
      .set_paragraph_styles(SetParagraphStylesIntent {
        paragraphs: ids.to_vec(),
        style: ParagraphStyle::Custom(3),
      })
      .expect("A restyle");
    let from = peer.oplog_vv();
    let body = body_text(&peer);
    body.insert(5, "INJECT").expect("peer insert");
    peer.commit();
    push_peer(&gate, &peer, &from);
    let t = std::time::Instant::now();
    let outcome = handle.apply_undo().expect("undo");
    eprintln!("[undo-overlap] content_overlap undo: {:?} applied={}", t.elapsed(), outcome.applied);
  }
}
