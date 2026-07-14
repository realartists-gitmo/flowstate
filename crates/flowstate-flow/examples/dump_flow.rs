//! Dumps a `.fl0` flow's topology and full cell text (including any notes typed into cells).
//!
//! Run: `cargo run -p flowstate-flow --example dump_flow -- <path.fl0>`

use flowstate_flow::load_flow_document;

fn main() {
  let path = std::env::args().nth(1).unwrap_or_else(|| "flow-ergonomics-fixture.fl0".to_string());
  let document = load_flow_document(&path).expect("load flow");
  let projection = document.projection();

  for sheet in &projection.sheets {
    let definition = projection.format.sheet_type(sheet.sheet_type_id);
    let column_name = |id| definition.and_then(|d: &flowstate_flow::SheetTypeDefinition| d.columns.iter().find(|c| c.id == id).map(|c| c.label.clone()));
    let label = |id| {
      sheet
        .cells
        .iter()
        .find(|c| c.id == id)
        .and_then(|c| c.summary_text().ok())
        .map(|t| t.lines().next().unwrap_or_default().trim().to_string())
        .unwrap_or_default()
    };
    println!("\n===== SHEET: {} ({} cells) =====", sheet.name, sheet.cells.len());
    for (index, cell) in sheet.cells.iter().enumerate() {
      let col = column_name(cell.column_id).unwrap_or_else(|| "?".into());
      let parent = cell.parent_id.map(label).unwrap_or_else(|| "—".into());
      let text = cell.document().map(|d| d.text.to_string()).unwrap_or_default();
      println!("[{index:2}] col={col:<6} parent={parent}");
      for line in text.lines() {
        println!("        | {line}");
      }
    }
  }
}
