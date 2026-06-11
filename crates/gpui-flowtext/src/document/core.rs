use std::{ops::Range, sync::Arc};

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

// -- Document and paragraphs ---------------------------------------------

#[derive(Clone, Debug)]
pub struct Document {
  pub text: Rope,
  pub paragraphs: Arc<Vec<Paragraph>>,
  pub blocks: Arc<Vec<Block>>,
  pub assets: AssetStore,
  pub ids: DocumentIds,
  pub sections: Arc<Vec<DocumentSection>>,
  // Auxiliary Fenwick-tree index over per-paragraph byte widths. Kept in sync
  // with `paragraphs` by the edit helpers in `edit_ops`. Not part of the
  // public API.
  pub offset_index: ParagraphOffsetIndex,
  pub theme: DocumentTheme,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DocumentInvariantError {
  ParagraphIdCount { paragraph_ids: usize, paragraphs: usize },
  BlockIdCount { block_ids: usize, blocks: usize },
  ParagraphBlockCount { paragraph_blocks: usize, paragraphs: usize },
  OffsetWidthCount { widths: usize, paragraphs: usize },
  OffsetTreeCount { tree: usize, expected: usize },
  ParagraphRangeStart { paragraph: usize, start: usize, expected: usize },
  ParagraphSeparator { paragraph: usize, offset: usize },
  ParagraphRangeInvalid { paragraph: usize, start: usize, end: usize, text_len: usize },
  ParagraphRangeLen { paragraph: usize, range_len: usize, run_len: usize },
  ParagraphRunZero { paragraph: usize, run: usize },
  ParagraphRunBoundary { paragraph: usize, offset: usize },
  OffsetWidthMismatch { paragraph: usize, width: Option<usize>, expected: usize },
  TextEndMismatch { paragraph_text_end: usize, text_len: usize },
  SectionStartMissing { paragraph: ParagraphId },
  SectionHeadingMissing { paragraph: ParagraphId },
  SectionEndMissing { paragraph: ParagraphId },
}

impl std::fmt::Display for DocumentInvariantError {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    match self {
      Self::ParagraphIdCount { paragraph_ids, paragraphs } => write!(f, "paragraph_id_len={paragraph_ids} paragraph_len={paragraphs}"),
      Self::BlockIdCount { block_ids, blocks } => write!(f, "block_id_len={block_ids} block_len={blocks}"),
      Self::ParagraphBlockCount { paragraph_blocks, paragraphs } => write!(f, "paragraph_block_count={paragraph_blocks} paragraph_len={paragraphs}"),
      Self::OffsetWidthCount { widths, paragraphs } => write!(f, "offset_width_len={widths} paragraph_len={paragraphs}"),
      Self::OffsetTreeCount { tree, expected } => write!(f, "offset_tree_len={tree} expected={expected}"),
      Self::ParagraphRangeStart { paragraph, start, expected } => write!(f, "paragraph={paragraph} range_start={start} expected={expected}"),
      Self::ParagraphSeparator { paragraph, offset } => write!(f, "paragraph={paragraph} missing_newline_separator_at={offset}"),
      Self::ParagraphRangeInvalid { paragraph, start, end, text_len } => write!(f, "paragraph={paragraph} invalid_range={start}..{end} text_len={text_len}"),
      Self::ParagraphRangeLen { paragraph, range_len, run_len } => write!(f, "paragraph={paragraph} run_len={run_len} byte_range_len={range_len}"),
      Self::ParagraphRunZero { paragraph, run } => write!(f, "paragraph={paragraph} zero_run={run}"),
      Self::ParagraphRunBoundary { paragraph, offset } => write!(f, "paragraph={paragraph} run_boundary_not_utf8={offset}"),
      Self::OffsetWidthMismatch { paragraph, width, expected } => write!(f, "paragraph={paragraph} offset_width={width:?} paragraph_width={expected}"),
      Self::TextEndMismatch { paragraph_text_end, text_len } => write!(f, "paragraph_text_end={paragraph_text_end} text_len={text_len}"),
      Self::SectionStartMissing { paragraph } => write!(f, "section_start_missing={paragraph:?}"),
      Self::SectionHeadingMissing { paragraph } => write!(f, "section_heading_missing={paragraph:?}"),
      Self::SectionEndMissing { paragraph } => write!(f, "section_end_missing={paragraph:?}"),
    }
  }
}

impl std::error::Error for DocumentInvariantError {}

