#[cfg(test)]
mod tests {
  use std::{fmt::Write as _, ops::Range, sync::{Arc, Mutex}};

  use anyhow::{Context as _, Result, anyhow, bail};
  use flowstate_collab::{
    SessionId,
    binding::DocBinding,
    local_apply::LocalApplier,
    patch_apply::apply_patches,
    projection, schema,
  };
  use gpui_flowtext::{
    Block, CanonicalOperation, Document, DocumentOffset, DocumentTheme, HighlightStyle, InputBlock, InputEquationBlock, InputEquationDisplay,
    InputEquationSyntax, InputParagraph, ParagraphStyle, RunSemanticStyle, RunStyle, RunStyles, block_from_input_block,
    delete_cross_paragraph_range, delete_range_in_paragraph, document_from_input_blocks, insert_block_id, insert_text_at, mutate_runs_in_range,
    paragraph_text, paragraphs_mut, plain, remove_block_ids, split_paragraph_at, update_paragraph_block,
  };
  use loro::{ExportMode, LoroDoc, Subscription, event::Subscriber};
  use proptest::{prelude::*, test_runner::TestCaseError};

  const MULTIBYTE: &str = "aé🌍\u{2028}x";

  proptest! {
    #![proptest_config(ProptestConfig { cases: 256, max_shrink_iters: 2048, ..ProptestConfig::default() })]

    #[test]
    fn peers_converge_with_reordered_duplicate_and_buffered_updates(
      peer_count in 2usize..=3,
      delivery_seed in any::<u64>(),
      ops in prop::collection::vec(fuzz_op(), 1..28),
    ) {
      run_program(peer_count, delivery_seed, &ops)
        .map_err(|error| TestCaseError::fail(format!("{error:#}\nprogram: {ops:#?}")))?;
    }
  }

  #[test]
  fn reordered_run_style_after_inserts_converges() {
    let ops = [
      FuzzOp::Style { peer: 0, paragraph: 0, start: 0, len: 0, style: 0 },
      FuzzOp::InsertText { peer: 0, paragraph: 0, byte: 0, text: 0, style: 0 },
      FuzzOp::Style { peer: 0, paragraph: 0, start: 0, len: 0, style: 0 },
      FuzzOp::InsertText { peer: 0, paragraph: 0, byte: 0, text: 0, style: 0 },
      FuzzOp::DeleteText { peer: 0, paragraph: 61, start: 31, len: 29 },
      FuzzOp::DeleteText { peer: 138, paragraph: 161, start: 6, len: 0 },
      FuzzOp::InsertText { peer: 0, paragraph: 125, byte: 20, text: 0, style: 0 },
      FuzzOp::Style { peer: 2, paragraph: 12, start: 0, len: 0, style: 1 },
    ];

    run_program(2, 4754897484207400829, &ops).expect("reordered run style after inserts should converge");
  }

  #[test]
  fn style_then_join_converges_when_updates_reorder() {
    let ops = [
      FuzzOp::Style {
        peer: 152,
        paragraph: 0,
        start: 87,
        len: 53,
        style: 3,
      },
      FuzzOp::Join { peer: 162, paragraph: 0 },
    ];

    run_program(2, 0, &ops).expect("style then join should converge");
  }

  #[test]
  fn insert_object_then_split_converges() {
    let ops = [
      FuzzOp::InsertObject { peer: 6, row: 70, source: 0 },
      FuzzOp::Split {
        peer: 0,
        paragraph: 188,
        byte: 0,
      },
    ];

    run_program(2, 0, &ops).expect("insert object then split should converge");
  }

  #[test]
  fn insert_object_at_start_then_split_converges_across_three_peers() {
    let ops = [
      FuzzOp::InsertObject { peer: 96, row: 129, source: 0 },
      FuzzOp::Split {
        peer: 108,
        paragraph: 91,
        byte: 0,
      },
    ];

    run_program(3, 0, &ops).expect("insert object at start then split should converge");
  }


  #[test]
  fn insert_object_between_paragraphs_does_not_create_invalid_join() {
    let ops = [
      FuzzOp::InsertObject { peer: 6, row: 70, source: 0 },
      FuzzOp::Join { peer: 162, paragraph: 0 },
    ];

    run_program(2, 0, &ops).expect("non-adjacent paragraph join should be skipped");
  }

  #[test]
  fn insert_object_between_paragraphs_then_text_edit_preserves_order() {
    let ops = [
      FuzzOp::InsertObject { peer: 7, row: 106, source: 0 },
      FuzzOp::DeleteText {
        peer: 19,
        paragraph: 48,
        start: 0,
        len: 0,
      },
    ];

    run_program(2, 0, &ops).expect("text edit after object insert should preserve block order");
  }

