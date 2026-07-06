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
