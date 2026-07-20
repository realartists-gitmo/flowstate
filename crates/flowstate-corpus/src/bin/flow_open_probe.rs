//! The `.fl0` cold-open + hotpath stage probe — the flow analogue of
//! `open_probe`. It builds synthetic flows at increasing cell counts (the
//! shipped fixture is only 11 cells, too small to expose per-cell / per-intent
//! scaling), then times the cold-open stages and the warm hotpath at each size.
//!
//! What it exposes:
//! - cold open (`load_flow_document` decode+materialize, `FlowRuntime::from_snapshot`)
//!   — both O(total cells) full materializations;
//! - the per-cell rich-text materialization the recent-card preview loader does;
//! - the WARM cost of a single cell keystroke and a single structural intent —
//!   whether they scale with TOTAL cells (the whole-board `refresh` rebuild) or
//!   stay flat (the goal: O(changed)).
//!
//! Usage: `cargo run -p flowstate-corpus --release --bin flow_open_probe [REPS]`

use std::time::{Duration, Instant};

use flowstate_collab::flow::{FlowDocHandle, FlowRuntime};
use flowstate_collab::local_write::GateHolder;
use flowstate_document::{InputParagraph, InputRun, InsertTextIntent, LocalIntent, LocalWriteAuthority as _, ParagraphStyle, RunStyles, TextAnchor};
use flowstate_flow::{CellId, CellSeed, FlowIntent, RowId};

/// Build a fully-populated `rows × cols` flow (every slot a cell carrying
/// `text`) and return the live handle plus the flat cell-id list, so callers can
/// both snapshot it and drive warm intents on it.
fn build_flow(rows: usize, cols: usize, text: &str) -> (FlowDocHandle, Vec<CellId>, Vec<RowId>) {
  let runtime = FlowRuntime::new_empty();
  let sheet_type = runtime.board().format.sheet_types[0].id;
  let (handle, _gate) = FlowDocHandle::new(runtime);
  let sheet = uuid::Uuid::new_v4();
  handle
    .apply(&FlowIntent::CreateSheet {
      sheet_id: sheet,
      name: "probe".into(),
      sheet_type_id: sheet_type,
    })
    .expect("create sheet");

  let row_ids: Vec<RowId> = (0..rows).map(|_| uuid::Uuid::new_v4()).collect();
  handle
    .apply(&FlowIntent::InsertRows {
      sheet_id: sheet,
      before: None,
      row_ids: row_ids.clone(),
    })
    .expect("insert rows");

  let columns: Vec<_> = handle
    .board_projection()
    .expect("board")
    .sheet(sheet)
    .expect("sheet")
    .columns
    .iter()
    .take(cols)
    .map(|column| column.id)
    .collect();

  let paragraphs = vec![InputParagraph {
    style: ParagraphStyle::Normal,
    runs: vec![InputRun {
      text: text.into(),
      styles: RunStyles::default(),
    }],
  }];

  let mut cell_ids = Vec::with_capacity(rows * columns.len());
  for &row_id in &row_ids {
    for &column_id in &columns {
      let cell_id = uuid::Uuid::new_v4();
      handle
        .apply(&FlowIntent::AddCell {
          sheet_id: sheet,
          cell_id,
          row_id,
          column_id,
          seed: CellSeed::Paragraphs(paragraphs.clone()),
        })
        .expect("add cell");
      cell_ids.push(cell_id);
    }
  }
  (handle, cell_ids, row_ids)
}

fn snapshot_of(handle: &FlowDocHandle) -> Vec<u8> {
  let guard = handle.gate().lock(GateHolder::DocumentService).expect("gate");
  guard.snapshot_bytes().expect("snapshot")
}

/// Median of a set of durations from `reps` timed closures.
fn timed_median(reps: usize, mut op: impl FnMut()) -> Duration {
  let mut samples: Vec<Duration> = (0..reps)
    .map(|_| {
      let t = Instant::now();
      op();
      t.elapsed()
    })
    .collect();
  samples.sort();
  samples[samples.len() / 2]
}

