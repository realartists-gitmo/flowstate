//! Cell TEXT execution (flow architecture spec Part 2.2): translate the
//! editor's [`LocalIntent`]s into MINIMAL Loro ops on one cell's flow — a
//! keystroke is one insert op, a restyle is mark ops over the changed range —
//! so concurrent same-cell edits merge char-level at the CRDT. Anchors
//! resolve inside the gate against the cell's own container (cursor bytes
//! preferred; durable paragraph identity + byte hint fallback via the cell's
//! scoped registry). Object/table/image intents are rejected: flow cells are
//! text-only surfaces (`flow_cell_surface`), and the executor enforces it.

use flowstate_flow::{CellId, loro_schema};
use gpui_flowtext::{
  DeleteRangeIntent, FragmentBlock, InsertRichFragmentIntent, InsertTextIntent, JoinParagraphsIntent, LocalIntent, ParagraphId,
  ReplaceMatchesIntent, RunStyles, SetMarksIntent, SetParagraphStyleIntent, SetParagraphStylesIntent, SplitParagraphIntent, TextAnchor,
  WriteRejected,
};
use loro::{ContainerTrait as _, LoroDoc, LoroMap, LoroText, cursor::Cursor, cursor::Side};
use rustc_hash::FxHashMap;

pub struct CellTextContext {
  doc: LoroDoc,
  text: LoroText,
  registry: LoroMap,
  /// paragraph record key ("paragraph.{u128}") → flow boundary unicode pos.
  boundaries_by_key: FxHashMap<String, usize>,
  text_len: usize,
}

impl CellTextContext {
  pub fn resolve(doc: &LoroDoc, cell_id: CellId) -> Result<Self, WriteRejected> {
    let record = loro_schema::cell_record(doc, cell_id).ok_or(WriteRejected::StructureViolation("unknown flow cell"))?;
    let flow = loro_schema::cell_flow(&record).ok_or(WriteRejected::StructureViolation("flow cell has no rich text"))?;
    let registry =
      loro_schema::cell_paragraph_registry(&flow).ok_or(WriteRejected::StructureViolation("flow cell has no paragraph registry"))?;
    let text = flow
      .ensure_mergeable_text(flowstate_document::FLOW_TEXT_KEY)
      .map_err(|_| WriteRejected::StructureViolation("flow cell text container unavailable"))?;
    let boundary_map = flowstate_document::paragraph_ids_by_boundary_in(doc, &registry, &text);
    let mut boundaries_by_key: FxHashMap<String, usize> = boundary_map
      .into_iter()
      .map(|(pos, key)| (key, pos))
      .collect();
    // A boundary without a resolvable registry record (an undo can restore a
    // record whose cursors point at the tombstoned `\n`, not the re-inserted
    // one) is projected under a FABRICATED id — stable, derived from the
    // boundary's OpID (`stable_boundary_metadata_keys`). Resolve those ids
    // here under the same law, so an editor anchored in such a paragraph
    // keeps typing while the runtime's repair pass re-mints the durable
    // record.
    for (boundary, fabricated_key) in fabricated_boundary_keys(&text, boundaries_by_key.values().copied().collect()) {
      boundaries_by_key.insert(fabricated_key, boundary);
    }
    let text_len = text.len_unicode();
    Ok(Self {
      doc: doc.clone(),
      text,
      registry,
      boundaries_by_key,
      text_len,
    })
  }

  fn paragraph_boundary(&self, paragraph: ParagraphId) -> Result<usize, WriteRejected> {
    // Registry keys are always the u128 form ("paragraph.{u128}") — both cell
    // seed paths write projection_paragraph_id keys — so the loro_id_u128 law
    // inverts exactly.
    let key = format!("paragraph.{}", paragraph.0);
    self
      .boundaries_by_key
      .get(&key)
      .copied()
      .ok_or(WriteRejected::UnresolvedParagraph(paragraph))
  }

  /// End (exclusive) of the paragraph starting at `boundary`: the next
  /// boundary, or the text end.
  fn paragraph_end(&self, boundary: usize) -> usize {
    self
      .boundaries_by_key
      .values()
      .copied()
      .filter(|candidate| *candidate > boundary)
      .min()
      .unwrap_or(self.text_len)
  }

