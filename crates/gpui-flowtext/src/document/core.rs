use std::{cell::Cell, ops::Range, sync::Arc};

use crop::Rope;
use gpui::{Hsla, Pixels, SharedString, black, px, rgb};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use super::paragraph_text_len;

pub const SOFT_LINE_BREAK: char = '\u{2028}';
pub const SOFT_LINE_BREAK_STR: &str = "\u{2028}";
pub const RICH_TEXT_CLIPBOARD_FORMAT: &str = "gpui-flowtext.rich-text-fragment.v1";

#[must_use]
pub fn rich_text_clipboard_format_is_supported(format: &str) -> bool {
  format == RICH_TEXT_CLIPBOARD_FORMAT
}

// -- Clipboard fragment ---------------------------------------------------

/// Internal clipboard fragment used to round-trip rich text via the system
/// clipboard. The `format` field acts as a magic string so we can distinguish
/// our payloads from anything else stored in the clipboard's metadata slot.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RichClipboardFragment {
  pub format: String,
  #[serde(default)]
  pub paragraphs: Vec<InputParagraph>,
  #[serde(default)]
  pub blocks: Vec<InputBlock>,
  #[serde(default)]
  pub assets: Vec<InputAsset>,
}

// -- DocumentProjection and paragraphs ---------------------------------------------

#[derive(Clone, Debug)]
pub struct DocumentProjection {
  /// Encoded canonical frontier this disposable projection was built from.
  /// Empty for standalone projections that are not backed by a CRDT runtime.
  pub frontier: Vec<u8>,
  pub text: Rope,
  pub paragraphs: ParagraphSeq,
  pub blocks: BlockSeq,
  pub assets: AssetStore,
  pub ids: DocumentIds,
  /// Canonical page/document sections projected from the CRDT.
  pub sections: Arc<Vec<DocumentSection>>,
  /// Disposable heading hierarchy derived from paragraph styles.
  pub outline: Arc<Vec<DocumentOutlineNode>>,
  pub theme: DocumentTheme,
}

// §act-four M3 Slice 4: `document.paragraphs` is a persistent `ParagraphSeq`.
// `paragraphs_mut` hands out `&mut ParagraphSeq` — NOT a `Drop` guard — so its
// borrow of `document` is released by NLL at last use, letting the mutation
// sites interleave a following block edit exactly as they did with the old
// `&mut Vec`. Mutations go through the sequence's copy-on-write methods
// (`set`/`get_mut`/`insert`/`remove`/`iter_mut`/`splice`), applied in place.
#[inline]
pub fn paragraphs_mut(document: &mut DocumentProjection) -> &mut ParagraphSeq {
  &mut document.paragraphs
}

#[hotpath::measure]
pub fn paragraph_blocks_from_paragraphs(paragraphs: &[Paragraph]) -> Vec<Block> {
  paragraphs.iter().cloned().map(Block::Paragraph).collect()
}

thread_local! {
  static DOCUMENT_SECTION_REBUILD_DEFERRAL_DEPTH: Cell<usize> = const { Cell::new(0) };
  static DOCUMENT_SECTION_REBUILD_DEFERRED_DIRTY: Cell<bool> = const { Cell::new(false) };
}

#[derive(Debug)]
pub struct DocumentSectionRebuildDeferral;

impl Drop for DocumentSectionRebuildDeferral {
  fn drop(&mut self) {
    DOCUMENT_SECTION_REBUILD_DEFERRAL_DEPTH.with(|depth| {
      depth.set(depth.get().saturating_sub(1));
    });
  }
}

#[hotpath::measure]
#[must_use]
pub fn defer_document_section_rebuilds() -> DocumentSectionRebuildDeferral {
  DOCUMENT_SECTION_REBUILD_DEFERRAL_DEPTH.with(|depth| {
    if depth.get() == 0 {
      DOCUMENT_SECTION_REBUILD_DEFERRED_DIRTY.with(|dirty| dirty.set(false));
    }
    depth.set(depth.get().saturating_add(1));
  });
  DocumentSectionRebuildDeferral
}

