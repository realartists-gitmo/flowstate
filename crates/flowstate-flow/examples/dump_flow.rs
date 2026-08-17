//! Dumps a `.fl0` flow's grid and full cell text (including any notes typed
//! into cells).
//!
//! Run: `cargo run -p flowstate-flow --example dump_flow -- <path.fl0>`

use flowstate_flow::load_flow_document;

fn main() {
  let path = std::env::args()
    .nth(1)
    .expect("usage: dump_flow <path.fl0>");
  let document = load_flow_document(&path).expect("load flow");
  let projection = document.projection();

  for sheet in &projection.sheets {
    println!(
      "\n===== SHEET: {} ({} columns × {} rows, {} cells) =====",
      sheet.name,
      sheet.columns.len(),
      sheet.rows.len(),
      sheet.cells().count()
    );
    let header: Vec<String> = sheet
      .columns
      .iter()
      .map(|column| match column.width {
        Some(width) => format!("{} (w {width:.0})", column.label),
        None => column.label.clone(),
      })
      .collect();
    println!("columns: {}", header.join(" | "));
    for (row_ix, row) in sheet.rows.iter().enumerate() {
      let height = row
        .height_override
        .map(|height| format!(" [h {height:.0}]"))
        .unwrap_or_default();
      println!("\n-- row {row_ix}{height} ({})", row.id);
      for (column_ix, slot) in row.cells.iter().enumerate() {
        let Some(cell) = slot else { continue };
        let struck = if cell.summary.struck { " [struck]" } else { "" };
        println!("  [{}]{} {}", sheet.columns[column_ix].label, struck, cell.id);
        match document.cell_document(cell.id) {
          Ok(cell_document) => {
            for line in cell_document.text.to_string().lines() {
              let line = line.trim_matches('\u{FEFF}');
              if !line.trim().is_empty() {
                println!("      {line}");
              }
            }
          },
          Err(error) => println!("      <cell text failed to materialize: {error}>"),
        }
      }
    }
    if !sheet.annotations.is_empty() {
      println!("\n   ink: {} stroke(s)", sheet.annotations.len());
    }
  }

  if !document.defects().is_empty() {
    println!("\n===== DEFECTS =====");
    for defect in document.defects() {
      println!("  {defect}");
    }
  }
}