  #[test]
  fn concurrent_insert_after_split_point_stays_in_second_half() -> Result<()> {
    let session = SessionId::from_bytes([88; 32]);
    let initial = document_from_input_blocks(DocumentTheme::default(), vec![paragraph_block("abcd")]);
    let mut amy = Peer::from_initial(session, initial)?;
    let snapshot = amy.snapshot()?;
    let mut bob = Peer::from_snapshot(session, &snapshot)?;

    amy.apply_op(&FuzzOp::Split {
      peer: 0,
      paragraph: 0,
      byte: 2,
    })?;
    let amy_updates = amy.drain_updates();

    bob.apply_op(&FuzzOp::InsertText {
      peer: 1,
      paragraph: 0,
      byte: 3,
      text: 4,
      style: 0,
    })?;
    let bob_updates = bob.drain_updates();

    for update in &bob_updates {
      amy.import_update(update)?;
    }
    for update in &amy_updates {
      bob.import_update(update)?;
    }

    assert_eq!(paragraph_text(&amy.document, 0), "ab");
    assert_eq!(paragraph_text(&amy.document, 1), "cxd");
    assert_eq!(paragraph_text(&bob.document, 0), "ab");
    assert_eq!(paragraph_text(&bob.document, 1), "cxd");
    Ok(())
  }

  #[test]
  fn concurrent_paragraph_style_applies_to_both_split_halves() -> Result<()> {
    let session = SessionId::from_bytes([89; 32]);
    let initial = document_from_input_blocks(DocumentTheme::default(), vec![paragraph_block("abcd")]);
    let mut amy = Peer::from_initial(session, initial)?;
    let snapshot = amy.snapshot()?;
    let mut bob = Peer::from_snapshot(session, &snapshot)?;

    amy.apply_op(&FuzzOp::Split {
      peer: 0,
      paragraph: 0,
      byte: 2,
    })?;
    let amy_updates = amy.drain_updates();

    let paragraph = bob.document.ids.paragraph_ids[0];
    paragraphs_mut(&mut bob.document)[0].style = ParagraphStyle::Custom(3);
    update_paragraph_block(&mut bob.document, 0);
    LocalApplier { doc: &bob.loro, binding: &mut bob.binding }
      .apply(&bob.document, &[CanonicalOperation::SetParagraphStyle {
        paragraph,
        style: ParagraphStyle::Custom(3),
      }])?;
    let bob_updates = bob.drain_updates();

    for update in &bob_updates {
      amy.import_update(update)?;
    }
    for update in &amy_updates {
      bob.import_update(update)?;
    }

    for peer in [&amy, &bob] {
      assert_eq!(paragraph_text(&peer.document, 0), "ab");
      assert_eq!(paragraph_text(&peer.document, 1), "cd");
      assert_eq!(peer.document.paragraphs[0].style, ParagraphStyle::Custom(3));
      assert_eq!(peer.document.paragraphs[1].style, ParagraphStyle::Custom(3));
    }
    Ok(())
  }

  #[test]
  fn concurrent_object_insert_move_interleaving_converges() {
    let ops = [
      FuzzOp::InsertText { peer: 0, paragraph: 0, byte: 0, text: 0, style: 0 },
      FuzzOp::InsertText { peer: 0, paragraph: 0, byte: 0, text: 0, style: 0 },
      FuzzOp::Join { peer: 86, paragraph: 76 },
      FuzzOp::Style { peer: 144, paragraph: 122, start: 123, len: 147, style: 171 },
      FuzzOp::DeleteObject { peer: 212, row: 116 },
      FuzzOp::Split { peer: 44, paragraph: 85, byte: 123 },
      FuzzOp::InsertText { peer: 47, paragraph: 61, byte: 160, text: 91, style: 192 },
      FuzzOp::DeleteObject { peer: 216, row: 243 },
      FuzzOp::DeleteText { peer: 86, paragraph: 45, start: 114, len: 95 },
      FuzzOp::InsertObject { peer: 122, row: 21, source: 177 },
      FuzzOp::InsertText { peer: 44, paragraph: 23, byte: 38, text: 111, style: 124 },
      FuzzOp::MoveObject { peer: 1, row: 223, to: 58 },
      FuzzOp::InsertObject { peer: 167, row: 184, source: 207 },
      FuzzOp::InsertText { peer: 199, paragraph: 128, byte: 125, text: 41, style: 165 },
      FuzzOp::MoveObject { peer: 211, row: 103, to: 135 },
    ];

    run_program(2, 9640017939277306283, &ops).expect("concurrent object move interleaving should converge");
  }