#[hotpath::measure]
#[must_use]
pub fn document_section_rebuilds_deferred() -> bool {
  DOCUMENT_SECTION_REBUILD_DEFERRAL_DEPTH.with(|depth| depth.get() > 0)
}

#[hotpath::measure]
#[must_use]
pub fn deferred_document_section_rebuild_requested() -> bool {
  DOCUMENT_SECTION_REBUILD_DEFERRED_DIRTY.with(Cell::get)
}

thread_local! {
  // §perf-heaven T2 tripwire: counts how many times `block_ix_for_paragraph`
  // fell through past the aligned fast path. The fallthrough is an O(log N)
  // tree rank query since T7.11 (the counter predates that and kept its role):
  // a hot path calling it once per paragraph still shows up here as O(paragraphs)
  // growth, which is why hot loops hoist a single `paragraph_block_rows` pass
  // instead. Regression tests reset it, drive a mass op, and assert it stayed
  // bounded.
  static BLOCK_IX_SCAN_COUNT: Cell<u64> = const { Cell::new(0) };
}

/// Number of fallthrough (non-aligned) lookups performed by
/// [`block_ix_for_paragraph`] since the last reset — O(log N) each since
/// T7.11. A per-paragraph caller on an object-bearing doc drives this to
/// O(paragraphs); the batched [`paragraph_block_rows`] path leaves it at zero.
#[must_use]
pub fn block_ix_scan_count() -> u64 {
  BLOCK_IX_SCAN_COUNT.with(Cell::get)
}

/// Reset the [`block_ix_scan_count`] tripwire (test/measurement harness use).
pub fn reset_block_ix_scan_count() {
  BLOCK_IX_SCAN_COUNT.with(|count| count.set(0));
}

#[hotpath::measure]
#[must_use]
pub fn block_ix_for_paragraph(document: &DocumentProjection, target_paragraph_ix: usize) -> Option<usize> {
  if document.blocks.len() == document.paragraphs.len()
    && document
      .blocks
      .get(target_paragraph_ix)
      .is_some_and(|block| matches!(block, Block::Paragraph(_)))
  {
    return Some(target_paragraph_ix);
  }

  // §perf-heaven T7.11: an O(log N) tree rank-select replaces the former
  // O(blocks) linear scan. The counter still records this object-doc, per-call
  // resolution so the intent-complexity guard keeps proving that mass ops hoist
  // `paragraph_block_rows` once instead of resolving per paragraph (the per-call
  // cost is now O(log N), but calling it O(paragraphs) times is still worse than
  // one O(blocks) pass, so the guard remains meaningful).
  BLOCK_IX_SCAN_COUNT.with(|count| count.set(count.get().saturating_add(1)));
  document.blocks.block_row_for_paragraph_ix(target_paragraph_ix)
}

/// Block row for EVERY paragraph in one pass — the batched sibling of
/// [`block_ix_for_paragraph`]. Callers that need rows for many paragraphs on
/// an object-bearing doc (where the aligned fast path misses) would otherwise
/// pay an O(blocks) scan per paragraph.
#[must_use]
pub fn paragraph_block_rows(document: &DocumentProjection) -> Vec<usize> {
  if document.blocks.len() == document.paragraphs.len() {
    return (0..document.blocks.len()).collect();
  }
  document
    .blocks
    .iter()
    .enumerate()
    .filter_map(|(row, block)| matches!(block, Block::Paragraph(_)).then_some(row))
    .collect()
}

#[hotpath::measure]
#[must_use]
pub fn document_position_for_offset(document: &DocumentProjection, offset: DocumentOffset) -> Option<DocumentPosition> {
  let paragraph = document.paragraphs.get(offset.paragraph)?;
  if offset.byte > paragraph_text_len(paragraph) {
    return None;
  }
  Some(DocumentPosition::Text {
    block_ix: block_ix_for_paragraph(document, offset.paragraph)?,
    byte: offset.byte,
  })
}

