//! Class 5 — docx import must not produce malformed canonical state.
//!
//! The impact-doc field logs showed ~1240 paragraphs projecting with NO durable metadata
//! record and NO paragraph-style mark (`missing_paragraph_metadata` /
//! `missing_paragraph_style_mark` / `missing_paragraph_block`, plus `boundary-without-mark`
//! fidelity violations), which drove a repair storm. The current importer writes a durable
//! record + style mark for every paragraph, so a valid import SHOULD yield zero of these
//! defects — this harness locks that invariant in so a regression (an import path that skips
//! `import_paragraph_record` / the boundary style mark) fails loudly, and gives a ready knob
//! to run the SAME assertion against a real .docx (e.g. the impact doc) via an env var.

use std::sync::atomic::{AtomicU64, Ordering};

use flowstate_document::{
  AssetId, CellId, ColumnId, InputBlock, InputBlockAlignment, InputImageBlock, InputImageSizing, InputParagraph, InputRun, InputTableBlock,
  InputTableCell, InputTableCellBlock, InputTableColumn, InputTableColumnWidth, InputTableRow, InputTableStyle, ParagraphStyle, RowId, RunStyles,
  SOFT_LINE_BREAK, document_from_input_blocks, document_from_loro_with_defects, flowstate_document_theme,
};

use crate::{import_docx_bytes_to_loro, write_docx};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

fn temp_docx_path() -> std::path::PathBuf {
  let sequence = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
  std::env::temp_dir().join(format!("flowstate-roundtrip-{}-{sequence}.docx", std::process::id()))
}

/// A structurally-rich document with the impact-doc's hazardous features at scale:
/// hundreds of paragraphs, images flanked by empty paragraphs, and a table.
fn rich_document(paragraphs: usize) -> flowstate_document::DocumentProjection {
  let para = |text: &str| {
    InputBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: if text.is_empty() {
        Vec::new()
      } else {
        vec![InputRun { text: text.to_string(), styles: RunStyles::default() }]
      },
    })
  };
  let image = || {
    InputBlock::Image(InputImageBlock {
      asset_id: AssetId(1),
      alt_text: "figure".to_string(),
      caption: None,
      sizing: InputImageSizing::Intrinsic,
      alignment: InputBlockAlignment::Left,
    })
  };
  let cell = |row: u128, col: u128, text: &str| InputTableCell {
    id: CellId(row * 100 + col),
    row_id: RowId(row),
    column_id: ColumnId(col),
    blocks: vec![InputTableCellBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![InputRun { text: text.to_string(), styles: RunStyles::default() }],
    })],
    row_span: 1,
    col_span: 1,
  };
  let table = InputBlock::Table(InputTableBlock {
    columns: vec![
      InputTableColumn { id: ColumnId(1), width: InputTableColumnWidth::Fraction(1) },
      InputTableColumn { id: ColumnId(2), width: InputTableColumnWidth::Fraction(1) },
    ],
    rows: vec![
      InputTableRow { id: RowId(1), cells: vec![cell(1, 1, "a"), cell(1, 2, "b")] },
      InputTableRow { id: RowId(2), cells: vec![cell(2, 1, "c"), cell(2, 2, "d")] },
    ],
    style: InputTableStyle { header_row: true },
  });

  let mut blocks = vec![para("Title of the brief"), table];
  for ix in 0..paragraphs {
    blocks.push(para(&format!("Body paragraph {ix} with argument text and citations.")));
    if ix % 50 == 0 {
      blocks.push(image());
      blocks.push(para("")); // empty paragraph adjacent to an object
    }
  }
  // Body run with an intra-paragraph soft break: exports as a body-level
  // `<w:br/>`, which the importer must map back to U+2028 — NOT '\n', which
  // would fabricate a record-less paragraph boundary (the impact-doc defect).
  blocks.push(para(&soft_break_probe_text()));
  document_from_input_blocks(flowstate_document_theme(), blocks)
}

fn soft_break_probe_text() -> String {
  format!("wrapped argument line{SOFT_LINE_BREAK}continued after a soft line break")
}

fn assert_import_defect_free(bytes: &[u8], label: &str) -> flowstate_document::DocumentProjection {
  let (imported, report) = import_docx_bytes_to_loro(bytes, "roundtrip").expect("import docx bytes");
  let (projection, defects) = document_from_loro_with_defects(&imported.doc).expect("project imported docx");
  assert!(
    defects.is_empty(),
    "{label}: importing produced {} projection defect(s) (paragraphs_imported={:?}) — the importer left canonical state a repair pass must fix; sample: {:?}",
    defects.len(),
    report.paragraphs_imported,
    defects.iter().take(5).collect::<Vec<_>>(),
  );
  projection
}

/// Build a rich document, write a real .docx, re-import it, and assert the projection has
/// ZERO defects — no record-less/mark-less paragraphs to trigger a repair storm.
#[test]
fn synthetic_docx_roundtrip_has_no_projection_defects() {
  let document = rich_document(400);
  let path = temp_docx_path();
  write_docx(&path, &document).expect("write docx");
  let bytes = std::fs::read(&path).expect("read docx");
  let _ = std::fs::remove_file(&path);
  let projection = assert_import_defect_free(&bytes, "synthetic roundtrip");
  // Defect-freeness alone would also pass if the break were silently DROPPED;
  // pin the soft break to U+2028 in the imported text as well.
  assert!(
    projection.text.to_string().contains(&soft_break_probe_text()),
    "synthetic roundtrip: body-level soft break did not survive import as U+2028",
  );
}

/// Run the same defect-free assertion against a REAL .docx supplied via
/// `FLOWSTATE_DOCX_FIXTURE=/path/to/file.docx` — point it at the impact-defense doc to
/// confirm (or catch) whether that specific file imports clean. Skips when unset so CI is
/// unaffected.
#[test]
fn external_docx_import_has_no_projection_defects() {
  let Some(path) = std::env::var_os("FLOWSTATE_DOCX_FIXTURE") else {
    eprintln!("FLOWSTATE_DOCX_FIXTURE unset — skipping external docx import check");
    return;
  };
  let bytes = std::fs::read(&path).expect("read FLOWSTATE_DOCX_FIXTURE");
  assert_import_defect_free(&bytes, &format!("external docx {path:?}"));
}
