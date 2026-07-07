use anyhow::Result;
use flowstate_document::{BlockId, DocumentOffset, DocumentProjection, ParagraphId, ParagraphStyle, RunStyles, document_from_loro, new_loro_document};
use gpui_flowtext::SemanticEditCommand as EditorSemanticCommand;
use loro::{ExportMode, LoroDoc};

use super::{CrdtRuntime, RuntimeEvent, editor_transaction_tests::assert_semantic_projection_eq};

/// Per-recipient delivery queues: `queues[recipient][sender]` holds Loro update
/// payloads emitted by `sender` that `recipient` has not imported yet.
type PeerQueues = Vec<Vec<Vec<u8>>>;

fn next_seed(seed: &mut u64) -> u64 {
  *seed = seed
    .wrapping_mul(6364136223846793005)
    .wrapping_add(1442695040888963407);
  *seed
}

fn seeded_peers(peer_count: usize, title: &str) -> Result<Vec<CrdtRuntime>> {
  let base = new_loro_document(title)?;
  let snapshot = base.export(ExportMode::Snapshot)?;
  (0..peer_count)
    .map(|peer_ix| {
      let doc = LoroDoc::new();
      // Deterministic peer ids keep concurrent-merge tie-breaking stable across runs.
      doc.set_peer_id(0x1000 + peer_ix as u64)?;
      let status = doc.import(&snapshot)?;
      assert!(status.pending.is_none(), "seed snapshot import must not leave pending dependencies");
      CrdtRuntime::from_doc(doc, None, None)
    })
    .collect()
}

/// Seed `peer_count` runtimes from an existing Loro `base` document (a fixture),
/// each with a deterministic peer id — like [`seeded_peers`] but from real document
/// structure (objects, empties, soft breaks, tables) instead of a blank doc.
fn seeded_peers_from_loro(peer_count: usize, base: &LoroDoc) -> Result<Vec<CrdtRuntime>> {
  let snapshot = base.export(ExportMode::Snapshot)?;
  (0..peer_count)
    .map(|peer_ix| {
      let doc = LoroDoc::new();
      doc.set_peer_id(0x1000 + peer_ix as u64)?;
      let status = doc.import(&snapshot)?;
      assert!(status.pending.is_none(), "seed snapshot import must not leave pending dependencies");
      CrdtRuntime::from_doc(doc, None, None)
    })
    .collect()
}

/// Structurally-rich fixture reproducing the impact-defense doc's problem features
/// in miniature — the structures blank-doc random stress never generates: images
/// flanked by empty paragraphs (projection coalescing vs live-body coordinates) and
/// intra-paragraph soft line breaks (U+2028). Seeding the convergence stress from
/// THIS is what exercises the projection<->Loro coordinate paths that broke on the
/// real doc but that the blank-doc suites structurally cannot reach.
fn structural_fixture() -> Result<LoroDoc> {
  use flowstate_document::{InputBlock, InputBlockAlignment, InputImageBlock, InputImageSizing, InputParagraph, InputRun};
  let para = |t: &str| {
    InputBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: if t.is_empty() {
        Vec::new()
      } else {
        vec![InputRun { text: t.to_string(), styles: RunStyles::default() }]
      },
    })
  };
  let image = || {
    InputBlock::Image(InputImageBlock {
      asset_id: flowstate_document::AssetId(1),
      alt_text: "img".to_string(),
      caption: None,
      sizing: InputImageSizing::Intrinsic,
      alignment: InputBlockAlignment::Left,
    })
  };
  let blocks = vec![
    para("Introduction with several words to edit."),
    image(),
    para(""), // coalesced empty immediately after an object
    para("Text after the first image."),
    para("Left of soft break\u{2028}right of soft break."), // intra-paragraph soft break
    para("alpha bravo charlie"),
    image(),
    para(""),
    para(""), // two empties after an object
    para("Two empties above me."),
    para("Closing remarks paragraph."),
  ];
  let source = flowstate_document::document_from_input_blocks(flowstate_document::flowstate_document_theme(), blocks);
  Ok(flowstate_document::document_to_loro(&source, "Structural fixture")?)
}

use flowstate_document::{CellId, ColumnId, InputTableCell, InputTableColumn, InputTableColumnWidth, InputTableRow, RowId};

/// A fresh table cell (single empty paragraph) at `(row_id, column_id)`.
fn table_cell(cell_id: u128, row_id: RowId, column_id: ColumnId, text: &str) -> InputTableCell {
  use flowstate_document::{InputParagraph, InputRun, InputTableCellBlock};
  InputTableCell {
    id: CellId(cell_id),
    row_id,
    column_id,
    blocks: vec![InputTableCellBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: if text.is_empty() {
        Vec::new()
      } else {
        vec![InputRun { text: text.to_string(), styles: RunStyles::default() }]
      },
    })],
    row_span: 1,
    col_span: 1,
  }
}

/// Fixture with a paragraph, a 2-row × 3-column table, and a trailing paragraph. Table
/// row/column/cell ids are small distinct constants (§P2b durable ids); editor-minted ids
/// use the per-peer `fresh_id` space, so they never collide.
fn table_fixture() -> Result<LoroDoc> {
  use flowstate_document::{InputBlock, InputParagraph, InputRun, InputTableBlock, InputTableStyle};
  let columns: Vec<InputTableColumn> = (0..3)
    .map(|c| InputTableColumn { id: ColumnId(100 + c), width: InputTableColumnWidth::Auto })
    .collect();
  let rows: Vec<InputTableRow> = (0..2)
    .map(|r| InputTableRow {
      id: RowId(200 + r),
      cells: (0..3)
        .map(|c| table_cell(300 + r * 10 + c, RowId(200 + r), ColumnId(100 + c), &format!("r{r}c{c}")))
        .collect(),
    })
    .collect();
  let para = |t: &str| {
    InputBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: vec![InputRun { text: t.to_string(), styles: RunStyles::default() }],
    })
  };
  let blocks = vec![
    para("Above the table."),
    InputBlock::Table(InputTableBlock { rows, columns, style: InputTableStyle { header_row: true } }),
    para("Below the table."),
  ];
  let source = flowstate_document::document_from_input_blocks(flowstate_document::flowstate_document_theme(), blocks);
  Ok(flowstate_document::document_to_loro(&source, "Table fixture")?)
}

/// The first table block in `projection` with its block id, if any.
fn first_table(projection: &DocumentProjection) -> Option<(BlockId, flowstate_document::TableBlock)> {
  projection.blocks.iter().enumerate().find_map(|(ix, block)| match block {
    flowstate_document::Block::Table(table) => Some((projection.ids.block_ids[ix], table.clone())),
    _ => None,
  })
}

fn all_version_vectors_equal(peers: &[CrdtRuntime]) -> bool {
  let first = peers[0].doc().state_vv();
  peers.iter().skip(1).all(|peer| peer.doc().state_vv() == first)
}

