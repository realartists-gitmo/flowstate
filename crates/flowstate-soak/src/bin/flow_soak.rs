//! Two-peer flow (.fl0) collaboration soak (flow spec build-order step 12).
//!
//! Both peers drive the REAL gated write path — structural `FlowIntent`s on
//! the board plus same-cell char-level typing through a `FlowCellAuthority`
//! (exactly what an attached `RichTextEditor` does) — and exchange Loro update
//! bytes in rounds like the session I/O pump. Prints wall-clock distributions
//! per stage and ends with the convergence proof: both maintained boards
//! byte-equal each other AND a fresh materialization of canonical state, and
//! the shared cell's text identical on both peers.
//!
//! ```text
//! cargo run -p flowstate-soak --release --bin flow_soak -- [--rounds N] [--keystrokes-per-round N]
//! ```

use std::sync::Arc;
use std::time::{Duration, Instant};

use clap::Parser;
use flowstate_collab::flow::{FlowCellAuthority, FlowDocHandle, FlowRuntime};
use flowstate_collab::local_write::WriteGate;
use flowstate_flow::format::{CellId, FlowFormat, SheetId};
use flowstate_flow::intents::{CellPlacement, CellSeed, FlowDropIntent, FlowIntent};
use flowstate_flow::loro_projection::materialize_board;
use gpui_flowtext::local_intents::{InsertTextIntent, LocalIntent, LocalWriteAuthority as _, TextAnchor};
use uuid::Uuid;

/// Two-peer flow collaboration hotpath soak.
#[derive(Parser)]
#[command(name = "flow-soak")]
struct Cli {
  /// Sync rounds (each round: typing + one structural op per peer, then a
  /// full update exchange).
  #[arg(long, default_value_t = 200)]
  rounds: usize,
  /// Same-cell keystrokes per peer per round.
  #[arg(long, default_value_t = 8)]
  keystrokes_per_round: usize,
}

struct Peer {
  name: &'static str,
  handle: Arc<FlowDocHandle>,
  authority: Arc<FlowCellAuthority>,
  #[allow(dead_code, reason = "keeps the gate alive for the handle's lifetime")]
  gate: Arc<WriteGate<FlowRuntime>>,
}

impl Peer {
  fn oplog_vv(&self) -> loro::VersionVector {
    self
      .handle
      .with_test_runtime(|runtime| runtime.oplog_version_vector())
  }
}

#[hotpath::main(functions_limit = 60)]
fn main() {
  let cli = Cli::parse();
  run(cli.rounds, cli.keystrokes_per_round).expect("flow soak failed");
}

fn run(rounds: usize, keystrokes_per_round: usize) -> anyhow::Result<()> {
  let build_profile = if cfg!(debug_assertions) { "debug" } else { "release" };
  println!("flow collab soak — build profile: {build_profile}, rounds: {rounds}, keystrokes/round/peer: {keystrokes_per_round}");

  let (peers, sheet, shared_cell) = spawn_two_peers()?;
  let [a, b] = peers;

  let mut commit_times = Vec::with_capacity(rounds * keystrokes_per_round * 2);
  let mut structural_times = Vec::with_capacity(rounds * 2);
  let mut import_times = Vec::with_capacity(rounds * 2);
  let mut round_cells: Vec<CellId> = Vec::new();

  for round in 0..rounds {
    // Concurrent same-cell typing on BOTH peers before any sync — the
    // headline char-merge shape.
    for (peer, seed_char) in [(&a, b'a'), (&b, b'n')] {
      for i in 0..keystrokes_per_round {
        let projection = peer
          .authority
          .canonical_projection()
          .map_err(|error| anyhow::anyhow!("{error}"))?;
        let paragraph = projection.ids.paragraph_ids[0];
        let started = Instant::now();
        peer
          .authority
          .apply(LocalIntent::InsertText(InsertTextIntent {
            at: TextAnchor::new(paragraph, usize::MAX),
            text: ((seed_char + ((round + i) % 13) as u8) as char).to_string(),
            style_override: None,
          }))
          .map_err(|error| anyhow::anyhow!("cell keystroke rejected: {error}"))?;
        commit_times.push(started.elapsed());
      }
    }

    // One structural op per peer: A appends a cell, B moves the shared cell
    // relative to it (reorder-vs-edit, the second headline shape).
    let new_cell = Uuid::new_v4();
    let started = Instant::now();
    a.handle
      .apply(FlowIntent::AddCell {
        sheet_id: sheet,
        cell_id: new_cell,
        placement: CellPlacement::ColumnEnd { column_index: 0 },
        seed: CellSeed::Empty,
      })
      .map_err(|error| anyhow::anyhow!("add-cell rejected: {error}"))?;
    structural_times.push(started.elapsed());
    round_cells.push(new_cell);
    if let Some(target) = round_cells.get(round.saturating_sub(1)).copied()
      && target != shared_cell
    {
      let started = Instant::now();
      // Racing against A's concurrent append — a rejection (target not yet
      // known to B) is expected traffic, anything else is a bug.
      match b.handle.apply(FlowIntent::MoveCellSubtree {
        sheet_id: sheet,
        cell_id: shared_cell,
        drop: FlowDropIntent::AfterSibling(target),
      }) {
        Ok(_) => structural_times.push(started.elapsed()),
        Err(error) => {
          let diagnostic = format!("{error}");
          anyhow::ensure!(
            diagnostic.contains("cell") || diagnostic.contains("sheet"),
            "unexpected move rejection: {diagnostic}"
          );
        },
      }
    }

    // Update exchange, both directions (the session pump shape).
    for (from, to) in [(&a, &b), (&b, &a)] {
      let vv = to.oplog_vv();
      let bytes = from
        .handle
        .with_test_runtime(|runtime| runtime.export_updates_for(&vv))
        .map_err(|error| anyhow::anyhow!("export failed: {error:#}"))?;
      if bytes.is_empty() {
        continue;
      }
      let started = Instant::now();
      to.handle
        .with_test_runtime(|runtime| runtime.import_remote_updates(&[bytes.as_slice()]))
        .map_err(|error| anyhow::anyhow!("import failed: {error:#}"))?;
      import_times.push(started.elapsed());
    }
  }

  // One quiescing double-exchange, then the convergence proof.
  for _ in 0..2 {
    for (from, to) in [(&a, &b), (&b, &a)] {
      let vv = to.oplog_vv();
      let bytes = from
        .handle
        .with_test_runtime(|runtime| runtime.export_updates_for(&vv))
        .map_err(|error| anyhow::anyhow!("export failed: {error:#}"))?;
      if !bytes.is_empty() {
        to.handle
          .with_test_runtime(|runtime| runtime.import_remote_updates(&[bytes.as_slice()]))
          .map_err(|error| anyhow::anyhow!("import failed: {error:#}"))?;
      }
    }
  }

  summarize("cell keystroke (authority commit, gate-held)", &mut commit_times);
  summarize("structural intent (gate-held)", &mut structural_times);
  summarize("remote import (gate-held)", &mut import_times);

  let board_a = a
    .handle
    .board_projection()
    .map_err(|error| anyhow::anyhow!("{error}"))?;
  let board_b = b
    .handle
    .board_projection()
    .map_err(|error| anyhow::anyhow!("{error}"))?;
  anyhow::ensure!(board_a == board_b, "peer boards diverged after quiescence");
  for peer in [&a, &b] {
    let fresh = peer
      .handle
      .with_test_runtime(|runtime| materialize_board(runtime.doc()).map(|(board, _defects)| board))
      .map_err(|error| anyhow::anyhow!("fresh materialize failed: {error:#}"))?;
    anyhow::ensure!(fresh == board_a, "{}: maintained board != fresh materialization", peer.name);
  }
  let text_a = cell_text(&a, shared_cell);
  let text_b = cell_text(&b, shared_cell);
  anyhow::ensure!(text_a == text_b, "shared cell text diverged");
  anyhow::ensure!(
    text_a.chars().count() >= rounds * keystrokes_per_round * 2,
    "typed characters were lost: {} < {}",
    text_a.chars().count(),
    rounds * keystrokes_per_round * 2,
  );
  println!(
    "\nconverged: {} cells, shared cell holds {} chars from both peers, boards byte-equal on both sides",
    board_a.sheets[0].cells.len(),
    text_a.chars().count(),
  );
  Ok(())
}

