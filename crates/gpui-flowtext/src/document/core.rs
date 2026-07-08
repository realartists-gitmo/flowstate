use std::{cell::Cell, ops::Range, sync::Arc};

use crop::Rope;
use gpui::{Hsla, Pixels, SharedString, black, px, rgb};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

// `paragraph_widths` and `paragraph_width` are free helpers that still live in
// the parent module. `ParagraphOffsetIndex`'s methods invoke them.
use super::{paragraph_text_len, paragraph_width, paragraph_widths};

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
  pub paragraphs: Arc<Vec<Paragraph>>,
  pub blocks: Arc<Vec<Block>>,
  pub assets: AssetStore,
  pub ids: DocumentIds,
  /// Canonical page/document sections projected from the CRDT.
  pub sections: Arc<Vec<DocumentSection>>,
  /// Disposable heading hierarchy derived from paragraph styles.
  pub outline: Arc<Vec<DocumentOutlineNode>>,
  // Auxiliary Fenwick-tree index over per-paragraph byte widths. Kept in sync
  // with `paragraphs` by the edit helpers in `edit_ops`. Not part of the
  // public API.
  pub offset_index: ParagraphOffsetIndex,
  pub theme: DocumentTheme,
}

// §perf: not hotpath-measured — usually an Arc uniqueness check; the rare
// shared-Arc deep clone shows up in its callers' numbers instead.
#[inline]
pub fn paragraphs_mut(document: &mut DocumentProjection) -> &mut Vec<Paragraph> {
  Arc::make_mut(&mut document.paragraphs)
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

      let mut paragraph_ix = 0_usize;
      for (ix, block) in document.blocks.iter().enumerate() {
        match block {
          Block::Paragraph(paragraph) => {
            if ix == *block_ix {
              if *byte <= paragraph_text_len(paragraph) {
                return Some(DocumentOffset {
                  paragraph: paragraph_ix,
                  byte: *byte,
                });
              }
              return None;
            }
            paragraph_ix += 1;
          },
          Block::Image(_) | Block::Equation(_) | Block::Table(_) => {
            if ix == *block_ix {
              return None;
            }
          },
        }
      }
      None
    },
    DocumentPosition::Object { .. } | DocumentPosition::TableCell { .. } => None,
  }
}

#[hotpath::measure]
pub fn update_paragraph_block(document: &mut DocumentProjection, paragraph_ix: usize) {
  let Some(paragraph) = document.paragraphs.get(paragraph_ix).cloned() else {
    return;
  };
  if let Some(block_ix) = block_ix_for_paragraph(document, paragraph_ix)
    && let Some(block) = Arc::make_mut(&mut document.blocks).get_mut(block_ix)
  {
    *block = Block::Paragraph(paragraph);
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
    if let Some(block) = Arc::make_mut(&mut document.blocks).get_mut(start_paragraph) {
      *block = Block::Paragraph(replacements[0].clone());
    }
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

  document.blocks = Arc::new(output);
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
  document
    .ids
    .block_ids
    .splice(block_start..block_end, replacement_ids);
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
    paragraph_ids: std::iter::repeat_with(new_paragraph_id)
      .take(paragraph_count)
      .collect(),
    block_ids: std::iter::repeat_with(new_block_id)
      .take(block_count)
      .collect(),
  }
}

#[hotpath::measure]
pub fn reconcile_document_ids(document: &mut DocumentProjection) {
  if document.ids.document_id == 0 {
    document.ids.document_id = new_document_id();
  }

  while document.ids.paragraph_ids.len() < document.paragraphs.len() {
    document.ids.paragraph_ids.push(new_paragraph_id());
  }
  document
    .ids
    .paragraph_ids
    .truncate(document.paragraphs.len());

  while document.ids.block_ids.len() < document.blocks.len() {
    document.ids.block_ids.push(new_block_id());
  }
  document.ids.block_ids.truncate(document.blocks.len());
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
  document
    .ids
    .paragraph_ids
    .insert(paragraph_ix.min(document.ids.paragraph_ids.len()), id);
  id
}

