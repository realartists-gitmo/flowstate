//! S12: the two-peer FLOW soak — random structural + cell-text traffic over
//! two gated `FlowRuntime`s with full exchange rounds, measuring the hotpath
//! budgets the spec names: cell keystrokes (the local-intent budget),
//! structural intents, and import batches. Exits non-zero on divergence.

use std::time::Instant;

use flowstate_collab::flow::{FlowDocHandle, FlowPublishEvent, FlowRuntime};
use flowstate_collab::local_write::GateHolder;
use flowstate_document::{InputParagraph, InputRun, RunStyles};
use flowstate_document::{InsertTextIntent, LocalIntent, LocalWriteAuthority as _, TextAnchor};
use flowstate_flow::CellId as Uuid;
use flowstate_flow::{CellSeed, FlowIntent, SheetId};

/// The .db8 local-intent keystroke budget is single-digit milliseconds on a
/// 2M-char body; a flow CELL keystroke touches one tiny text container and
/// must come in far under it.
const KEYSTROKE_BUDGET_MS: f64 = 4.0;

struct Rng(u64);
impl Rng {
  fn next(&mut self) -> u64 {
    self.0 ^= self.0 << 13;
    self.0 ^= self.0 >> 7;
    self.0 ^= self.0 << 17;
    self.0
  }
  fn below(&mut self, bound: usize) -> usize {
    (self.next() % bound.max(1) as u64) as usize
  }
  fn uuid(&mut self) -> Uuid {
    Uuid::from_u128((u128::from(self.next()) << 64) | u128::from(self.next()))
  }
}

fn percentile(samples: &mut [f64], fraction: f64) -> f64 {
  if samples.is_empty() {
    return 0.0;
  }
  samples.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
  let index = ((samples.len() - 1) as f64 * fraction).round() as usize;
  samples[index]
}

fn report(label: &str, samples: &mut [f64]) -> f64 {
  let p50 = percentile(samples, 0.50);
  let p95 = percentile(samples, 0.95);
  let max = samples.iter().copied().fold(0.0_f64, f64::max);
  println!("{label:>24}  n={:<6} p50={p50:>8.3}ms  p95={p95:>8.3}ms  max={max:>8.3}ms", samples.len());
  p95
}

fn drain(handle: &FlowDocHandle) -> Vec<Vec<u8>> {
  let mut guard = handle.gate().lock(GateHolder::DocumentService).unwrap();
  guard
    .take_pending_publish()
    .into_iter()
    .map(|FlowPublishEvent::LocalUpdate { bytes, .. }| bytes)
    .collect()
}

