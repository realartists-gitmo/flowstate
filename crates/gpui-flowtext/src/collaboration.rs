// Canonical edit ops are the editor boundary; flowstate-collab persists the resulting durable source, and sync transports it.
use std::ops::Range;

use serde::{Deserialize, Serialize};

use super::{Block, BlockId, Document, DocumentSpan, HighlightStyle, ParagraphId, ParagraphStyle, RunSemanticStyle, RunStyles, new_block_id, new_paragraph_id};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct TableCellId(pub u128);

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum Db8GranularValue {
  Bool(bool),
  I64(i64),
  String(String),
}

#[must_use]
fn granular_record_id_u128(id: u128) -> String {
  format!("{id:032x}")
}

const DB8_MARK_SEMANTIC: &str = "semantic";
const DB8_MARK_DIRECT_UNDERLINE: &str = "direct_underline";
const DB8_MARK_STRIKETHROUGH: &str = "strikethrough";
const DB8_MARK_HIGHLIGHT: &str = "highlight";

#[derive(Clone, Copy, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct StableDocumentOffset {
  pub paragraph: ParagraphId,
  pub byte: usize,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct StableEditorSelection {
  pub anchor: StableDocumentOffset,
  pub head: StableDocumentOffset,
}

#[derive(Clone, Debug, Eq, PartialEq, Hash, Serialize, Deserialize)]
pub struct StableExternalCaret {
  pub offset: StableDocumentOffset,
  pub color_rgb: u32,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Db8PresenceTarget {
  Paragraph {
    paragraph_id: ParagraphId,
    byte: usize,
  },
  Block {
    block_id: BlockId,
  },
  TableCell {
    block_id: BlockId,
    row: usize,
    cell: usize,
    byte: usize,
  },
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Db8PresencePayload {
  Caret { target: Db8PresenceTarget },
  Range { anchor: Db8PresenceTarget, head: Db8PresenceTarget },
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireDb8PresenceTarget {
  Paragraph {
    paragraph_id: String,
    byte: usize,
  },
  Block {
    block_id: String,
  },
  TableCell {
    block_id: String,
    row: usize,
    cell: usize,
    byte: usize,
  },
}

impl WireDb8PresenceTarget {
  fn from_target(target: &Db8PresenceTarget) -> Self {
    match target {
      Db8PresenceTarget::Paragraph { paragraph_id, byte } => Self::Paragraph {
        paragraph_id: paragraph_id.0.to_string(),
        byte: *byte,
      },
      Db8PresenceTarget::Block { block_id } => Self::Block {
        block_id: block_id.0.to_string(),
      },
      Db8PresenceTarget::TableCell { block_id, row, cell, byte } => Self::TableCell {
        block_id: block_id.0.to_string(),
        row: *row,
        cell: *cell,
        byte: *byte,
      },
    }
  }

  fn into_target(self) -> Option<Db8PresenceTarget> {
    match self {
      Self::Paragraph { paragraph_id, byte } => Some(Db8PresenceTarget::Paragraph {
        paragraph_id: ParagraphId(paragraph_id.parse().ok()?),
        byte,
      }),
      Self::Block { block_id } => Some(Db8PresenceTarget::Block {
        block_id: BlockId(block_id.parse().ok()?),
      }),
      Self::TableCell { block_id, row, cell, byte } => Some(Db8PresenceTarget::TableCell {
        block_id: BlockId(block_id.parse().ok()?),
        row,
        cell,
        byte,
      }),
    }
  }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
enum WireDb8PresencePayload {
  Caret {
    target: WireDb8PresenceTarget,
  },
  Range {
    anchor: WireDb8PresenceTarget,
    head: WireDb8PresenceTarget,
  },
}

impl From<&Db8PresencePayload> for WireDb8PresencePayload {
  fn from(payload: &Db8PresencePayload) -> Self {
    match payload {
      Db8PresencePayload::Caret { target } => Self::Caret {
        target: WireDb8PresenceTarget::from_target(target),
      },
      Db8PresencePayload::Range { anchor, head } => Self::Range {
        anchor: WireDb8PresenceTarget::from_target(anchor),
        head: WireDb8PresenceTarget::from_target(head),
      },
    }
  }
}

impl WireDb8PresencePayload {
  fn into_payload(self) -> Option<Db8PresencePayload> {
    match self {
      Self::Caret { target } => Some(Db8PresencePayload::Caret {
        target: target.into_target()?,
      }),
      Self::Range { anchor, head } => Some(Db8PresencePayload::Range {
        anchor: anchor.into_target()?,
        head: head.into_target()?,
      }),
    }
  }
}

#[must_use]
pub fn serialize_db8_presence_payload(payload: &Db8PresencePayload) -> Option<String> {
  serde_json::to_string(&WireDb8PresencePayload::from(payload)).ok()
}

#[must_use]
pub fn parse_db8_presence_payload(payload: &str) -> Option<Db8PresencePayload> {
  serde_json::from_str::<WireDb8PresencePayload>(payload)
    .ok()?
    .into_payload()
}
#[derive(Clone, Debug, Default)]
pub struct DocumentIdentityMap {
  paragraph_ids: Vec<ParagraphId>,
  block_ids: Vec<BlockId>,
  table_cell_ids: Vec<Vec<Vec<TableCellId>>>,
  // Map from ParagraphId to current index for fast CRDT → editor lookup
  paragraph_id_to_index: HashMap<ParagraphId, usize>,
}

use std::collections::HashMap;

#[hotpath::measure_all]
impl DocumentIdentityMap {
  #[must_use]
  pub fn new(document: &Document) -> Self {
    let mut this = Self::default();
    this.reconcile(document);
    this
  }

  pub fn reconcile(&mut self, document: &Document) {
    self.paragraph_ids.clone_from(&document.ids.paragraph_ids);
    self.block_ids.clone_from(&document.ids.block_ids);
    self
      .table_cell_ids
      .resize_with(document.blocks.len(), Vec::new);
    self.table_cell_ids.truncate(document.blocks.len());
    for (block_ix, block) in document.blocks.iter().enumerate() {
      let Block::Table(table) = block else {
        self.table_cell_ids[block_ix].clear();
        continue;
      };
      let rows = &mut self.table_cell_ids[block_ix];
      rows.resize_with(table.rows.len(), Vec::new);
      rows.truncate(table.rows.len());
      for (row_ix, row) in table.rows.iter().enumerate() {
        resize_ids(&mut rows[row_ix], row.cells.len(), TableCellId);
      }
    }
    // Rebuild the index map
    self.paragraph_id_to_index.clear();
    for (ix, &id) in self.paragraph_ids.iter().enumerate() {
      self.paragraph_id_to_index.insert(id, ix);
    }
  }

  pub fn insert_split_paragraph(&mut self, paragraph_ix: usize, block_ix: usize) {
    let new_para_id = new_paragraph_id();
    self
      .paragraph_ids
      .insert((paragraph_ix + 1).min(self.paragraph_ids.len()), new_para_id);
    let block_insert_ix = (block_ix + 1).min(self.block_ids.len());
    self.block_ids.insert(block_insert_ix, new_block_id());
    self.table_cell_ids.insert(block_insert_ix, Vec::new());
    // Update index map
    for (ix, &id) in self.paragraph_ids.iter().enumerate().skip(paragraph_ix + 1) {
      self.paragraph_id_to_index.insert(id, ix);
    }
  }
  #[must_use]
  pub fn paragraph_id(&self, paragraph_ix: usize) -> Option<ParagraphId> {
    self.paragraph_ids.get(paragraph_ix).copied()
  }

  #[must_use]
  pub fn block_index(&self, id: BlockId) -> Option<usize> {
    self.block_ids.iter().position(|candidate| *candidate == id)
  }

  #[must_use]
  pub fn block_id(&self, block_ix: usize) -> Option<BlockId> {
    self.block_ids.get(block_ix).copied()
  }

  #[must_use]
  pub fn table_cell_position(&self, id: TableCellId) -> Option<(usize, usize, usize)> {
    for (block_ix, rows) in self.table_cell_ids.iter().enumerate() {
      for (row_ix, row) in rows.iter().enumerate() {
        if let Some(cell_ix) = row.iter().position(|candidate| *candidate == id) {
          return Some((block_ix, row_ix, cell_ix));
        }
      }
    }
    None
  }

  #[must_use]
  pub fn table_cell_id(&self, block_ix: usize, row_ix: usize, cell_ix: usize) -> Option<TableCellId> {
    self
      .table_cell_ids
      .get(block_ix)?
      .get(row_ix)?
      .get(cell_ix)
      .copied()
  }

  #[must_use]
  pub fn remap_stable_offset(&self, offset: StableDocumentOffset, document: &Document) -> Option<super::DocumentOffset> {
    let paragraph_ix = self.paragraph_index(offset.paragraph)?;
    let paragraph = document.paragraphs.get(paragraph_ix)?;
    let byte = offset.byte.min(super::paragraph_text_len(paragraph));
    Some(super::DocumentOffset {
      paragraph: paragraph_ix,
      byte,
    })
  }

  #[must_use]
  pub fn remap_stable_selection(&self, selection: StableEditorSelection, document: &Document) -> Option<super::EditorSelection> {
    Some(super::EditorSelection {
      anchor: self.remap_stable_offset(selection.anchor, document)?,
      head: self.remap_stable_offset(selection.head, document)?,
    })
  }

  #[must_use]
  pub fn remap_stable_external_caret(&self, caret: StableExternalCaret, document: &Document) -> Option<super::ExternalCaret> {
    Some(super::ExternalCaret {
      offset: self.remap_stable_offset(caret.offset, document)?,
      color_rgb: caret.color_rgb,
    })
  }

  #[must_use]
  pub fn paragraph_index(&self, id: ParagraphId) -> Option<usize> {
    self.paragraph_id_to_index.get(&id).copied()
  }
}

#[hotpath::measure]
fn resize_ids<T>(ids: &mut Vec<T>, len: usize, wrap: impl Fn(u128) -> T)
where
  T: std::marker::Copy,
{
  while ids.len() < len {
    ids.push(wrap(uuid::Uuid::new_v4().as_u128()));
  }
  ids.truncate(len);
}

#[derive(Clone, Debug)]
pub enum CanonicalOperation {
  InsertText {
    paragraph: ParagraphId,
    byte: usize,
    text: String,
    styles: RunStyles,
  },
  DeleteRange {
    start_paragraph: ParagraphId,
    start_byte: usize,
    end_paragraph: ParagraphId,
    end_byte: usize,
  },
  SplitParagraph {
    paragraph: ParagraphId,
    byte: usize,
    new_paragraph: ParagraphId,
  },
  JoinParagraphs {
    first: ParagraphId,
    second: ParagraphId,
  },
  SetParagraphStyle {
    paragraph: ParagraphId,
    style: ParagraphStyle,
  },
  SetRunStyles {
    paragraph: ParagraphId,
    range: Range<usize>,
    styles: RunStyles,
  },
  InsertBlock {
    block: BlockId,
    block_ix: usize,
  },
  DeleteBlock {
    block: BlockId,
  },
  MoveBlock {
    block: BlockId,
    new_block_ix: usize,
  },
  ReplaceParagraphSpan {
    start_paragraph: Option<ParagraphId>,
    before: DocumentSpan,
    after: DocumentSpan,
  },
  ReplaceBlock {
    block: Option<BlockId>,
  },
  ReplaceDocument,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
enum WireCanonicalOperation {
  InsertText {
    paragraph: ParagraphId,
    byte: usize,
    text: String,
    styles: RunStyles,
  },
  DeleteRange {
    start_paragraph: ParagraphId,
    start_byte: usize,
    end_paragraph: ParagraphId,
    end_byte: usize,
  },
  SplitParagraph {
    paragraph: ParagraphId,
    byte: usize,
    new_paragraph: ParagraphId,
  },
  JoinParagraphs {
    first: ParagraphId,
    second: ParagraphId,
  },
  SetParagraphStyle {
    paragraph: ParagraphId,
    style: ParagraphStyle,
  },
  SetRunStyles {
    paragraph: ParagraphId,
    range: Range<usize>,
    styles: RunStyles,
  },
  ReplaceParagraphSpan {
    start_paragraph: Option<ParagraphId>,
    before: DocumentSpan,
    after: DocumentSpan,
  },
}

impl WireCanonicalOperation {
  fn from_canonical(operation: &CanonicalOperation) -> Option<Self> {
    match operation {
      CanonicalOperation::InsertText {
        paragraph,
        byte,
        text,
        styles,
      } => Some(Self::InsertText {
        paragraph: *paragraph,
        byte: *byte,
        text: text.clone(),
        styles: *styles,
      }),
      CanonicalOperation::DeleteRange {
        start_paragraph,
        start_byte,
        end_paragraph,
        end_byte,
      } => Some(Self::DeleteRange {
        start_paragraph: *start_paragraph,
        start_byte: *start_byte,
        end_paragraph: *end_paragraph,
        end_byte: *end_byte,
      }),
      CanonicalOperation::SplitParagraph {
        paragraph,
        byte,
        new_paragraph,
      } => Some(Self::SplitParagraph {
        paragraph: *paragraph,
        byte: *byte,
        new_paragraph: *new_paragraph,
      }),
      CanonicalOperation::JoinParagraphs { first, second } => Some(Self::JoinParagraphs {
        first: *first,
        second: *second,
      }),
      CanonicalOperation::SetParagraphStyle { paragraph, style } => Some(Self::SetParagraphStyle {
        paragraph: *paragraph,
        style: *style,
      }),
      CanonicalOperation::SetRunStyles { paragraph, range, styles } => Some(Self::SetRunStyles {
        paragraph: *paragraph,
        range: range.clone(),
        styles: *styles,
      }),
      CanonicalOperation::ReplaceParagraphSpan {
        start_paragraph,
        before,
        after,
      } => Some(Self::ReplaceParagraphSpan {
        start_paragraph: *start_paragraph,
        before: before.clone(),
        after: after.clone(),
      }),
      CanonicalOperation::InsertBlock { .. }
      | CanonicalOperation::DeleteBlock { .. }
      | CanonicalOperation::MoveBlock { .. }
      | CanonicalOperation::ReplaceBlock { .. }
      | CanonicalOperation::ReplaceDocument => None,
    }
  }

  fn into_canonical(self) -> CanonicalOperation {
    match self {
      Self::InsertText {
        paragraph,
        byte,
        text,
        styles,
      } => CanonicalOperation::InsertText {
        paragraph,
        byte,
        text,
        styles,
      },
      Self::DeleteRange {
        start_paragraph,
        start_byte,
        end_paragraph,
        end_byte,
      } => CanonicalOperation::DeleteRange {
        start_paragraph,
        start_byte,
        end_paragraph,
        end_byte,
      },
      Self::SplitParagraph {
        paragraph,
        byte,
        new_paragraph,
      } => CanonicalOperation::SplitParagraph {
        paragraph,
        byte,
        new_paragraph,
      },
      Self::JoinParagraphs { first, second } => CanonicalOperation::JoinParagraphs { first, second },
      Self::SetParagraphStyle { paragraph, style } => CanonicalOperation::SetParagraphStyle { paragraph, style },
      Self::SetRunStyles { paragraph, range, styles } => CanonicalOperation::SetRunStyles { paragraph, range, styles },
      Self::ReplaceParagraphSpan {
        start_paragraph,
        before,
        after,
      } => CanonicalOperation::ReplaceParagraphSpan {
        start_paragraph,
        before,
        after,
      },
    }
  }
}

pub fn encode_canonical_operations(operations: &[CanonicalOperation]) -> Option<Vec<u8>> {
  let wire_operations = operations
    .iter()
    .map(WireCanonicalOperation::from_canonical)
    .collect::<Option<Vec<_>>>()?;
  postcard::to_stdvec(&wire_operations).ok()
}

pub fn decode_canonical_operations(bytes: &[u8]) -> Option<Vec<CanonicalOperation>> {
  postcard::from_bytes::<Vec<WireCanonicalOperation>>(bytes)
    .ok()
    .map(|operations| {
      operations
        .into_iter()
        .map(WireCanonicalOperation::into_canonical)
        .collect()
    })
}
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
struct Db8ParagraphMetadata {
  style: ParagraphStyle,
  runs: Vec<super::TextRun>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum Db8CollabSourceMutation {
  InsertText {
    text_id: String,
    byte_offset: usize,
    text: String,
  },
  DeleteText {
    text_id: String,
    byte_offset: usize,
    byte_len: usize,
  },
  DeleteTextToEnd {
    text_id: String,
    byte_offset: usize,
  },
  MarkText {
    text_id: String,
    range: Range<usize>,
    key: String,
    value: Db8GranularValue,
  },
  UnmarkText {
    text_id: String,
    range: Range<usize>,
    key: String,
  },
  SetTextMetadata {
    text_id: String,
    metadata: Vec<u8>,
  },
  ClearTextMetadata {
    text_id: String,
  },
  InsertParagraph {
    text_id: String,
    after_text_id: Option<String>,
  },
  RemoveParagraph {
    text_id: String,
  },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct Db8CollabAdapterResult {
  pub mutations: Vec<Db8CollabSourceMutation>,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct Db8CollabAdapter;

impl Db8CollabAdapter {
  #[must_use]
  pub fn adapt(operations: &[CanonicalOperation]) -> Db8CollabAdapterResult {
    let mut result = Db8CollabAdapterResult::default();
    for operation in operations {
      Self::adapt_operation(operation, &mut result);
    }
    result
  }

  fn adapt_operation(operation: &CanonicalOperation, result: &mut Db8CollabAdapterResult) {
    match operation {
      CanonicalOperation::InsertText {
        paragraph,
        byte,
        text,
        styles,
      } => {
        let text_id = granular_record_id_u128(paragraph.0);
        result.mutations.push(Db8CollabSourceMutation::InsertText {
          text_id: text_id.clone(),
          byte_offset: *byte,
          text: text.clone(),
        });
        if *styles != RunStyles::default() {
          let range = *byte..*byte + text.len();
          Self::push_exact_style_set_mutations(result, text_id, range, *styles);
        }
      },
      CanonicalOperation::DeleteRange {
        start_paragraph,
        start_byte,
        end_paragraph,
        end_byte,
      } if start_paragraph == end_paragraph => {
        result.mutations.push(Db8CollabSourceMutation::DeleteText {
          text_id: granular_record_id_u128(start_paragraph.0),
          byte_offset: *start_byte,
          byte_len: end_byte.saturating_sub(*start_byte),
        });
      },
      CanonicalOperation::DeleteRange {
        start_paragraph,
        start_byte,
        end_paragraph,
        end_byte,
      } => {
        // Cross-paragraph delete: trim both endpoints.
        // Intermediate paragraphs must be removed separately by canonical_operations_for_content_replacement
        // which generates proper paragraph removals when the document structure changes.
        result.mutations.push(Db8CollabSourceMutation::DeleteTextToEnd {
          text_id: granular_record_id_u128(start_paragraph.0),
          byte_offset: *start_byte,
        });
        result.mutations.push(Db8CollabSourceMutation::DeleteText {
          text_id: granular_record_id_u128(end_paragraph.0),
          byte_offset: 0,
          byte_len: *end_byte,
        });
      },
      CanonicalOperation::SetParagraphStyle { paragraph, style } => {
        let metadata = postcard::to_stdvec(&Db8ParagraphMetadata {
          style: *style,
          runs: Vec::new(),
        })
        .unwrap_or_default();
        result
          .mutations
          .push(Db8CollabSourceMutation::SetTextMetadata {
            text_id: granular_record_id_u128(paragraph.0),
            metadata,
          });
      },
      CanonicalOperation::SetRunStyles { paragraph, range, styles } => {
        Self::adapt_run_style_mutation(result, *paragraph, range.clone(), *styles);
      },
      CanonicalOperation::ReplaceParagraphSpan {
        start_paragraph,
        before,
        after,
      } => {
        Self::adapt_span_replacement(result, *start_paragraph, before, after);
      },
      CanonicalOperation::SplitParagraph {
        paragraph,
        new_paragraph,
        ..
      } => {
        // InsertParagraph creates the new paragraph container.
        // The text transfer is handled by subsequent InsertText operations
        // generated by canonical_operations_for_content_replacement.
        result
          .mutations
          .push(Db8CollabSourceMutation::InsertParagraph {
            text_id: granular_record_id_u128(new_paragraph.0),
            after_text_id: Some(granular_record_id_u128(paragraph.0)),
          });
      },
      CanonicalOperation::JoinParagraphs { second, .. } => {
        // RemoveParagraph removes the second paragraph container.
        // Text and style transfers are handled by the edit pipeline which generates
        // InsertText + RemoveParagraph sequences through canonical_operations_for_content_replacement.
        result
          .mutations
          .push(Db8CollabSourceMutation::RemoveParagraph {
            text_id: granular_record_id_u128(second.0),
          });
      },
      CanonicalOperation::InsertBlock { .. }
      | CanonicalOperation::DeleteBlock { .. }
      | CanonicalOperation::MoveBlock { .. }
      | CanonicalOperation::ReplaceBlock { .. } => {
        // Block operations are not currently mapped to CRDT mutations.
        // If blocks are editable during collaboration, these need schema support.
      },
      CanonicalOperation::ReplaceDocument => {
        // ReplaceDocument is generated when the adapter cannot represent an edit incrementally.
        // This signals that a full document snapshot replacement is needed.
        // The caller must handle this by generating full-source mutations via diff or snapshot.
      },
    }
  }

  fn adapt_span_replacement(
    result: &mut Db8CollabAdapterResult,
    start_paragraph: Option<ParagraphId>,
    before: &DocumentSpan,
    after: &DocumentSpan,
  ) {
    if before.paragraphs.len() != after.paragraphs.len() || before.text.len() != after.text.len() {
      return;
    }
    let (Some(paragraph_id), [before_paragraph], [after_paragraph]) =
      (start_paragraph, before.paragraphs.as_slice(), after.paragraphs.as_slice())
    else {
      return;
    };
    let text_id = granular_record_id_u128(paragraph_id.0);
    if before_paragraph.style != after_paragraph.style {
      let metadata = postcard::to_stdvec(&Db8ParagraphMetadata {
        style: after_paragraph.style,
        runs: after_paragraph.runs.clone(),
      })
      .unwrap_or_default();
      result
        .mutations
        .push(Db8CollabSourceMutation::SetTextMetadata {
          text_id: text_id.clone(),
          metadata,
        });
    }
    Self::adapt_run_diffs(result, text_id, before_paragraph, after_paragraph);
  }

  fn adapt_run_style_mutation(result: &mut Db8CollabAdapterResult, paragraph: ParagraphId, range: Range<usize>, styles: RunStyles) {
    let text_id = granular_record_id_u128(paragraph.0);
    Self::push_exact_style_set_mutations(result, text_id, range, styles);
  }

  fn push_exact_style_set_mutations(result: &mut Db8CollabAdapterResult, text_id: String, range: Range<usize>, styles: RunStyles) {
    for key in [DB8_MARK_SEMANTIC, DB8_MARK_DIRECT_UNDERLINE, DB8_MARK_STRIKETHROUGH, DB8_MARK_HIGHLIGHT] {
      result.mutations.push(Db8CollabSourceMutation::UnmarkText {
        text_id: text_id.clone(),
        range: range.clone(),
        key: key.to_string(),
      });
    }
    Self::push_style_mark_mutations(result, text_id, range, RunStyles::default(), styles);
  }

  fn push_style_mark_mutations(result: &mut Db8CollabAdapterResult, text_id: String, range: Range<usize>, before: RunStyles, after: RunStyles) {
    if before.semantic != after.semantic {
      if let Some(value) = semantic_mark_value(after.semantic) {
        result.mutations.push(Db8CollabSourceMutation::MarkText {
          text_id: text_id.clone(),
          range: range.clone(),
          key: DB8_MARK_SEMANTIC.to_string(),
          value,
        });
      } else {
        result.mutations.push(Db8CollabSourceMutation::UnmarkText {
          text_id: text_id.clone(),
          range: range.clone(),
          key: DB8_MARK_SEMANTIC.to_string(),
        });
      }
    }
    if before.direct_underline != after.direct_underline {
      if after.direct_underline {
        result.mutations.push(Db8CollabSourceMutation::MarkText {
          text_id: text_id.clone(),
          range: range.clone(),
          key: DB8_MARK_DIRECT_UNDERLINE.to_string(),
          value: Db8GranularValue::Bool(true),
        });
      } else {
        result.mutations.push(Db8CollabSourceMutation::UnmarkText {
          text_id: text_id.clone(),
          range: range.clone(),
          key: DB8_MARK_DIRECT_UNDERLINE.to_string(),
        });
      }
    }
    if before.strikethrough != after.strikethrough {
      if after.strikethrough {
        result.mutations.push(Db8CollabSourceMutation::MarkText {
          text_id: text_id.clone(),
          range: range.clone(),
          key: DB8_MARK_STRIKETHROUGH.to_string(),
          value: Db8GranularValue::Bool(true),
        });
      } else {
        result.mutations.push(Db8CollabSourceMutation::UnmarkText {
          text_id: text_id.clone(),
          range: range.clone(),
          key: DB8_MARK_STRIKETHROUGH.to_string(),
        });
      }
    }
    if before.highlight != after.highlight {
      if let Some(highlight) = after.highlight {
        result.mutations.push(Db8CollabSourceMutation::MarkText {
          text_id,
          range,
          key: DB8_MARK_HIGHLIGHT.to_string(),
          value: highlight_mark_value(highlight),
        });
      } else {
        result.mutations.push(Db8CollabSourceMutation::UnmarkText {
          text_id,
          range,
          key: DB8_MARK_HIGHLIGHT.to_string(),
        });
      }
    }
  }

  fn adapt_run_diffs(result: &mut Db8CollabAdapterResult, text_id: String, before: &super::Paragraph, after: &super::Paragraph) {
    let before_len: usize = before.runs.iter().map(|run| run.len).sum();
    let after_len: usize = after.runs.iter().map(|run| run.len).sum();
    let text_len = before_len.min(after_len);
    if text_len == 0 {
      return;
    }

    let mut boundaries = vec![0, text_len];
    let mut offset = 0usize;
    for run in &before.runs {
      offset = (offset + run.len).min(text_len);
      boundaries.push(offset);
    }
    offset = 0;
    for run in &after.runs {
      offset = (offset + run.len).min(text_len);
      boundaries.push(offset);
    }
    boundaries.sort_unstable();
    boundaries.dedup();

    for pair in boundaries.windows(2) {
      let start = pair[0];
      let end = pair[1];
      if start == end {
        continue;
      }
      let before_styles = styles_at_offset(&before.runs, start);
      let after_styles = styles_at_offset(&after.runs, start);
      Self::push_style_mark_mutations(result, text_id.clone(), start..end, before_styles, after_styles);
    }
  }
}

fn styles_at_offset(runs: &[super::TextRun], offset: usize) -> RunStyles {
  let mut cursor = 0usize;
  for run in runs {
    let end = cursor + run.len;
    if offset < end {
      return run.styles;
    }
    cursor = end;
  }
  RunStyles::default()
}

fn semantic_mark_value(style: RunSemanticStyle) -> Option<Db8GranularValue> {
  match style {
    RunSemanticStyle::Plain => None,
    RunSemanticStyle::Custom(slot) => Some(Db8GranularValue::I64(i64::from(slot))),
  }
}

fn highlight_mark_value(style: HighlightStyle) -> Db8GranularValue {
  match style {
    HighlightStyle::Custom(slot) => Db8GranularValue::I64(i64::from(slot)),
  }
}

impl CollaborationEdit {
  #[must_use]
  pub fn from_operations(operations: Vec<CanonicalOperation>) -> Self {
    let adapter = Db8CollabAdapter::adapt(&operations);
    Self {
      operations,
      source_mutations: adapter.mutations,
    }
  }
}

#[derive(Clone, Debug, Default)]
pub struct CollaborationEdit {
  pub operations: Vec<CanonicalOperation>,
  pub source_mutations: Vec<Db8CollabSourceMutation>,
}
#[cfg(test)]
mod tests {
  use super::*;
  use crate::{Paragraph, TextRun};
  #[test]
  fn canonical_insert_text_round_trips_through_wire_operations() {
    let operation = CanonicalOperation::InsertText {
      paragraph: ParagraphId(1),
      byte: 2,
      text: "hi".to_string(),
      styles: RunStyles::default(),
    };

    let encoded = encode_canonical_operations(std::slice::from_ref(&operation)).unwrap();
    let decoded = decode_canonical_operations(&encoded).unwrap();

    assert!(matches!(
      decoded.as_slice(),
      [CanonicalOperation::InsertText {
        paragraph,
        byte: 2,
        text,
        styles,
      }] if *paragraph == ParagraphId(1) && text == "hi" && *styles == RunStyles::default()
    ));
  }

  #[test]
  fn canonical_replace_paragraph_span_round_trips_through_wire_operations() {
    let before = DocumentSpan {
      start_paragraph: 2,
      text: "abc".to_string(),
      paragraphs: vec![Paragraph {
        style: ParagraphStyle::Normal,
        byte_range: 0..3,
        runs: vec![TextRun {
          len: 3,
          styles: RunStyles::default(),
        }],
        version: 7,
      }],
    };
    let after = DocumentSpan {
      start_paragraph: 2,
      text: "ab\nc".to_string(),
      paragraphs: vec![
        Paragraph {
          style: ParagraphStyle::Normal,
          byte_range: 0..2,
          runs: vec![TextRun {
            len: 2,
            styles: RunStyles::default(),
          }],
          version: 8,
        },
        Paragraph {
          style: ParagraphStyle::Normal,
          byte_range: 3..4,
          runs: vec![TextRun {
            len: 1,
            styles: RunStyles::default(),
          }],
          version: 0,
        },
      ],
    };
    let operation = CanonicalOperation::ReplaceParagraphSpan {
      start_paragraph: Some(ParagraphId(9)),
      before: before.clone(),
      after: after.clone(),
    };

    let encoded = encode_canonical_operations(std::slice::from_ref(&operation)).unwrap();
    let decoded = decode_canonical_operations(&encoded).unwrap();

    let [
      CanonicalOperation::ReplaceParagraphSpan {
        start_paragraph,
        before: decoded_before,
        after: decoded_after,
      },
    ] = decoded.as_slice()
    else {
      panic!("expected one paragraph-span replacement operation");
    };
    assert_eq!(*start_paragraph, Some(ParagraphId(9)));
    assert_eq!(decoded_before, &before);
    assert_eq!(decoded_after, &after);
  }

  #[test]
  fn unsupported_structural_operations_are_not_encoded_by_the_current_wire_path() {
    assert!(
      encode_canonical_operations(&[CanonicalOperation::InsertBlock {
        block: BlockId(1),
        block_ix: 0,
      }])
      .is_none()
    );
    assert!(encode_canonical_operations(&[CanonicalOperation::ReplaceDocument]).is_none());
  }

  #[test]
  fn mixed_supported_and_unsupported_operations_do_not_encode() {
    assert!(
      encode_canonical_operations(&[
        CanonicalOperation::InsertText {
          paragraph: ParagraphId(1),
          byte: 0,
          text: "a".to_string(),
          styles: RunStyles::default(),
        },
        CanonicalOperation::ReplaceDocument,
      ])
      .is_none()
    );
  }

  #[test]
  fn granular_record_ids_use_canonical_hex_encoding() {
    assert_eq!(granular_record_id_u128(7), "00000000000000000000000000000007");
  }

  #[test]
  fn adapter_converts_text_insert_and_delete_into_granular_source_mutations() {
    let insert = CanonicalOperation::InsertText {
      paragraph: ParagraphId(7),
      byte: 3,
      text: "xy".to_string(),
      styles: RunStyles::default(),
    };
    let delete = CanonicalOperation::DeleteRange {
      start_paragraph: ParagraphId(7),
      start_byte: 1,
      end_paragraph: ParagraphId(7),
      end_byte: 4,
    };

    let result = Db8CollabAdapter::adapt(&[insert, delete]);

    assert!(matches!(
      result.mutations.as_slice(),
      [
        Db8CollabSourceMutation::InsertText {
          text_id,
          byte_offset: 3,
          text,
        },
        Db8CollabSourceMutation::DeleteText {
          text_id: delete_text_id,
          byte_offset: 1,
          byte_len: 3,
        },
      ] if text_id == delete_text_id && text_id == &granular_record_id_u128(7) && text == "xy"
    ));
  }

  #[test]
  fn adapter_maps_style_span_updates_to_metadata_and_marks() {
    let before = DocumentSpan {
      start_paragraph: 0,
      text: "abc".to_string(),
      paragraphs: vec![Paragraph {
        style: ParagraphStyle::Normal,
        byte_range: 0..3,
        runs: vec![TextRun {
          len: 3,
          styles: RunStyles::default(),
        }],
        version: 0,
      }],
    };
    let after = DocumentSpan {
      start_paragraph: 0,
      text: "abc".to_string(),
      paragraphs: vec![Paragraph {
        style: ParagraphStyle::Custom(2),
        byte_range: 0..3,
        runs: vec![TextRun {
          len: 3,
          styles: RunStyles {
            direct_underline: true,
            ..RunStyles::default()
          },
        }],
        version: 1,
      }],
    };

    let result = Db8CollabAdapter::adapt(&[CanonicalOperation::ReplaceParagraphSpan {
      start_paragraph: Some(ParagraphId(9)),
      before,
      after,
    }]);

    assert!(matches!(
      result.mutations.as_slice(),
      [
        Db8CollabSourceMutation::SetTextMetadata { text_id, .. },
        Db8CollabSourceMutation::MarkText { text_id: mark_id, key, .. }
      ] if text_id == mark_id && text_id == &granular_record_id_u128(9) && key == "direct_underline"
    ));
  }

  #[test]
  fn adapter_maps_run_style_marks_to_db8_granular_schema() {
    let result = Db8CollabAdapter::adapt(&[CanonicalOperation::SetRunStyles {
      paragraph: ParagraphId(9),
      range: 1..4,
      styles: RunStyles {
        semantic: RunSemanticStyle::Custom(2),
        direct_underline: true,
        strikethrough: true,
        highlight: Some(HighlightStyle::Custom(3)),
      },
    }]);

    let marks = result
      .mutations
      .iter()
      .filter_map(|mutation| match mutation {
        Db8CollabSourceMutation::MarkText { key, value, .. } => Some((key.as_str(), value)),
        _ => None,
      })
      .collect::<Vec<_>>();

    assert_eq!(marks.len(), 4);
    assert!(marks.contains(&("semantic", &Db8GranularValue::I64(2))));
    assert!(marks.contains(&("direct_underline", &Db8GranularValue::Bool(true))));
    assert!(marks.contains(&("strikethrough", &Db8GranularValue::Bool(true))));
    assert!(marks.contains(&("highlight", &Db8GranularValue::I64(3))));
  }

  #[test]
  fn adapter_clears_existing_style_marks_for_exact_run_style_sets() {
    let result = Db8CollabAdapter::adapt(&[CanonicalOperation::SetRunStyles {
      paragraph: ParagraphId(9),
      range: 2..5,
      styles: RunStyles::default(),
    }]);

    let unmarks = result
      .mutations
      .iter()
      .filter_map(|mutation| match mutation {
        Db8CollabSourceMutation::UnmarkText { key, range, .. } => Some((key.as_str(), range.clone())),
        _ => None,
      })
      .collect::<Vec<_>>();

    assert_eq!(unmarks.len(), 4);
    assert!(unmarks.contains(&("semantic", 2..5)));
    assert!(unmarks.contains(&("direct_underline", 2..5)));
    assert!(unmarks.contains(&("strikethrough", 2..5)));
    assert!(unmarks.contains(&("highlight", 2..5)));
  }

  #[test]
  fn adapter_maps_replace_document_as_noop() {
    let result = Db8CollabAdapter::adapt(&[CanonicalOperation::ReplaceDocument]);
    assert!(result.mutations.is_empty());
  }

  #[test]
  fn adapter_maps_split_paragraph_to_insert_paragraph() {
    let operations = vec![CanonicalOperation::SplitParagraph {
      paragraph: ParagraphId(9),
      byte: 4,
      new_paragraph: ParagraphId(10),
    }];
    let edit = CollaborationEdit::from_operations(operations.clone());

    assert_eq!(
      edit.source_mutations,
      vec![Db8CollabSourceMutation::InsertParagraph {
        text_id: granular_record_id_u128(10),
        after_text_id: Some(granular_record_id_u128(9)),
      }]
    );
    assert!(decode_canonical_operations(&encode_canonical_operations(&operations).unwrap()).is_some());
  }

  #[test]
  fn db8_presence_payload_round_trips_through_json() {
    let payload = Db8PresencePayload::Range {
      anchor: Db8PresenceTarget::Paragraph {
        paragraph_id: ParagraphId(11),
        byte: 3,
      },
      head: Db8PresenceTarget::Paragraph {
        paragraph_id: ParagraphId(12),
        byte: 9,
      },
    };

    let encoded = serialize_db8_presence_payload(&payload).unwrap();
    let decoded = parse_db8_presence_payload(&encoded).unwrap();

    assert_eq!(decoded, payload);
  }

  #[test]
  fn db8_presence_payload_rejects_malformed_json() {
    assert!(parse_db8_presence_payload("db8:3:1").is_none());
    assert!(parse_db8_presence_payload("{\"kind\":\"range\"}").is_none());
  }

  #[test]
  fn db8_presence_payload_supports_block_and_table_cell_targets() {
    let block = Db8PresencePayload::Caret {
      target: Db8PresenceTarget::Block { block_id: BlockId(5) },
    };
    let cell = Db8PresencePayload::Caret {
      target: Db8PresenceTarget::TableCell {
        block_id: BlockId(7),
        row: 2,
        cell: 1,
        byte: 4,
      },
    };

    assert_eq!(parse_db8_presence_payload(&serialize_db8_presence_payload(&block).unwrap()), Some(block));
    assert_eq!(parse_db8_presence_payload(&serialize_db8_presence_payload(&cell).unwrap()), Some(cell));
  }
}