#[hotpath::measure]
pub fn validate_document_invariants(document: &Document) -> Result<(), DocumentInvariantError> {
  if document.ids.paragraph_ids.len() != document.paragraphs.len() {
    return Err(DocumentInvariantError::ParagraphIdCount {
      paragraph_ids: document.ids.paragraph_ids.len(),
      paragraphs: document.paragraphs.len(),
    });
  }
  if document.ids.block_ids.len() != document.blocks.len() {
    return Err(DocumentInvariantError::BlockIdCount {
      block_ids: document.ids.block_ids.len(),
      blocks: document.blocks.len(),
    });
  }
  let paragraph_blocks = document.blocks.iter().filter(|block| matches!(block, Block::Paragraph(_))).count();
  if paragraph_blocks != document.paragraphs.len() {
    return Err(DocumentInvariantError::ParagraphBlockCount {
      paragraph_blocks,
      paragraphs: document.paragraphs.len(),
    });
  }
  if document.offset_index.widths.len() != document.paragraphs.len() {
    return Err(DocumentInvariantError::OffsetWidthCount {
      widths: document.offset_index.widths.len(),
      paragraphs: document.paragraphs.len(),
    });
  }
  if document.offset_index.tree.len() != document.offset_index.widths.len() + 1 {
    return Err(DocumentInvariantError::OffsetTreeCount {
      tree: document.offset_index.tree.len(),
      expected: document.offset_index.widths.len() + 1,
    });
  }

  let full_text = document.text.to_string();
  let text_len = full_text.len();
  let mut previous_end = 0usize;
  for (ix, paragraph) in document.paragraphs.iter().enumerate() {
    let expected_start = previous_end + usize::from(ix > 0);
    if paragraph.byte_range.start != expected_start {
      return Err(DocumentInvariantError::ParagraphRangeStart {
        paragraph: ix,
        start: paragraph.byte_range.start,
        expected: expected_start,
      });
    }
    if ix > 0 && full_text.as_bytes().get(previous_end) != Some(&b'\n') {
      return Err(DocumentInvariantError::ParagraphSeparator {
        paragraph: ix,
        offset: previous_end,
      });
    }
    if paragraph.byte_range.end < paragraph.byte_range.start || paragraph.byte_range.end > text_len {
      return Err(DocumentInvariantError::ParagraphRangeInvalid {
        paragraph: ix,
        start: paragraph.byte_range.start,
        end: paragraph.byte_range.end,
        text_len,
      });
    }
    let run_len = paragraph.runs.iter().map(|run| run.len).sum::<usize>();
    let range_len = paragraph.byte_range.end - paragraph.byte_range.start;
    if run_len != range_len {
      return Err(DocumentInvariantError::ParagraphRangeLen { paragraph: ix, range_len, run_len });
    }
    let mut run_offset = paragraph.byte_range.start;
    for (run_ix, run) in paragraph.runs.iter().enumerate() {
      if run.len == 0 {
        return Err(DocumentInvariantError::ParagraphRunZero { paragraph: ix, run: run_ix });
      }
      if !full_text.is_char_boundary(run_offset) {
        return Err(DocumentInvariantError::ParagraphRunBoundary { paragraph: ix, offset: run_offset });
      }
      run_offset += run.len;
    }
    if !full_text.is_char_boundary(run_offset) {
      return Err(DocumentInvariantError::ParagraphRunBoundary { paragraph: ix, offset: run_offset });
    }
    if document.offset_index.widths.get(ix).copied() != Some(paragraph_width(document.paragraphs.as_slice(), ix).unwrap_or(range_len)) {
      return Err(DocumentInvariantError::OffsetWidthMismatch {
        paragraph: ix,
        width: document.offset_index.widths.get(ix).copied(),
        expected: paragraph_width(document.paragraphs.as_slice(), ix).unwrap_or(range_len),
      });
    }
    previous_end = paragraph.byte_range.end;
  }
  if previous_end != text_len {
    return Err(DocumentInvariantError::TextEndMismatch { paragraph_text_end: previous_end, text_len });
  }

  for section in document.sections.iter() {
    if !document.ids.paragraph_ids.contains(&section.start_paragraph) {
      return Err(DocumentInvariantError::SectionStartMissing { paragraph: section.start_paragraph });
    }
    if let Some(paragraph) = section.heading_paragraph
      && !document.ids.paragraph_ids.contains(&paragraph)
    {
      return Err(DocumentInvariantError::SectionHeadingMissing { paragraph });
    }
    if let Some(paragraph) = section.end_paragraph_exclusive
      && !document.ids.paragraph_ids.contains(&paragraph)
    {
      return Err(DocumentInvariantError::SectionEndMissing { paragraph });
    }
  }
  Ok(())
}

#[hotpath::measure]
pub fn paragraphs_mut(document: &mut Document) -> &mut Vec<Paragraph> {
  Arc::make_mut(&mut document.paragraphs)
}