pub fn run(rounds: usize) {
  println!("two-peer flow soak: {rounds} rounds");
  let mut rng = Rng(0xf10c_50a0 ^ rounds as u64);

  let seed_runtime = FlowRuntime::new_empty();
  let sheet_type = seed_runtime.board().format.sheet_types[0].id;
  let (seed, _gate) = FlowDocHandle::new(seed_runtime);
  let sheet: SheetId = rng.uuid();
  seed
    .apply(&FlowIntent::CreateSheet {
      sheet_id: sheet,
      name: "Soak".into(),
      sheet_type_id: sheet_type,
    })
    .unwrap();
  let rows: Vec<flowstate_flow::RowId> = (0..8).map(|_| rng.uuid()).collect();
  seed
    .apply(&FlowIntent::InsertRows {
      sheet_id: sheet,
      before: None,
      row_ids: rows.clone(),
    })
    .unwrap();
  let columns: Vec<flowstate_flow::ColumnId> = seed
    .board_projection()
    .unwrap()
    .sheet(sheet)
    .unwrap()
    .columns
    .iter()
    .map(|column| column.id)
    .collect();
  for index in 0..4_usize {
    seed
      .apply(&FlowIntent::AddCell {
        sheet_id: sheet,
        cell_id: rng.uuid(),
        row_id: rows[index],
        column_id: columns[index % 2],
        seed: CellSeed::Paragraphs(vec![InputParagraph {
          style: flowstate_document::PARAGRAPH_TAG,
          runs: vec![InputRun {
            text: format!("seed {index}"),
            styles: RunStyles::default(),
          }],
        }]),
      })
      .unwrap();
  }
  let snapshot = {
    let guard = seed.gate().lock(GateHolder::DocumentService).unwrap();
    guard.snapshot_bytes().unwrap()
  };
  let peers: Vec<FlowDocHandle> = (0..2)
    .map(|_| FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).unwrap()).0)
    .collect();

  let mut keystrokes: Vec<f64> = Vec::new();
  let mut structurals: Vec<f64> = Vec::new();
  let mut imports: Vec<f64> = Vec::new();

  for round in 0..rounds {
    for peer in &peers {
      let cells: Vec<_> = peer
        .board_projection()
        .unwrap()
        .sheet(sheet)
        .map(|sheet| sheet.cells().map(|cell| cell.id).collect())
        .unwrap_or_default();
      if cells.is_empty() {
        continue;
      }
      // Cell keystroke (the hotpath budget under measurement).
      let cell = cells[rng.below(cells.len())];
      if let Ok(projection) = peer.cell_projection(cell)
        && !projection.ids.paragraph_ids.is_empty()
      {
        let authority = peer.cell_authority(cell);
        let at = TextAnchor::new(projection.ids.paragraph_ids[0], 0);
        let started = Instant::now();
        let _ = authority.apply(LocalIntent::InsertText(InsertTextIntent {
          at,
          text: "k".into(),
          style_override: None,
        }));
        keystrokes.push(started.elapsed().as_secs_f64() * 1e3);
      }
      // Structural intent (occupied-slot rejections are legal outcomes).
      let structural = match rng.below(3) {
        0 => FlowIntent::AddCell {
          sheet_id: sheet,
          cell_id: rng.uuid(),
          row_id: rows[rng.below(rows.len())],
          column_id: columns[rng.below(columns.len())],
          seed: CellSeed::Empty,
        },
        1 if cells.len() > 1 => FlowIntent::SetCellAddress {
          sheet_id: sheet,
          cell_id: cells[rng.below(cells.len())],
          row_id: rows[rng.below(rows.len())],
          column_id: columns[rng.below(columns.len())],
        },
        _ => FlowIntent::SetCellStruck {
          sheet_id: sheet,
          cell_id: cells[rng.below(cells.len())],
          struck: round % 2 == 0,
        },
      };
      let started = Instant::now();
      let _ = peer.apply(&structural);
      structurals.push(started.elapsed().as_secs_f64() * 1e3);
    }
    // Exchange: every publish reaches the other peer as one import batch.
    for _ in 0..3 {
      let batches: Vec<Vec<Vec<u8>>> = peers.iter().map(drain).collect();
      let mut any = false;
      for (source, updates) in batches.iter().enumerate() {
        if updates.is_empty() {
          continue;
        }
        any = true;
        for (target, peer) in peers.iter().enumerate() {
          if target == source {
            continue;
          }
          let blobs: Vec<&[u8]> = updates.iter().map(Vec::as_slice).collect();
          let started = Instant::now();
          peer.import_remote_updates(&blobs).unwrap();
          imports.push(started.elapsed().as_secs_f64() * 1e3);
        }
      }
      if !any {
        break;
      }
    }
  }

  // Convergence gate.
  let reference = peers[0].board_projection().unwrap();
  let other = peers[1].board_projection().unwrap();
  assert_eq!(reference, other, "two-peer flow soak diverged after {rounds} rounds");

  println!("converged after {rounds} rounds; budgets:");
  let keystroke_p95 = report("cell keystroke", &mut keystrokes);
  report("structural intent", &mut structurals);
  report("import batch", &mut imports);
  assert!(
    keystroke_p95 <= KEYSTROKE_BUDGET_MS,
    "cell keystroke p95 {keystroke_p95:.3}ms exceeds the {KEYSTROKE_BUDGET_MS}ms local-intent budget"
  );
  println!("cell keystroke p95 within the {KEYSTROKE_BUDGET_MS}ms local-intent budget ✓");
}