#[cfg_attr(feature = "hotpath", hotpath::main)]
fn main() {
  let reps: usize = std::env::args().nth(1).and_then(|v| v.parse().ok()).unwrap_or(40);
  let text = "AT: Adaptation solves — tech curve outpaces damages, Sovacool '26 financing gap";

  // (rows, cols) → total cells. Spread wide enough to read the scaling curve.
  let sizes = [(10usize, 3usize), (40, 5), (120, 7)];

  println!("flow_open_probe — reps={reps}, cell text = {} chars\n", text.len());
  println!("{:>6} {:>10} {:>10} | {:>12} {:>12} {:>12} | {:>12} {:>12}", "cells", "rows", "cols", "load_doc", "from_snap", "from_flow_doc", "keystroke", "structural");
  println!("{}", "-".repeat(104));

  for (rows, cols) in sizes {
    let (handle, cell_ids, _row_ids) = build_flow(rows, cols, text);
    let cells = cell_ids.len();
    let snapshot = snapshot_of(&handle);
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join(format!("probe-{cells}.fl0"));
    flowstate_flow::persistence::save_snapshot_to(&path, &snapshot).expect("save");

    // --- COLD OPEN ---
    // load_flow_document: zstd decode + Loro import + FlowDocument::reload
    // (materializes every cell for summaries).
    let load_doc = timed_median(reps.min(10), || {
      let doc = flowstate_flow::load_flow_document(&path).expect("load");
      std::hint::black_box(&doc);
    });
    // FlowRuntime::from_snapshot: import + refresh(None) = full materialize +
    // whole-board summary map + text-container index.
    let from_snap = timed_median(reps.min(10), || {
      let rt = FlowRuntime::from_snapshot(&snapshot).expect("from_snapshot");
      std::hint::black_box(&rt);
    });
    // The app open path: `from_flow_document` REUSES the loaded document's board
    // (seed, no rematerialize) — should be far below `from_snap` (full
    // materialize), the old cost.
    let from_flow_doc = {
      let document = flowstate_flow::load_flow_document(&path).expect("load");
      timed_median(reps.min(10), || {
        let rt = FlowRuntime::from_flow_document(&document).expect("from_flow_document");
        std::hint::black_box(&rt);
      })
    };

    // --- WARM HOTPATH (on the live handle at this size) ---
    // A single cell keystroke through the real authority: this is the budgeted
    // path. If it scales with TOTAL cells, the whole-board `refresh` rebuild is
    // the culprit.
    let target = cell_ids[cell_ids.len() / 2];
    let paragraph = handle.cell_projection(target).expect("cell projection").ids.paragraph_ids[0];
    let authority = handle.cell_authority(target);
    let keystroke = timed_median(reps, || {
      authority
        .apply(LocalIntent::InsertText(InsertTextIntent {
          at: TextAnchor::new(paragraph, 0),
          text: "x".into(),
          style_override: None,
        }))
        .expect("keystroke");
    });
    // A structural round-trip: delete a cell, then add a fresh one in its slot.
    // Both are now incremental (patch the retained board) on a clean board, so
    // this should be flat in total cells too.
    let sheet_id = handle.board_projection().expect("board").sheets[0].id;
    let (row_id, column_id) = {
      let board = handle.board_projection().expect("board");
      let sheet = &board.sheets[0];
      let (row_ix, column_ix) = sheet.cell_position(target).expect("target position");
      (sheet.rows[row_ix].id, sheet.columns[column_ix].id)
    };
    let mut current = target;
    let structural = timed_median(reps, || {
      handle.apply(&FlowIntent::DeleteCell { sheet_id, cell_id: current }).expect("delete");
      let fresh = uuid::Uuid::new_v4();
      handle
        .apply(&FlowIntent::AddCell {
          sheet_id,
          cell_id: fresh,
          row_id,
          column_id,
          seed: CellSeed::Paragraphs(vec![InputParagraph {
            style: ParagraphStyle::Normal,
            runs: vec![InputRun {
              text: "restruck".into(),
              styles: RunStyles::default(),
            }],
          }]),
        })
        .expect("add");
      current = fresh;
    });

    println!(
      "{cells:>6} {rows:>10} {cols:>10} | {:>12} {:>12} {:>12} | {:>12} {:>12}",
      format!("{load_doc:?}"),
      format!("{from_snap:?}"),
      format!("{from_flow_doc:?}"),
      format!("{keystroke:?}"),
      format!("{structural:?}"),
    );
  }

  println!("\nRead the columns down: load_doc/from_snap/from_flow_doc should grow ~linearly with cells (one-time, off-thread OK).");
  println!("keystroke/structural growing with cells = whole-board refresh rebuild per intent (the hotspot to make O(changed)).");
}