#[hotpath::measure]
pub fn paragraph_blocks_from_paragraphs(paragraphs: &[Paragraph]) -> Vec<Block> {
  paragraphs.iter().cloned().map(Block::Paragraph).collect()
}

#[hotpath::measure]
#[must_use]
pub fn block_ix_for_paragraph(document: &Document, target_paragraph_ix: usize) -> Option<usize> {
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

#[hotpath::measure]
#[must_use]
pub fn document_position_for_offset(document: &Document, offset: DocumentOffset) -> Option<DocumentPosition> {
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
pub fn document_offset_for_position(document: &Document, position: &DocumentPosition) -> Option<DocumentOffset> {
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
pub fn update_paragraph_block(document: &mut Document, paragraph_ix: usize) {
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
pub fn replace_paragraph_blocks(document: &mut Document, start_paragraph: usize, old_count: usize, replacements: &[Paragraph]) {
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
    rich_block_ids: FxHashMap::default(),
  }
}

#[hotpath::measure]
pub fn reconcile_document_ids(document: &mut Document) {
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
pub fn paragraph_index_for_id(document: &Document, id: ParagraphId) -> Option<usize> {
  document
    .ids
    .paragraph_ids
    .iter()
    .position(|candidate| *candidate == id)
}

#[hotpath::measure]
#[must_use]
pub fn paragraph_id_at(document: &Document, paragraph_ix: usize) -> Option<ParagraphId> {
  document.ids.paragraph_ids.get(paragraph_ix).copied()
}

#[hotpath::measure]
#[must_use]
pub fn block_id_at(document: &Document, block_ix: usize) -> Option<BlockId> {
  document.ids.block_ids.get(block_ix).copied()
}

#[hotpath::measure]
pub fn insert_paragraph_id(document: &mut Document, paragraph_ix: usize) -> ParagraphId {
  let id = new_paragraph_id();
  document
    .ids
    .paragraph_ids
    .insert(paragraph_ix.min(document.ids.paragraph_ids.len()), id);
  id
}

#[hotpath::measure]
pub fn insert_block_id(document: &mut Document, block_ix: usize) -> BlockId {
  let id = new_block_id();
  document
    .ids
    .block_ids
    .insert(block_ix.min(document.ids.block_ids.len()), id);
  id
}

#[hotpath::measure]
pub fn remove_paragraph_ids(document: &mut Document, range: Range<usize>) {
  let start = range.start.min(document.ids.paragraph_ids.len());
  let end = range.end.min(document.ids.paragraph_ids.len());
  if start < end {
    document.ids.paragraph_ids.drain(start..end);
  }
}

#[hotpath::measure]
pub fn remove_block_ids(document: &mut Document, range: Range<usize>) {
  let start = range.start.min(document.ids.block_ids.len());
  let end = range.end.min(document.ids.block_ids.len());
  if start < end {
    document.ids.block_ids.drain(start..end);
  }
}

#[hotpath::measure]
pub fn rebuild_document_sections(document: &mut Document) {
  reconcile_document_ids(document);
  let mut sections: Vec<DocumentSection> = Vec::new();
  let mut stack: Vec<(usize, SectionId)> = Vec::new();

  for (paragraph_ix, paragraph) in document.paragraphs.iter().enumerate() {
    let Some((level, kind)) = section_level_and_kind(document, paragraph.style) else {
      continue;
    };
    while stack
      .last()
      .is_some_and(|(ancestor_level, _)| *ancestor_level >= level)
    {
      if let Some((_, section_id)) = stack.pop() {
        for section in sections
          .iter_mut()
          .filter(|section| section.id == section_id)
        {
          section.end_paragraph_exclusive = paragraph_id_at(document, paragraph_ix);
        }
      }
    }
    let paragraph_id = paragraph_id_at(document, paragraph_ix).unwrap_or_else(new_paragraph_id);
    let parent_id = stack.last().map(|(_, id)| *id);
    let id = section_id_for_heading(paragraph_id, kind);
    sections.push(DocumentSection {
      id,
      parent_id,
      kind,
      heading_paragraph: Some(paragraph_id),
      start_paragraph: paragraph_id,
      end_paragraph_exclusive: None,
    });
    stack.push((level, id));
  }

  for (_, section_id) in stack {
    if let Some(section) = sections.iter_mut().find(|section| section.id == section_id) {
      section.end_paragraph_exclusive = None;
    }
  }
  document.sections = Arc::new(sections);
}

#[hotpath::measure]
fn section_level_and_kind(document: &Document, style: ParagraphStyle) -> Option<(usize, SectionKind)> {
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