  #[test]
  fn buffered_object_split_join_move_program_converges() {
    let ops = [
      FuzzOp::Style { peer: 34, paragraph: 235, start: 63, len: 151, style: 35 },
      FuzzOp::DeleteText { peer: 10, paragraph: 205, start: 201, len: 24 },
      FuzzOp::Style { peer: 244, paragraph: 167, start: 13, len: 60, style: 35 },
      FuzzOp::InsertText { peer: 159, paragraph: 175, byte: 242, text: 64, style: 7 },
      FuzzOp::Style { peer: 86, paragraph: 113, start: 237, len: 135, style: 240 },
      FuzzOp::DeleteText { peer: 186, paragraph: 150, start: 45, len: 253 },
      FuzzOp::InsertText { peer: 236, paragraph: 70, byte: 124, text: 228, style: 238 },
      FuzzOp::Style { peer: 212, paragraph: 57, start: 248, len: 168, style: 47 },
      FuzzOp::Style { peer: 128, paragraph: 69, start: 13, len: 201, style: 209 },
      FuzzOp::InsertObject { peer: 208, row: 159, source: 38 },
      FuzzOp::DeleteText { peer: 70, paragraph: 2, start: 247, len: 227 },
      FuzzOp::DeleteText { peer: 45, paragraph: 59, start: 175, len: 74 },
      FuzzOp::InsertText { peer: 234, paragraph: 209, byte: 216, text: 230, style: 207 },
      FuzzOp::DeleteText { peer: 197, paragraph: 38, start: 169, len: 78 },
      FuzzOp::InsertText { peer: 118, paragraph: 146, byte: 65, text: 27, style: 176 },
      FuzzOp::Split { peer: 32, paragraph: 124, byte: 11 },
      FuzzOp::Join { peer: 19, paragraph: 41 },
      FuzzOp::MoveObject { peer: 94, row: 196, to: 197 },
      FuzzOp::DeleteText { peer: 56, paragraph: 171, start: 218, len: 154 },
      FuzzOp::DeleteObject { peer: 222, row: 98 },
    ];

    run_program(3, 6385737403235304726, &ops).expect("minimized buffered update program should converge");
  }

  #[derive(Clone, Debug)]
  enum FuzzOp {
    InsertText { peer: u8, paragraph: u8, byte: u8, text: u8, style: u8 },
    DeleteText { peer: u8, paragraph: u8, start: u8, len: u8 },
    Split { peer: u8, paragraph: u8, byte: u8 },
    Join { peer: u8, paragraph: u8 },
    Style { peer: u8, paragraph: u8, start: u8, len: u8, style: u8 },
    InsertObject { peer: u8, row: u8, source: u8 },
    DeleteObject { peer: u8, row: u8 },
    MoveObject { peer: u8, row: u8, to: u8 },
  }

  impl FuzzOp {
    fn peer(&self) -> u8 {
      match self {
        Self::InsertText { peer, .. }
        | Self::DeleteText { peer, .. }
        | Self::Split { peer, .. }
        | Self::Join { peer, .. }
        | Self::Style { peer, .. }
        | Self::InsertObject { peer, .. }
        | Self::DeleteObject { peer, .. }
        | Self::MoveObject { peer, .. } => *peer,
      }
    }
  }

  fn fuzz_op() -> impl Strategy<Value = FuzzOp> {
    prop_oneof![
      7 => (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(peer, paragraph, byte, text, style)| FuzzOp::InsertText { peer, paragraph, byte, text, style }),
      7 => (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(peer, paragraph, start, len)| FuzzOp::DeleteText { peer, paragraph, start, len }),
      1 => (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(peer, paragraph, byte)| FuzzOp::Split { peer, paragraph, byte }),
      1 => (any::<u8>(), any::<u8>()).prop_map(|(peer, paragraph)| FuzzOp::Join { peer, paragraph }),
      2 => (any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(peer, paragraph, start, len, style)| FuzzOp::Style { peer, paragraph, start, len, style }),
      1 => (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(peer, row, source)| FuzzOp::InsertObject { peer, row, source }),
      1 => (any::<u8>(), any::<u8>()).prop_map(|(peer, row)| FuzzOp::DeleteObject { peer, row }),
      1 => (any::<u8>(), any::<u8>(), any::<u8>()).prop_map(|(peer, row, to)| FuzzOp::MoveObject { peer, row, to }),
    ]
  }