#[hotpath::measure]
pub fn insert_block_id(document: &mut DocumentProjection, block_ix: usize) -> BlockId {
  let id = new_block_id();
  document
    .ids
    .block_ids
    .insert(block_ix.min(document.ids.block_ids.len()), id);
  id
}

#[hotpath::measure]
pub fn remove_paragraph_ids(document: &mut DocumentProjection, range: Range<usize>) {
  let start = range.start.min(document.ids.paragraph_ids.len());
  let end = range.end.min(document.ids.paragraph_ids.len());
  if start < end {
    document.ids.paragraph_ids.drain(start..end);
  }
}

#[hotpath::measure]
pub fn remove_block_ids(document: &mut DocumentProjection, range: Range<usize>) {
  let start = range.start.min(document.ids.block_ids.len());
  let end = range.end.min(document.ids.block_ids.len());
  if start < end {
    document.ids.block_ids.drain(start..end);
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

  for (paragraph_ix, paragraph) in document.paragraphs.iter().enumerate() {
    let Some((level, kind)) = section_level_and_kind(document, paragraph.style) else {
      continue;
    };
    while stack
      .last()
      .is_some_and(|(ancestor_level, _)| *ancestor_level >= level)
    {
      if let Some((_, node_index)) = stack.pop() {
        outline[node_index].end_paragraph_exclusive = paragraph_id_at(document, paragraph_ix);
      }
    }
    let paragraph_id = paragraph_id_at(document, paragraph_ix).unwrap_or_else(new_paragraph_id);
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
    });
    stack.push((level, node_index));
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

/// Fenwick-tree (binary indexed tree) over the byte widths of each paragraph,
/// plus the raw widths. Lets us compute the absolute byte offset of any
/// paragraph in O(log N) and update it incrementally as the document is
/// edited.
#[derive(Clone, Debug)]
pub struct ParagraphOffsetIndex {
  pub widths: Vec<usize>,
  pub tree: Vec<usize>,
}

#[hotpath::measure_all]
impl ParagraphOffsetIndex {
  #[must_use]
  pub fn new(paragraphs: &[Paragraph]) -> Self {
    let mut index = Self {
      widths: paragraph_widths(paragraphs),
      tree: vec![0; paragraphs.len() + 1],
    };
    for ix in 0..index.widths.len() {
      index.add(ix, index.widths[ix] as isize);
    }
    index
  }

  pub fn rebuild(&mut self, paragraphs: &[Paragraph]) {
    *self = Self::new(paragraphs);
  }

  #[must_use]
  pub fn paragraph_start(&self, paragraph_ix: usize) -> usize {
    self.prefix_sum(paragraph_ix)
  }

  pub fn update_paragraph_width(&mut self, paragraph_ix: usize, paragraphs: &[Paragraph]) {
    if paragraph_ix >= self.widths.len() || self.tree.len() != self.widths.len() + 1 {
      self.rebuild(paragraphs);
      return;
    }
    let Some(width) = paragraph_width(paragraphs, paragraph_ix) else {
      return;
    };
    let old_width = self.widths[paragraph_ix];
    if old_width == width {
      return;
    }
    self.widths[paragraph_ix] = width;
    self.add(paragraph_ix, width as isize - old_width as isize);
  }

  fn add(&mut self, paragraph_ix: usize, delta: isize) {
    if delta == 0 {
      return;
    }
    let mut ix = paragraph_ix + 1;
    while ix < self.tree.len() {
      self.tree[ix] = self.tree[ix].saturating_add_signed(delta);
      ix += ix & (!ix + 1);
    }
  }

  fn prefix_sum(&self, paragraph_count: usize) -> usize {
    let mut ix = paragraph_count.min(self.widths.len());
    let mut sum = 0;
    while ix > 0 {
      sum += self.tree[ix];
      ix &= ix - 1;
    }
    sum
  }
}
