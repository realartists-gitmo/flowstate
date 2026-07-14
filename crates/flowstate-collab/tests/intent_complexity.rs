//! Complexity-shape counter assertions (Loro-first spec §11/§13.10, I-8).
//!
//! Counts, not clocks: a single-character insert into a LARGE document must
//! touch O(1) containers and emit O(inserted-bytes) work — never per-run,
//! per-paragraph, or per-block work. These assertions cannot be satisfied by
//! faster hardware or left-on-the-table optimizations; they pin the complexity
//! class itself. Wall-clock ratchet ceilings live in the release perf harness
//! (env-gated), not here.

#[cfg(test)]
mod tests {
  use flowstate_collab::crdt_runtime::CrdtRuntime;
  use flowstate_collab::local_write::{
    DeleteRangeIntent, InsertObjectIntent, InsertTextIntent, JoinParagraphsIntent, LocalDocHandle, LocalWriteConfig, SetMarksIntent,
    SetParagraphStylesIntent, SplitParagraphIntent, TextAnchor,
  };
  use flowstate_document::{
    AssetId, InputBlock, InputBlockAlignment, InputImageBlock, InputImageSizing, ParagraphStyle, RunStyles, block_ix_for_paragraph,
    block_ix_scan_count, reset_block_ix_scan_count,
  };

  /// Paragraph count for the "large" fixture. Kept CI-friendly in debug builds
  /// (the always-on debug audit full-rebuilds per commit by design); the shape
  /// assertions are size-independent, which is exactly the point.
  const PARAGRAPHS: usize = 400;

