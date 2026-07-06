//! §P2a projection repair-convergence tests.
//!
//! These exercise the runtime side of the repair pipeline: a reported defect is
//! repaired convergently, re-projecting the repaired document yields zero of that
//! defect, the repair is idempotent across passes, and two peers repairing the
//! same defect converge on identical canonical state. Also covers the §5
//! sentinel-newline protection in the `DeleteRange` preflight.

use anyhow::Result;
use flowstate_document::{
  MARK_PARAGRAPH_STYLE, ProjectionDefect, RunStyles, document_from_loro_with_defects, loro_schema::body_text, new_loro_document,
};
use loro::ExportMode;

use super::{CrdtRuntime, SemanticCommand};

fn has_defect(defects: &[ProjectionDefect], predicate: impl Fn(&ProjectionDefect) -> bool) -> bool {
  defects.iter().any(predicate)
}

#[test]
fn constructor_repairs_orphan_object_placeholder() -> Result<()> {
  let doc = new_loro_document("Orphan")?;
  body_text(&doc).insert(1, "\u{FFFC}")?;
  doc.commit();

  let (_, before) = document_from_loro_with_defects(&doc)?;
  assert!(
    has_defect(&before, |d| matches!(d, ProjectionDefect::OrphanObjectPlaceholder { .. })),
    "the stray placeholder must be reported before repair"
  );

  // Opening the runtime projects, finds the defect, and repairs it.
  let runtime = CrdtRuntime::from_doc(doc, None, None)?;
  assert!(
    !body_text(runtime.doc()).to_string().contains('\u{FFFC}'),
    "the orphan placeholder must be deleted canonically"
  );
  let (_, after) = document_from_loro_with_defects(runtime.doc())?;
  assert!(
    !has_defect(&after, |d| matches!(d, ProjectionDefect::OrphanObjectPlaceholder { .. })),
    "re-projecting the repaired document must yield zero orphan-placeholder defects"
  );
  Ok(())
}

#[test]
fn repairs_missing_paragraph_metadata_boundary() -> Result<()> {
  let doc = new_loro_document("Missing metadata")?;
  let body = body_text(&doc);
  let end = body.len_unicode();
  // A second paragraph boundary that carries a style mark but no durable record.
  body.insert(end, "\nextra")?;
  body.mark(end..end + 1, MARK_PARAGRAPH_STYLE, 0_i64)?;
  doc.commit();

  let (_, before) = document_from_loro_with_defects(&doc)?;
  assert!(
    has_defect(&before, |d| matches!(d, ProjectionDefect::MissingParagraphMetadata { .. })),
    "the boundary without durable metadata must be reported"
  );

  let runtime = CrdtRuntime::from_doc(doc, None, None)?;
  let (_, after) = document_from_loro_with_defects(runtime.doc())?;
  assert!(
    !has_defect(&after, |d| matches!(
      d,
      ProjectionDefect::MissingParagraphMetadata { .. } | ProjectionDefect::MissingParagraphBlock { .. }
    )),
    "the repair must write durable paragraph + block records so the defect clears"
  );
  Ok(())
}

#[test]
fn repair_is_idempotent_across_passes() -> Result<()> {
  let doc = new_loro_document("Idempotent")?;
  body_text(&doc).insert(1, "\u{FFFC}")?;
  doc.commit();

  // First pass happens at construction.
  let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
  let body_after_first = body_text(runtime.doc()).to_string();

  // A second explicit pass over freshly collected (now empty) defects is a no-op.
  let (_, residual) = document_from_loro_with_defects(runtime.doc())?;
  let events = runtime.schedule_projection_repairs(residual)?;
  assert!(events.is_empty(), "a converged document must produce no further repair events");
  assert_eq!(
    body_text(runtime.doc()).to_string(),
    body_after_first,
    "a second repair pass must not mutate an already-repaired document"
  );
  Ok(())
}

#[test]
fn two_runtimes_repairing_same_orphan_converge() -> Result<()> {
  let base = new_loro_document("Converge")?;
  body_text(&base).insert(1, "\u{FFFC}")?;
  base.commit();
  let base_vv = base.state_vv();

  // Two independent forks (distinct peer ids) each repair the same orphan.
  let mut peer_a = CrdtRuntime::from_doc(base.fork(), None, None)?;
  let mut peer_b = CrdtRuntime::from_doc(base.fork(), None, None)?;

  // Exchange the concurrent repairs and let each converge them.
  let update_a = peer_a.doc().export(ExportMode::updates(&base_vv))?;
  let update_b = peer_b.doc().export(ExportMode::updates(&base_vv))?;
  peer_b.import_remote_update(&update_a)?;
  peer_a.import_remote_update(&update_b)?;

  let body_a = body_text(peer_a.doc()).to_string();
  let body_b = body_text(peer_b.doc()).to_string();
  assert_eq!(body_a, body_b, "concurrent repairs must converge to identical canonical body state");
  assert_eq!(body_a, "\n", "both peers must converge on the lone sentinel newline");

  let (_, defects_a) = document_from_loro_with_defects(peer_a.doc())?;
  let (_, defects_b) = document_from_loro_with_defects(peer_b.doc())?;
  assert!(!has_defect(&defects_a, |d| matches!(d, ProjectionDefect::OrphanObjectPlaceholder { .. })));
  assert!(!has_defect(&defects_b, |d| matches!(d, ProjectionDefect::OrphanObjectPlaceholder { .. })));
  Ok(())
}

#[test]
fn delete_range_preserves_boundary_sentinel_newline() -> Result<()> {
  let mut runtime = CrdtRuntime::new_empty("Sentinel")?;
  runtime.command(SemanticCommand::InsertText {
    unicode_index: 1,
    text: "hello".to_string(),
    styles: RunStyles::default(),
  })?;
  assert_eq!(body_text(runtime.doc()).to_string(), "\nhello");

  // A whole-document delete that reaches position 0 is clamped so the sentinel
  // survives; only the text after it is removed.
  runtime.command(SemanticCommand::DeleteRange {
    unicode_index: 0,
    unicode_len: 6,
  })?;
  assert_eq!(body_text(runtime.doc()).to_string(), "\n");
  Ok(())
}

#[test]
fn delete_range_rejects_sentinel_only_delete() -> Result<()> {
  let mut runtime = CrdtRuntime::new_empty("Sentinel")?;
  runtime.command(SemanticCommand::InsertText {
    unicode_index: 1,
    text: "hi".to_string(),
    styles: RunStyles::default(),
  })?;

  // Deleting only position 0 (the sentinel) is rejected in preflight: no mutation.
  let events = runtime.command(SemanticCommand::DeleteRange {
    unicode_index: 0,
    unicode_len: 1,
  })?;
  assert!(events.is_empty(), "a sentinel-only delete must be rejected, producing no events");
  assert_eq!(body_text(runtime.doc()).to_string(), "\nhi");
  Ok(())
}
