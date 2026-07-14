//! Generates a labeled `.fl0` flow document for drag-and-drop ergonomics testing.
//!
//! Run: `cargo run -p flowstate-flow --example ergonomics_fixture -- [out.fl0]`
//!
//! Every cell's tag text is a unique, stable label (A1, B1, …) that shows on the board and in the
//! telemetry log, so the instruction guide and `FLOWSTATE_DRAG_LOG` output line up by name. The
//! topology deliberately spans the move types the guide exercises: sibling runs, deep parent→child
//! chains, orphans mid-column, and cells of varied height.

use flowstate_document::{
  DocumentProjection, InputParagraph, InputRun, PARAGRAPH_ANALYTIC, PARAGRAPH_TAG, PARAGRAPH_UNDERTAG, ParagraphStyle, RunStyles, SEMANTIC_CITE,
  document_from_input, flowstate_document_theme,
};
use flowstate_flow::{CellId, FlowDocument, SheetId, save_flow_document};

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

fn content(label: &str, tier: Tier) -> DocumentProjection {
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
  document_from_input(flowstate_document_theme(), paragraphs)
}

fn add(document: &mut FlowDocument, sheet: SheetId, column: usize, parent: Option<CellId>, label: &str, tier: Tier) -> CellId {
  let id = document
    .add_plain_cell(sheet, column, parent, None)
    .expect("add cell");
  document
    .replace_cell_document(sheet, id, &content(label, tier))
    .expect("set cell content");
  id
}

fn main() {
  let out = std::env::args()
    .nth(1)
    .unwrap_or_else(|| "flow-ergonomics-fixture.fl0".to_string());
  let mut document = FlowDocument::new();
  let affirmative = document.projection().format.sheet_types[0].id; // 1AC 1NC 2AC Block 1AR 2NR 2AR
  let negative = document.projection().format.sheet_types[1].id; // 1NC 2AC Block 1AR 2NR 2AR

  // ---- Sheet 1: "Ergonomics" (Affirmative, 7 columns) ----
  let sheet = document
    .create_sheet("Ergonomics", affirmative)
    .expect("create sheet");

  // A1 with two responses B1/B2, and a deep chain B1 -> C1 -> D1 -> E1 -> F1.
  let a1 = add(&mut document, sheet, 0, None, "A1", Tier::Medium);
  let b1 = add(&mut document, sheet, 1, Some(a1), "B1", Tier::Card);
  add(&mut document, sheet, 1, Some(a1), "B2", Tier::Short);
  let c1 = add(&mut document, sheet, 2, Some(b1), "C1", Tier::Medium);
  let d1 = add(&mut document, sheet, 3, Some(c1), "D1", Tier::Short);
  let e1 = add(&mut document, sheet, 4, Some(d1), "E1", Tier::Short);
  add(&mut document, sheet, 5, Some(e1), "F1", Tier::Medium);

  // A2 with a response B3 that has two children C2/C3.
  let a2 = add(&mut document, sheet, 0, None, "A2", Tier::Tall);
  let b3 = add(&mut document, sheet, 1, Some(a2), "B3", Tier::Medium);
  add(&mut document, sheet, 2, Some(b3), "C2", Tier::Short);
  add(&mut document, sheet, 2, Some(b3), "C3", Tier::Tall);

  // A3 leaf root, plus its response B5.
  let a3 = add(&mut document, sheet, 0, None, "A3", Tier::Short);
  add(&mut document, sheet, 1, Some(a3), "B5", Tier::Short);

  // Orphans (no parent) sitting in interior columns.
  add(&mut document, sheet, 1, None, "B4", Tier::Short);
  add(&mut document, sheet, 2, None, "C4", Tier::Short);

  // ---- Sheet 2: "Scratch" (Negative, 6 columns) ----
  let scratch = document
    .create_sheet("Scratch", negative)
    .expect("create sheet");
  let s1 = add(&mut document, scratch, 0, None, "S1", Tier::Short);
  add(&mut document, scratch, 1, Some(s1), "S2", Tier::Short);
  add(&mut document, scratch, 0, None, "S3", Tier::Medium);

  save_flow_document(&out, &document).expect("save fixture");
  let projection = document.projection();
  let cells: usize = projection
    .sheets
    .iter()
    .map(|sheet| sheet.cells.len())
    .sum();
  println!("wrote {out} — {} sheets, {cells} cells", projection.sheets.len());
}