  fn large_fixture() -> LocalDocHandle {
    let core = CrdtRuntime::new_empty("complexity").expect("runtime");
    let (handle, _gate) = LocalDocHandle::new(core, LocalWriteConfig::default());
    let mut paragraph = handle.projection().expect("projection").ids.paragraph_ids[0];
    for i in 0..PARAGRAPHS - 1 {
      handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(paragraph, usize::MAX),
          text: format!("paragraph {i} body text with several words"),
          style_override: None,
        })
        .expect("seed insert");
      let outcome = handle
        .split_paragraph(SplitParagraphIntent {
          at: TextAnchor::new(paragraph, usize::MAX),
          inherited_style: ParagraphStyle::Normal,
        })
        .expect("seed split");
      let projection = match outcome {
        flowstate_collab::local_write::LocalWriteOutcome::Committed(_) => handle.projection().expect("projection"),
        flowstate_collab::local_write::LocalWriteOutcome::CommittedWithRebuild { .. } => handle.projection().expect("projection"),
      };
      paragraph = *projection.ids.paragraph_ids.last().expect("last paragraph");
    }
    handle
  }

  /// Like [`large_fixture`] but with image objects interspersed every
  /// `OBJECT_EVERY` paragraphs, so `blocks.len() != paragraphs.len()` and the
  /// O(1) aligned fast path in `block_ix_for_paragraph` MISSES — the exact
  /// condition under which a per-paragraph caller becomes the §perf-heaven T2
  /// quadratic. Returns the handle plus the paragraph count.
  fn object_bearing_fixture() -> (LocalDocHandle, usize) {
    const OBJECT_EVERY: usize = 8;
    let core = CrdtRuntime::new_empty("t2-complexity").expect("runtime");
    let (handle, _gate) = LocalDocHandle::new(core, LocalWriteConfig::default());
    // 1. Build the paragraphs (text + split), no objects — the stable seed.
    let mut paragraph = handle.projection().expect("projection").ids.paragraph_ids[0];
    for i in 0..PARAGRAPHS - 1 {
      handle
        .insert_text(InsertTextIntent {
          at: TextAnchor::new(paragraph, usize::MAX),
          text: format!("paragraph {i} body text with several words"),
          style_override: None,
        })
        .expect("seed insert");
      handle
        .split_paragraph(SplitParagraphIntent {
          at: TextAnchor::new(paragraph, usize::MAX),
          inherited_style: ParagraphStyle::Normal,
        })
        .expect("seed split");
      paragraph = *handle
        .projection()
        .expect("projection")
        .ids
        .paragraph_ids
        .last()
        .expect("last paragraph");
    }
    // 2. Insert image objects at byte 0 of every OBJECT_EVERY-th paragraph — the
    // identity-anchored position the convergence tests use (robust, unlike the
    // end-of-paragraph interleave). Tolerate the rare reject; we only need SOME
    // objects so `blocks.len() != paragraphs.len()` and the aligned fast path
    // misses. Snapshot the ids first so the shifting projection doesn't matter.
    let ids = handle
      .projection()
      .expect("projection")
      .ids
      .paragraph_ids
      .clone();
    for (n, paragraph_id) in ids.iter().enumerate() {
      if n % OBJECT_EVERY == OBJECT_EVERY / 2 {
        let _ = handle.insert_object(InsertObjectIntent {
          at: TextAnchor::new(*paragraph_id, 0),
          block: InputBlock::Image(InputImageBlock {
            asset_id: AssetId(1),
            alt_text: format!("img{n}"),
            caption: None,
            sizing: InputImageSizing::Intrinsic,
            alignment: InputBlockAlignment::Left,
            external_url: None,
          }),
        });
      }
    }
    let projection = handle.projection().expect("projection");
    (handle, projection.paragraphs.len())
  }

  /// §perf-heaven T2 tripwire. On an object-bearing doc (fast path misses), the
  /// mass-op patch-synthesis loops must NOT scan `block_ix_for_paragraph` once
  /// per paragraph — that was the O(paragraphs²) cost behind mass-restyle,
  /// select-all mark, replace-all, and cross-paragraph delete. This test is
  /// self-validating: it first proves the counter TRIPS under a naive
  /// per-paragraph lookup (so the guard is not vacuous), then asserts the real
  /// mass ops keep it bounded by a small constant.
  #[test]
  fn mass_ops_do_not_scan_block_index_per_paragraph() {
    let (handle, paragraph_count) = object_bearing_fixture();
    let projection = handle.projection().expect("projection");
    assert!(
      projection.blocks.len() > projection.paragraphs.len(),
      "fixture must carry objects so the aligned fast path misses (blocks={}, paragraphs={})",
      projection.blocks.len(),
      projection.paragraphs.len(),
    );

    // (a) Prove the tripwire can TRIP: a naive per-paragraph lookup on THIS doc
    // drives one O(blocks) scan per paragraph. If this does not climb, the
    // fixture is not exercising the quadratic-prone path and the guard below
    // would be meaningless.
    reset_block_ix_scan_count();
    for ix in 0..projection.paragraphs.len() {
      let _ = block_ix_for_paragraph(&projection, ix);
    }
    let naive_scans = block_ix_scan_count();
    assert!(
      naive_scans >= paragraph_count as u64,
      "object fixture should force a scan per paragraph under naive lookup, got {naive_scans} for {paragraph_count} paragraphs",
    );

    // (b) The real select-all mark op must stay bounded — O(a few), not
    // O(paragraphs). It routes through the hoisted `paragraph_block_rows`.
    let all = projection.ids.paragraph_ids.to_vec();
    let first = *all.first().expect("first paragraph");
    let last = *all.last().expect("last paragraph");
    reset_block_ix_scan_count();
    handle
      .set_marks(SetMarksIntent {
        start: TextAnchor::new(first, 0),
        end: TextAnchor::new(last, usize::MAX),
        styles: RunStyles::default(),
      })
      .expect("mass set-marks commits");
    let mark_scans = block_ix_scan_count();
    assert!(
      mark_scans <= 16,
      "select-all mark scanned block index per paragraph (T2 quadratic regression): {mark_scans} scans over {paragraph_count} paragraphs",
    );

    // (c) The real mass paragraph-style op must stay bounded too.
    reset_block_ix_scan_count();
    handle
      .set_paragraph_styles(SetParagraphStylesIntent {
        paragraphs: all,
        style: ParagraphStyle::Custom(2),
      })
      .expect("mass set-paragraph-styles commits");
    let style_scans = block_ix_scan_count();
    assert!(
      style_scans <= 16,
      "mass restyle scanned block index per paragraph (T2 quadratic regression): {style_scans} scans over {paragraph_count} paragraphs",
    );
  }

  #[test]
  fn single_char_insert_is_o1_in_document_size() {
    let handle = large_fixture();
    let projection = handle.projection().expect("projection");
    assert!(projection.paragraphs.len() >= PARAGRAPHS - 1);
    let middle = projection.ids.paragraph_ids[projection.paragraphs.len() / 2];

    let outcome = handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(middle, 3),
        text: "x".into(),
        style_override: None,
      })
      .expect("insert commits");
    let counters = outcome.commit().counters;
    assert!(!counters.full_rebuild, "single-char insert must never full-rebuild (I-14)");
    assert!(
      counters.containers_touched <= 2,
      "insert touches O(1) containers, got {}",
      counters.containers_touched
    );
    assert!(counters.loro_ops <= 4, "insert emits O(1) Loro ops, got {}", counters.loro_ops);
    assert_eq!(
      counters.marks_emitted, 0,
      "plain typing emits ZERO style ops (spec §9 — inheritance is Loro's job)"
    );
    assert!(counters.patch_count <= 2, "insert patches O(1) paragraphs, got {}", counters.patch_count);
  }

  #[test]
  fn insert_with_style_override_marks_only_inserted_range() {
    let handle = large_fixture();
    let projection = handle.projection().expect("projection");
    let middle = projection.ids.paragraph_ids[projection.paragraphs.len() / 2];
    let outcome = handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(middle, 0),
        text: "bold".into(),
        style_override: Some(RunStyles::default()),
      })
      .expect("insert commits");
    let counters = outcome.commit().counters;
    assert!(!counters.full_rebuild);
    // Marks bounded by the style-key count, never by run/paragraph counts.
    assert!(
      counters.marks_emitted <= 8,
      "override marks are O(style keys), got {}",
      counters.marks_emitted
    );
    assert!(counters.loro_ops <= 12, "override insert stays O(1), got {}", counters.loro_ops);
  }

  #[test]
  fn split_and_join_are_o1_in_document_size() {
    let handle = large_fixture();
    let projection = handle.projection().expect("projection");
    let mid_ix = projection.paragraphs.len() / 2;
    let middle = projection.ids.paragraph_ids[mid_ix];

    let outcome = handle
      .split_paragraph(SplitParagraphIntent {
        at: TextAnchor::new(middle, 5),
        inherited_style: ParagraphStyle::Normal,
      })
      .expect("split commits");
    let counters = outcome.commit().counters;
    assert!(!counters.full_rebuild, "split must patch, not rebuild");
    assert!(counters.patch_count <= 3, "split patches O(1) blocks, got {}", counters.patch_count);

    let projection = handle.projection().expect("projection");
    let first = projection.ids.paragraph_ids[mid_ix];
    let second = projection.ids.paragraph_ids[mid_ix + 1];
    let outcome = handle
      .join_paragraphs(JoinParagraphsIntent { first, second })
      .expect("join commits");
    let counters = outcome.commit().counters;
    assert!(!counters.full_rebuild, "join must patch, not rebuild");
    assert!(counters.patch_count <= 3, "join patches O(1) blocks, got {}", counters.patch_count);
  }

  #[test]
  fn delete_range_cost_bounded_by_range_not_document() {
    let handle = large_fixture();
    let projection = handle.projection().expect("projection");
    let middle = projection.ids.paragraph_ids[projection.paragraphs.len() / 2];
    let outcome = handle
      .delete_range(DeleteRangeIntent {
        start: TextAnchor::new(middle, 0),
        end: TextAnchor::new(middle, 5),
      })
      .expect("delete commits");
    let counters = outcome.commit().counters;
    assert!(!counters.full_rebuild);
    assert!(counters.containers_touched <= 2);
    assert!(counters.patch_count <= 2);
  }

  #[test]
  fn style_range_cost_bounded_by_touched_paragraphs() {
    let handle = large_fixture();
    let projection = handle.projection().expect("projection");
    let ix = projection.paragraphs.len() / 2;
    let paragraph = projection.ids.paragraph_ids[ix];
    let outcome = handle
      .set_marks(SetMarksIntent {
        start: TextAnchor::new(paragraph, 0),
        end: TextAnchor::new(paragraph, usize::MAX),
        styles: RunStyles::default(),
      })
      .expect("marks commit");
    let counters = outcome.commit().counters;
    assert!(!counters.full_rebuild);
    assert!(
      counters.patch_count <= 2,
      "one-paragraph restyle patches one paragraph, got {}",
      counters.patch_count
    );
    assert!(
      counters.marks_emitted <= 8,
      "restyle emits O(style keys) marks, got {}",
      counters.marks_emitted
    );
  }
}