#[hotpath::measure]
#[must_use]
pub fn document_offset_for_position(document: &DocumentProjection, position: &DocumentPosition) -> Option<DocumentOffset> {
  match position {
    DocumentPosition::Text { block_ix, byte } => {
      if document.blocks.len() == document.paragraphs.len()
        && let Some(Block::Paragraph(paragraph)) = document.blocks.get(*block_ix)
      {
        if *byte <= paragraph_text_len(paragraph) {
          return Some(DocumentOffset {
            paragraph: *block_ix,
            byte: *byte,
          });
        }
        return None;
      }

      // §perf-heaven T7.12: O(log N) rank query replaces the O(blocks) scan.
      // `paragraph_ix_for_block_row` returns `Some` only when the row is a
      // paragraph (object rows and out-of-range → `None`), so the following
      // `get` is guaranteed to hand back that paragraph for the byte-bound check.
      let paragraph_ix = document.blocks.paragraph_ix_for_block_row(*block_ix)?;
      let Some(Block::Paragraph(paragraph)) = document.blocks.get(*block_ix) else {
        return None;
      };
      (*byte <= paragraph_text_len(paragraph)).then_some(DocumentOffset {
        paragraph: paragraph_ix,
        byte: *byte,
      })
    },
    DocumentPosition::Object { .. } | DocumentPosition::TableCell { .. } => None,
  }
}

#[hotpath::measure]
pub fn update_paragraph_block(document: &mut DocumentProjection, paragraph_ix: usize) {
  if let Some(block_ix) = block_ix_for_paragraph(document, paragraph_ix) {
    update_paragraph_block_at(document, paragraph_ix, block_ix);
  }
}

/// Row-aware variant of [`update_paragraph_block`] for callers that have
/// ALREADY resolved the paragraph's block row (the projection patch-apply
/// carries an accurate `row_hint`). Skips the O(blocks) `block_ix_for_paragraph`
/// re-scan — the §perf-heaven T2 quadratic behind mass restyle: one scan per
/// patch over an object-bearing doc was O(paragraphs²).
pub fn update_paragraph_block_at(document: &mut DocumentProjection, paragraph_ix: usize, block_ix: usize) {
  let Some(paragraph) = document.paragraphs.get(paragraph_ix).cloned() else {
    return;
  };
  if matches!(document.blocks.get(block_ix), Some(Block::Paragraph(_))) {
    document.blocks.set(block_ix, Block::Paragraph(paragraph));
  }
}

#[hotpath::measure]
pub fn replace_paragraph_blocks(document: &mut DocumentProjection, start_paragraph: usize, old_count: usize, replacements: &[Paragraph]) {
  // Fast path: a single in-place paragraph update in a paragraph-only-aligned
  // document. Block ids and order are unchanged, so we replace just that one
  // block instead of rebuilding the whole block vector.
  if old_count == 1
    && replacements.len() == 1
    && document.blocks.len() == document.paragraphs.len()
    && matches!(document.blocks.get(start_paragraph), Some(Block::Paragraph(_)))
  {
    document.blocks.set(start_paragraph, Block::Paragraph(replacements[0].clone()));
    reconcile_document_ids(document);
    rebuild_document_sections(document);
    return;
  }

  let block_start = block_ix_for_paragraph(document, start_paragraph).unwrap_or(document.blocks.len());
  let mut paragraph_ix = 0;
  let mut output = Vec::with_capacity(document.blocks.len() + replacements.len());
  let mut inserted_replacements = false;

  for block in document.blocks.iter() {
    match block {
      Block::Paragraph(_) if paragraph_ix >= start_paragraph && paragraph_ix < start_paragraph + old_count => {
        if !inserted_replacements {
          output.extend(replacements.iter().cloned().map(Block::Paragraph));
          inserted_replacements = true;
        }
        paragraph_ix += 1;
      },
      Block::Paragraph(paragraph) => {
        if !inserted_replacements && paragraph_ix >= start_paragraph {
          output.extend(replacements.iter().cloned().map(Block::Paragraph));
          inserted_replacements = true;
        }
        output.push(Block::Paragraph(paragraph.clone()));
        paragraph_ix += 1;
      },
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => output.push(block.clone()),
    }
  }

  if !inserted_replacements {
    output.extend(replacements.iter().cloned().map(Block::Paragraph));
  }
  if output.is_empty()
    && let Some(paragraph) = document.paragraphs.first()
  {
    output.push(Block::Paragraph(paragraph.clone()));
  }

  document.blocks = BlockSeq::from_vec(output);
  let block_end = (block_start + old_count).min(document.ids.block_ids.len());
  let replacement_ids = if old_count == replacements.len() {
    document.ids.block_ids[block_start..block_end].to_vec()
  } else {
    let mut ids = Vec::with_capacity(replacements.len());
    if let Some(first) = document.ids.block_ids.get(block_start).copied() {
      ids.push(first);
    }
    while ids.len() < replacements.len() {
      ids.push(new_block_id());
    }
    ids
  };
  std::sync::Arc::make_mut(&mut document.ids.block_ids).splice(block_start..block_end, replacement_ids);
  reconcile_document_ids(document);
  rebuild_document_sections(document);
}