/// Direct anti-entropy exchange. Runtime construction commits per-replica
/// registration ops outside any delivery queue, so both tests use this to
/// reach a shared frontier before (and after) queue-based delivery.
fn synchronize_until_converged(peers: &mut [CrdtRuntime]) -> Result<()> {
  for _round in 0..8 {
    if all_version_vectors_equal(peers) {
      return Ok(());
    }
    for source_ix in 0..peers.len() {
      for target_ix in 0..peers.len() {
        if source_ix == target_ix {
          continue;
        }
        let update = peers[source_ix].export_updates_for(&peers[target_ix].doc().state_vv())?;
        if !update.is_empty() {
          peers[target_ix].import_remote_update(&update)?;
        }
      }
    }
  }
  if all_version_vectors_equal(peers) {
    return Ok(());
  }
  anyhow::bail!("peers failed to converge during direct synchronization")
}

fn collect_local_update_bytes(events: &[RuntimeEvent]) -> Vec<Vec<u8>> {
  events
    .iter()
    .filter_map(|event| match event {
      RuntimeEvent::LocalUpdate { bytes, .. } => Some(bytes.clone()),
      _ => None,
    })
    .collect()
}

/// Fan every `LocalUpdate` emitted by `source_ix` out to all other peers'
/// queues. Imports can also emit repair `LocalUpdate`s, so delivery routes
/// through this too (with the importing peer as the source).
fn enqueue_local_updates(queues: &mut [PeerQueues], source_ix: usize, events: &[RuntimeEvent]) {
  for bytes in collect_local_update_bytes(events) {
    for (recipient_ix, senders) in queues.iter_mut().enumerate() {
      if recipient_ix != source_ix {
        senders[source_ix].push(bytes.clone());
      }
    }
  }
}

fn pending_queue_pairs(queues: &[PeerQueues]) -> Vec<(usize, usize)> {
  let mut pairs = Vec::new();
  for (recipient_ix, senders) in queues.iter().enumerate() {
    for (sender_ix, queue) in senders.iter().enumerate() {
      if !queue.is_empty() {
        pairs.push((recipient_ix, sender_ix));
      }
    }
  }
  pairs
}

fn deliver_random_pending_update(peers: &mut [CrdtRuntime], queues: &mut [PeerQueues], seed: &mut u64, context: &str) -> bool {
  let pending = pending_queue_pairs(queues);
  if pending.is_empty() {
    return false;
  }
  let (recipient_ix, sender_ix) = pending[((next_seed(seed) >> 32) as usize) % pending.len()];
  let queue = &mut queues[recipient_ix][sender_ix];
  // Random-position pop makes delivery out of order on purpose; Loro must park
  // updates with missing dependencies as pending and apply them once deps land.
  let position = ((next_seed(seed) >> 32) as usize) % queue.len();
  let update = queue.remove(position);
  let events = peers[recipient_ix]
    .import_remote_update(&update)
    .unwrap_or_else(|error| panic!("peer {recipient_ix} failed to import update from peer {sender_ix} ({context}): {error:?}"));
  enqueue_local_updates(queues, recipient_ix, &events);
  true
}

fn drain_all_queues_randomized(peers: &mut [CrdtRuntime], queues: &mut [PeerQueues], seed: &mut u64) -> Result<()> {
  // Generous budget: every queued update plus any import-time repair updates.
  for drain_step in 0..20_000_u64 {
    if !deliver_random_pending_update(peers, queues, seed, &format!("final drain step {drain_step}")) {
      return Ok(());
    }
  }
  anyhow::bail!("delivery queues failed to drain within the iteration budget")
}

fn random_editor_command(seed: &mut u64, projection: &DocumentProjection, peer_ix: usize, step: u64) -> EditorSemanticCommand {
  let paragraph_count = projection.paragraphs.len();
  let paragraph_ix = ((next_seed(seed) >> 32) as usize) % paragraph_count;
  let text = flowstate_document::paragraph_text(projection, paragraph_ix);
  let action = (next_seed(seed) >> 32) % 4;
  match action {
    0 => {
      // ASCII-only content keeps every byte offset a valid char boundary.
      let byte = ((next_seed(seed) >> 32) as usize) % (text.len() + 1);
      EditorSemanticCommand::InsertText {
        at: DocumentOffset {
          paragraph: paragraph_ix,
          byte,
        },
        text: char::from(b'a' + (step % 26) as u8).to_string(),
        styles: RunStyles::default(),
      }
    },
    1 if !text.is_empty() => {
      let byte = ((next_seed(seed) >> 32) as usize) % text.len();
      EditorSemanticCommand::DeleteRange {
        range: DocumentOffset {
          paragraph: paragraph_ix,
          byte,
        }..DocumentOffset {
          paragraph: paragraph_ix,
          byte: byte + 1,
        },
      }
    },
    2 if paragraph_count < 24 => {
      let byte = ((next_seed(seed) >> 32) as usize) % (text.len() + 1);
      let block_ix = flowstate_document::block_ix_for_paragraph(projection, paragraph_ix).expect("every paragraph must have a block");
      EditorSemanticCommand::SplitParagraph {
        at: DocumentOffset {
          paragraph: paragraph_ix,
          byte,
        },
        source_paragraph: projection.ids.paragraph_ids[paragraph_ix],
        source_block: projection.ids.block_ids[block_ix],
        // Per-peer id namespaces keep editor-minted identities collision-free across replicas.
        new_paragraph: ParagraphId(0x5000_0000 + ((peer_ix as u128) << 20) + u128::from(step)),
        new_block: BlockId(0x6000_0000 + ((peer_ix as u128) << 20) + u128::from(step)),
        inherited_style: projection.paragraphs[paragraph_ix].style,
      }
    },
    _ => EditorSemanticCommand::SetParagraphStyle {
      paragraph: projection.ids.paragraph_ids[paragraph_ix],
      style: if step.is_multiple_of(2) {
        ParagraphStyle::Normal
      } else {
        ParagraphStyle::Custom(2)
      },
    },
  }
}

fn paragraph_texts(projection: &DocumentProjection) -> Vec<String> {
  (0..projection.paragraphs.len())
    .map(|paragraph_ix| flowstate_document::paragraph_text(projection, paragraph_ix))
    .collect()
}

/// Strict semantic equality with a compact paragraph-text diff on divergence.
fn assert_projections_converged(left: &DocumentProjection, right: &DocumentProjection, context: &str) {
  let left_texts = paragraph_texts(left);
  let right_texts = paragraph_texts(right);
  if left_texts != right_texts {
    eprintln!("paragraph text divergence ({context}):");
    for paragraph_ix in 0..left_texts.len().max(right_texts.len()) {
      let left_text = left_texts.get(paragraph_ix).map_or("<missing>", String::as_str);
      let right_text = right_texts.get(paragraph_ix).map_or("<missing>", String::as_str);
      if left_text != right_text {
        eprintln!("  paragraph {paragraph_ix}: left={left_text:?} right={right_text:?}");
      }
    }
    panic!("paragraph texts diverged: {context}");
  }
  assert_semantic_projection_eq(left, right, context);
}

