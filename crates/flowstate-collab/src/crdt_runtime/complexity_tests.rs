//! Class 1 — algorithmic-complexity regression tests for the large-document actor hang.
//!
//! The impact-doc field logs showed single `apply-editor-commands` / `import-remote-update`
//! calls blocking the CRDT actor thread for 7–17 seconds on a ~6000-paragraph document.
//! Wall-clock is a poor CI signal (it is dominated by fidelity-tracing overhead and varies
//! with the machine), so these tests assert on the two ALGORITHMIC quantities whose
//! super-linear growth is the actual hazard, via always-on counters in
//! `flowstate_document::instrument`:
//!
//!   * `full_projections` — how many whole-document `document_from_loro` rebuilds a single
//!     operation triggers. A repair storm (or any accidental reproject-in-a-loop) makes
//!     this grow with document size / defect count; it must stay O(1) per operation.
//!   * `cursor_pos_resolves` — per-cursor `get_cursor_pos` history-traced resolutions. One
//!     full projection must resolve boundaries via the batched `query_text_id_positions`
//!     resolver (O(objects) fallbacks), NOT one `get_cursor_pos` per paragraph — the
//!     O(records) scan that once pegged the actor at 100% CPU.
//!
//! Both signals are independent of wall-clock and of whether fidelity tracing is enabled,
//! so a regression is caught deterministically the moment it lands.

use anyhow::Result;
use flowstate_document::{
  InputBlock, InputBlockAlignment, InputImageBlock, InputImageSizing, InputParagraph, InputRun, MARK_PARAGRAPH_STYLE, ParagraphStyle, RunStyles,
  document_from_loro, document_from_loro_with_defects, instrument, loro_schema::body_text,
};
use gpui_flowtext::SemanticEditCommand as EditorSemanticCommand;
use loro::LoroDoc;

use super::CrdtRuntime;

/// A canonical Loro doc of `paragraphs` paragraphs with `images` images spread through it.
fn large_loro_doc(paragraphs: usize, images: usize) -> LoroDoc {
  let image_every = if images == 0 { usize::MAX } else { (paragraphs / images).max(1) };
  let mut blocks = Vec::with_capacity(paragraphs + images);
  for ix in 0..paragraphs {
    blocks.push(InputBlock::Paragraph(InputParagraph {
      style: if ix % 40 == 0 { ParagraphStyle::Custom(2) } else { ParagraphStyle::Normal },
      runs: vec![InputRun {
        text: format!("Paragraph {ix} carries several words to edit and reflow."),
        styles: RunStyles::default(),
      }],
    }));
    if ix > 0 && ix % image_every == 0 {
      blocks.push(InputBlock::Image(InputImageBlock {
        asset_id: flowstate_document::AssetId(1),
        alt_text: "img".to_string(),
        caption: None,
        sizing: InputImageSizing::Intrinsic,
        alignment: InputBlockAlignment::Left,
      }));
    }
  }
  let source = flowstate_document::document_from_input_blocks(flowstate_document::flowstate_document_theme(), blocks);
  flowstate_document::document_to_loro(&source, "Large doc").expect("materialize large fixture")
}

/// One full projection must resolve cursors in O(objects), never O(paragraphs): holding the
/// object count fixed, an 8× larger paragraph count must NOT multiply the per-cursor
/// `get_cursor_pos` resolutions (that would be the O(records) boundary scan regressing back).
#[test]
fn full_projection_cursor_resolves_scale_with_objects_not_paragraphs() -> Result<()> {
  let small = large_loro_doc(500, 4);
  let large = large_loro_doc(4000, 4);

  let before = instrument::snapshot();
  let _ = document_from_loro(&small)?;
  let small_work = instrument::snapshot().since(before);

  let before = instrument::snapshot();
  let _ = document_from_loro(&large)?;
  let large_work = instrument::snapshot().since(before);

  assert_eq!(small_work.full_projections, 1, "one document_from_loro is one full projection");
  assert_eq!(large_work.full_projections, 1);
  // 8× the paragraphs, same objects: cursor resolutions must stay flat (allow a small
  // constant slack). An O(records) regression would blow this to ~8× and fail loudly.
  let ceiling = small_work.cursor_pos_resolves.max(8) * 3;
  assert!(
    large_work.cursor_pos_resolves <= ceiling,
    "per-projection get_cursor_pos scaled with paragraph count (O(records) regression): 500-para={} vs 4000-para={} (ceiling {ceiling})",
    small_work.cursor_pos_resolves,
    large_work.cursor_pos_resolves,
  );
  Ok(())
}

