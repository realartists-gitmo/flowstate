//! Flow board convergence fuzz (build-order step 5): N peers apply random
//! STRUCTURAL intents through their gated flow handles, sync in rounds, and
//! must converge — every peer's maintained board equal to every other's AND
//! equal to a fresh materialization of its own canonical state (the one
//! derivation law). Mirrors the .db8 `intent_fuzz` harness shape: fixed CI
//! seeds + an ignored long-horizon soak.

use std::sync::Arc;

use flowstate_collab::flow::{FlowDocHandle, FlowRuntime, FlowWriteRejected};
use flowstate_collab::local_write::WriteGate;
use flowstate_flow::format::{AnnotationOriginator, AnnotationStroke, BoardPoint, BoardRect, CellId, FlowFormat, SheetId, StrokeStyle};
use flowstate_flow::intents::{CellPlacement, CellSeed, FlowDropIntent, FlowIntent, RelativePosition};
use flowstate_flow::loro_projection::materialize_board;
use gpui_flowtext::{InputParagraph, InputRun, ParagraphStyle, RunStyles};
use uuid::Uuid;

struct Rng(u64);

impl Rng {
  fn new(seed: u64) -> Self {
    Self(seed.max(1))
  }

  fn next(&mut self) -> u64 {
    self.0 ^= self.0 << 13;
    self.0 ^= self.0 >> 7;
    self.0 ^= self.0 << 17;
    self.0
  }

  fn below(&mut self, bound: usize) -> usize {
    (self.next() % bound.max(1) as u64) as usize
  }
}

struct Peer {
  handle: Arc<FlowDocHandle>,
  #[allow(dead_code, reason = "keeps the gate alive for the handle's lifetime")]
  gate: Arc<WriteGate<FlowRuntime>>,
}

impl Peer {
  fn board(&self) -> flowstate_flow::projection::FlowBoardProjection {
    self.handle.board_projection().expect("board")
  }

  fn oplog_vv(&self) -> loro::VersionVector {
    self.handle.with_test_runtime(|runtime| runtime.oplog_version_vector())
  }
}

fn spawn_peers(count: usize, format: &FlowFormat, base_sheet: SheetId) -> Vec<Peer> {
  let seed_runtime = FlowRuntime::new(format).expect("seed runtime");
  let (seed_handle, _seed_gate) = FlowDocHandle::new(seed_runtime);
  seed_handle
    .apply(FlowIntent::CreateSheet {
      sheet_id: base_sheet,
      name: "Fuzz".into(),
      sheet_type_id: format.sheet_types[0].id,
    })
    .expect("seed sheet");
  let snapshot = seed_handle.with_test_runtime(|runtime| runtime.snapshot().expect("seed snapshot"));
  (0..count)
    .map(|_| {
      let (handle, gate) = FlowDocHandle::new(FlowRuntime::from_snapshot(&snapshot).expect("peer runtime"));
      Peer { handle, gate }
    })
    .collect()
}

fn sync_all(peers: &[Peer]) {
  // Full mesh, two passes → quiescent for any peer count used here.
  for _ in 0..2 {
    for a in 0..peers.len() {
      for b in 0..peers.len() {
        if a == b {
          continue;
        }
        let vv = peers[b].oplog_vv();
        let bytes = peers[a].handle.with_test_runtime(|runtime| runtime.export_updates_for(&vv));
        if let Ok(bytes) = bytes
          && !bytes.is_empty()
        {
          peers[b]
            .handle
            .with_test_runtime(|runtime| runtime.import_remote_updates(&[bytes.as_slice()]))
            .expect("import");
        }
      }
    }
  }
}

fn stroke(sheet: SheetId, originator: &str, rng: &mut Rng) -> AnnotationStroke {
  AnnotationStroke {
    id: Uuid::from_u128(rng.next() as u128 | ((rng.next() as u128) << 64)),
    sheet_id: sheet,
    originator: AnnotationOriginator(originator.into()),
    points: vec![BoardPoint { x: 0.0, y: 0.0 }, BoardPoint { x: 1.0, y: 1.0 }],
    style: StrokeStyle {
      color_rgba: 0xff00_00ff,
      width: 1.0,
      opacity: 1.0,
    },
    bbox: BoardRect::default(),
  }
}