#[test]
fn three_peer_random_interleaving_converges_semantically() -> Result<()> {
  let mut peers = seeded_peers(3, "Three peer convergence")?;
  synchronize_until_converged(&mut peers)?;
  let mut queues: Vec<PeerQueues> = vec![vec![Vec::new(); peers.len()]; peers.len()];
  let mut seed = 0x2545_f491_4f6c_dd1d_u64;

  // Per step a peer re-projects its whole (growing) document to build the next
  // command, so wall-clock grows ~quadratically with the step count; 80 random
  // interleaved operations across three peers keep the suite CI-fast while still
  // exercising deep out-of-order convergence.
  for step in 0..80_u64 {
    let want_delivery = (next_seed(&mut seed) >> 32) % 5 < 2;
    if want_delivery && deliver_random_pending_update(&mut peers, &mut queues, &mut seed, &format!("random phase step {step}")) {
      continue;
    }
    let peer_ix = ((next_seed(&mut seed) >> 32) as usize) % peers.len();
    let projection = peers[peer_ix].projection_snapshot()?;
    let command = random_editor_command(&mut seed, &projection, peer_ix, step);
    let transaction_id = ((peer_ix as u128) << 64) | u128::from(step + 1);
    // Each peer's edits are serialized against its own live projection, so a
    // staleness rejection here would be a runtime bug, not a fuzz artifact.
    let commit = peers[peer_ix]
      .apply_editor_commands(transaction_id, &projection.frontier, &[command], None)
      .unwrap_or_else(|error| panic!("peer {peer_ix} rejected a serialized local edit at step {step}: {error:?}"));
    enqueue_local_updates(&mut queues, peer_ix, &commit.events);
  }

  drain_all_queues_randomized(&mut peers, &mut queues, &mut seed)?;

  for peer_ix in 1..peers.len() {
    assert_eq!(
      peers[0].doc().state_vv(),
      peers[peer_ix].doc().state_vv(),
      "version vector mismatch after full drain: peer 0 vs peer {peer_ix}"
    );
    assert_eq!(
      peers[0].doc().state_frontiers().encode(),
      peers[peer_ix].doc().state_frontiers().encode(),
      "frontier mismatch after full drain: peer 0 vs peer {peer_ix}"
    );
    let left = peers[0].projection_snapshot()?;
    let right = peers[peer_ix].projection_snapshot()?;
    assert_projections_converged(&left, &right, &format!("peer 0 vs peer {peer_ix} after full drain"));
  }
  // Materializer equivalence invariant: each peer's incrementally maintained
  // projection matches a fresh full projection of its own LoroDoc.
  for (peer_ix, peer) in peers.iter().enumerate() {
    let incremental = peer.projection_snapshot()?;
    let fresh = document_from_loro(peer.doc())?;
    assert_projections_converged(&incremental, &fresh, &format!("peer {peer_ix} incremental vs fresh Loro projection"));
  }
  Ok(())
}

// ============================================================================
// N-peer full-operation convergence fuzz harness.
//
// Convergence is a PROPERTY: for any peer count and any sequence of valid
// operations, after all updates drain (a) every peer's projection is identical
// and (b) each peer's incrementally-maintained projection equals a fresh full
// `document_from_loro` rebuild. The blank-doc suites above never exercised
// objects, empty paragraphs, soft breaks, or tables, so they structurally could
// not reach the coordinate/coalescing paths that broke on the real doc. This
// harness seeds from structural fixtures, drives every op family from each peer's
// live projection, delivers updates out of order, and asserts the property across
// N peers. Op families are added incrementally; each returned command must be one
// the runtime SHOULD accept.
// ============================================================================

/// Per-peer-namespaced fresh id. Editor-minted paragraph/block/row/column ids must
/// never collide across replicas; real ids are Uuid-derived u128 so this small
/// structured space (per-peer high bits + monotonic sequence) effectively never
/// collides with them or across peers.
fn fresh_id(peer_ix: usize, op_seq: u64, salt: u64) -> u128 {
  0x5000_0000_u128 + ((peer_ix as u128) << 44) + (u128::from(op_seq) << 8) + u128::from(salt)
}

/// A uniformly random VALID char-boundary byte offset in `0..=text.len()`.
fn random_char_boundary(text: &str, seed: &mut u64) -> usize {
  let char_count = text.chars().count();
  let pick = ((next_seed(seed) >> 32) as usize) % (char_count + 1);
  text.char_indices().nth(pick).map_or(text.len(), |(byte, _)| byte)
}

/// Generate a random valid editor command from `projection` (this peer's live
/// state), or `None` when the chosen op doesn't fit the current document (caller
/// retries). Paragraph/text families for now; block/table/object families are
/// added as the generator grows toward full coverage.
fn generate_command(seed: &mut u64, projection: &DocumentProjection, peer_ix: usize, op_seq: u64) -> Option<EditorSemanticCommand> {
  let paragraph_count = projection.paragraphs.len();
  if paragraph_count == 0 {
    return None;
  }
  let paragraph_ix = ((next_seed(seed) >> 32) as usize) % paragraph_count;
  let text = flowstate_document::paragraph_text(projection, paragraph_ix);

  match (next_seed(seed) >> 32) % 10 {
    // Insert text; ~1/8 of inserts are an intra-paragraph soft line break (U+2028),
    // which must stay inside the paragraph and never become a body boundary.
    0..=2 => {
      let byte = random_char_boundary(&text, seed);
      let insert = if (next_seed(seed) >> 32).is_multiple_of(8) {
        "\u{2028}".to_string()
      } else {
        char::from(b'a' + ((next_seed(seed) >> 32) % 26) as u8).to_string()
      };
      Some(EditorSemanticCommand::InsertText {
        at: DocumentOffset { paragraph: paragraph_ix, byte },
        text: insert,
        styles: RunStyles::default(),
      })
    },
    // Delete a single char within a paragraph.
    3 if !text.is_empty() => {
      let chars: Vec<(usize, char)> = text.char_indices().collect();
      let pick = ((next_seed(seed) >> 32) as usize) % chars.len();
      let start = chars[pick].0;
      let end = start + chars[pick].1.len_utf8();
      Some(EditorSemanticCommand::DeleteRange {
        range: DocumentOffset { paragraph: paragraph_ix, byte: start }..DocumentOffset { paragraph: paragraph_ix, byte: end },
      })
    },
    // Split a paragraph at a random boundary.
    4 => {
      let byte = random_char_boundary(&text, seed);
      let block_ix = flowstate_document::block_ix_for_paragraph(projection, paragraph_ix)?;
      Some(EditorSemanticCommand::SplitParagraph {
        at: DocumentOffset { paragraph: paragraph_ix, byte },
        source_paragraph: projection.ids.paragraph_ids[paragraph_ix],
        source_block: projection.ids.block_ids[block_ix],
        new_paragraph: ParagraphId(fresh_id(peer_ix, op_seq, 1)),
        new_block: BlockId(fresh_id(peer_ix, op_seq, 2)),
        inherited_style: projection.paragraphs[paragraph_ix].style,
      })
    },
    // Join this paragraph with the previous one.
    5 if paragraph_ix > 0 => Some(EditorSemanticCommand::JoinParagraphs {
      first: projection.ids.paragraph_ids[paragraph_ix - 1],
      second: projection.ids.paragraph_ids[paragraph_ix],
    }),
    // Restyle a random sub-range of runs. Combined with soft-break inserts this drives
    // the concurrent-import coordinate paths that surfaced the touched-paragraph and
    // deleted-boundary divergences (fixed by the live-body-starts mapping + the
    // paragraph-count rebuild backstop in crdt_runtime.rs).
    6..=7 if !text.is_empty() => {
      let a = random_char_boundary(&text, seed);
      let b = random_char_boundary(&text, seed);
      let (start, end) = if a <= b { (a, b) } else { (b, a) };
      if start == end {
        return None;
      }
      let styles = RunStyles {
        semantic: if (next_seed(seed) >> 32).is_multiple_of(2) {
          flowstate_document::RunSemanticStyle::Plain
        } else {
          flowstate_document::RunSemanticStyle::Custom(((next_seed(seed) >> 32) % 3) as u8)
        },
        direct_underline: (next_seed(seed) >> 32).is_multiple_of(3),
        strikethrough: (next_seed(seed) >> 32).is_multiple_of(5),
        highlight: None,
      };
      Some(EditorSemanticCommand::SetRunStyles {
        paragraph: projection.ids.paragraph_ids[paragraph_ix],
        range: start..end,
        styles,
      })
    },
    // Toggle paragraph style.
    _ => Some(EditorSemanticCommand::SetParagraphStyle {
      paragraph: projection.ids.paragraph_ids[paragraph_ix],
      style: if (next_seed(seed) >> 32).is_multiple_of(2) {
        ParagraphStyle::Normal
      } else {
        ParagraphStyle::Custom(((next_seed(seed) >> 32) % 4) as u8)
      },
    }),
  }
}