/// A single editor transaction must trigger a BOUNDED number of full projection rebuilds,
/// independent of document size — the repair storm made this ~63 rebuilds per edit, which
/// is what serialized the actor thread. On a clean doc it should be a small constant.
#[test]
fn editor_transaction_triggers_bounded_full_rebuilds() -> Result<()> {
  const MAX_REBUILDS_PER_EDIT: u64 = 4;
  for paragraphs in [500usize, 4000] {
    let mut runtime = CrdtRuntime::from_doc(large_loro_doc(paragraphs, 4), None, None)?;
    let projection = runtime.projection_snapshot()?;
    let command = EditorSemanticCommand::InsertText {
      at: flowstate_document::DocumentOffset { paragraph: paragraphs / 2, byte: 0 },
      text: "z".to_string(),
      styles: RunStyles::default(),
    };
    let before = instrument::snapshot();
    runtime.apply_editor_commands(1, &projection.frontier, &[command], None)?;
    let work = instrument::snapshot().since(before);
    assert!(
      work.full_projections <= MAX_REBUILDS_PER_EDIT,
      "one editor transaction on a {paragraphs}-paragraph doc triggered {} full rebuilds (max {MAX_REBUILDS_PER_EDIT}) — a repair storm / reproject loop",
      work.full_projections,
    );
  }
  Ok(())
}

/// Importing a remote update must trigger a bounded number of full rebuilds too — the field
/// logs showed import blocking the actor for ~8s. Assert the rebuild count is a small
/// constant independent of document size.
#[test]
fn remote_import_triggers_bounded_full_rebuilds() -> Result<()> {
  const MAX_REBUILDS_PER_IMPORT: u64 = 4;
  for paragraphs in [500usize, 4000] {
    // Peer A edits; peer B (same-sized doc) imports A's update.
    let doc = large_loro_doc(paragraphs, 4);
    let snapshot = doc.export(loro::ExportMode::Snapshot)?;
    let mut a = CrdtRuntime::from_doc(doc, None, None)?;
    let doc_b = LoroDoc::new();
    doc_b.set_peer_id(0x2000)?;
    doc_b.import(&snapshot)?;
    let mut b = CrdtRuntime::from_doc(doc_b, None, None)?;

    let projection = a.projection_snapshot()?;
    let commit = a.apply_editor_commands(
      1,
      &projection.frontier,
      &[EditorSemanticCommand::InsertText {
        at: flowstate_document::DocumentOffset { paragraph: paragraphs / 2, byte: 0 },
        text: "z".to_string(),
        styles: RunStyles::default(),
      }],
      None,
    )?;
    let update = commit
      .events
      .iter()
      .find_map(|event| match event {
        super::RuntimeEvent::LocalUpdate { bytes, .. } => Some(bytes.clone()),
        _ => None,
      })
      .expect("edit must emit a local update");

    let before = instrument::snapshot();
    b.import_remote_update(&update)?;
    let work = instrument::snapshot().since(before);
    assert!(
      work.full_projections <= MAX_REBUILDS_PER_IMPORT,
      "importing one remote edit into a {paragraphs}-paragraph doc triggered {} full rebuilds (max {MAX_REBUILDS_PER_IMPORT})",
      work.full_projections,
    );
  }
  Ok(())
}

/// A repair pass over K record-less paragraph boundaries must converge with a BOUNDED number
/// of full rebuilds — NOT one (or more) per defect. This is the repair-storm guard: seed a
/// large defect count and assert construction (which projects, detects, and repairs) rebuilds
/// a small constant number of times and clears the defects.
#[test]
fn repair_pass_converges_with_bounded_rebuilds() -> Result<()> {
  const DEFECTS: usize = 400;
  const MAX_REBUILDS: u64 = 12;

  let doc = flowstate_document::new_loro_document("Repair storm")?;
  let body = body_text(&doc);
  // Append K paragraph boundaries that carry a style mark but no durable metadata record —
  // each is a MissingParagraphMetadata + MissingParagraphBlock defect the repair must fix.
  for ix in 0..DEFECTS {
    let end = body.len_unicode();
    body.insert(end, &format!("\np{ix}"))?;
    body.mark(end..end + 1, MARK_PARAGRAPH_STYLE, 0_i64)?;
  }
  doc.commit();

  let (_, before) = document_from_loro_with_defects(&doc)?;
  let defects_before = before.len();
  assert!(defects_before >= DEFECTS, "expected at least {DEFECTS} seeded defects, got {defects_before}");

  let start = instrument::snapshot();
  let runtime = CrdtRuntime::from_doc(doc, None, None)?;
  let work = instrument::snapshot().since(start);

  let (_, after) = document_from_loro_with_defects(runtime.doc())?;
  assert!(
    after.len() < defects_before / 4,
    "repair did not converge: {defects_before} defects before, {} after",
    after.len(),
  );
  assert!(
    work.full_projections <= MAX_REBUILDS,
    "repairing {DEFECTS} defects triggered {} full rebuilds (max {MAX_REBUILDS}) — the repair pass is reprojecting per-defect, not per-batch",
    work.full_projections,
  );
  Ok(())
}