  /// Resolve an anchor to an absolute flow unicode position. Cursor bytes
  /// win (they encode the container — a foreign cell's cursor fails safely);
  /// durable paragraph identity + byte hint is the fallback.
  fn resolve_anchor(&self, anchor: &TextAnchor) -> Result<usize, WriteRejected> {
    if let Some(bytes) = &anchor.cursor
      && let Ok(cursor) = Cursor::decode(bytes)
      && cursor.container == self.text.id()
      && let Ok(resolved) = self.doc.get_cursor_pos(&cursor)
    {
      return Ok(resolved.current.pos.min(self.text_len));
    }
    let boundary = self.paragraph_boundary(anchor.paragraph)?;
    let start = boundary + 1; // skip the boundary sentinel
    let end = self.paragraph_end(boundary);
    if start > self.text_len {
      return Ok(self.text_len);
    }
    let paragraph_text = self
      .text
      .slice(start, end)
      .map_err(|_| WriteRejected::StructureViolation("paragraph slice failed"))?;
    let byte = anchor.byte_hint.min(paragraph_text.len());
    let chars = paragraph_text
      .get(..byte)
      .map_or_else(|| paragraph_text.chars().count(), |prefix| prefix.chars().count());
    Ok((start + chars).min(end))
  }

  fn refresh_len(&mut self) {
    self.text_len = self.text.len_unicode();
  }
}

/// Apply one editor intent as minimal ops. Returns the (still-uncommitted)
/// mutation and the post-edit caret flow position (unicode index into the
/// cell text, `None` when the caret doesn't move); the runtime owns
/// commit/refresh/streams/publish + mapping the caret to a projection offset.
pub fn execute_cell_text(doc: &LoroDoc, cell_id: CellId, intent: &LocalIntent) -> Result<Option<usize>, WriteRejected> {
  let mut ctx = CellTextContext::resolve(doc, cell_id)?;
  match intent {
    LocalIntent::InsertText(insert) => insert_text(&mut ctx, insert),
    LocalIntent::DeleteRange(delete) => delete_range(&mut ctx, delete),
    LocalIntent::SetMarks(marks) => set_marks(&ctx, marks).map(|()| None),
    LocalIntent::SetParagraphStyle(style) => set_paragraph_style(&ctx, style).map(|()| None),
    LocalIntent::SetParagraphStyles(styles) => set_paragraph_styles(&ctx, styles).map(|()| None),
    LocalIntent::SplitParagraph(split) => split_paragraph(&mut ctx, split),
    LocalIntent::JoinParagraphs(join) => join_paragraphs(&mut ctx, join),
    LocalIntent::InsertRichFragment(fragment) => insert_rich_fragment(&mut ctx, fragment),
    LocalIntent::ReplaceMatches(matches) => replace_matches(&mut ctx, matches).map(|()| None),
    LocalIntent::InsertObject(_)
    | LocalIntent::ReplaceObject(_)
    | LocalIntent::DeleteBlocks(_)
    | LocalIntent::MoveBlock(_)
    | LocalIntent::ReplaceEquationSourceRange(_)
    | LocalIntent::ReplaceImageAltText(_)
    | LocalIntent::SetImageLayout(_)
    | LocalIntent::Table(_)
    | LocalIntent::TableCellText(_) => Err(WriteRejected::StructureViolation("flow cells do not contain objects")),
  }
}

fn loro_err(_: loro::LoroError) -> WriteRejected {
  WriteRejected::StructureViolation("flow cell text op failed")
}

/// Every `\n` boundary in `text` NOT covered by `covered`, paired with its
/// projection-fabricated record key in u128 form (`paragraph.{u128}` of the
/// stable anchor-derived key) — the exact ids `materialize_single_flow` emits
/// for record-less boundaries, inverted the same way registry keys are.
fn fabricated_boundary_keys(text: &LoroText, covered: rustc_hash::FxHashSet<usize>) -> Vec<(usize, String)> {
  let mut keys = Vec::new();
  for (pos, ch) in text.to_string().chars().enumerate() {
    if ch != '\n' || covered.contains(&pos) {
      continue;
    }
    if let Some((paragraph_key, _)) = flowstate_document::loro_projection::stable_boundary_metadata_keys(text, pos) {
      keys.push((
        pos,
        format!("paragraph.{}", flowstate_document::loro_projection::loro_id_u128(&paragraph_key)),
      ));
    }
  }
  keys
}

