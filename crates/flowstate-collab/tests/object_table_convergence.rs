//! N-peer structural convergence over the full object + table intent surface
//! (Loro-first spec §13.7), rebuilt from the raw-command-era multi-peer and
//! table convergence suites.
//!
//! Coverage carried over from the deleted `multi_peer_convergence_tests.rs` /
//! `table_convergence_tests.rs` op inventory, now driven exclusively through
//! `LocalDocHandle` intents: text insert (incl. intra-paragraph U+2028 soft
//! breaks) / delete / split / join / run marks / paragraph style, object
//! insert / delete / move / replace, image alt-text / caption / layout,
//! equation source edits, rich-fragment paste, and all 9 table ops
//! (row insert/delete/move, column insert/delete/move, cell replace, cell
//! span, column width) with durable §P2b identities.
//!
//! Convergence is the PROPERTY: intents race across peers (rejections from
//! stale identities are legal outcomes, I-15); after every full-mesh sync round
//! all peers must agree on body text, projection paragraphs/runs/styles,
//! block-id sequences, block kinds, and table topology — and at the end each
//! peer's incrementally maintained projection must equal a fresh
//! `document_from_loro` rebuild of its own doc (materializer equivalence).

#[cfg(test)]
mod tests {
  use std::sync::Arc;

  use flowstate_collab::crdt_runtime::CrdtRuntime;
  use flowstate_collab::local_write::{
    DeleteBlocksIntent, DeleteRangeIntent, FragmentBlock, GateHolder, InsertObjectIntent, InsertRichFragmentIntent, InsertTextIntent,
    JoinParagraphsIntent, LocalDocHandle, LocalWriteConfig, MoveBlockIntent, ReplaceEquationSourceRangeIntent, ReplaceImageAltTextIntent,
    ReplaceImageCaptionIntent, ReplaceObjectIntent, SetImageLayoutIntent, SetMarksIntent, SetParagraphStyleIntent, SplitParagraphIntent,
    TableIntent, TextAnchor, WriteGate, WriteRejected,
  };
  use flowstate_document::{
    AssetId, Block, BlockId, CellId, ColumnId, DocumentProjection, InputBlock, InputBlockAlignment, InputEquationBlock,
    InputEquationDisplay, InputEquationSyntax, InputImageBlock, InputImageSizing, InputParagraph, InputRun, InputTableBlock,
    InputTableCell, InputTableCellBlock, InputTableColumn, InputTableColumnWidth, InputTableRow, InputTableStyle, ParagraphStyle, RowId,
    RunSemanticStyle, RunStyles, TableBlock, TableCell, TableCellBlock, document_from_loro, paragraph_text, paragraph_text_len,
  };
  use uuid::Uuid;

  // ---------------------------------------------------------------------------
  // Harness (the intent_fuzz.rs Peer/sync_all pattern)
  // ---------------------------------------------------------------------------

  /// Deterministic xorshift PRNG — reproducible fuzz, no wall-clock dependence.
  /// (Editor-minted durable ids inside payloads are uuid-minted, matching the
  /// production write path; determinism here is about the op SEQUENCE.)
  struct Rng(u64);

  impl Rng {
    fn new(seed: u64) -> Self {
      Self(seed.max(1))
    }

    fn next(&mut self) -> u64 {
      let mut x = self.0;
      x ^= x << 13;
      x ^= x >> 7;
      x ^= x << 17;
      self.0 = x;
      x
    }

    fn below(&mut self, bound: usize) -> usize {
      if bound == 0 { 0 } else { (self.next() % bound as u64) as usize }
    }
  }

  struct Peer {
    handle: LocalDocHandle,
    gate: Arc<WriteGate<CrdtRuntime>>,
    synced_vv: Vec<loro::VersionVector>,
  }

  impl Peer {
    fn new(title: &str, peer_count: usize) -> Self {
      let core = CrdtRuntime::new_empty(title).expect("runtime");
      let (handle, gate) = LocalDocHandle::new(core, LocalWriteConfig::default());
      Self {
        handle,
        gate,
        synced_vv: vec![loro::VersionVector::default(); peer_count],
      }
    }

    fn projection(&self) -> DocumentProjection {
      self.handle.projection().expect("projection")
    }

    fn export_updates_since(&self, vv: &loro::VersionVector) -> Vec<u8> {
      let guard = self.gate.lock(GateHolder::ExportUpdates).expect("gate");
      guard.doc().export(loro::ExportMode::updates(vv)).expect("export")
    }

    fn state_vv(&self) -> loro::VersionVector {
      let guard = self.gate.lock(GateHolder::ExportUpdates).expect("gate");
      guard.doc().state_vv()
    }

    fn import(&self, bytes: &[u8]) {
      if bytes.is_empty() {
        return;
      }
      let mut guard = self.gate.lock(GateHolder::ImportChunk).expect("gate");
      guard.import_remote_update(bytes).expect("import");
    }

    fn body_text(&self) -> String {
      let guard = self.gate.lock(GateHolder::ExportUpdates).expect("gate");
      flowstate_document::loro_schema::body_text(guard.doc()).to_string()
    }

    /// Fresh full rebuild of this peer's canonical Loro state (the materializer
    /// equivalence reference).
    fn fresh_projection(&self) -> DocumentProjection {
      let guard = self.gate.lock(GateHolder::ExportUpdates).expect("gate");
      document_from_loro(guard.doc()).expect("document_from_loro")
    }
  }

  /// Full-mesh sync, iterated to quiescence (mirror of intent_fuzz):
  /// importing an undo can COMMIT convergent repairs (records whose cursor
  /// anchors died with the deleted content), and those repair updates need
  /// delivery too. A bounded pass count keeps a non-converging repair loop
  /// from masquerading as convergence.
  fn sync_all(peers: &mut [Peer]) {
    for _pass in 0..8 {
      let before: Vec<_> = peers.iter().map(Peer::state_vv).collect();
      for from in 0..peers.len() {
        let from_vv_now = peers[from].state_vv();
        for to in 0..peers.len() {
          if from == to {
            continue;
          }
          let since = peers[to].synced_vv[from].clone();
          let bytes = peers[from].export_updates_since(&since);
          peers[to].import(&bytes);
          peers[to].synced_vv[from] = from_vv_now.clone();
          if std::env::var("FUZZ_PER_OP_CHECK").is_ok()
            && let Err(reason) = projections_agree(&peers[to].projection(), &peers[to].fresh_projection())
          {
            panic!("sync pass {_pass}: peer {to} deviates from own canonical after importing from {from}: {reason}");
          }
        }
      }
      if peers.iter().map(Peer::state_vv).collect::<Vec<_>>() == before {
        return;
      }
    }
    panic!("sync_all did not quiesce within 8 passes (non-converging repair loop?)");
  }