/// Drive `peer_count` peers seeded from `base` through `steps` rounds of concurrent
/// random edits with out-of-order delivery, then assert the convergence property.
/// Apply errors on generated edge-commands are tolerated (counted, skipped); the
/// real signal is the post-drain projection equality across all peers and the
/// incremental-vs-full materializer equivalence per peer.
fn run_convergence_fuzz(peer_count: usize, base: &LoroDoc, steps: u64, seed_init: u64) -> Result<()> {
  run_convergence_fuzz_with(peer_count, base, steps, seed_init, generate_command)
}

type CommandGenerator = fn(&mut u64, &DocumentProjection, usize, u64) -> Option<EditorSemanticCommand>;

fn run_convergence_fuzz_with(peer_count: usize, base: &LoroDoc, steps: u64, seed_init: u64, generator: CommandGenerator) -> Result<()> {
  let mut peers = seeded_peers_from_loro(peer_count, base)?;
  synchronize_until_converged(&mut peers)?;
  let mut queues: Vec<PeerQueues> = vec![vec![Vec::new(); peer_count]; peer_count];
  let mut seed = seed_init.max(1);
  let mut op_seq = 0_u64;
  let (mut applied, mut rejected, mut skipped) = (0_u64, 0_u64, 0_u64);

  for step in 0..steps {
    // Every peer edits concurrently from its own live projection each round — the
    // genuine simultaneous edits a single tester can't produce by hand.
    #[allow(clippy::needless_range_loop, reason = "indexed access: each peer applies its own edit and mutates peers[peer_ix]")]
    for peer_ix in 0..peer_count {
      let projection = peers[peer_ix].projection_snapshot()?;
      let mut command = None;
      for _ in 0..8 {
        if let Some(candidate) = generator(&mut seed, &projection, peer_ix, op_seq) {
          command = Some(candidate);
          break;
        }
      }
      let Some(command) = command else {
        skipped += 1;
        continue;
      };
      op_seq += 1;
      let transaction_id = ((peer_ix as u128) << 96) | ((step as u128) << 40) | u128::from(op_seq);
      match peers[peer_ix].apply_editor_commands(transaction_id, &projection.frontier, &[command], None) {
        Ok(commit) => {
          applied += 1;
          enqueue_local_updates(&mut queues, peer_ix, &commit.events);
        },
        Err(_error) => rejected += 1,
      }
    }
    // Deliver a random out-of-order subset, leaving some updates in flight so peers
    // stay concurrently diverged.
    let deliveries = (next_seed(&mut seed) >> 32) % 4;
    for _ in 0..deliveries {
      if !deliver_random_pending_update(&mut peers, &mut queues, &mut seed, "fuzz") {
        break;
      }
    }
  }

  drain_all_queues_randomized(&mut peers, &mut queues, &mut seed)?;
  synchronize_until_converged(&mut peers)?;

  let context = format!("N={peer_count} seed={seed_init} (applied={applied} rejected={rejected} skipped={skipped})");
  for peer_ix in 1..peer_count {
    assert_eq!(
      peers[0].doc().state_vv(),
      peers[peer_ix].doc().state_vv(),
      "version vector mismatch: peer 0 vs {peer_ix} [{context}]"
    );
    let left = peers[0].projection_snapshot()?;
    let right = peers[peer_ix].projection_snapshot()?;
    assert_projections_converged(&left, &right, &format!("peer 0 vs {peer_ix} [{context}]"));
  }
  for (peer_ix, peer) in peers.iter().enumerate() {
    let incremental = peer.projection_snapshot()?;
    let fresh = document_from_loro(peer.doc())?;
    assert_projections_converged(&incremental, &fresh, &format!("peer {peer_ix} incremental-vs-full [{context}]"));
  }
  eprintln!("fuzz ok: {context}");
  Ok(())
}

// KNOWN-FAILING (ignored): surfaces the incremental-vs-full COALESCING PARITY bug.
// `document_from_loro`/`push_flow_blocks` coalesces an object-adjacent empty paragraph,
// but the incremental replay does not — so an edit that turns an object-adjacent
// paragraph empty (e.g. deleting its last char) leaves the incremental projection with
// one more paragraph than the full rebuild. Minimal single-peer repro: structural_fixture,
// seed 0xB2, 15 ops (a "Two empties above me." split into "T"+"wo…", then delete the "T").
// See docs/collab-coalescing-parity.md. Un-ignore once the incremental path coalesces
// object-adjacent empties to match the full rebuild.
#[test]
fn npeer_fuzz_structural_fixture_paragraph_ops() -> Result<()> {
  for peer_count in 2..=5 {
    for seed in [0xA1u64, 0xC3] {
      let base = structural_fixture()?;
      run_convergence_fuzz(peer_count, &base, 120, seed)?;
    }
  }
  Ok(())
}