/// The spec's cell-registry repair pass (mirrors the body's
/// `MissingParagraphMetadata` repair): write a durable paragraph record for
/// every boundary the registry no longer resolves, keyed under the SAME
/// stable derivation the projection fabricates — so the repaired record's id
/// equals the fabricated id on every peer and Loro map LWW converges the
/// concurrent repairs. Check-before-write: coverage is re-derived live.
/// Never commits; the caller owns the `repair`-origin commit.
///
/// `should_attempt` is the caller's per-key quarantine gate (attempt caps);
/// a refused key is skipped without writing. Returns the record keys written
/// (empty = nothing to repair).
pub(super) fn repair_missing_paragraph_records(doc: &LoroDoc, cell_id: CellId, mut should_attempt: impl FnMut(&str) -> bool) -> Vec<String> {
  let Some(record) = loro_schema::cell_record(doc, cell_id) else {
    return Vec::new(); // cell deleted since classification: nothing to repair
  };
  let Some(flow) = loro_schema::cell_flow(&record) else {
    return Vec::new();
  };
  let Some(registry) = loro_schema::cell_paragraph_registry(&flow) else {
    return Vec::new();
  };
  let Ok(text) = flow.ensure_mergeable_text(flowstate_document::FLOW_TEXT_KEY) else {
    return Vec::new();
  };
  let covered = flowstate_document::paragraph_ids_by_boundary_in(doc, &registry, &text)
    .into_keys()
    .collect();
  let mut written = Vec::new();
  for (boundary, key) in fabricated_boundary_keys(&text, covered) {
    if !should_attempt(&key) {
      continue;
    }
    let Ok(paragraph_record) = registry.ensure_mergeable_map(&key) else {
      continue;
    };
    if paragraph_record.insert("id", key.as_str()).is_err() {
      continue;
    }
    if let Some(cursor) = text.get_cursor(boundary, Side::Left) {
      let _ = paragraph_record.insert("start_cursor", cursor.encode());
    }
    if let Some(cursor) = text.get_cursor(boundary, Side::Right) {
      let _ = paragraph_record.insert("boundary_cursor", cursor.encode());
    }
    let _ = paragraph_record.ensure_mergeable_map("attrs");
    written.push(key);
  }
  written
}

fn insert_text(ctx: &mut CellTextContext, intent: &InsertTextIntent) -> Result<Option<usize>, WriteRejected> {
  if intent.text.is_empty() {
    return Err(WriteRejected::EmptyIntent);
  }
  if intent.text.contains('\n') {
    return Err(WriteRejected::StructureViolation("plain inserts must not contain structural newlines"));
  }
  let pos = ctx.resolve_anchor(&intent.at)?.max(1); // never before the seed sentinel
  let inserted = intent.text.chars().count();
  ctx.text.insert(pos, &intent.text).map_err(loro_err)?;
  if let Some(styles) = intent.style_override {
    apply_run_styles(&ctx.text, pos..pos + inserted, styles)?;
  }
  ctx.refresh_len();
  // Caret lands just after the inserted text.
  Ok(Some(pos + inserted))
}

fn delete_range(ctx: &mut CellTextContext, intent: &DeleteRangeIntent) -> Result<Option<usize>, WriteRejected> {
  let a = ctx.resolve_anchor(&intent.start)?;
  let b = ctx.resolve_anchor(&intent.end)?;
  let (start, end) = if a <= b { (a, b) } else { (b, a) };
  if start == end {
    return Err(WriteRejected::EmptyIntent);
  }
  if start == 0 {
    return Err(WriteRejected::StructureViolation("the leading sentinel is structural"));
  }
  // Deleting across boundaries removes their sentinels: drop the orphaned
  // registry records so identity stays exact.
  let doomed: Vec<String> = ctx
    .boundaries_by_key
    .iter()
    .filter(|(_, pos)| **pos >= start && **pos < end)
    .map(|(key, _)| key.clone())
    .collect();
  ctx.text.delete(start, end - start).map_err(loro_err)?;
  for key in doomed {
    if ctx.registry.get(&key).is_some() {
      ctx.registry.delete(&key).map_err(loro_err)?;
    }
    ctx.boundaries_by_key.remove(&key);
  }
  ctx.refresh_len();
  // Caret collapses to the start of the removed range.
  Ok(Some(start))
}

fn set_marks(ctx: &CellTextContext, intent: &SetMarksIntent) -> Result<(), WriteRejected> {
  let a = ctx.resolve_anchor(&intent.start)?;
  let b = ctx.resolve_anchor(&intent.end)?;
  let (start, end) = if a <= b { (a, b) } else { (b, a) };
  if start == end {
    return Err(WriteRejected::EmptyIntent);
  }
  apply_run_styles(&ctx.text, start..end, intent.styles)
}

