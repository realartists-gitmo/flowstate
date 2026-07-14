//! Generates a labeled `.fl0` (v2, Loro-first) flow document for drag-and-drop
//! ergonomics testing.
//!
//! Run: `cargo run -p flowstate-flow --example ergonomics_fixture -- [out.fl0]`
//!
//! Every cell's tag text is a unique, stable label (A1, B1, …) that shows on the board and in the
//! telemetry log, so the instruction guide and `FLOWSTATE_DRAG_LOG` output line up by name. The
//! topology deliberately spans the move types the guide exercises: sibling runs, deep parent→child
//! chains, orphans mid-column, and cells of varied height. Cells are written in canonical DFS
//! order so the persisted order lists match the write-path invariant.

use flowstate_document::{
  InputParagraph, InputRun, PARAGRAPH_ANALYTIC, PARAGRAPH_TAG, PARAGRAPH_UNDERTAG, ParagraphStyle, RunStyles, SEMANTIC_CITE,
};
use flowstate_flow::{
  CellId, FlowFormat, SheetId, encode_fl0_snapshot,
  loro_projection::materialize_board,
  loro_schema::{cell_flow_from_paragraphs, init_flow_document, write_cell, write_sheet},
};
use loro::LoroDoc;

#[derive(Clone, Copy)]
enum Tier {
  Short,
  Medium,
  Tall,
  Card,
}

fn run(text: &str) -> InputRun {
  InputRun {
    text: text.into(),
    styles: RunStyles::default(),
  }
}

fn cite(text: &str) -> InputRun {
  let mut run = run(text);
  run.styles.semantic = SEMANTIC_CITE;
  run
}

fn paragraph(style: ParagraphStyle, runs: Vec<InputRun>) -> InputParagraph {
  InputParagraph { style, runs }
}

fn content(label: &str, tier: Tier) -> Vec<InputParagraph> {
  let mut paragraphs = vec![paragraph(PARAGRAPH_TAG, vec![run(label)])];
  match tier {
    Tier::Short => {},
    Tier::Medium => paragraphs.push(paragraph(PARAGRAPH_UNDERTAG, vec![run(&format!("underview supporting {label}"))])),
    Tier::Tall => {
      paragraphs.push(paragraph(PARAGRAPH_UNDERTAG, vec![run(&format!("underview supporting {label}"))]));
      paragraphs.push(paragraph(
        PARAGRAPH_ANALYTIC,
        vec![run(&format!(
          "analytic {label}: first supporting line, long enough to wrap and add real height"
        ))],
      ));
      paragraphs.push(paragraph(
        PARAGRAPH_ANALYTIC,
        vec![run(&format!("analytic {label}: second supporting line"))],
      ));
    },
    Tier::Card => paragraphs.push(paragraph(
      ParagraphStyle::Normal,
      vec![
        cite(&format!("{label} — Author 2024")),
        run(" full card body that stays hidden in the summary projection"),
      ],
    )),
  }
  paragraphs
}

struct Fixture {
  doc: LoroDoc,
  format: FlowFormat,
  sheet_type_index: usize,
  next_order: usize,
}

impl Fixture {
  fn sheet(&mut self, name: &str, sheet_type_index: usize, order_index: usize) -> SheetId {
    let sheet = SheetId::new_v4();
    write_sheet(&self.doc, sheet, name, self.format.sheet_types[sheet_type_index].id, order_index).expect("write sheet");
    self.sheet_type_index = sheet_type_index;
    self.next_order = 0;
    sheet
  }

  fn add(&mut self, sheet: SheetId, column: usize, parent: Option<CellId>, label: &str, tier: Tier) -> CellId {
    let id = CellId::new_v4();
    let column_id = self.format.sheet_types[self.sheet_type_index].columns[column].id;
    let cell = write_cell(&self.doc, sheet, id, column_id, parent, self.next_order).expect("write cell");
    self.next_order += 1;
    cell_flow_from_paragraphs(&cell, &content(label, tier)).expect("seed cell content");
    id
  }
}

fn main() {
  let out = std::env::args()
    .nth(1)
    .unwrap_or_else(|| "flow-ergonomics-fixture.fl0".to_string());
  let doc = LoroDoc::new();
  let format = FlowFormat::policy_debate();
  init_flow_document(&doc, &format).expect("init flow document");
  let mut fixture = Fixture {
    doc,
    format: format.clone(),
    sheet_type_index: 0,
    next_order: 0,
  };

  // ---- Sheet 1: "Ergonomics" (Affirmative) — written in canonical DFS order:
  // each root followed by its whole subtree, orphans as trailing roots.
  let sheet = fixture.sheet("Ergonomics", 0, 0);

  // A1 with a deep chain B1 -> C1 -> D1 -> E1 -> F1, then sibling B2.
  let a1 = fixture.add(sheet, 0, None, "A1", Tier::Medium);
  let b1 = fixture.add(sheet, 1, Some(a1), "B1", Tier::Card);
  let c1 = fixture.add(sheet, 2, Some(b1), "C1", Tier::Medium);
  let d1 = fixture.add(sheet, 3, Some(c1), "D1", Tier::Short);
  let e1 = fixture.add(sheet, 4, Some(d1), "E1", Tier::Short);
  fixture.add(sheet, 5, Some(e1), "F1", Tier::Medium);
  fixture.add(sheet, 1, Some(a1), "B2", Tier::Short);

  // A2 with a response B3 that has two children C2/C3.
  let a2 = fixture.add(sheet, 0, None, "A2", Tier::Tall);
  let b3 = fixture.add(sheet, 1, Some(a2), "B3", Tier::Medium);
  fixture.add(sheet, 2, Some(b3), "C2", Tier::Short);
  fixture.add(sheet, 2, Some(b3), "C3", Tier::Tall);

  // A3 leaf root, plus its response B5.
  let a3 = fixture.add(sheet, 0, None, "A3", Tier::Short);
  fixture.add(sheet, 1, Some(a3), "B5", Tier::Short);

  // Orphans (no parent) sitting in interior columns.
  fixture.add(sheet, 1, None, "B4", Tier::Short);
  fixture.add(sheet, 2, None, "C4", Tier::Short);

  // ---- Sheet 2: "Scratch" (Negative) ----
  let scratch = fixture.sheet("Scratch", 1, 1);
  let s1 = fixture.add(scratch, 0, None, "S1", Tier::Short);
  fixture.add(scratch, 1, Some(s1), "S2", Tier::Short);
  fixture.add(scratch, 0, None, "S3", Tier::Medium);

  fixture.doc.commit();
  let snapshot = fixture
    .doc
    .export(loro::ExportMode::Snapshot)
    .expect("export snapshot");
  std::fs::write(&out, encode_fl0_snapshot(&snapshot)).expect("save fixture");
  let (board, defects) = materialize_board(&fixture.doc).expect("materialize board");
  assert!(defects.is_empty(), "fixture must materialize defect-free: {defects:?}");
  let cells: usize = board.sheets.iter().map(|sheet| sheet.cells.len()).sum();
  println!("wrote {out} — {} sheets, {cells} cells", board.sheets.len());
}