/// High-density coordinate-stress command generator: a heavier mix of intra-paragraph
/// soft-break (U+2028) inserts and `SetRunStyles` sub-range restyles than the general
/// fuzz generator, deliberately tuned to hammer the concurrent-remote-import coordinate
/// paths — the touched-paragraph mapping (`paragraph_at_body_unicode_with`) and the
/// deleted-boundary structural detection. Regressions here manifest as an incremental
/// projection that drops a soft break (missed touched paragraph) or keeps a paragraph the
/// authoritative rebuild dropped (missed deleted boundary). Used by
/// `npeer_incremental_import_equivalence_under_coordinate_stress`.
fn coordinate_stress_command(seed: &mut u64, projection: &DocumentProjection, peer_ix: usize, op_seq: u64) -> Option<EditorSemanticCommand> {
  let paragraph_count = projection.paragraphs.len();
  if paragraph_count == 0 {
    return None;
  }
  let paragraph_ix = ((next_seed(seed) >> 32) as usize) % paragraph_count;
  let text = flowstate_document::paragraph_text(projection, paragraph_ix);
  match (next_seed(seed) >> 32) % 10 {
    0..=2 => {
      let byte = random_char_boundary(&text, seed);
      let insert = if (next_seed(seed) >> 32).is_multiple_of(4) {
        "\u{2028}".to_string()
      } else {
        char::from(b'a' + ((next_seed(seed) >> 32) % 26) as u8).to_string()
      };
      Some(EditorSemanticCommand::InsertText {
        at: DocumentOffset { paragraph: paragraph_ix, byte },
        text: insert,
        styles: RunStyles::default(),
      })
    },
    3 if !text.is_empty() => {
      let chars: Vec<(usize, char)> = text.char_indices().collect();
      let pick = ((next_seed(seed) >> 32) as usize) % chars.len();
      let start = chars[pick].0;
      let end = start + chars[pick].1.len_utf8();
      Some(EditorSemanticCommand::DeleteRange {
        range: DocumentOffset { paragraph: paragraph_ix, byte: start }..DocumentOffset { paragraph: paragraph_ix, byte: end },
      })
    },
    4 => {
      let byte = random_char_boundary(&text, seed);
      let block_ix = flowstate_document::block_ix_for_paragraph(projection, paragraph_ix)?;
      Some(EditorSemanticCommand::SplitParagraph {
        at: DocumentOffset { paragraph: paragraph_ix, byte },
        source_paragraph: projection.ids.paragraph_ids[paragraph_ix],
        source_block: projection.ids.block_ids[block_ix],
        new_paragraph: ParagraphId(fresh_id(peer_ix, op_seq, 1)),
        new_block: BlockId(fresh_id(peer_ix, op_seq, 2)),
        inherited_style: projection.paragraphs[paragraph_ix].style,
      })
    },
    5 if paragraph_ix > 0 => Some(EditorSemanticCommand::JoinParagraphs {
      first: projection.ids.paragraph_ids[paragraph_ix - 1],
      second: projection.ids.paragraph_ids[paragraph_ix],
    }),
    // SetRunStyles over a random sub-range.
    6..=7 if !text.is_empty() => {
      let a = random_char_boundary(&text, seed);
      let b = random_char_boundary(&text, seed);
      let (start, end) = if a <= b { (a, b) } else { (b, a) };
      if start == end {
        return None;
      }
      let styles = RunStyles {
        semantic: if (next_seed(seed) >> 32).is_multiple_of(2) {
          flowstate_document::RunSemanticStyle::Plain
        } else {
          flowstate_document::RunSemanticStyle::Custom(((next_seed(seed) >> 32) % 3) as u8)
        },
        direct_underline: (next_seed(seed) >> 32).is_multiple_of(3),
        strikethrough: (next_seed(seed) >> 32).is_multiple_of(5),
        highlight: None,
      };
      Some(EditorSemanticCommand::SetRunStyles {
        paragraph: projection.ids.paragraph_ids[paragraph_ix],
        range: start..end,
        styles,
      })
    },
    _ => Some(EditorSemanticCommand::SetParagraphStyle {
      paragraph: projection.ids.paragraph_ids[paragraph_ix],
      style: if (next_seed(seed) >> 32).is_multiple_of(2) {
        ParagraphStyle::Normal
      } else {
        ParagraphStyle::Custom(((next_seed(seed) >> 32) % 4) as u8)
      },
    }),
  }
}

/// Assert peer `peer_ix`'s incrementally-maintained projection equals a fresh full
/// `document_from_loro` rebuild of its own doc (the materializer-equivalence invariant),
/// dumping the accumulated event history on the first divergence for pinpoint debugging.
fn assert_incremental_matches_fresh(peers: &[CrdtRuntime], peer_ix: usize, context: &str, history: &[String]) -> Result<bool> {
  let incremental = peers[peer_ix].projection_snapshot()?;
  let fresh = document_from_loro(peers[peer_ix].doc())?;
  let inc_texts = paragraph_texts(&incremental);
  let fresh_texts = paragraph_texts(&fresh);
  if inc_texts != fresh_texts || incremental.ids.paragraph_ids != fresh.ids.paragraph_ids {
    eprintln!("DIVERGENCE at peer {peer_ix} ({context})");
    eprintln!("incremental paras ({}): {inc_texts:?}", inc_texts.len());
    eprintln!("fresh       paras ({}): {fresh_texts:?}", fresh_texts.len());
    eprintln!("incremental ids: {:?}", incremental.ids.paragraph_ids);
    eprintln!("fresh       ids: {:?}", fresh.ids.paragraph_ids);
    eprintln!("--- event history ({}) ---", history.len());
    for (i, line) in history.iter().enumerate() {
      eprintln!("  [{i}] {line}");
    }
    return Ok(false);
  }
  Ok(true)
}

/// Regression for the concurrent-import coordinate bugs (crdt_runtime.rs): under a heavy
/// mix of soft-break inserts and run-style edits across peers with out-of-order delivery,
/// each peer's incremental projection must equal a fresh rebuild after EVERY local apply
/// AND EVERY remote import (not just at the converged end state — the divergences were
/// transient-then-persistent). Before the fix, the incremental path dropped a soft break
/// (touched-paragraph mapping used the stale pre-import unicode index) and kept a
/// paragraph the authority dropped (deleted-boundary detection missed the structural
/// change). Fixes: `paragraph_at_body_unicode_with` maps against live-body starts, plus a
/// paragraph-count rebuild backstop.
#[test]
fn npeer_incremental_import_equivalence_under_coordinate_stress() -> Result<()> {
  for peer_count in [2usize, 3] {
    for seed_init in [0x1111u64, 0x2222, 0x3333, 0xB2, 0xC3, 0xDEAD] {
      let base = new_loro_document("coordinate stress")?;
      let mut peers = seeded_peers_from_loro(peer_count, &base)?;
      synchronize_until_converged(&mut peers)?;
      let mut queues: Vec<PeerQueues> = vec![vec![Vec::new(); peer_count]; peer_count];
      let mut seed = seed_init.max(1);
      let mut op_seq = 0u64;
      let mut history: Vec<String> = Vec::new();
      for step in 0..150u64 {
        #[allow(clippy::needless_range_loop)]
        for peer_ix in 0..peer_count {
          let projection = peers[peer_ix].projection_snapshot()?;
          let Some(command) = coordinate_stress_command(&mut seed, &projection, peer_ix, op_seq) else {
            continue;
          };
          op_seq += 1;
          let transaction_id = ((peer_ix as u128) << 96) | ((step as u128) << 40) | u128::from(op_seq);
          match peers[peer_ix].apply_editor_commands(transaction_id, &projection.frontier, &[command.clone()], None) {
            Ok(commit) => {
              history.push(format!("peer {peer_ix} APPLY {command:?}"));
              enqueue_local_updates(&mut queues, peer_ix, &commit.events);
              if !assert_incremental_matches_fresh(&peers, peer_ix, &format!("after local apply step {step}"), &history)? {
                panic!("divergence after local apply: N={peer_count} seed={seed_init:#x} step={step}");
              }
            },
            Err(_) => {},
          }
        }
        let deliveries = (next_seed(&mut seed) >> 32) % 4;
        for _ in 0..deliveries {
          let pending = pending_queue_pairs(&queues);
          if pending.is_empty() {
            break;
          }
          let (recipient_ix, sender_ix) = pending[((next_seed(&mut seed) >> 32) as usize) % pending.len()];
          let queue = &mut queues[recipient_ix][sender_ix];
          let position = ((next_seed(&mut seed) >> 32) as usize) % queue.len();
          let update = queue.remove(position);
          let events = peers[recipient_ix].import_remote_update(&update)?;
          history.push(format!("peer {recipient_ix} IMPORT from {sender_ix}"));
          enqueue_local_updates(&mut queues, recipient_ix, &events);
          if !assert_incremental_matches_fresh(&peers, recipient_ix, &format!("after import step {step}"), &history)? {
            panic!("divergence after import: N={peer_count} seed={seed_init:#x} step={step}");
          }
        }
      }
      // Checked drain: deliver each remaining update and verify equivalence right after.
      for drain_step in 0..20_000u64 {
        let pending = pending_queue_pairs(&queues);
        if pending.is_empty() {
          break;
        }
        let (recipient_ix, sender_ix) = pending[((next_seed(&mut seed) >> 32) as usize) % pending.len()];
        let queue = &mut queues[recipient_ix][sender_ix];
        let position = ((next_seed(&mut seed) >> 32) as usize) % queue.len();
        let update = queue.remove(position);
        let events = peers[recipient_ix].import_remote_update(&update)?;
        history.push(format!("peer {recipient_ix} DRAIN-IMPORT from {sender_ix} (drain {drain_step})"));
        enqueue_local_updates(&mut queues, recipient_ix, &events);
        if !assert_incremental_matches_fresh(&peers, recipient_ix, &format!("after drain-import {drain_step}"), &history)? {
          panic!("divergence during drain: N={peer_count} seed={seed_init:#x} drain_step={drain_step}");
        }
      }
      synchronize_until_converged(&mut peers)?;
      for peer_ix in 0..peer_count {
        if !assert_incremental_matches_fresh(&peers, peer_ix, "after full drain", &history)? {
          panic!("divergence after drain: N={peer_count} seed={seed_init:#x}");
        }
      }
      eprintln!("coord-stress ok N={peer_count} seed={seed_init:#x} ops={op_seq}");
    }
  }
  Ok(())
}