/// Exact mark semantics per axis: a non-default value marks the range, a
/// default value unmarks it (so toggling OFF works over inherited styles).
fn apply_run_styles(text: &LoroText, range: std::ops::Range<usize>, styles: RunStyles) -> Result<(), WriteRejected> {
  use flowstate_document::{MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE, MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH};
  if let gpui_flowtext::RunSemanticStyle::Custom(slot) = styles.semantic {
    text
      .mark(range.clone(), MARK_RUN_SEMANTIC_STYLE, i64::from(slot))
      .map_err(loro_err)?;
  } else {
    text
      .unmark(range.clone(), MARK_RUN_SEMANTIC_STYLE)
      .map_err(loro_err)?;
  }
  if let Some(gpui_flowtext::HighlightStyle::Custom(slot)) = styles.highlight {
    text
      .mark(range.clone(), MARK_HIGHLIGHT_STYLE, i64::from(slot))
      .map_err(loro_err)?;
  } else {
    text
      .unmark(range.clone(), MARK_HIGHLIGHT_STYLE)
      .map_err(loro_err)?;
  }
  if styles.direct_underline {
    text
      .mark(range.clone(), MARK_DIRECT_UNDERLINE, true)
      .map_err(loro_err)?;
  } else {
    text
      .unmark(range.clone(), MARK_DIRECT_UNDERLINE)
      .map_err(loro_err)?;
  }
  if styles.strikethrough {
    text
      .mark(range.clone(), MARK_STRIKETHROUGH, true)
      .map_err(loro_err)?;
  } else {
    text.unmark(range, MARK_STRIKETHROUGH).map_err(loro_err)?;
  }
  Ok(())
}

/// The .db8 mark-value convention (`loro_import::paragraph_style_value`):
/// Normal = 0, Custom(slot) = slot + 1.
fn paragraph_style_value(style: gpui_flowtext::ParagraphStyle) -> i64 {
  match style {
    gpui_flowtext::ParagraphStyle::Normal => 0,
    gpui_flowtext::ParagraphStyle::Custom(slot) => i64::from(slot) + 1,
  }
}

fn set_paragraph_style(ctx: &CellTextContext, intent: &SetParagraphStyleIntent) -> Result<(), WriteRejected> {
  let boundary = ctx.paragraph_boundary(intent.paragraph)?;
  ctx
    .text
    .mark(
      boundary..boundary + 1,
      flowstate_document::MARK_PARAGRAPH_STYLE,
      paragraph_style_value(intent.style),
    )
    .map_err(loro_err)
}

fn set_paragraph_styles(ctx: &CellTextContext, intent: &SetParagraphStylesIntent) -> Result<(), WriteRejected> {
  let mut any = false;
  for paragraph in &intent.paragraphs {
    let Ok(boundary) = ctx.paragraph_boundary(*paragraph) else {
      continue; // stale targets are skipped, not rejected
    };
    ctx
      .text
      .mark(
        boundary..boundary + 1,
        flowstate_document::MARK_PARAGRAPH_STYLE,
        paragraph_style_value(intent.style),
      )
      .map_err(loro_err)?;
    any = true;
  }
  if any { Ok(()) } else { Err(WriteRejected::EmptyIntent) }
}

/// Split: insert a boundary sentinel, scrub run styles off it (sentinel
/// hygiene), style it, and mint the new paragraph's registry record.
fn split_paragraph(ctx: &mut CellTextContext, intent: &SplitParagraphIntent) -> Result<Option<usize>, WriteRejected> {
  let pos = ctx.resolve_anchor(&intent.at)?.max(1);
  ctx.text.insert(pos, "\n").map_err(loro_err)?;
  apply_run_styles(&ctx.text, pos..pos + 1, RunStyles::default())?;
  ctx
    .text
    .mark(
      pos..pos + 1,
      flowstate_document::MARK_PARAGRAPH_STYLE,
      paragraph_style_value(intent.inherited_style),
    )
    .map_err(loro_err)?;
  mint_paragraph_record(ctx, pos)?;
  ctx.refresh_len();
  // Caret lands at the start of the new paragraph, just past its sentinel.
  Ok(Some(pos + 1))
}

fn mint_paragraph_record(ctx: &mut CellTextContext, boundary: usize) -> Result<(), WriteRejected> {
  let key = format!("paragraph.{}", uuid::Uuid::new_v4().as_u128());
  let record = ctx.registry.ensure_mergeable_map(&key).map_err(loro_err)?;
  record.insert("id", key.as_str()).map_err(loro_err)?;
  if let Some(cursor) = ctx.text.get_cursor(boundary, Side::Left) {
    record
      .insert("start_cursor", cursor.encode())
      .map_err(loro_err)?;
  }
  if let Some(cursor) = ctx.text.get_cursor(boundary, Side::Right) {
    record
      .insert("boundary_cursor", cursor.encode())
      .map_err(loro_err)?;
  }
  record.ensure_mergeable_map("attrs").map_err(loro_err)?;
  ctx.boundaries_by_key.insert(key, boundary);
  Ok(())
}