#[hotpath::measure]
#[must_use]
pub fn new_document_id() -> u128 {
  uuid::Uuid::new_v4().as_u128()
}

#[hotpath::measure]
#[must_use]
pub fn new_paragraph_id() -> ParagraphId {
  ParagraphId(uuid::Uuid::new_v4().as_u128())
}

#[hotpath::measure]
#[must_use]
pub fn new_block_id() -> BlockId {
  BlockId(uuid::Uuid::new_v4().as_u128())
}

#[hotpath::measure]
#[must_use]
pub fn new_section_id() -> SectionId {
  SectionId(uuid::Uuid::new_v4().as_u128())
}

#[hotpath::measure]
#[must_use]
pub fn document_ids_for_shape(paragraph_count: usize, block_count: usize) -> DocumentIds {
  DocumentIds {
    document_id: new_document_id(),
    paragraph_ids: std::sync::Arc::new(std::iter::repeat_with(new_paragraph_id).take(paragraph_count).collect()),
    block_ids: std::sync::Arc::new(std::iter::repeat_with(new_block_id).take(block_count).collect()),
  }
}

#[hotpath::measure]
pub fn reconcile_document_ids(document: &mut DocumentProjection) {
  if document.ids.document_id == 0 {
    document.ids.document_id = new_document_id();
  }

  if document.ids.paragraph_ids.len() != document.paragraphs.len() {
    let ids = std::sync::Arc::make_mut(&mut document.ids.paragraph_ids);
    while ids.len() < document.paragraphs.len() {
      ids.push(new_paragraph_id());
    }
    ids.truncate(document.paragraphs.len());
  }

  if document.ids.block_ids.len() != document.blocks.len() {
    let ids = std::sync::Arc::make_mut(&mut document.ids.block_ids);
    while ids.len() < document.blocks.len() {
      ids.push(new_block_id());
    }
    ids.truncate(document.blocks.len());
  }
}

#[hotpath::measure]
#[must_use]
pub fn paragraph_index_for_id(document: &DocumentProjection, id: ParagraphId) -> Option<usize> {
  document
    .ids
    .paragraph_ids
    .iter()
    .position(|candidate| *candidate == id)
}

// §perf: not hotpath-measured — O(1) id lookups whose measurement hooks cost
// far more than the lookup and polluted profiles at millions of calls.
#[inline]
#[must_use]
pub fn paragraph_id_at(document: &DocumentProjection, paragraph_ix: usize) -> Option<ParagraphId> {
  document.ids.paragraph_ids.get(paragraph_ix).copied()
}

#[inline]
#[must_use]
pub fn block_id_at(document: &DocumentProjection, block_ix: usize) -> Option<BlockId> {
  document.ids.block_ids.get(block_ix).copied()
}

#[hotpath::measure]
pub fn insert_paragraph_id(document: &mut DocumentProjection, paragraph_ix: usize) -> ParagraphId {
  let id = new_paragraph_id();
  let at = paragraph_ix.min(document.ids.paragraph_ids.len());
  std::sync::Arc::make_mut(&mut document.ids.paragraph_ids).insert(at, id);
  id
}