fn fuzz_object_input(seed: &mut u64) -> flowstate_document::InputBlock {
  use flowstate_document::{InputBlock, InputBlockAlignment, InputEquationBlock, InputEquationDisplay, InputEquationSyntax, InputImageBlock, InputImageSizing};
  if (next_seed(seed) >> 32).is_multiple_of(2) {
    InputBlock::Image(InputImageBlock {
      asset_id: flowstate_document::AssetId(1),
      alt_text: format!("img{}", (next_seed(seed) >> 32) % 100),
      caption: None,
      sizing: InputImageSizing::Intrinsic,
      alignment: InputBlockAlignment::Left,
    })
  } else {
    InputBlock::Equation(InputEquationBlock {
      source: format!("x^{}", (next_seed(seed) >> 32) % 10),
      syntax: InputEquationSyntax::Latex,
      display: InputEquationDisplay::Display,
    })
  }
}

fn object_block_indices(projection: &DocumentProjection) -> Vec<usize> {
  (0..projection.blocks.len())
    .filter(|ix| !matches!(projection.blocks[*ix], flowstate_document::Block::Paragraph(_)))
    .collect()
}

/// Full object block-structure command generator: InsertBlock / DeleteBlock / MoveBlock /
/// ReplaceBlock at ANY position (including the leading index 0), interleaved with
/// paragraph coordinate stress. Exercises the object coalescing + sentinel-first-region
/// paths that the incremental replay cannot model — now that object-bearing docs adopt the
/// canonical rebuild as their prediction (crdt_runtime.rs), the runtime converges over the
/// whole object-structural surface.
fn object_structural_command(seed: &mut u64, projection: &DocumentProjection, peer_ix: usize, op_seq: u64) -> Option<EditorSemanticCommand> {
  // ~40% object block ops, else paragraph coordinate stress (keeps objects churning).
  if (next_seed(seed) >> 32) % 100 >= 40 {
    return coordinate_stress_command(seed, projection, peer_ix, op_seq);
  }
  let block_count = projection.blocks.len();
  let objects = object_block_indices(projection);
  let image_ix = |seed: &mut u64| {
    let images: Vec<usize> = (0..projection.blocks.len())
      .filter(|ix| matches!(projection.blocks[*ix], flowstate_document::Block::Image(_)))
      .collect();
    (!images.is_empty()).then(|| images[((next_seed(seed) >> 32) as usize) % images.len()])
  };
  let equation_ix = |seed: &mut u64| {
    let eqs: Vec<usize> = (0..projection.blocks.len())
      .filter(|ix| matches!(projection.blocks[*ix], flowstate_document::Block::Equation(_)))
      .collect();
    (!eqs.is_empty()).then(|| eqs[((next_seed(seed) >> 32) as usize) % eqs.len()])
  };
  match (next_seed(seed) >> 32) % 7 {
    // InsertBlock at any projection block index (0..=len; 0 = new leading block).
    0 => Some(EditorSemanticCommand::InsertBlock {
      block: BlockId(fresh_id(peer_ix, op_seq, 3)),
      block_ix: ((next_seed(seed) >> 32) as usize) % (block_count + 1),
      after: fuzz_object_input(seed),
    }),
    // DeleteBlock an existing object.
    1 if !objects.is_empty() => {
      let ix = objects[((next_seed(seed) >> 32) as usize) % objects.len()];
      Some(EditorSemanticCommand::DeleteBlock { block: projection.ids.block_ids[ix] })
    },
    // MoveBlock an existing object to any index (including 0).
    2 if !objects.is_empty() && block_count > 1 => {
      let ix = objects[((next_seed(seed) >> 32) as usize) % objects.len()];
      Some(EditorSemanticCommand::MoveBlock {
        block: projection.ids.block_ids[ix],
        new_block_ix: ((next_seed(seed) >> 32) as usize) % block_count,
      })
    },
    // ReplaceBlock an existing object with a fresh object.
    3 if !objects.is_empty() => {
      let ix = objects[((next_seed(seed) >> 32) as usize) % objects.len()];
      Some(EditorSemanticCommand::ReplaceBlock {
        block: Some(projection.ids.block_ids[ix]),
        block_ix: ix,
        after: fuzz_object_input(seed),
      })
    },
    // Image property ops (alt text / layout / caption) on an existing image.
    4 => image_ix(seed).map(|ix| EditorSemanticCommand::ReplaceImageAltText {
      image: projection.ids.block_ids[ix],
      text: format!("alt-{}", (next_seed(seed) >> 32) % 1000),
    }),
    5 => image_ix(seed).map(|ix| {
      use flowstate_document::{InputBlockAlignment, InputImageSizing, InputParagraph, InputRun};
      match (next_seed(seed) >> 32) % 2 {
        0 => EditorSemanticCommand::SetImageLayout {
          image: projection.ids.block_ids[ix],
          sizing: match (next_seed(seed) >> 32) % 3 {
            0 => InputImageSizing::Intrinsic,
            1 => InputImageSizing::FitWidth,
            _ => InputImageSizing::Fixed { width_px: 100 + ((next_seed(seed) >> 32) % 200) as u32, height_px: None },
          },
          alignment: match (next_seed(seed) >> 32) % 3 {
            0 => InputBlockAlignment::Left,
            1 => InputBlockAlignment::Center,
            _ => InputBlockAlignment::Right,
          },
        },
        _ => EditorSemanticCommand::ReplaceImageCaption {
          image: projection.ids.block_ids[ix],
          caption: ((next_seed(seed) >> 32).is_multiple_of(2)).then(|| InputParagraph {
            style: ParagraphStyle::Normal,
            runs: vec![InputRun { text: format!("cap-{}", (next_seed(seed) >> 32) % 1000), styles: RunStyles::default() }],
          }),
        },
      }
    }),
    // Equation source edit on an existing equation.
    _ => equation_ix(seed).map(|ix| EditorSemanticCommand::ReplaceEquationSourceRange {
      equation: projection.ids.block_ids[ix],
      range: 0..0,
      text: format!("+{}", (next_seed(seed) >> 32) % 10),
    }),
  }
}

