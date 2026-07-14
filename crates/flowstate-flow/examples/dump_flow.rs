//! Dumps a `.fl0` (v2, Loro-first) flow's topology and full cell text
//! (including any notes typed into cells).
//!
//! Run: `cargo run -p flowstate-flow --example dump_flow -- <path.fl0>`

use flowstate_flow::{loro_projection::materialize_board, loro_schema, read_fl0};
use loro::LoroDoc;

fn main() {
  let path = std::env::args()
    .nth(1)
    .unwrap_or_else(|| "flow-ergonomics-fixture.fl0".to_string());
  let snapshot = read_fl0(&path).expect("read .fl0");
  let doc = LoroDoc::new();
  doc
    .import_with(&snapshot, "remote")
    .expect("import snapshot");
  let (board, defects) = materialize_board(&doc).expect("materialize board");
  if !defects.is_empty() {
    println!("normalization defects: {defects:?}");
  }

  for sheet in &board.sheets {
    let definition = board.format.sheet_type(sheet.sheet_type_id);
    let column_name = |id| {
      definition.and_then(|definition: &flowstate_flow::SheetTypeDefinition| {
        definition
          .columns
          .iter()
          .find(|column| column.id == id)
          .map(|column| column.label.clone())
      })
    };
    let label = |id| {
      sheet
        .cells
        .iter()
        .find(|cell| cell.id == id)
        .map(|cell| {
          cell
            .summary
            .summary_text
            .lines()
            .next()
            .unwrap_or_default()
            .trim()
            .to_string()
        })
        .unwrap_or_default()
    };
    println!("\n===== SHEET: {} ({} cells) =====", sheet.name, sheet.cells.len());
    for (index, cell) in sheet.cells.iter().enumerate() {
      let col = column_name(cell.column_id).unwrap_or_else(|| "?".into());
      let parent = cell.parent_id.map(label).unwrap_or_else(|| "—".into());
      let text = loro_schema::cell_text(&doc, cell.id)
        .map(|text| text.to_string())
        .unwrap_or_default();
      println!("[{index:2}] col={col:<6} parent={parent}");
      for line in text.lines() {
        println!("        | {line}");
      }
    }
  }
}