#[hotpath::measure]
pub fn insert_block_id(document: &mut DocumentProjection, block_ix: usize) -> BlockId {
  let id = new_block_id();
  let at = block_ix.min(document.ids.block_ids.len());
  std::sync::Arc::make_mut(&mut document.ids.block_ids).insert(at, id);
  id
}

#[hotpath::measure]
pub fn remove_paragraph_ids(document: &mut DocumentProjection, range: Range<usize>) {
  let start = range.start.min(document.ids.paragraph_ids.len());
  let end = range.end.min(document.ids.paragraph_ids.len());
  if start < end {
    std::sync::Arc::make_mut(&mut document.ids.paragraph_ids).drain(start..end);
  }
}

#[hotpath::measure]
pub fn remove_block_ids(document: &mut DocumentProjection, range: Range<usize>) {
  let start = range.start.min(document.ids.block_ids.len());
  let end = range.end.min(document.ids.block_ids.len());
  if start < end {
    std::sync::Arc::make_mut(&mut document.ids.block_ids).drain(start..end);
  }
}

#[hotpath::measure]
pub fn rebuild_document_outline(document: &mut DocumentProjection) {
  if document_section_rebuilds_deferred() {
    DOCUMENT_SECTION_REBUILD_DEFERRED_DIRTY.with(|dirty| dirty.set(true));
    return;
  }
  rebuild_document_outline_now(document);
}

#[hotpath::measure]
pub fn rebuild_document_outline_now(document: &mut DocumentProjection) {
  reconcile_document_ids(document);
  document.outline = Arc::new(document_outline(document));
}

/// Compatibility name retained for existing edit primitives. This rebuilds
/// only the derived heading outline; canonical `document.sections` are never
/// modified.
#[hotpath::measure]
pub fn rebuild_document_sections(document: &mut DocumentProjection) {
  rebuild_document_outline(document);
}

#[hotpath::measure]
pub fn rebuild_document_sections_now(document: &mut DocumentProjection) {
  DOCUMENT_SECTION_REBUILD_DEFERRED_DIRTY.with(|dirty| dirty.set(false));
  rebuild_document_outline_now(document);
}