/// Full table-op command generator over the first table in `projection`: the 9 table
/// SemanticEditCommand variants (row/column insert-delete-move, cell replace, cell span,
/// column width), interleaved with paragraph coordinate stress. Editor-minted row/column/
/// cell ids use the per-peer `fresh_id` space so they never collide across replicas.
fn table_op_command(seed: &mut u64, projection: &DocumentProjection, peer_ix: usize, op_seq: u64) -> Option<EditorSemanticCommand> {
  if (next_seed(seed) >> 32) % 100 >= 55 {
    return coordinate_stress_command(seed, projection, peer_ix, op_seq);
  }
  let (table_id, table) = first_table(projection)?;
  if table.rows.is_empty() || table.columns.is_empty() {
    return None;
  }
  let pick = |seed: &mut u64, n: usize| ((next_seed(seed) >> 32) as usize) % n.max(1);
  let row = &table.rows[pick(seed, table.rows.len())];
  let column = &table.columns[pick(seed, table.columns.len())];
  match (next_seed(seed) >> 32) % 9 {
    // InsertTableRow after a random row (or head), with one cell per existing column.
    0 => {
      let new_row_id = RowId(fresh_id(peer_ix, op_seq, 10));
      let cells = table
        .columns
        .iter()
        .enumerate()
        .map(|(c, col)| table_cell(fresh_id(peer_ix, op_seq, 20 + c as u64), new_row_id, col.id, "new"))
        .collect();
      Some(EditorSemanticCommand::InsertTableRow {
        table: table_id,
        new_row_id,
        after_row: if (next_seed(seed) >> 32).is_multiple_of(3) { None } else { Some(row.id) },
        row: InputTableRow { id: new_row_id, cells },
      })
    },
    // DeleteTableRow (only when more than one row remains meaningful).
    1 if table.rows.len() > 1 => Some(EditorSemanticCommand::DeleteTableRow { table: table_id, row_id: row.id }),
    // MoveTableRow to after a random row (or head).
    2 if table.rows.len() > 1 => Some(EditorSemanticCommand::MoveTableRow {
      table: table_id,
      row_id: row.id,
      after_row: if (next_seed(seed) >> 32).is_multiple_of(3) { None } else { Some(table.rows[pick(seed, table.rows.len())].id) },
    }),
    // InsertTableColumn after a random column (or head), with one cell per existing row.
    3 => {
      let new_column_id = ColumnId(fresh_id(peer_ix, op_seq, 30));
      let cells = table
        .rows
        .iter()
        .enumerate()
        .map(|(r, tr)| table_cell(fresh_id(peer_ix, op_seq, 40 + r as u64), tr.id, new_column_id, "col"))
        .collect();
      Some(EditorSemanticCommand::InsertTableColumn {
        table: table_id,
        new_column_id,
        after_column: if (next_seed(seed) >> 32).is_multiple_of(3) { None } else { Some(column.id) },
        width: InputTableColumnWidth::Auto,
        cells,
      })
    },
    // DeleteTableColumn (only when more than one column remains).
    4 if table.columns.len() > 1 => Some(EditorSemanticCommand::DeleteTableColumn { table: table_id, column_id: column.id }),
    // MoveTableColumn to after a random column (or head).
    5 if table.columns.len() > 1 => Some(EditorSemanticCommand::MoveTableColumn {
      table: table_id,
      column_id: column.id,
      after_column: if (next_seed(seed) >> 32).is_multiple_of(3) { None } else { Some(table.columns[pick(seed, table.columns.len())].id) },
    }),
    // ReplaceTableCell at (row, column) with fresh content.
    6 => Some(EditorSemanticCommand::ReplaceTableCell {
      table: table_id,
      row_id: row.id,
      column_id: column.id,
      cell: table_cell(fresh_id(peer_ix, op_seq, 50), row.id, column.id, "edited"),
    }),
    // SetTableCellSpan (bounded small spans).
    7 => Some(EditorSemanticCommand::SetTableCellSpan {
      table: table_id,
      row_id: row.id,
      column_id: column.id,
      row_span: 1 + ((next_seed(seed) >> 32) % 2) as u16,
      column_span: 1 + ((next_seed(seed) >> 32) % 2) as u16,
    }),
    // SetTableColumnWidth on a random column index.
    _ => Some(EditorSemanticCommand::SetTableColumnWidth {
      table: table_id,
      column_ix: pick(seed, table.columns.len()),
      width: match (next_seed(seed) >> 32) % 3 {
        0 => InputTableColumnWidth::Auto,
        1 => InputTableColumnWidth::FixedPx(50 + ((next_seed(seed) >> 32) % 200) as u32),
        _ => InputTableColumnWidth::Fraction(1 + ((next_seed(seed) >> 32) % 4) as u32),
      },
    }),
  }
}

/// Generate a `ReplaceParagraphSpan` replacing a random paragraph range with a single
/// fresh paragraph (the compound paste/replace op) — captured `before` from the live
/// projection so the staged validation accepts it, fresh per-peer-namespaced ids on
/// `after` so identities never collide. Interleaved with paragraph coordinate stress.
fn replace_span_command(seed: &mut u64, projection: &DocumentProjection, peer_ix: usize, op_seq: u64) -> Option<EditorSemanticCommand> {
  use flowstate_document::{DocumentSpan, Paragraph, TextRun};
  if (next_seed(seed) >> 32) % 100 >= 35 {
    return coordinate_stress_command(seed, projection, peer_ix, op_seq);
  }
  let paragraph_count = projection.paragraphs.len();
  if paragraph_count == 0 {
    return None;
  }
  let start = ((next_seed(seed) >> 32) as usize) % paragraph_count;
  let end = (start + 1 + ((next_seed(seed) >> 32) as usize) % 2).min(paragraph_count);
  let before = flowstate_document::capture_document_span(projection, start..end);
  if before.paragraph_ids.len() != before.paragraphs.len() || before.block_ids.is_empty() {
    return None;
  }
  let text = format!("span{}", (next_seed(seed) >> 32) % 1000);
  let after = DocumentSpan {
    start_paragraph: start,
    paragraphs: vec![Paragraph {
      style: ParagraphStyle::Normal,
      byte_range: 0..text.len(),
      runs: vec![TextRun { len: text.len(), styles: RunStyles::default() }],
      version: 0,
    }],
    paragraph_ids: vec![ParagraphId(fresh_id(peer_ix, op_seq, 60))],
    block_ids: vec![BlockId(fresh_id(peer_ix, op_seq, 61))],
    text,
  };
  Some(EditorSemanticCommand::ReplaceParagraphSpan {
    start: Some(DocumentOffset { paragraph: start, byte: 0 }),
    before,
    after,
  })
}