  fn run_program(peer_count: usize, delivery_seed: u64, ops: &[FuzzOp]) -> Result<()> {
    let session = SessionId::from_bytes([77; 32]);
    let initial = document_from_input_blocks(
      DocumentTheme::default(),
      vec![
        paragraph_block(MULTIBYTE),
        paragraph_block("second aé🌍"),
      ],
    );
    let mut peers = Vec::with_capacity(peer_count);
    let first = Peer::from_initial(session, initial)?;
    let snapshot = first.snapshot()?;
    peers.push(first);
    for _ in 1..peer_count {
      peers.push(Peer::from_snapshot(session, &snapshot)?);
    }

    let mut network = Vec::new();
    for (step, op) in ops.iter().enumerate() {
      let source = usize::from(op.peer()) % peer_count;
      if !peers[source].apply_op(op)? {
        continue;
      }
      for update in peers[source].drain_updates() {
        for target in 0..peer_count {
          if target == source {
            continue;
          }
          let copies = 1 + usize::from(pseudo(delivery_seed, step, source, target, 0).is_multiple_of(5));
          for copy in 0..copies {
            let buffered = pseudo(delivery_seed, step, source, target, copy).is_multiple_of(7);
            let key = pseudo(delivery_seed, step, source, target, copy + 17)
              .saturating_add(if buffered { 10_000 + step as u64 } else { 0 });
            network.push(Message { key, target, bytes: update.clone() });
          }
        }
      }
    }

    network.sort_by_key(|message| message.key);
    for message in network {
      peers[message.target].import_update(&message.bytes)?;
    }

    let projected = projection::document_from_loro(&peers[0].loro, DocumentTheme::default())?;
    let expected = canonical_document_bytes(&projected)?;
    for (peer_ix, peer) in peers.iter().enumerate() {
      let peer_projection = projection::document_from_loro(&peer.loro, DocumentTheme::default())?;
      let projection_bytes = canonical_document_bytes(&peer_projection)?;
      let document_bytes = canonical_document_bytes(&peer.document)?;
      if projection_bytes != expected {
        bail!("peer Loro projection diverged");
      }
      if document_bytes != expected {
        bail!(
          "peer {peer_ix} document diverged from Loro projection\nexpected: {}\nactual: {}\npeer projection: {}",
          describe_document(&projected),
          describe_document(&peer.document),
          describe_document(&peer_projection),
        );
      }
    }
    Ok(())
  }

  struct Message {
    key: u64,
    target: usize,
    bytes: Vec<u8>,
  }

  struct Peer {
    document: Document,
    loro: LoroDoc,
    binding: DocBinding,
    updates: Arc<Mutex<Vec<Vec<u8>>>>,
    _subscription: Subscription,
  }

  impl Peer {
    fn from_initial(session: SessionId, document: Document) -> Result<Self> {
      let loro = schema::new_configured_doc();
      projection::populate_from_document(&loro, session, "convergence", &document)?;
      let binding = DocBinding::build(&loro, &document)?;
      Ok(Self::with_subscription(document, loro, binding))
    }

    fn from_snapshot(session: SessionId, snapshot: &[u8]) -> Result<Self> {
      let loro = schema::new_configured_doc();
      loro.import(snapshot)?;
      projection::verify_lineage(&loro, session)?;
      let document = projection::document_from_loro(&loro, DocumentTheme::default())?;
      let binding = DocBinding::build(&loro, &document)?;
      Ok(Self::with_subscription(document, loro, binding))
    }

    fn with_subscription(document: Document, loro: LoroDoc, binding: DocBinding) -> Self {
      let updates = Arc::new(Mutex::new(Vec::new()));
      let captured = updates.clone();
      let subscription = loro.subscribe_local_update(Box::new(move |bytes| {
        captured.lock().expect("update capture lock should not be poisoned").push(bytes.clone());
        true
      }));
      Self { document, loro, binding, updates, _subscription: subscription }
    }

    fn snapshot(&self) -> Result<Vec<u8>> {
      Ok(self.loro.export(ExportMode::Snapshot)?)
    }

    fn drain_updates(&self) -> Vec<Vec<u8>> {
      let mut updates = self.updates.lock().expect("update capture lock should not be poisoned");
      std::mem::take(&mut *updates)
    }

    fn apply_op(&mut self, op: &FuzzOp) -> Result<bool> {
      let Some(ops) = self.mutate_document(op)? else {
        return Ok(false);
      };
      LocalApplier { doc: &self.loro, binding: &mut self.binding }
        .apply(&self.document, &ops)?;
      Ok(true)
    }