fn spawn_two_peers() -> anyhow::Result<([Peer; 2], SheetId, CellId)> {
  let format = FlowFormat::policy_debate();
  let (seed_handle, _seed_gate) = FlowDocHandle::new(FlowRuntime::new(&format)?);
  let sheet = Uuid::new_v4();
  let cell = Uuid::new_v4();
  seed_handle
    .apply(FlowIntent::CreateSheet {
      sheet_id: sheet,
      name: "Soak".into(),
      sheet_type_id: format.sheet_types[0].id,
    })
    .map_err(|error| anyhow::anyhow!("seed sheet: {error}"))?;
  seed_handle
    .apply(FlowIntent::AddCell {
      sheet_id: sheet,
      cell_id: cell,
      placement: CellPlacement::ColumnEnd { column_index: 0 },
      seed: CellSeed::Empty,
    })
    .map_err(|error| anyhow::anyhow!("seed cell: {error}"))?;
  let snapshot = seed_handle.with_test_runtime(|runtime| runtime.snapshot())?;
  let peers: Vec<Peer> = ["peer-a", "peer-b"]
    .into_iter()
    .map(|name| -> anyhow::Result<Peer> {
      let (handle, gate) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot)?);
      let authority = handle.cell_authority(cell);
      handle
        .open_cell(cell)
        .map_err(|error| anyhow::anyhow!("open cell: {error}"))?;
      Ok(Peer {
        name,
        handle,
        authority,
        gate,
      })
    })
    .collect::<anyhow::Result<_>>()?;
  let [a, b] = peers.try_into().ok().expect("two peers");
  Ok(([a, b], sheet, cell))
}

fn cell_text(peer: &Peer, cell: CellId) -> String {
  peer.handle.with_test_runtime(|runtime| {
    let rows = flowstate_flow::loro_projection::materialize_cell_rows(runtime.doc(), cell).expect("cell rows");
    rows
      .blocks
      .iter()
      .filter_map(|block| match block {
        gpui_flowtext::InputBlock::Paragraph(paragraph) => Some(
          paragraph
            .runs
            .iter()
            .map(|run| run.text.as_str())
            .collect::<String>(),
        ),
        _ => None,
      })
      .collect::<Vec<_>>()
      .join("\n")
  })
}

fn summarize(label: &str, times: &mut [Duration]) {
  if times.is_empty() {
    println!("{label:<48} (no samples)");
    return;
  }
  times.sort_unstable();
  let pick = |q: f64| times[((times.len() - 1) as f64 * q) as usize];
  println!(
    "{label:<48} n={:<5} p50={:>10.3?} p90={:>10.3?} p99={:>10.3?} max={:>10.3?}",
    times.len(),
    pick(0.50),
    pick(0.90),
    pick(0.99),
    times[times.len() - 1],
  );
}
