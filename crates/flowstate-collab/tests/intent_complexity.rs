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
    DeleteRangeIntent, InsertTextIntent, JoinParagraphsIntent, LocalDocHandle, LocalWriteConfig, SetMarksIntent, SplitParagraphIntent,
    TextAnchor,
  };
  use flowstate_document::{ParagraphStyle, RunStyles};

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
    assert!(counters.containers_touched <= 2, "insert touches O(1) containers, got {}", counters.containers_touched);
    assert!(counters.loro_ops <= 4, "insert emits O(1) Loro ops, got {}", counters.loro_ops);
    assert_eq!(counters.marks_emitted, 0, "plain typing emits ZERO style ops (spec §9 — inheritance is Loro's job)");
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
    assert!(counters.marks_emitted <= 8, "override marks are O(style keys), got {}", counters.marks_emitted);
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
    assert!(counters.patch_count <= 2, "one-paragraph restyle patches one paragraph, got {}", counters.patch_count);
    assert!(counters.marks_emitted <= 8, "restyle emits O(style keys) marks, got {}", counters.marks_emitted);
  }
}