    fn mutate_document(&mut self, op: &FuzzOp) -> Result<Option<Vec<CanonicalOperation>>> {
      match *op {
        FuzzOp::InsertText { paragraph, byte, text, style, .. } => {
          if self.document.paragraphs.is_empty() {
            return Ok(None);
          }
          let paragraph_ix = usize::from(paragraph) % self.document.paragraphs.len();
          let value = text_choice(text);
          let styles = style_choice(style);
          let offset = byte_boundary(&paragraph_text(&self.document, paragraph_ix), byte);
          let paragraph_id = self.document.ids.paragraph_ids[paragraph_ix];
          insert_text_preserving_blocks(&mut self.document, paragraph_ix, offset, value, styles);
          Ok(Some(vec![CanonicalOperation::InsertText {
            paragraph: paragraph_id,
            byte: offset,
            text: value.to_string(),
            styles,
          }]))
        },
        FuzzOp::DeleteText { paragraph, start, len, .. } => {
          if self.document.paragraphs.is_empty() {
            return Ok(None);
          }
          let paragraph_ix = usize::from(paragraph) % self.document.paragraphs.len();
          let text = paragraph_text(&self.document, paragraph_ix);
          let Some(range) = non_empty_range(&text, start, len) else {
            return Ok(None);
          };
          let paragraph_id = self.document.ids.paragraph_ids[paragraph_ix];
          delete_text_preserving_blocks(&mut self.document, paragraph_ix, range.clone());
          Ok(Some(vec![CanonicalOperation::DeleteRange {
            start_paragraph: paragraph_id,
            start_byte: range.start,
            end_paragraph: paragraph_id,
            end_byte: range.end,
          }]))
        },
        FuzzOp::Split { paragraph, byte, .. } => {
          if self.document.paragraphs.len() >= 6 || self.document.paragraphs.is_empty() {
            return Ok(None);
          }
          let paragraph_ix = usize::from(paragraph) % self.document.paragraphs.len();
          let split_byte = byte_boundary(&paragraph_text(&self.document, paragraph_ix), byte);
          let paragraph_id = self.document.ids.paragraph_ids[paragraph_ix];
          let Some(new_paragraph) = split_paragraph_preserving_blocks(&mut self.document, paragraph_ix, split_byte) else {
            return Ok(None);
          };
          Ok(Some(vec![CanonicalOperation::SplitParagraph {
            paragraph: paragraph_id,
            byte: split_byte,
            new_paragraph,
          }]))
        },
        FuzzOp::Join { paragraph, .. } => {
          let pairs = adjacent_paragraph_pairs(&self.document);
          if pairs.is_empty() {
            return Ok(None);
          }
          let first_ix = pairs[usize::from(paragraph) % pairs.len()];
          let first = self.document.ids.paragraph_ids[first_ix];
          let second = self.document.ids.paragraph_ids[first_ix + 1];
          let first_len = paragraph_text(&self.document, first_ix).len();
          delete_cross_paragraph_range(
            &mut self.document,
            DocumentOffset { paragraph: first_ix, byte: first_len }..DocumentOffset { paragraph: first_ix + 1, byte: 0 },
          );
          Ok(Some(vec![CanonicalOperation::JoinParagraphs { first, second }]))
        },
        FuzzOp::Style { paragraph, start, len, style, .. } => {
          if self.document.paragraphs.is_empty() {
            return Ok(None);
          }
          let paragraph_ix = usize::from(paragraph) % self.document.paragraphs.len();
          let text = paragraph_text(&self.document, paragraph_ix);
          let Some(range) = non_empty_range(&text, start, len) else {
            return Ok(None);
          };
          let styles = style_choice(style);
          let paragraph_id = self.document.ids.paragraph_ids[paragraph_ix];
          mutate_runs_in_range(
            &mut self.document,
            DocumentOffset { paragraph: paragraph_ix, byte: range.start }..DocumentOffset { paragraph: paragraph_ix, byte: range.end },
            |run_styles| *run_styles = styles,
          );
          Ok(Some(vec![CanonicalOperation::SetRunStyles { paragraph: paragraph_id, range, styles }]))
        },
        FuzzOp::InsertObject { row, source, .. } => {
          if self.document.blocks.len() >= 8 {
            return Ok(None);
          }
          let row = usize::from(row) % (self.document.blocks.len() + 1);
          let input = equation_block(equation_source(source));
          Arc::make_mut(&mut self.document.blocks).insert(row, block_from_input_block(&input));
          let block = insert_block_id(&mut self.document, row);
          Ok(Some(vec![CanonicalOperation::InsertBlock { block, block_ix: row }]))
        },
        FuzzOp::DeleteObject { row, .. } => {
          let objects = object_rows(&self.document);
          if objects.is_empty() {
            return Ok(None);
          }
          let row = objects[usize::from(row) % objects.len()];
          let block = self.document.ids.block_ids[row];
          Arc::make_mut(&mut self.document.blocks).remove(row);
          remove_block_ids(&mut self.document, row..row + 1);
          Ok(Some(vec![CanonicalOperation::DeleteBlock { block }]))
        },
        FuzzOp::MoveObject { row, to, .. } => {
          let objects = object_rows(&self.document);
          if objects.is_empty() || self.document.blocks.len() < 2 {
            return Ok(None);
          }
          let from = objects[usize::from(row) % objects.len()];
          let to = usize::from(to) % self.document.blocks.len();
          if from == to {
            return Ok(None);
          }
          let block = self.document.ids.block_ids[from];
          let moved_block = Arc::make_mut(&mut self.document.blocks).remove(from);
          let insert_ix = to.min(self.document.blocks.len());
          Arc::make_mut(&mut self.document.blocks).insert(insert_ix, moved_block);
          let block_id = self.document.ids.block_ids.remove(from);
          self.document.ids.block_ids.insert(insert_ix.min(self.document.ids.block_ids.len()), block_id);
          Ok(Some(vec![CanonicalOperation::MoveBlock { block, new_block_ix: to }]))
        },
      }
    }