/// One random structural intent against `peer`. Rejections from racing
/// deletions are expected; anything else is a bug.
fn random_intent(rng: &mut Rng, peer: &Peer, sheet: SheetId, step: usize) -> Result<(), FlowWriteRejected> {
  let board = peer.board();
  let cells: Vec<CellId> = board
    .sheet(sheet)
    .map(|sheet| sheet.cells.iter().map(|cell| cell.id).collect())
    .unwrap_or_default();
  let pick = |rng: &mut Rng, cells: &[CellId]| cells.get(rng.below(cells.len())).copied();
  let intent = match rng.below(10) {
    0 => FlowIntent::AddCell {
      sheet_id: sheet,
      cell_id: Uuid::from_u128(rng.next() as u128 | ((rng.next() as u128) << 64)),
      placement: CellPlacement::ColumnEnd {
        column_index: rng.below(3),
      },
      seed: CellSeed::Empty,
    },
    1 => match pick(rng, &cells) {
      Some(parent) => FlowIntent::AddCell {
        sheet_id: sheet,
        cell_id: Uuid::from_u128(rng.next() as u128 | ((rng.next() as u128) << 64)),
        placement: if rng.below(2) == 0 {
          CellPlacement::ResponseTo { parent }
        } else {
          CellPlacement::FirstResponseTo { parent }
        },
        seed: CellSeed::Empty,
      },
      None => return Ok(()),
    },
    2 => match pick(rng, &cells) {
      Some(of) => FlowIntent::AddCell {
        sheet_id: sheet,
        cell_id: Uuid::from_u128(rng.next() as u128 | ((rng.next() as u128) << 64)),
        placement: CellPlacement::Sibling {
          of,
          position: if rng.below(2) == 0 {
            RelativePosition::Before
          } else {
            RelativePosition::After
          },
        },
        seed: CellSeed::Empty,
      },
      None => return Ok(()),
    },
    3 => match pick(rng, &cells) {
      Some(cell_id) => FlowIntent::DeleteCell { sheet_id: sheet, cell_id },
      None => return Ok(()),
    },
    4 => match (pick(rng, &cells), pick(rng, &cells)) {
      (Some(cell_id), Some(target)) if cell_id != target => FlowIntent::MoveCellSubtree {
        sheet_id: sheet,
        cell_id,
        drop: match rng.below(5) {
          0 => FlowDropIntent::BeforeSibling(target),
          1 => FlowDropIntent::AfterSibling(target),
          2 => FlowDropIntent::FirstChildOf(target),
          3 => FlowDropIntent::LastChildOf(target),
          _ => FlowDropIntent::RootInColumn {
            column_index: rng.below(3),
            insertion_index: rng.below(cells.len() + 1),
          },
        },
      },
      _ => return Ok(()),
    },
    5 => match pick(rng, &cells) {
      Some(cell_id) => FlowIntent::SetCellStruck {
        sheet_id: sheet,
        cell_id,
        struck: rng.below(2) == 0,
      },
      None => return Ok(()),
    },
    6 => match pick(rng, &cells) {
      Some(cell_id) => FlowIntent::ReplaceCellContent {
        sheet_id: sheet,
        cell_id,
        paragraphs: vec![InputParagraph {
          style: ParagraphStyle::Custom(3),
          runs: vec![InputRun {
            text: format!("edit {step}"),
            styles: RunStyles::default(),
          }],
        }],
      },
      None => return Ok(()),
    },
    7 => FlowIntent::AddAnnotation {
      sheet_id: sheet,
      stroke: stroke(sheet, "fuzz", rng),
    },
    8 => FlowIntent::ClearAnnotations {
      sheet_id: Some(sheet),
      originator: AnnotationOriginator("fuzz".into()),
    },
    _ => FlowIntent::RenameSheet {
      sheet_id: sheet,
      name: format!("Fuzz {step}"),
    },
  };
  peer.handle.apply(intent).map(|_| ())
}

fn run_fuzz(seed: u64, peers_n: usize, rounds: usize, ops_per_round: usize) {
  let mut rng = Rng::new(seed);
  let format = FlowFormat::policy_debate();
  let sheet = Uuid::new_v4();
  let peers = spawn_peers(peers_n, &format, sheet);

  for round in 0..rounds {
    for op in 0..ops_per_round {
      let peer_ix = rng.below(peers_n);
      let step = round * ops_per_round + op;
      match random_intent(&mut rng, &peers[peer_ix], sheet, step) {
        Ok(())
        | Err(FlowWriteRejected::EmptyIntent | FlowWriteRejected::StructureViolation(_))
        | Err(FlowWriteRejected::UnknownSheet(_) | FlowWriteRejected::UnknownCell(_)) => {},
        Err(other) => panic!("seed {seed} round {round} op {op}: unexpected rejection {other}"),
      }
    }
    sync_all(&peers);

    // Self-consistency: maintained board == fresh materialization.
    for (ix, peer) in peers.iter().enumerate() {
      let maintained = peer.board();
      let fresh = peer
        .handle
        .with_test_runtime(|runtime| materialize_board(runtime.doc()).expect("fresh materialize").0);
      assert_eq!(
        maintained, fresh,
        "seed {seed} round {round}: peer {ix} maintained board deviates from its own canonical state"
      );
    }
    // Cross-peer convergence.
    let reference = peers[0].board();
    let reference_vv = peers[0].oplog_vv();
    for (ix, peer) in peers.iter().enumerate().skip(1) {
      assert_eq!(
        peer.oplog_vv(),
        reference_vv,
        "seed {seed} round {round}: peer {ix} version vector differs after quiescent sync"
      );
      assert_eq!(peer.board(), reference, "seed {seed} round {round}: peer {ix} board diverged");
    }
  }
}

#[test]
fn two_peer_flow_fuzz_converges() {
  for seed in [1, 7, 42, 505, 1337, 8738] {
    run_fuzz(seed, 2, 6, 12);
  }
}

#[test]
fn three_peer_flow_fuzz_converges() {
  for seed in [3, 99, 4242, 20260707] {
    run_fuzz(seed, 3, 5, 10);
  }
}

/// Long-horizon randomized soak (10k+ intents across the sweep), ignored by
/// default like the .db8 soaks — run via `heaven.sh soak`; tune with
/// `FUZZ_SOAK_SEEDS`/`FUZZ_SOAK_ROUNDS`/`FUZZ_SOAK_OPS`.
#[test]
#[ignore = "long-horizon soak — run via `heaven.sh soak`"]
fn flow_fuzz_soak() {
  let seeds: u64 = std::env::var("FUZZ_SOAK_SEEDS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(500);
  let rounds: usize = std::env::var("FUZZ_SOAK_ROUNDS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(8);
  let ops: usize = std::env::var("FUZZ_SOAK_OPS")
    .ok()
    .and_then(|v| v.parse().ok())
    .unwrap_or(16);
  for seed in 1..=seeds {
    let peers = 2 + (seed % 3) as usize;
    run_fuzz(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15), peers, rounds, ops);
    if seed % 25 == 0 {
      eprintln!("[soak] flow_convergence {seed}/{seeds}");
    }
  }
}