  // ---------------------------------------------------------------------------
  // Structural projection comparison (extends intent_fuzz's projections_agree
  // with block kinds + table row/column id sequences + cell texts)
  // ---------------------------------------------------------------------------

  fn block_kind(block: &Block) -> &'static str {
    match block {
      Block::Paragraph(_) => "paragraph",
      Block::Image(_) => "image",
      Block::Equation(_) => "equation",
      Block::Table(_) => "table",
    }
  }

  fn cell_text(cell: &TableCell) -> String {
    cell
      .blocks
      .iter()
      .filter_map(|block| match block {
        TableCellBlock::Paragraph(paragraph) => Some(paragraph.text.as_str()),
        TableCellBlock::Table(_) => None,
      })
      .collect()
  }

  fn tables_agree(left: &TableBlock, right: &TableBlock) -> Result<(), String> {
    let left_rows: Vec<RowId> = left.rows.iter().map(|row| row.id).collect();
    let right_rows: Vec<RowId> = right.rows.iter().map(|row| row.id).collect();
    if left_rows != right_rows {
      return Err(format!("table row id sequences differ: {left_rows:?} != {right_rows:?}"));
    }
    let left_columns: Vec<ColumnId> = left.columns.iter().map(|column| column.id).collect();
    let right_columns: Vec<ColumnId> = right.columns.iter().map(|column| column.id).collect();
    if left_columns != right_columns {
      return Err(format!("table column id sequences differ: {left_columns:?} != {right_columns:?}"));
    }
    for (row_ix, (left_row, right_row)) in left.rows.iter().zip(&right.rows).enumerate() {
      if left_row.cells.len() != right_row.cells.len() {
        return Err(format!("table row {row_ix} cell count {} != {}", left_row.cells.len(), right_row.cells.len()));
      }
      for (col_ix, (left_cell, right_cell)) in left_row.cells.iter().zip(&right_row.cells).enumerate() {
        if (left_cell.row_id, left_cell.column_id) != (right_cell.row_id, right_cell.column_id) {
          return Err(format!("table cell ({row_ix},{col_ix}) coordinates differ"));
        }
        if (left_cell.row_span, left_cell.col_span) != (right_cell.row_span, right_cell.col_span) {
          return Err(format!("table cell ({row_ix},{col_ix}) spans differ"));
        }
        if cell_text(left_cell) != cell_text(right_cell) {
          return Err(format!(
            "table cell ({row_ix},{col_ix}) text {:?} != {:?}",
            cell_text(left_cell),
            cell_text(right_cell)
          ));
        }
      }
    }
    Ok(())
  }

  fn blocks_agree(left: &DocumentProjection, right: &DocumentProjection) -> Result<(), String> {
    if left.blocks.len() != right.blocks.len() {
      return Err(format!("block count {} != {}", left.blocks.len(), right.blocks.len()));
    }
    for ix in 0..left.blocks.len() {
      let (left_block, right_block) = (&left.blocks[ix], &right.blocks[ix]);
      if block_kind(left_block) != block_kind(right_block) {
        return Err(format!("block[{ix}] kind {} != {}", block_kind(left_block), block_kind(right_block)));
      }
      match (left_block, right_block) {
        (Block::Image(left_image), Block::Image(right_image)) => {
          if left_image.alt_text != right_image.alt_text
            || left_image.sizing != right_image.sizing
            || left_image.alignment != right_image.alignment
            || left_image.caption.is_some() != right_image.caption.is_some()
          {
            return Err(format!("block[{ix}] image metadata differs"));
          }
        },
        (Block::Equation(left_equation), Block::Equation(right_equation)) => {
          if left_equation.source != right_equation.source {
            return Err(format!(
              "block[{ix}] equation source {:?} != {:?}",
              left_equation.source, right_equation.source
            ));
          }
        },
        (Block::Table(left_table), Block::Table(right_table)) => {
          tables_agree(left_table, right_table).map_err(|reason| format!("block[{ix}]: {reason}"))?;
        },
        _ => {},
      }
    }
    Ok(())
  }

  fn projections_agree(left: &DocumentProjection, right: &DocumentProjection) -> Result<(), String> {
    if left.paragraphs.len() != right.paragraphs.len() {
      return Err(format!("paragraph count {} != {}", left.paragraphs.len(), right.paragraphs.len()));
    }
    for ix in 0..left.paragraphs.len() {
      if paragraph_text(left, ix) != paragraph_text(right, ix) {
        return Err(format!("paragraph[{ix}] text {:?} != {:?}", paragraph_text(left, ix), paragraph_text(right, ix)));
      }
      if left.paragraphs[ix].style != right.paragraphs[ix].style {
        return Err(format!(
          "paragraph[{ix}] style differs: left {:?} right {:?} (kinds: {:?})",
          left.paragraphs[ix].style,
          right.paragraphs[ix].style,
          left.blocks.iter().map(block_kind).collect::<Vec<_>>()
        ));
      }
      if left.paragraphs[ix].runs != right.paragraphs[ix].runs {
        return Err(format!("paragraph[{ix}] runs differ: {:?} != {:?}", left.paragraphs[ix].runs, right.paragraphs[ix].runs));
      }
    }
    if left.ids.paragraph_ids != right.ids.paragraph_ids {
      return Err(format!(
        "paragraph ids differ:\n  left  {:?}\n  right {:?}",
        left.ids.paragraph_ids, right.ids.paragraph_ids
      ));
    }
    if left.ids.block_ids != right.ids.block_ids {
      return Err(format!(
        "block ids differ:\n  left  {:?}\n  right {:?}",
        left.ids.block_ids, right.ids.block_ids
      ));
    }
    blocks_agree(left, right)
  }

  fn assert_converged(peers: &[Peer], context: &str) {
    // Self-consistency first (classifier, mirror of intent_fuzz): a peer whose
    // maintained projection deviates from a fresh rematerialization of its OWN
    // canonical state is a derivation bug; self-consistent peers that disagree
    // with each other are CRDT-level divergence.
    for (ix, peer) in peers.iter().enumerate() {
      if let Err(reason) = projections_agree(&peer.projection(), &peer.fresh_projection()) {
        panic!("{context}: peer {ix} projection deviates from own canonical: {reason}");
      }
    }
    let reference_text = peers[0].body_text();
    let reference = peers[0].projection();
    for (ix, peer) in peers.iter().enumerate().skip(1) {
      assert_eq!(peer.body_text(), reference_text, "{context}: peer {ix} body text diverged");
      if let Err(reason) = projections_agree(&peer.projection(), &reference) {
        panic!("{context}: peer {ix} projection diverged: {reason}");
      }
    }
  }

  /// Materializer equivalence: each peer's incrementally maintained projection
  /// equals a fresh full `document_from_loro` rebuild of its own doc.
  fn assert_materializer_equivalence(peers: &[Peer], context: &str) {
    for (ix, peer) in peers.iter().enumerate() {
      if let Err(reason) = projections_agree(&peer.projection(), &peer.fresh_projection()) {
        panic!("{context}: peer {ix} incremental projection != fresh document_from_loro rebuild: {reason}");
      }
    }
  }

  // ---------------------------------------------------------------------------
  // Payload builders (the old suites' fixture builders, editor-minted ids via
  // uuid exactly like the production write path)
  // ---------------------------------------------------------------------------

  fn input_paragraph(text: &str) -> InputParagraph {
    InputParagraph {
      style: ParagraphStyle::Normal,
      runs: if text.is_empty() {
        Vec::new()
      } else {
        vec![InputRun {
          text: text.to_string(),
          styles: RunStyles::default(),
        }]
      },
    }
  }

  fn image_input(rng: &mut Rng) -> InputBlock {
    InputBlock::Image(InputImageBlock {
      asset_id: AssetId(1),
      alt_text: format!("img{}", rng.below(100)),
      caption: None,
      sizing: InputImageSizing::Intrinsic,
      alignment: InputBlockAlignment::Left,
    })
  }

  fn equation_input(rng: &mut Rng) -> InputBlock {
    InputBlock::Equation(InputEquationBlock {
      source: format!("x^{}", rng.below(10)),
      syntax: InputEquationSyntax::Latex,
      display: InputEquationDisplay::Display,
    })
  }

  fn object_input(rng: &mut Rng) -> InputBlock {
    if rng.below(2) == 0 { image_input(rng) } else { equation_input(rng) }
  }

  fn table_cell_input(row_id: RowId, column_id: ColumnId, text: &str) -> InputTableCell {
    InputTableCell {
      // §P2b law: a cell's identity IS its coordinate (deterministic mix) —
      // never a random id, or writers keyed by coordinate and readers keyed by
      // record diverge into duplicate cells.
      id: CellId::from_coordinate(row_id, column_id),
      row_id,
      column_id,
      blocks: vec![InputTableCellBlock::Paragraph(input_paragraph(text))],
      row_span: 1,
      col_span: 1,
    }
  }

  fn fresh_row_input(columns: &[ColumnId], text: &str) -> InputTableRow {
    let row_id = RowId(Uuid::new_v4().as_u128());
    InputTableRow {
      id: row_id,
      cells: columns.iter().map(|column| table_cell_input(row_id, *column, text)).collect(),
    }
  }

  /// A `rows`x`columns` table input with uuid-minted durable ids and per-cell
  /// coordinate text (`r{r}c{c}`), the old `table_fixture` in miniature.
  fn table_input(rows: usize, columns: usize) -> InputBlock {
    let column_ids: Vec<ColumnId> = (0..columns).map(|_| ColumnId(Uuid::new_v4().as_u128())).collect();
    let input_rows: Vec<InputTableRow> = (0..rows)
      .map(|r| {
        let row_id = RowId(Uuid::new_v4().as_u128());
        InputTableRow {
          id: row_id,
          cells: column_ids
            .iter()
            .enumerate()
            .map(|(c, column)| table_cell_input(row_id, *column, &format!("r{r}c{c}")))
            .collect(),
        }
      })
      .collect();
    InputBlock::Table(InputTableBlock {
      rows: input_rows,
      columns: column_ids
        .into_iter()
        .map(|id| InputTableColumn {
          id,
          width: InputTableColumnWidth::Auto,
        })
        .collect(),
      style: InputTableStyle { header_row: true },
    })
  }

  fn first_table(projection: &DocumentProjection) -> Option<(BlockId, TableBlock)> {
    projection.blocks.iter().enumerate().find_map(|(ix, block)| match block {
      Block::Table(table) => Some((projection.ids.block_ids[ix], table.clone())),
      _ => None,
    })
  }

  fn object_block_indices(projection: &DocumentProjection) -> Vec<usize> {
    (0..projection.blocks.len())
      .filter(|ix| !matches!(projection.blocks[*ix], Block::Paragraph(_)))
      .collect()
  }

  // ---------------------------------------------------------------------------
  // Seeding (through the intent API — peer 0 builds the fixture, mesh syncs)
  // ---------------------------------------------------------------------------

  /// The old `structural_fixture` in miniature: paragraphs with editable words,
  /// objects flanked by empty paragraphs (projection coalescing vs live-body
  /// coordinates), and an intra-paragraph U+2028 soft break. Built as ONE rich
  /// fragment (which also exercises the compound paste intent).
  fn seed_structural(peer: &Peer, rng: &mut Rng) {
    let projection = peer.projection();
    let paragraph = projection.ids.paragraph_ids[0];
    peer
      .handle
      .insert_rich_fragment(InsertRichFragmentIntent {
        at: TextAnchor::new(paragraph, 0),
        blocks: vec![
          FragmentBlock::Paragraph(input_paragraph("Introduction with several words to edit.")),
          FragmentBlock::Object(image_input(rng)),
          FragmentBlock::Paragraph(input_paragraph("")),
          FragmentBlock::Paragraph(input_paragraph("Text after the first image.")),
          FragmentBlock::Paragraph(input_paragraph("Left of soft break\u{2028}right of soft break.")),
          FragmentBlock::Paragraph(input_paragraph("alpha bravo charlie")),
          FragmentBlock::Object(equation_input(rng)),
          FragmentBlock::Paragraph(input_paragraph("")),
          FragmentBlock::Paragraph(input_paragraph("Closing remarks paragraph.")),
        ],
      })
      .expect("seed structural fixture");
  }

  /// The old `table_fixture`: a paragraph, a 2x3 table, a trailing paragraph.
  fn seed_table(peer: &Peer, _rng: &mut Rng) {
    let projection = peer.projection();
    let paragraph = projection.ids.paragraph_ids[0];
    peer
      .handle
      .insert_rich_fragment(InsertRichFragmentIntent {
        at: TextAnchor::new(paragraph, 0),
        blocks: vec![
          FragmentBlock::Paragraph(input_paragraph("Above the table.")),
          FragmentBlock::Object(table_input(2, 3)),
          FragmentBlock::Paragraph(input_paragraph("Below the table.")),
        ],
      })
      .expect("seed table fixture");
  }

  // ---------------------------------------------------------------------------
  // Random intent generators
  // ---------------------------------------------------------------------------

  fn random_styles(rng: &mut Rng) -> RunStyles {
    let mut styles = RunStyles::default();
    if rng.below(2) == 0 {
      styles.semantic = RunSemanticStyle::Custom((rng.below(4) + 1) as u8);
    }
    if rng.below(3) == 0 {
      styles.direct_underline = true;
    }
    if rng.below(4) == 0 {
      styles.strikethrough = true;
    }
    styles
  }

  /// Paragraph/text coordinate stress: inserts (with occasional intra-paragraph
  /// U+2028 soft breaks — the coordinate paths that broke on the real doc),
  /// deletes, splits, joins, run marks, paragraph styles. Byte hints are raw and
  /// clamped to char boundaries by the resolver.
  fn random_text_intent(rng: &mut Rng, peer: &Peer, step: usize) -> Result<(), WriteRejected> {
    let projection = peer.projection();
    if projection.paragraphs.is_empty() {
      return Ok(());
    }
    let paragraph_ix = rng.below(projection.paragraphs.len());
    let paragraph = projection.ids.paragraph_ids[paragraph_ix];
    let text_len = paragraph_text_len(&projection.paragraphs[paragraph_ix]);
    let byte = rng.below(text_len + 1);
    let text_arm = rng.below(10);
    if std::env::var("FUZZ_PER_OP_CHECK").is_ok() {
      eprintln!("step {step}: text sub-arm {text_arm} paragraph_ix {paragraph_ix} byte {byte}");
    }
    match text_arm {
      0..=3 => {
        let text = if rng.below(6) == 0 { "\u{2028}".to_string() } else { format!("s{step}") };
        peer
          .handle
          .insert_text(InsertTextIntent {
            at: TextAnchor::new(paragraph, byte),
            text,
            style_override: (rng.below(4) == 0).then(|| random_styles(rng)),
          })
          .map(|_| ())
      },
      4 => {
        if text_len == 0 {
          return Ok(());
        }
        let start = rng.below(text_len);
        let end = (start + 1 + rng.below(3)).min(text_len);
        peer
          .handle
          .delete_range(DeleteRangeIntent {
            start: TextAnchor::new(paragraph, start),
            end: TextAnchor::new(paragraph, end),
          })
          .map(|_| ())
      },
      5 if projection.paragraphs.len() < 32 => peer
        .handle
        .split_paragraph(SplitParagraphIntent {
          at: TextAnchor::new(paragraph, byte),
          inherited_style: if rng.below(2) == 0 { ParagraphStyle::Normal } else { ParagraphStyle::Custom(1) },
        })
        .map(|_| ()),
      6 => {
        if paragraph_ix + 1 >= projection.paragraphs.len() {
          return Ok(());
        }
        let second = projection.ids.paragraph_ids[paragraph_ix + 1];
        peer
          .handle
          .join_paragraphs(JoinParagraphsIntent { first: paragraph, second })
          .map(|_| ())
      },
      7..=8 => {
        if text_len == 0 {
          return Ok(());
        }
        let start = rng.below(text_len);
        let end = (start + 1 + rng.below(4)).min(text_len);
        peer
          .handle
          .set_marks(SetMarksIntent {
            start: TextAnchor::new(paragraph, start),
            end: TextAnchor::new(paragraph, end),
            styles: random_styles(rng),
          })
          .map(|_| ())
      },
      _ => peer
        .handle
        .set_paragraph_style(SetParagraphStyleIntent {
          paragraph,
          style: if rng.below(2) == 0 { ParagraphStyle::Normal } else { ParagraphStyle::Custom((rng.below(3) + 1) as u8) },
        })
        .map(|_| ()),
    }
  }

  /// Object block-structure ops (the old `object_structural_command` surface):
  /// insert/delete/move/replace at identity-anchored positions plus image
  /// alt/caption/layout, equation source edits, and rich-fragment paste.
  fn random_object_intent(rng: &mut Rng, peer: &Peer, step: usize) -> Result<(), WriteRejected> {
    let projection = peer.projection();
    if projection.paragraphs.is_empty() {
      return Ok(());
    }
    let paragraph_ix = rng.below(projection.paragraphs.len());
    let paragraph = projection.ids.paragraph_ids[paragraph_ix];
    let text_len = paragraph_text_len(&projection.paragraphs[paragraph_ix]);
    let objects = object_block_indices(&projection);
    let pick_object = |rng: &mut Rng| objects.get(rng.below(objects.len().max(1))).copied();
    let images: Vec<usize> = (0..projection.blocks.len())
      .filter(|ix| matches!(projection.blocks[*ix], Block::Image(_)))
      .collect();
    let equations: Vec<usize> = (0..projection.blocks.len())
      .filter(|ix| matches!(projection.blocks[*ix], Block::Equation(_)))
      .collect();
    let object_arm = rng.below(9);
    if std::env::var("FUZZ_PER_OP_CHECK").is_ok() {
      eprintln!("step {step}: object sub-arm {object_arm} paragraph_ix {paragraph_ix}");
    }
    match object_arm {
      // InsertObject at an identity-anchored position (byte 0 = before the
      // paragraph's block, byte > 0 = after it — both positions fuzzed).
      0..=1 => {
        let byte = if rng.below(2) == 0 { 0 } else { text_len };
        let block = object_input(rng);
        peer
          .handle
          .insert_object(InsertObjectIntent {
            at: TextAnchor::new(paragraph, byte),
            block,
          })
          .map(|_| ())
      },
      2 => {
        let Some(object_ix) = pick_object(rng) else { return Ok(()) };
        peer
          .handle
          .delete_blocks(DeleteBlocksIntent {
            blocks: vec![projection.ids.block_ids[object_ix]],
          })
          .map(|_| ())
      },
      3 => {
        let Some(object_ix) = pick_object(rng) else { return Ok(()) };
        let before = if rng.below(4) == 0 {
          None // move to document end
        } else {
          Some(projection.ids.block_ids[rng.below(projection.blocks.len())])
        };
        peer
          .handle
          .move_block(MoveBlockIntent {
            block: projection.ids.block_ids[object_ix],
            before,
          })
          .map(|_| ())
      },
      4 => {
        let Some(object_ix) = pick_object(rng) else { return Ok(()) };
        let after = object_input(rng);
        peer
          .handle
          .replace_object(ReplaceObjectIntent {
            block: projection.ids.block_ids[object_ix],
            after,
          })
          .map(|_| ())
      },
      5 => {
        let Some(&image_ix) = images.get(rng.below(images.len().max(1))) else { return Ok(()) };
        let image = projection.ids.block_ids[image_ix];
        match rng.below(3) {
          0 => peer
            .handle
            .replace_image_alt_text(ReplaceImageAltTextIntent {
              image,
              text: format!("alt-{step}"),
            })
            .map(|_| ()),
          1 => peer
            .handle
            .replace_image_caption(ReplaceImageCaptionIntent {
              image,
              caption: (rng.below(2) == 0).then(|| input_paragraph(&format!("cap-{step}"))),
            })
            .map(|_| ()),
          _ => peer
            .handle
            .set_image_layout(SetImageLayoutIntent {
              image,
              sizing: match rng.below(3) {
                0 => InputImageSizing::Intrinsic,
                1 => InputImageSizing::FitWidth,
                _ => InputImageSizing::Fixed {
                  width_px: 100 + rng.below(200) as u32,
                  height_px: None,
                },
              },
              alignment: match rng.below(3) {
                0 => InputBlockAlignment::Left,
                1 => InputBlockAlignment::Center,
                _ => InputBlockAlignment::Right,
              },
            })
            .map(|_| ()),
        }
      },
      6 => {
        let Some(&equation_ix) = equations.get(rng.below(equations.len().max(1))) else { return Ok(()) };
        peer
          .handle
          .replace_equation_source_range(ReplaceEquationSourceRangeIntent {
            equation: projection.ids.block_ids[equation_ix],
            range: 0..0,
            text: format!("+{}", rng.below(10)),
          })
          .map(|_| ())
      },
      // Rich-fragment paste: paragraph + object + paragraph in one compound
      // intent (one gate hold, one commit).
      _ => {
        let object = object_input(rng);
        peer
          .handle
          .insert_rich_fragment(InsertRichFragmentIntent {
            at: TextAnchor::new(paragraph, rng.below(text_len + 1)),
            blocks: vec![
              FragmentBlock::Paragraph(input_paragraph(&format!("frag{step}a"))),
              FragmentBlock::Object(object),
              FragmentBlock::Paragraph(input_paragraph(&format!("frag{step}b"))),
            ],
          })
          .map(|_| ())
      },
    }
  }

  /// All 9 table ops over the first table (durable §P2b identities throughout).
  fn random_table_intent(rng: &mut Rng, peer: &Peer, step: usize) -> Result<(), WriteRejected> {
    let projection = peer.projection();
    let Some((table_id, table)) = first_table(&projection) else {
      return random_text_intent(rng, peer, step);
    };
    if table.rows.is_empty() || table.columns.is_empty() {
      return Ok(());
    }
    let row = table.rows[rng.below(table.rows.len())].id;
    let column = table.columns[rng.below(table.columns.len())].id;
    let column_ids: Vec<ColumnId> = table.columns.iter().map(|c| c.id).collect();
    let table_arm = rng.below(9);
    if std::env::var("FUZZ_PER_OP_CHECK").is_ok() {
      eprintln!("step {step}: table sub-arm {table_arm} row {row:?} column {column:?}");
    }
    let intent = match table_arm {
      0 => TableIntent::InsertRow {
        table: table_id,
        after_row: if rng.below(3) == 0 { None } else { Some(row) },
        row: fresh_row_input(&column_ids, "new"),
      },
      1 if table.rows.len() > 1 => TableIntent::DeleteRow { table: table_id, row },
      2 if table.rows.len() > 1 => TableIntent::MoveRow {
        table: table_id,
        row,
        after_row: if rng.below(3) == 0 {
          None
        } else {
          Some(table.rows[rng.below(table.rows.len())].id)
        },
      },
      3 => TableIntent::InsertColumn {
        table: table_id,
        after_column: if rng.below(3) == 0 { None } else { Some(column) },
        width: InputTableColumnWidth::Auto,
      },
      4 if table.columns.len() > 1 => TableIntent::DeleteColumn { table: table_id, column },
      5 if table.columns.len() > 1 => TableIntent::MoveColumn {
        table: table_id,
        column,
        after_column: if rng.below(3) == 0 {
          None
        } else {
          Some(table.columns[rng.below(table.columns.len())].id)
        },
      },
      6 => TableIntent::ReplaceCell {
        table: table_id,
        row,
        column,
        cell: table_cell_input(row, column, &format!("edit{step}")),
      },
      7 => TableIntent::SetCellSpan {
        table: table_id,
        row,
        column,
        row_span: 1 + rng.below(2) as u16,
        column_span: 1 + rng.below(2) as u16,
      },
      _ => TableIntent::SetColumnWidth {
        table: table_id,
        column,
        width: match rng.below(3) {
          0 => InputTableColumnWidth::Auto,
          1 => InputTableColumnWidth::FixedPx(50 + rng.below(200) as u32),
          _ => InputTableColumnWidth::Fraction(1 + rng.below(4) as u32),
        },
      },
    };
    peer.handle.table_op(intent).map(|_| ())
  }

  /// ~60% text coordinate stress / ~40% object block-structure ops.
  fn random_structural_intent(rng: &mut Rng, peer: &Peer, step: usize) -> Result<(), WriteRejected> {
    let arm = rng.below(10);
    if std::env::var("FUZZ_PER_OP_CHECK").is_ok() {
      eprintln!("step {step}: structural arm {arm}");
    }
    match arm {
      0..=4 => random_text_intent(rng, peer, step),
      // Undo/redo of OBJECT-structural history under concurrency (spec §10):
      // intent_fuzz covers body-text undo; this leg covers undoing object
      // inserts/moves/replaces and their transforms against remote edits.
      5 => peer.handle.apply_undo().map(|_| ()),
      6 => peer.handle.apply_redo().map(|_| ()),
      _ => random_object_intent(rng, peer, step),
    }
  }

  /// ~50% text stress / ~35% table ops / ~15% undo-redo (durable-id table
  /// history under undo transforms).
  fn random_table_mix_intent(rng: &mut Rng, peer: &Peer, step: usize) -> Result<(), WriteRejected> {
    let arm = rng.below(100);
    if std::env::var("FUZZ_PER_OP_CHECK").is_ok() {
      eprintln!("step {step}: table-mix arm {arm}");
    }
    match arm {
      0..=49 => random_text_intent(rng, peer, step),
      50..=57 => peer.handle.apply_undo().map(|_| ()),
      58..=64 => peer.handle.apply_redo().map(|_| ()),
      _ => random_table_intent(rng, peer, step),
    }
  }

  // ---------------------------------------------------------------------------
  // Fuzz driver
  // ---------------------------------------------------------------------------

  /// Rejections from stale identities, empty/degenerate intents, structural
  /// refusals, and compensated no-op mutations are legal fuzz outcomes (I-15 /
  /// I-10) — only convergence is asserted. Wedge states must fail loudly.
  fn tolerate(result: Result<(), WriteRejected>, seed: u64, round: usize, op: usize) {
    match result {
      Ok(()) => {},
      Err(fatal @ (WriteRejected::GatePoisoned | WriteRejected::CompensationFailed { .. })) => {
        panic!("seed {seed} round {round} op {op}: doc wedged: {fatal}")
      },
      Err(_) => {},
    }
  }

  type IntentGenerator = fn(&mut Rng, &Peer, usize) -> Result<(), WriteRejected>;
  type Seeder = fn(&Peer, &mut Rng);

  fn run_fuzz(seed: u64, peers_n: usize, rounds: usize, ops_per_round: usize, seeder: Seeder, generator: IntentGenerator) {
    let mut rng = Rng::new(seed);
    let mut peers: Vec<Peer> = (0..peers_n).map(|_| Peer::new("structural fuzz", peers_n)).collect();
    // Converge the mergeable empty seeds, build the fixture on peer 0 through
    // the intent API, and converge again so every peer shares the identities.
    sync_all(&mut peers);
    seeder(&peers[0], &mut rng);
    sync_all(&mut peers);
    assert_converged(&peers, &format!("seed {seed} post-fixture"));

    let per_op_check = std::env::var("FUZZ_PER_OP_CHECK").is_ok();
    for round in 0..rounds {
      for op in 0..ops_per_round {
        let peer_ix = rng.below(peers_n);
        let step = round * ops_per_round + op;
        tolerate(generator(&mut rng, &peers[peer_ix], step), seed, round, op);
        if per_op_check {
          let maintained = peers[peer_ix].projection();
          let fresh = peers[peer_ix].fresh_projection();
          eprintln!(
            "  after step {step}: maintained={:?} fresh={:?}",
            maintained.ids.paragraph_ids.iter().map(|id| id.0 % 100000).collect::<Vec<_>>(),
            fresh.ids.paragraph_ids.iter().map(|id| id.0 % 100000).collect::<Vec<_>>(),
          );
          if let Err(reason) = projections_agree(&maintained, &fresh) {
            let fresh_again = peers[peer_ix].fresh_projection();
            let determinism = match projections_agree(&fresh, &fresh_again) {
              Ok(()) => "fresh is deterministic".to_string(),
              Err(inner) => format!("fresh is NON-DETERMINISTIC: {inner}"),
            };
            panic!("seed {seed} round {round} op {op} (peer {peer_ix}): projection deviates from own canonical ({determinism}): {reason}");
          }
        }
      }
      sync_all(&mut peers);
      assert_converged(&peers, &format!("seed {seed} round {round}"));
    }
    assert_materializer_equivalence(&peers, &format!("seed {seed} final"));
  }

  // ---------------------------------------------------------------------------
  // Fuzz suites
  // ---------------------------------------------------------------------------

  /// Object block-structure convergence: insert/delete/move/replace + image and
  /// equation metadata + rich fragments over the structural fixture (objects
  /// flanked by empties, soft breaks) — the coordinate/coalescing paths blank
  /// docs structurally cannot reach.
  #[test]
  fn npeer_object_structural_fuzz_converges() {
    for seed in [0xA1, 0xB2, 0xC3] {
      run_fuzz(seed, 2, 6, 10, seed_structural, random_structural_intent);
    }
    run_fuzz(0x1111, 3, 5, 8, seed_structural, random_structural_intent);
  }

  /// Full table-op convergence: the 9 table intents with durable row/column/cell
  /// identities plus text stress, seeded from the 2x3 table fixture.
  #[test]
  fn npeer_table_op_fuzz_converges() {
    for seed in [7, 42, 20260707] {
      run_fuzz(seed, 2, 6, 10, seed_table, random_table_mix_intent);
    }
    run_fuzz(0x2222, 3, 5, 8, seed_table, random_table_mix_intent);
  }

  // ---------------------------------------------------------------------------
  // Directed concurrent regressions (semantics carried from the old suites)
  // ---------------------------------------------------------------------------

  /// Two peers with one seeded table, converged and ready for concurrent edits.
  fn table_pair() -> (Vec<Peer>, BlockId, TableBlock) {
    let mut rng = Rng::new(1);
    let mut peers: Vec<Peer> = (0..2).map(|_| Peer::new("table directed", 2)).collect();
    sync_all(&mut peers);
    seed_table(&peers[0], &mut rng);
    sync_all(&mut peers);
    let projection = peers[0].projection();
    let (table_id, table) = first_table(&projection).expect("seeded table");
    (peers, table_id, table)
  }

  fn projected_table(peer: &Peer) -> TableBlock {
    first_table(&peer.projection()).expect("table present").1
  }

  fn row_texts(table: &TableBlock) -> Vec<String> {
    table
      .rows
      .iter()
      .map(|row| row.cells.iter().map(cell_text).collect::<Vec<_>>().join("|"))
      .collect()
  }

  /// Both peers insert a row after the SAME anchor row concurrently: both rows
  /// and both originals must survive the merge (durable-id row identity).
  #[test]
  fn concurrent_row_inserts_after_same_anchor_both_survive() {
    let (mut peers, table_id, table) = table_pair();
    let anchor = table.rows[0].id;
    let column_ids: Vec<ColumnId> = table.columns.iter().map(|c| c.id).collect();

    peers[0]
      .handle
      .table_op(TableIntent::InsertRow {
        table: table_id,
        after_row: Some(anchor),
        row: fresh_row_input(&column_ids, "A"),
      })
      .expect("peer 0 row insert");
    peers[1]
      .handle
      .table_op(TableIntent::InsertRow {
        table: table_id,
        after_row: Some(anchor),
        row: fresh_row_input(&column_ids, "B"),
      })
      .expect("peer 1 row insert");
    sync_all(&mut peers);
    assert_converged(&peers, "concurrent row inserts");
    assert_materializer_equivalence(&peers, "concurrent row inserts");

    let merged = projected_table(&peers[0]);
    assert_eq!(merged.rows.len(), 4, "both inserted rows plus the two originals must survive");
    let texts = row_texts(&merged);
    assert!(texts.contains(&"r0c0|r0c1|r0c2".to_string()), "original first row survives: {texts:?}");
    assert!(texts.contains(&"r1c0|r1c1|r1c2".to_string()), "original second row survives: {texts:?}");
    assert!(texts.contains(&"A|A|A".to_string()), "peer 0's row survives: {texts:?}");
    assert!(texts.contains(&"B|B|B".to_string()), "peer 1's row survives: {texts:?}");
  }

  /// Concurrent rewrites of DISTINCT durable cells are independent: both edits
  /// survive, untouched cells stay put.
  #[test]
  fn concurrent_distinct_cell_edits_are_independent() {
    let (mut peers, table_id, table) = table_pair();
    let (r1, r2) = (table.rows[0].id, table.rows[1].id);
    let (c1, c3) = (table.columns[0].id, table.columns[2].id);

    peers[0]
      .handle
      .table_op(TableIntent::ReplaceCell {
        table: table_id,
        row: r1,
        column: c1,
        cell: table_cell_input(r1, c1, "A-EDIT"),
      })
      .expect("peer 0 cell edit");
    peers[1]
      .handle
      .table_op(TableIntent::ReplaceCell {
        table: table_id,
        row: r2,
        column: c3,
        cell: table_cell_input(r2, c3, "B-EDIT"),
      })
      .expect("peer 1 cell edit");
    sync_all(&mut peers);
    assert_converged(&peers, "concurrent distinct cell edits");
    assert_materializer_equivalence(&peers, "concurrent distinct cell edits");

    let merged = projected_table(&peers[0]);
    assert_eq!(cell_text(&merged.rows[0].cells[0]), "A-EDIT", "peer 0's cell edit survives");
    assert_eq!(cell_text(&merged.rows[1].cells[2]), "B-EDIT", "peer 1's cell edit survives");
    assert_eq!(cell_text(&merged.rows[0].cells[1]), "r0c1", "untouched cell unchanged");
    assert_eq!(cell_text(&merged.rows[1].cells[0]), "r1c0", "untouched cell unchanged");
  }

  /// FS-010: concurrent add-row x add-column leaves the (new row, new column)
  /// crossing uncreated by either peer — topology repair must synthesize it
  /// deterministically as ONE empty paragraph so every peer reads an identical
  /// full grid.
  #[test]
  fn concurrent_add_row_and_add_column_repairs_cross_cell() {
    let (mut peers, table_id, table) = table_pair();
    let base_rows: Vec<RowId> = table.rows.iter().map(|r| r.id).collect();
    let base_columns: Vec<ColumnId> = table.columns.iter().map(|c| c.id).collect();

    peers[0]
      .handle
      .table_op(TableIntent::InsertRow {
        table: table_id,
        after_row: Some(base_rows[1]),
        row: fresh_row_input(&base_columns, "r"),
      })
      .expect("peer 0 row insert");
    peers[1]
      .handle
      .table_op(TableIntent::InsertColumn {
        table: table_id,
        after_column: Some(base_columns[2]),
        width: InputTableColumnWidth::Auto,
      })
      .expect("peer 1 column insert");
    sync_all(&mut peers);
    assert_converged(&peers, "add-row x add-column repair");
    assert_materializer_equivalence(&peers, "add-row x add-column repair");

    let merged = projected_table(&peers[0]);
    assert_eq!(merged.rows.len(), 3, "two originals plus the inserted row");
    assert_eq!(merged.columns.len(), 4, "three originals plus the inserted column");
    for (row_ix, row) in merged.rows.iter().enumerate() {
      assert_eq!(row.cells.len(), 4, "row {row_ix} must be a full 4-column grid after repair");
    }
    let new_row_ix = merged
      .rows
      .iter()
      .position(|row| !base_rows.contains(&row.id))
      .expect("inserted row present");
    let new_column_ix = merged
      .columns
      .iter()
      .position(|column| !base_columns.contains(&column.id))
      .expect("inserted column present");
    let cross = &merged.rows[new_row_ix].cells[new_column_ix];
    assert_eq!(
      cross.blocks.len(),
      1,
      "synthesized cross cell must be one empty paragraph, not a doubled flow (got {} blocks)",
      cross.blocks.len()
    );
    assert_eq!(cell_text(cross), "", "synthesized cross cell must be empty");
  }

  /// Concurrent same-paragraph edits against the same base: both inserts and one
  /// of the two concurrent paragraph styles must survive (the old two-peer
  /// concurrent-paragraph regression, intent-flavored).
  #[test]
  fn concurrent_same_paragraph_edits_converge() {
    let mut peers: Vec<Peer> = (0..2).map(|_| Peer::new("same paragraph", 2)).collect();
    sync_all(&mut peers);
    let paragraph = peers[0].projection().ids.paragraph_ids[0];
    peers[0]
      .handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(paragraph, 0),
        text: "hello world".into(),
        style_override: None,
      })
      .expect("seed text");
    sync_all(&mut peers);

    peers[0]
      .handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(paragraph, 0),
        text: "A>".into(),
        style_override: None,
      })
      .expect("peer 0 insert");
    peers[0]
      .handle
      .set_paragraph_style(SetParagraphStyleIntent {
        paragraph,
        style: ParagraphStyle::Custom(2),
      })
      .expect("peer 0 style");
    peers[1]
      .handle
      .insert_text(InsertTextIntent {
        at: TextAnchor::new(paragraph, usize::MAX),
        text: "<B".into(),
        style_override: None,
      })
      .expect("peer 1 insert");
    peers[1]
      .handle
      .set_paragraph_style(SetParagraphStyleIntent {
        paragraph,
        style: ParagraphStyle::Custom(3),
      })
      .expect("peer 1 style");
    sync_all(&mut peers);
    assert_converged(&peers, "concurrent same-paragraph edits");
    assert_materializer_equivalence(&peers, "concurrent same-paragraph edits");

    let projection = peers[0].projection();
    assert_eq!(paragraph_text(&projection, 0), "A>hello world<B", "both concurrent inserts must survive the merge");
    let style = projection.paragraphs[0].style;
    assert!(
      matches!(style, ParagraphStyle::Custom(2) | ParagraphStyle::Custom(3)),
      "converged paragraph style must be one of the concurrently applied styles, got {style:?}"
    );
  }

  /// Minimal repro scaffold for the undo+table derivation class the fuzz
  /// caught (seed 7: SetCellSpan after undo/redo of column topology leaves
  /// the maintained projection's spans stale vs a fresh rematerialization).
  #[test]
  fn table_ops_after_undo_redo_stay_canonical() {
    let mut rng = Rng::new(1);
    let peers: Vec<Peer> = (0..1).map(|_| Peer::new("undo-table", 1)).collect();
    seed_table(&peers[0], &mut rng);
    let peer = &peers[0];
    let check = |label: &str| {
      if let Err(reason) = projections_agree(&peer.projection(), &peer.fresh_projection()) {
        panic!("{label}: projection deviates from own canonical: {reason}");
      }
    };
    check("post-seed");

    let (table_id, table) = first_table(&peer.projection()).expect("table");
    let column0 = table.columns[0].id;
    let row0 = table.rows[0].id;

    peer
      .handle
      .table_op(TableIntent::InsertColumn {
        table: table_id,
        after_column: Some(column0),
        width: InputTableColumnWidth::Auto,
      })
      .expect("insert column");
    check("post-insert-column");
    peer.handle.apply_undo().expect("undo insert column");
    check("post-undo");
    peer.handle.apply_redo().expect("redo insert column");
    check("post-redo");

    peer
      .handle
      .table_op(TableIntent::SetCellSpan {
        table: table_id,
        row: row0,
        column: column0,
        row_span: 2,
        column_span: 2,
      })
      .expect("set cell span");
    check("post-set-span");

    peer.handle.apply_undo().expect("undo set span");
    check("post-undo-span");
    peer.handle.apply_redo().expect("redo set span");
    check("post-redo-span");
  }

  /// TEMP shrink harness: single-peer sweep (no imports) to shrink the fuzz
  /// finding to a local-only sequence.
  #[test]
  #[ignore]
  fn scratch_single_peer_table_sweep() {
    for seed in 1..200 {
      run_fuzz(seed, 1, 3, 12, seed_table, random_table_mix_intent);
    }
  }

  #[test]
  #[ignore]
  fn scratch_single_peer_object_sweep() {
    for seed in 1..200 {
      run_fuzz(seed, 1, 3, 12, seed_structural, random_structural_intent);
    }
  }

  /// Minimal repro (from the single-peer fuzz shrink, seed 1): undo the
  /// seeded fragment, rebuild some text, then insert an object at the FIRST
  /// paragraph's byte 0. The maintained projection keeps the seeded root
  /// paragraph id while a fresh rematerialization elects a fabricated
  /// interstitial identity for the after-object row.
  #[test]
  fn object_insert_at_paragraph_start_after_undo_stays_canonical() {
    let mut rng = Rng::new(1);
    let peers: Vec<Peer> = (0..1).map(|_| Peer::new("obj-undo", 1)).collect();
    let peer = &peers[0];
    seed_structural(peer, &mut rng);
    peer.handle.apply_undo().expect("undo the seeded fixture");

    let check = |label: &str| {
      if let Err(reason) = projections_agree(&peer.projection(), &peer.fresh_projection()) {
        panic!("{label}: projection deviates from own canonical: {reason}");
      }
    };
    check("post-undo");
    let dump_registry = |label: &str| {
      let guard = peer.gate.lock(GateHolder::ExportUpdates).expect("gate");
      let doc = guard.doc();
      let root = doc.get_map("flowstate.root");
      let Some(loro::ValueOrContainer::Container(loro::Container::Map(paragraphs))) = root.get("paragraphs_by_id") else {
        eprintln!("{label}: no paragraphs_by_id");
        return;
      };
      for key in paragraphs.keys() {
        let record = match paragraphs.get(&key) {
          Some(loro::ValueOrContainer::Container(loro::Container::Map(map))) => map,
          _ => continue,
        };
        let cursor_state: Vec<String> = ["boundary_cursor", "start_cursor"]
          .iter()
          .map(|field| {
            match record.get(field) {
              Some(loro::ValueOrContainer::Value(loro::LoroValue::Binary(bytes))) => {
                match loro::cursor::Cursor::decode(&bytes) {
                  Ok(cursor) => match doc.get_cursor_pos(&cursor) {
                    Ok(pos) => format!("{field}=pos {}", pos.current.pos),
                    Err(error) => format!("{field}=unresolved({error})"),
                  },
                  Err(_) => format!("{field}=undecodable"),
                }
              },
              _ => format!("{field}=absent"),
            }
          })
          .collect();
        eprintln!("{label}: record {key} {cursor_state:?}");
      }
    };
    dump_registry("post-undo");

    let projection = peer.projection();
    let paragraph = projection.ids.paragraph_ids[0];
    peer
      .handle
      .insert_object(InsertObjectIntent {
        at: TextAnchor::new(paragraph, 0),
        block: {
          let mut object_rng = Rng::new(42);
          object_input(&mut object_rng)
        },
      })
      .expect("insert object at paragraph start");
    dump_registry("post-insert");
    {
      let guard = peer.gate.lock(GateHolder::ExportUpdates).expect("gate");
      let (fresh_a, defects_a) = flowstate_document::document_from_loro_with_defects(guard.doc()).expect("fresh a");
      let (fresh_b, defects_b) = flowstate_document::document_from_loro_with_defects(guard.doc()).expect("fresh b");
      eprintln!(
        "fresh determinism: a={:?} b={:?} body={:?}\n  defects_a={:?}\n  defects_b={:?}",
        fresh_a.ids.paragraph_ids,
        fresh_b.ids.paragraph_ids,
        flowstate_document::loro_schema::body_text(guard.doc()).to_string(),
        defects_a.iter().map(|d| d.stable_key()).collect::<Vec<_>>(),
        defects_b.iter().map(|d| d.stable_key()).collect::<Vec<_>>(),
      );
    }
    check("post-insert-object");
  }
}