    fn import_update(&mut self, update: &[u8]) -> Result<()> {
      let document = Arc::new(self.document.clone());
      let binding = Arc::new(Mutex::new(std::mem::take(&mut self.binding)));
      let patches = Arc::new(Mutex::new(Vec::new()));
      let error = Arc::new(Mutex::new(None::<String>));
      let doc = self.loro.clone();
      let callback: Subscriber = Arc::new({
        let binding = binding.clone();
        let patches = patches.clone();
        let error = error.clone();
        move |event| {
          let result = binding
            .lock()
            .map_err(|lock_error| anyhow!("binding lock poisoned: {lock_error}"))
            .and_then(|mut binding| {
              flowstate_collab::remote_apply::RemoteApplier { doc: &doc, binding: &mut binding }
                .apply_event(&document, &event)
            });
          match result {
            Ok(mut produced) => patches.lock().expect("patch lock should not be poisoned").append(&mut produced),
            Err(apply_error) => *error.lock().expect("error lock should not be poisoned") = Some(format!("{apply_error:#}")),
          }
        }
      });
      let subscription = self.loro.subscribe_root(callback);
      let import_result = self.loro.import_with(update, "remote");
      drop(subscription);
      self.binding = Arc::try_unwrap(binding)
        .map_err(|_| anyhow!("binding callback outlived import"))?
        .into_inner()
        .map_err(|error| anyhow!("binding lock poisoned: {error}"))?;
      import_result.context("importing remote update failed")?;
      let remote_error = error.lock().expect("error lock should not be poisoned").take();
      if let Some(error) = remote_error {
        bail!("remote apply failed: {error}");
      }
      let patches = Arc::try_unwrap(patches)
        .map_err(|_| anyhow!("patch callback outlived import"))?
        .into_inner()
        .map_err(|error| anyhow!("patch lock poisoned: {error}"))?;
      apply_patches(&mut self.document, &mut self.binding, &self.loro, &patches)?;
      Ok(())
    }
  }

  fn paragraph_block(text: &str) -> InputBlock {
    InputBlock::Paragraph(InputParagraph { style: ParagraphStyle::Normal, runs: vec![plain(text)] })
  }

  fn equation_block(source: &str) -> InputBlock {
    InputBlock::Equation(InputEquationBlock {
      source: source.to_string(),
      syntax: InputEquationSyntax::Latex,
      display: InputEquationDisplay::Display,
    })
  }

  fn object_rows(document: &Document) -> Vec<usize> {
    document
      .blocks
      .iter()
      .enumerate()
      .filter_map(|(ix, block)| (!matches!(block, Block::Paragraph(_))).then_some(ix))
      .collect()
  }

  fn adjacent_paragraph_pairs(document: &Document) -> Vec<usize> {
    let mut pairs = Vec::new();
    let mut paragraph_ix = 0;
    let mut previous_paragraph_ix = None;
    for block in document.blocks.iter() {
      if matches!(block, Block::Paragraph(_)) {
        if let Some(previous) = previous_paragraph_ix {
          pairs.push(previous);
        }
        previous_paragraph_ix = Some(paragraph_ix);
        paragraph_ix += 1;
      } else {
        previous_paragraph_ix = None;
      }
    }
    pairs
  }