fn join_paragraphs(ctx: &mut CellTextContext, intent: &JoinParagraphsIntent) -> Result<Option<usize>, WriteRejected> {
  let first = ctx.paragraph_boundary(intent.first)?;
  let second = ctx.paragraph_boundary(intent.second)?;
  if second <= first {
    return Err(WriteRejected::StructureViolation("join targets must be ordered"));
  }
  if ctx.paragraph_end(first) != second {
    return Err(WriteRejected::StructureViolation("join targets must be adjacent"));
  }
  if second == 0 {
    return Err(WriteRejected::StructureViolation("the leading sentinel is structural"));
  }
  ctx.text.delete(second, 1).map_err(loro_err)?;
  let key = format!("paragraph.{}", intent.second.0);
  if ctx.registry.get(&key).is_some() {
    ctx.registry.delete(&key).map_err(loro_err)?;
  }
  ctx.boundaries_by_key.remove(&key);
  ctx.refresh_len();
  // Caret lands at the seam where the boundary sentinel was removed.
  Ok(Some(second))
}

fn insert_rich_fragment(ctx: &mut CellTextContext, intent: &InsertRichFragmentIntent) -> Result<Option<usize>, WriteRejected> {
  if intent.blocks.is_empty() {
    return Err(WriteRejected::EmptyIntent);
  }
  let mut pos = ctx.resolve_anchor(&intent.at)?.max(1);
  for (index, block) in intent.blocks.iter().enumerate() {
    let FragmentBlock::Paragraph(paragraph) = block else {
      return Err(WriteRejected::StructureViolation("flow cells do not contain objects"));
    };
    if index > 0 {
      ctx.text.insert(pos, "\n").map_err(loro_err)?;
      apply_run_styles(&ctx.text, pos..pos + 1, RunStyles::default())?;
      ctx
        .text
        .mark(
          pos..pos + 1,
          flowstate_document::MARK_PARAGRAPH_STYLE,
          paragraph_style_value(paragraph.style),
        )
        .map_err(loro_err)?;
      mint_paragraph_record(ctx, pos)?;
      pos += 1;
    }
    for run in &paragraph.runs {
      if run.text.is_empty() {
        continue;
      }
      let chars = run.text.chars().count();
      ctx.text.insert(pos, &run.text).map_err(loro_err)?;
      if run.styles != RunStyles::default() {
        apply_run_styles(&ctx.text, pos..pos + chars, run.styles)?;
      }
      pos += chars;
    }
  }
  ctx.refresh_len();
  // Caret lands at the end of the inserted fragment.
  Ok(Some(pos))
}

/// Back-to-front so earlier ranges never shift; unresolvable or reordered
/// matches are skipped, not rejected.
fn replace_matches(ctx: &mut CellTextContext, intent: &ReplaceMatchesIntent) -> Result<(), WriteRejected> {
  if intent.replacement.contains('\n') {
    return Err(WriteRejected::StructureViolation("replacement must not contain structural newlines"));
  }
  let mut resolved: Vec<(usize, usize, Option<RunStyles>)> = Vec::new();
  for candidate in &intent.matches {
    let (Ok(start), Ok(end)) = (ctx.resolve_anchor(&candidate.start), ctx.resolve_anchor(&candidate.end)) else {
      continue;
    };
    if start == 0 || end <= start {
      continue;
    }
    resolved.push((start, end, candidate.styles));
  }
  if resolved.is_empty() {
    return Err(WriteRejected::EmptyIntent);
  }
  resolved.sort_by_key(|(start, _, _)| std::cmp::Reverse(*start));
  let mut floor = usize::MAX;
  let replacement_chars = intent.replacement.chars().count();
  for (start, end, styles) in resolved {
    if end > floor {
      continue; // overlaps a higher (already replaced) match
    }
    ctx.text.delete(start, end - start).map_err(loro_err)?;
    if !intent.replacement.is_empty() {
      ctx
        .text
        .insert(start, &intent.replacement)
        .map_err(loro_err)?;
      if let Some(styles) = styles {
        apply_run_styles(&ctx.text, start..start + replacement_chars, styles)?;
      }
    }
    floor = start;
  }
  ctx.refresh_len();
  Ok(())
}