/// N-peer convergence over `ReplaceParagraphSpan` (the compound replace/paste op) plus
/// paragraph coordinate stress, on the blank and structural fixtures.
#[test]
fn npeer_fuzz_replace_paragraph_span() -> Result<()> {
  for peer_count in 2..=3 {
    for seed in [0x1111u64, 0x2222, 0xB2] {
      let blank = new_loro_document("Replace span blank")?;
      run_convergence_fuzz_with(peer_count, &blank, 100, seed, replace_span_command)?;
      let structural = structural_fixture()?;
      run_convergence_fuzz_with(peer_count, &structural, 100, seed, replace_span_command)?;
    }
  }
  Ok(())
}

/// N-peer convergence over the full table-op surface (the 9 table SemanticEditCommand
/// variants) plus paragraph coordinate stress, seeded from the table fixture, with
/// out-of-order delivery. Asserts every peer converges AND each peer's incremental
/// projection equals a fresh rebuild.
#[test]
fn npeer_fuzz_table_ops() -> Result<()> {
  for peer_count in 2..=4 {
    for seed in [0x1111u64, 0x2222, 0xB2] {
      let base = table_fixture()?;
      run_convergence_fuzz_with(peer_count, &base, 120, seed, table_op_command)?;
    }
  }
  Ok(())
}

/// Single-peer regression for the object block-structure ops. Combines the positioning fix
/// (`projection_block_lead_pos_in_loro`) with the object-doc canonical-prediction adoption:
/// drives Insert/Delete/Move/Replace at all positions on the object-bearing structural
/// fixture and asserts the incrementally-maintained projection equals a fresh
/// `document_from_loro` after every op.
#[test]
fn object_block_positioning_single_peer() -> Result<()> {
  for seed_init in [0x1u64, 0x2, 0x3, 0x7, 0x11, 0xB2, 0xC3, 0x2222] {
    let base = structural_fixture()?;
    let mut peer = seeded_peers_from_loro(1, &base)?.pop().unwrap();
    let mut seed = seed_init.max(1);
    let mut history: Vec<String> = Vec::new();
    for op_seq in 0..300u64 {
      let projection = peer.projection_snapshot()?;
      let Some(command) = object_structural_command(&mut seed, &projection, 0, op_seq) else {
        continue;
      };
      let transaction_id = u128::from(op_seq + 1);
      if peer.apply_editor_commands(transaction_id, &projection.frontier, &[command.clone()], None).is_err() {
        continue;
      }
      history.push(format!("APPLY {command:?}"));
      if !assert_incremental_matches_fresh(std::slice::from_ref(&peer), 0, &format!("after op {op_seq}"), &history)? {
        panic!("object block-structure divergence at seed={seed_init:#x} op_seq={op_seq}");
      }
    }
    eprintln!("block-positioning ok seed={seed_init:#x} ops={}", history.len());
  }
  Ok(())
}

/// N-peer convergence over the FULL object block-structure surface (Insert/Delete/Move/
/// Replace at all positions) plus paragraph coordinate stress, seeded from the
/// object-bearing structural fixture, with out-of-order delivery. Asserts every peer's
/// projection converges AND each peer's incremental projection equals a fresh rebuild.
#[test]
fn npeer_fuzz_object_structural_ops() -> Result<()> {
  for peer_count in 2..=4 {
    for seed in [0x1111u64, 0x2222, 0xB2] {
      let base = structural_fixture()?;
      run_convergence_fuzz_with(peer_count, &base, 120, seed, object_structural_command)?;
    }
  }
  Ok(())
}

#[test]
fn npeer_fuzz_blank_paragraph_ops() -> Result<()> {
  for peer_count in 2..=5 {
    for seed in [0x1111u64, 0x2222] {
      let base = new_loro_document("Blank fuzz")?;
      run_convergence_fuzz(peer_count, &base, 120, seed)?;
    }
  }
  Ok(())
}

#[test]
fn two_peer_concurrent_same_paragraph_edits_converge() -> Result<()> {
  let mut peers = seeded_peers(2, "Two peer concurrent paragraph")?;
  synchronize_until_converged(&mut peers)?;

  let base = peers[0].projection_snapshot()?;
  let seed_commit = peers[0].apply_editor_commands(
    1,
    &base.frontier,
    &[EditorSemanticCommand::InsertText {
      at: DocumentOffset { paragraph: 0, byte: 0 },
      text: "hello world".to_string(),
      styles: RunStyles::default(),
    }],
    None,
  )?;
  let seed_updates = collect_local_update_bytes(&seed_commit.events);
  assert!(!seed_updates.is_empty(), "seed edit must emit local update bytes");
  for update in &seed_updates {
    peers[1].import_remote_update(update)?;
  }
  synchronize_until_converged(&mut peers)?;

  let base_a = peers[0].projection_snapshot()?;
  let base_b = peers[1].projection_snapshot()?;
  assert_eq!(base_a.frontier, base_b.frontier, "peers must share a frontier before the concurrent edits");
  let shared_paragraph = base_a.ids.paragraph_ids[0];
  assert_eq!(shared_paragraph, base_b.ids.paragraph_ids[0]);

  // Concurrent batches against the same base frontier and the same paragraph.
  let commit_a = peers[0].apply_editor_commands(
    2,
    &base_a.frontier,
    &[
      EditorSemanticCommand::InsertText {
        at: DocumentOffset { paragraph: 0, byte: 0 },
        text: "A>".to_string(),
        styles: RunStyles::default(),
      },
      EditorSemanticCommand::SetParagraphStyle {
        paragraph: shared_paragraph,
        style: ParagraphStyle::Custom(2),
      },
    ],
    None,
  )?;
  let commit_b = peers[1].apply_editor_commands(
    3,
    &base_b.frontier,
    &[
      EditorSemanticCommand::InsertText {
        at: DocumentOffset {
          paragraph: 0,
          byte: "hello world".len(),
        },
        text: "<B".to_string(),
        styles: RunStyles::default(),
      },
      EditorSemanticCommand::SetParagraphStyle {
        paragraph: shared_paragraph,
        style: ParagraphStyle::Custom(3),
      },
    ],
    None,
  )?;
  for update in collect_local_update_bytes(&commit_a.events) {
    peers[1].import_remote_update(&update)?;
  }
  for update in collect_local_update_bytes(&commit_b.events) {
    peers[0].import_remote_update(&update)?;
  }
  // Mop up any import-time repair updates before asserting convergence.
  synchronize_until_converged(&mut peers)?;

  let left = peers[0].projection_snapshot()?;
  let right = peers[1].projection_snapshot()?;
  assert_projections_converged(&left, &right, "two peers after concurrent same-paragraph edits");
  assert_eq!(
    flowstate_document::paragraph_text(&left, 0),
    "A>hello world<B",
    "both concurrent inserts must survive the merge"
  );
  let converged_style = left.paragraphs[0].style;
  assert!(
    matches!(converged_style, ParagraphStyle::Custom(2) | ParagraphStyle::Custom(3)),
    "converged paragraph style must be one of the concurrently applied styles, got {converged_style:?}"
  );
  for (peer_ix, peer) in peers.iter().enumerate() {
    let incremental = peer.projection_snapshot()?;
    let fresh = document_from_loro(peer.doc())?;
    assert_projections_converged(&incremental, &fresh, &format!("peer {peer_ix} incremental vs fresh Loro projection"));
  }
  Ok(())
}