  fn insert_text_preserving_blocks(document: &mut Document, paragraph_ix: usize, byte: usize, text: &str, styles: RunStyles) {
    let Some(snapshot) = block_order_snapshot(document, paragraph_ix) else {
      return;
    };
    insert_text_at(document, paragraph_ix, byte, text, styles);
    restore_block_order(document, paragraph_ix, snapshot);
  }

  fn delete_text_preserving_blocks(document: &mut Document, paragraph_ix: usize, range: Range<usize>) {
    let Some(snapshot) = block_order_snapshot(document, paragraph_ix) else {
      return;
    };
    delete_range_in_paragraph(document, paragraph_ix, range);
    restore_block_order(document, paragraph_ix, snapshot);
  }

  fn split_paragraph_preserving_blocks(document: &mut Document, paragraph_ix: usize, byte: usize) -> Option<gpui_flowtext::ParagraphId> {
    let block_ix = block_ix_for_paragraph_ix(document, paragraph_ix)?;
    let old_blocks = document.blocks.as_ref().clone();
    let old_block_ids = document.ids.block_ids.clone();

    split_paragraph_at(document, paragraph_ix, byte);

    let new_paragraph_id = *document.ids.paragraph_ids.get(paragraph_ix + 1)?;
    let new_block_id = document
      .ids
      .block_ids
      .iter()
      .copied()
      .find(|id| !old_block_ids.contains(id))?;
    let left = document.paragraphs.get(paragraph_ix).cloned()?;
    let right = document.paragraphs.get(paragraph_ix + 1).cloned()?;

    let mut blocks = old_blocks;
    *blocks.get_mut(block_ix)? = Block::Paragraph(left);
    blocks.insert(block_ix + 1, Block::Paragraph(right));
    document.blocks = Arc::new(blocks);

    let mut block_ids = old_block_ids;
    block_ids.insert(block_ix + 1, new_block_id);
    document.ids.block_ids = block_ids;
    gpui_flowtext::rebuild_document_sections(document);

    Some(new_paragraph_id)
  }

  fn block_order_snapshot(document: &Document, paragraph_ix: usize) -> Option<(usize, Vec<Block>, Vec<gpui_flowtext::BlockId>)> {
    Some((block_ix_for_paragraph_ix(document, paragraph_ix)?, document.blocks.as_ref().clone(), document.ids.block_ids.clone()))
  }

  fn restore_block_order(document: &mut Document, paragraph_ix: usize, (block_ix, mut blocks, block_ids): (usize, Vec<Block>, Vec<gpui_flowtext::BlockId>)) {
    if let Some(paragraph) = document.paragraphs.get(paragraph_ix).cloned()
      && let Some(block) = blocks.get_mut(block_ix)
    {
      *block = Block::Paragraph(paragraph);
    }
    document.blocks = Arc::new(blocks);
    document.ids.block_ids = block_ids;
    gpui_flowtext::rebuild_document_sections(document);
  }

  fn block_ix_for_paragraph_ix(document: &Document, target_paragraph_ix: usize) -> Option<usize> {
    let mut paragraph_ix = 0;
    for (block_ix, block) in document.blocks.iter().enumerate() {
      if matches!(block, Block::Paragraph(_)) {
        if paragraph_ix == target_paragraph_ix {
          return Some(block_ix);
        }
        paragraph_ix += 1;
      }
    }
    None
  }