/// Computes the disposable heading hierarchy purely from paragraph styles,
/// order, and stable paragraph ids.
#[hotpath::measure]
#[must_use]
pub fn document_outline(document: &DocumentProjection) -> Vec<DocumentOutlineNode> {
  let mut outline: Vec<DocumentOutlineNode> = Vec::new();
  // §perf: the stack holds each open node's index into `outline` rather than its
  // SectionId. `outline` is append-only (nodes are never removed), so the index
  // stays valid, and closing a section becomes an O(1) index instead of the former
  // O(outline) linear `find` per pop — turning this from O(headings²) to O(headings).
  let mut stack: Vec<(usize, usize)> = Vec::new();

  // B-S6: the walk is BLOCK-ordered so cell-resident headings interleave at
  // their table's document position (structure descends).
  let mut paragraph_ix = 0usize;
  for (block_ix, block) in document.blocks.iter().enumerate() {
    match block {
      Block::Paragraph(paragraph) => {
        let current_ix = paragraph_ix;
        paragraph_ix += 1;
        let Some((level, kind)) = section_level_and_kind(document, paragraph.style) else {
          continue;
        };
        while stack
          .last()
          .is_some_and(|(ancestor_level, _)| *ancestor_level >= level)
        {
          if let Some((_, node_index)) = stack.pop() {
            outline[node_index].end_paragraph_exclusive = paragraph_id_at(document, current_ix);
          }
        }
        let paragraph_id = paragraph_id_at(document, current_ix).unwrap_or_else(new_paragraph_id);
        let parent_id = stack.last().map(|(_, node_index)| outline[*node_index].id);
        let id = section_id_for_heading(paragraph_id, kind);
        let node_index = outline.len();
        outline.push(DocumentOutlineNode {
          id,
          parent_id,
          kind,
          heading_paragraph: paragraph_id,
          start_paragraph: paragraph_id,
          end_paragraph_exclusive: None,
          cell_address: None,
        });
        stack.push((level, node_index));
      },
      Block::Table(table) => {
        // Cell headings are LEAF nodes under the current section — a Pocket
        // in a cell roots a real card in the outline, but never parents
        // body content (the stack is untouched).
        let table_block = document
          .ids
          .block_ids
          .get(block_ix)
          .map_or(0, |block_id| block_id.0);
        let parent_id = stack.last().map(|(_, node_index)| outline[*node_index].id);
        for (row_ix, row) in table.rows.iter().enumerate() {
          for (cell_ix, cell) in row.cells.iter().enumerate() {
            for (cell_paragraph_ix, cell_block) in cell.blocks.iter().enumerate() {
              let TableCellBlock::Paragraph(cell_paragraph) = cell_block else {
                continue;
              };
              let Some((_, kind)) = section_level_and_kind(document, cell_paragraph.paragraph.style) else {
                continue;
              };
              // Deterministic synthetic identity from the address — stable
              // across rebuilds, never colliding with body paragraph ids
              // (the high tag bit marks the cell namespace).
              let synthetic = ParagraphId(
                (1_u128 << 127)
                  ^ table_block
                  ^ ((row_ix as u128) << 96)
                  ^ ((cell_ix as u128) << 64)
                  ^ ((cell_paragraph_ix as u128) << 32),
              );
              outline.push(DocumentOutlineNode {
                id: section_id_for_heading(synthetic, kind),
                parent_id,
                kind,
                heading_paragraph: synthetic,
                start_paragraph: synthetic,
                end_paragraph_exclusive: Some(synthetic),
                cell_address: Some(OutlineCellAddress {
                  table_block,
                  row_ix,
                  cell_ix,
                  cell_paragraph_ix,
                }),
              });
            }
          }
        }
      },
      Block::Image(_) | Block::Equation(_) => {},
    }
  }

  outline
}

/// Whether the paragraph at `paragraph_ix` carries a heading (section) style.
#[hotpath::measure]
#[must_use]
pub fn paragraph_is_heading(document: &DocumentProjection, paragraph_ix: usize) -> bool {
  document
    .paragraphs
    .get(paragraph_ix)
    .is_some_and(|paragraph| section_level_and_kind(document, paragraph.style).is_some())
}

/// Whether any paragraph in `range` (clamped to the paragraph count) is a heading.
/// Lets callers decide whether a content edit can skip [`rebuild_document_outline`].
#[hotpath::measure]
#[must_use]
pub fn range_contains_heading(document: &DocumentProjection, range: Range<usize>) -> bool {
  let end = range.end.min(document.paragraphs.len());
  (range.start..end).any(|paragraph_ix| paragraph_is_heading(document, paragraph_ix))
}

// §perf: not hotpath-measured — a map probe per paragraph per outline pass;
// the hooks dominated it at scale.
#[inline]
fn section_level_and_kind(document: &DocumentProjection, style: ParagraphStyle) -> Option<(usize, SectionKind)> {
  match style {
    ParagraphStyle::Normal => None,
    ParagraphStyle::Custom(slot) => {
      let style = document.theme.custom_paragraph_styles.get(&(slot & 0x7f))?;
      Some((
        usize::from(style.section_level?),
        SectionKind::Custom(style.section_kind.unwrap_or(slot & 0x7f)),
      ))
    },
  }
}

const fn section_id_for_heading(paragraph_id: ParagraphId, kind: SectionKind) -> SectionId {
  let kind_slot = match kind {
    SectionKind::Custom(slot) => 1_u128 + slot as u128,
  };
  SectionId(paragraph_id.0 ^ (kind_slot << 120))
}

// §act-four M3 Slice 2b: the Fenwick `ParagraphOffsetIndex` has been removed.
// Paragraph offsets are now derived from the block tree's paragraph-space
// monoid (`BlockSeq::paragraph_start`), which subsumes it at `O(log N)` with no
// separate structure to keep in sync or snapshot per version.