  fn text_choice(ix: u8) -> &'static str {
    match ix % 6 {
      0 => "a",
      1 => "é",
      2 => "🌍",
      3 => "\u{2028}",
      4 => "x",
      _ => MULTIBYTE,
    }
  }

  fn equation_source(ix: u8) -> &'static str {
    match ix % 4 {
      0 => "x = aé",
      1 => "y = 🌍^2",
      2 => "z = a + \u{2028}x",
      _ => MULTIBYTE,
    }
  }

  fn style_choice(ix: u8) -> RunStyles {
    match ix % 5 {
      0 => RunStyles::default(),
      1 => RunStyles::default().with(RunStyle::Semantic(1)),
      2 => RunStyles::default().with(RunStyle::Highlight(2)),
      3 => RunStyles::default().with_direct_underline(),
      _ => RunStyles::default().with(RunStyle::Semantic(3)).with_strikethrough(),
    }
  }

  fn byte_boundary(text: &str, pick: u8) -> usize {
    let boundaries = byte_boundaries(text);
    boundaries[usize::from(pick) % boundaries.len()]
  }

  fn non_empty_range(text: &str, start_pick: u8, len_pick: u8) -> Option<Range<usize>> {
    let boundaries = byte_boundaries(text);
    if boundaries.len() < 2 {
      return None;
    }
    let start_ix = usize::from(start_pick) % (boundaries.len() - 1);
    let remaining = boundaries.len() - start_ix - 1;
    let end_ix = start_ix + 1 + usize::from(len_pick) % remaining;
    Some(boundaries[start_ix]..boundaries[end_ix])
  }

  fn byte_boundaries(text: &str) -> Vec<usize> {
    text
      .char_indices()
      .map(|(byte, _)| byte)
      .chain(std::iter::once(text.len()))
      .collect()
  }

  fn pseudo(seed: u64, step: usize, source: usize, target: usize, copy: usize) -> u64 {
    let mut value = seed
      ^ ((step as u64).wrapping_mul(0x9E37_79B9_7F4A_7C15))
      ^ ((source as u64) << 17)
      ^ ((target as u64) << 33)
      ^ ((copy as u64) << 49);
    value ^= value >> 30;
    value = value.wrapping_mul(0xBF58_476D_1CE4_E5B9);
    value ^= value >> 27;
    value.wrapping_mul(0x94D0_49BB_1331_11EB) ^ (value >> 31)
  }

  fn canonical_document_bytes(document: &Document) -> Result<Vec<u8>> {
    let mut bytes = Vec::new();
    let mut paragraph_ix = 0;
    for block in document.blocks.iter() {
      match block {
        Block::Paragraph(paragraph) => {
          bytes.push(b'p');
          push_paragraph_style(&mut bytes, paragraph.style);
          for run in &paragraph.runs {
            push_usize(&mut bytes, run.len);
            push_run_styles(&mut bytes, run.styles);
          }
          let text = paragraph_text(document, paragraph_ix);
          push_usize(&mut bytes, text.len());
          bytes.extend_from_slice(text.as_bytes());
          paragraph_ix += 1;
        },
        Block::Image(_) | Block::Equation(_) | Block::Table(_) => {
          bytes.push(match block {
            Block::Image(_) => b'i',
            Block::Equation(_) => b'e',
            Block::Table(_) => b't',
            Block::Paragraph(_) => unreachable!(),
          });
          let payload = schema::payload_from_block(block, &document.assets).context("object block should have a payload")?;
          let payload = postcard::to_stdvec(&payload)?;
          push_usize(&mut bytes, payload.len());
          bytes.extend_from_slice(&payload);
        },
      }
    }
    Ok(bytes)
  }

  fn describe_document(document: &Document) -> String {
    let mut output = String::new();
    let mut paragraph_ix = 0;
    for (block_ix, block) in document.blocks.iter().enumerate() {
      match block {
        Block::Paragraph(paragraph) => {
          let text = paragraph_text(document, paragraph_ix);
          let _ = write!(output, "#{block_ix}:p:{text:?}:");
          for run in &paragraph.runs {
            let _ = write!(output, "{}@{:?};", run.len, run.styles);
          }
          paragraph_ix += 1;
        },
        Block::Image(_) => {
          let _ = write!(output, "#{block_ix}:image;");
        },
        Block::Equation(_) => {
          let _ = write!(output, "#{block_ix}:equation;");
        },
        Block::Table(_) => {
          let _ = write!(output, "#{block_ix}:table;");
        },
      }
    }
    output
  }

  fn push_paragraph_style(bytes: &mut Vec<u8>, style: ParagraphStyle) {
    match style {
      ParagraphStyle::Normal => bytes.extend_from_slice(&[0, 0]),
      ParagraphStyle::Custom(slot) => bytes.extend_from_slice(&[1, slot]),
    }
  }

  fn push_run_styles(bytes: &mut Vec<u8>, styles: RunStyles) {
    match styles.semantic {
      RunSemanticStyle::Plain => bytes.extend_from_slice(&[0, 0]),
      RunSemanticStyle::Custom(slot) => bytes.extend_from_slice(&[1, slot]),
    }
    bytes.push(u8::from(styles.direct_underline));
    bytes.push(u8::from(styles.strikethrough));
    match styles.highlight {
      Some(HighlightStyle::Custom(slot)) => bytes.extend_from_slice(&[1, slot]),
      None => bytes.extend_from_slice(&[0, 0]),
    }
  }

  fn push_usize(bytes: &mut Vec<u8>, value: usize) {
    bytes.extend_from_slice(&(value as u64).to_le_bytes());
  }
}
