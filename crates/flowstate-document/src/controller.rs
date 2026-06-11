use std::hash::{DefaultHasher, Hash, Hasher};
use std::io;
use std::ops::Range;
use std::sync::Arc;

use flowstate_collab::{
  ActorId, AnchoredPosition, AnchoredSelection, DocumentId as CollabDocumentId, FlowAssetId, FlowAssetReference, FlowChangeSummary, FlowCommit,
  FlowDocument, FlowDocumentSeed, FlowEdit, FlowId, FlowImportOutcome, FlowImportPolicy, FlowInlineMark, FlowMarkValue, FlowNode, FlowNodeId,
  FlowNodeKind, FlowNodeRecord, FlowParagraphInsert, FlowSeedFlow, FlowSeedNode, FlowTextInsert, FlowUndoManager, MaterializedFlowWindow,
  ReplicaId, Role, blake3_hash,
};
use loro::cursor::Side;
use serde::{Deserialize, Serialize};

mod rich_blocks;
mod editor_authority;

pub use editor_authority::{Db8CommitOutbox, Db8EditorAuthority};

use crate::{
  AssetStore, AuthoritativeSourcePosition, AuthoritativeSourceSelection, Block, BlockId, Document, DocumentIds, DocumentOffset,
  DocumentParagraphInput, DocumentRunInput, DocumentStyleManifest, DocumentTheme, EditorSelection, InputParagraph, ParagraphId,
  ParagraphStyle, RunSemanticStyle, RunStyles, TextRun, db8_runs_from_marks, deserialize_paragraph_metadata, document_from_paragraphs,
  document_text_slice, paragraph_span_byte_range, paragraph_text, paragraphs_mut, rebuild_document_offset_index, rebuild_document_sections,
  serialize_block_metadata, serialize_paragraph_metadata, validate_document_invariants,
};

const MARK_SEMANTIC: &str = "semantic";
const MARK_DIRECT_UNDERLINE: &str = "direct_underline";
const MARK_STRIKETHROUGH: &str = "strikethrough";
const MARK_HIGHLIGHT: &str = "highlight";
const ANCHORED_SELECTION_PREFIX: &str = "db8-loro-selection-v1:";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Db8SourcePosition {
  pub paragraph_id: ParagraphId,
  pub byte: usize,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct Db8FlowMetadata {
  style_manifest: DocumentStyleManifest,
}

#[derive(Clone, Debug)]
pub enum Db8EditIntent {
  RegisterAsset {
    asset: crate::AssetRecord,
  },
  InsertText {
    at: Db8SourcePosition,
    text: String,
    styles: RunStyles,
  },
  InsertParagraphFragment {
    at: Db8SourcePosition,
    paragraphs: Vec<InputParagraph>,
    new_paragraph_ids: Vec<ParagraphId>,
  },
  DeleteText {
    start: Db8SourcePosition,
    end: Db8SourcePosition,
  },
  SplitParagraph {
    at: Db8SourcePosition,
    new_paragraph_id: ParagraphId,
    style: ParagraphStyle,
  },
  JoinParagraph {
    second_paragraph_id: ParagraphId,
  },
  SetParagraphStyle {
    paragraph_id: ParagraphId,
    style: ParagraphStyle,
  },
  SetRunStyles {
    paragraph_id: ParagraphId,
    range: Range<usize>,
    styles: RunStyles,
  },
  InsertBlock {
    block_id: BlockId,
    block_ix: usize,
    block: Block,
  },
  DeleteBlock {
    block_id: BlockId,
  },
  SetEquationSource {
    block_id: BlockId,
    source: String,
  },
  SetImageProperties {
    block_id: BlockId,
    image: crate::ImageBlock,
  },
  InsertTableRow {
    table_id: BlockId,
    after_row_id: Option<BlockId>,
    row_id: BlockId,
    cells: Vec<(BlockId, ParagraphId)>,
  },
  DeleteTableRow {
    row_id: BlockId,
  },
  InsertTableCell {
    row_id: BlockId,
    after_cell_id: Option<BlockId>,
    cell_id: BlockId,
    paragraph_id: ParagraphId,
  },
  DeleteTableCell {
    cell_id: BlockId,
  },
  SetTableProperties {
    table_id: BlockId,
    column_widths: Vec<crate::TableColumnWidth>,
    style: crate::TableStyle,
  },
}

#[derive(Clone, Debug)]
pub struct Db8ProjectionDelta {
  pub before_frontier: Vec<u8>,
  pub after_frontier: Vec<u8>,
  pub source_hash: Option<[u8; 32]>,
  pub changes: FlowChangeSummary,
  pub replaced_blocks_before: Range<usize>,
  pub replacement_blocks_after: Range<usize>,
  pub affected_paragraphs_before: Range<usize>,
  pub affected_paragraphs_after: Range<usize>,
  pub projection: Document,
}

impl Db8ProjectionDelta {
  #[must_use]
  pub fn into_editor_update(
    self,
    origin: crate::AuthoritativeProjectionOrigin,
    selection: Option<EditorSelection>,
  ) -> crate::AuthoritativeProjectionUpdate {
    crate::AuthoritativeProjectionUpdate {
      document: self.projection,
      affected_paragraphs_before: self.affected_paragraphs_before,
      affected_paragraphs_after: self.affected_paragraphs_after,
      selection,
      origin,
    }
  }
}

#[derive(Clone, Debug)]
pub struct Db8ControllerCommit {
  pub source: FlowCommit,
  pub projection: Db8ProjectionDelta,
  pub registered_assets: Vec<crate::AssetRecord>,
  pub selection: Option<EditorSelection>,
}

#[derive(Clone, Debug)]
struct ProjectionImpact {
  replaced_blocks_before: Range<usize>,
  replacement_blocks_after: Range<usize>,
  affected_paragraphs_before: Range<usize>,
  affected_paragraphs_after: Range<usize>,
}

#[derive(Clone, Copy, Debug)]
struct TypingBurst {
  paragraph_id: ParagraphId,
  next_byte: usize,
  styles: RunStyles,
}

#[derive(Debug)]
pub struct Db8DocumentController {
  source: FlowDocument,
  projection: Document,
  undo: FlowUndoManager,
  typing_burst: Option<TypingBurst>,
}

impl Db8DocumentController {
  pub fn from_document(document: &Document, actor_id: ActorId, replica_id: ReplicaId) -> io::Result<Self> {
    let document = crate::persistence::io::document_for_serialization(document);
    let seed = db8_flow_seed(&document)?;
    let document_id = CollabDocumentId(uuid::Uuid::from_u128(document.ids.document_id));
    let source = FlowDocument::from_seed(document_id, actor_id, replica_id, &seed).map_err(collab_to_io)?;
    Self::from_source(source, document.assets.clone())
  }

  pub fn from_source(source: FlowDocument, assets: AssetStore) -> io::Result<Self> {
    let projection = materialize_db8_flow_document(&source, assets)?;
    let undo = source.new_undo_manager();
    Ok(Self {
      source,
      projection,
      undo,
      typing_burst: None,
    })
  }

  pub fn from_snapshot(snapshot: &[u8], expected_document_id: CollabDocumentId, replica_id: ReplicaId, assets: AssetStore) -> io::Result<Self> {
    let source = FlowDocument::from_snapshot(snapshot, Some(expected_document_id), replica_id).map_err(collab_to_io)?;
    Self::from_source(source, assets)
  }

  #[must_use]
  pub const fn source(&self) -> &FlowDocument {
    &self.source
  }

  #[must_use]
  pub const fn projection(&self) -> &Document {
    &self.projection
  }

  pub fn apply_intent(&mut self, role: Role, intent: Db8EditIntent) -> io::Result<Db8ControllerCommit> {
    self.apply_intents(role, &[intent])
  }

  pub fn apply_intents(&mut self, role: Role, intents: &[Db8EditIntent]) -> io::Result<Db8ControllerCommit> {
    self.apply_intents_with_undo_selection(role, intents, None)
  }

  pub fn apply_intents_with_undo_selection(
    &mut self,
    role: Role,
    intents: &[Db8EditIntent],
    undo_selection: Option<AnchoredSelection>,
  ) -> io::Result<Db8ControllerCommit> {
    let isolated_undo_group = !self.continues_typing_burst(intents);
    if isolated_undo_group {
      self.undo.begin_isolated_group().map_err(collab_to_io)?;
    }
    if let Err(error) = self.undo.set_selection_for_next_item(undo_selection) {
      if isolated_undo_group {
        self.undo.end_isolated_group();
      }
      return Err(collab_to_io(error));
    }
    let result = self.apply_intents_inner(role, intents);
    let clear_result = self.undo.set_selection_for_next_item(None).map_err(collab_to_io);
    if isolated_undo_group {
      self.undo.end_isolated_group();
    }
    clear_result?;
    if result.is_ok() {
      self.update_typing_burst(intents);
    } else {
      self.typing_burst = None;
    }
    result
  }

  fn apply_intents_inner(&mut self, role: Role, intents: &[Db8EditIntent]) -> io::Result<Db8ControllerCommit> {
    let before_frontier = self.source.frontier().map_err(collab_to_io)?;
    let registered_assets = intents
      .iter()
      .filter_map(|intent| match intent {
        Db8EditIntent::RegisterAsset { asset } => Some(asset.clone()),
        _ => None,
      })
      .collect::<Vec<_>>();
    let edits = intents
      .iter()
      .map(|intent| self.flow_edit_for_intent(intent))
      .collect::<io::Result<Vec<_>>>()?;
    let source = self.source.apply_edits(role, &edits).map_err(collab_to_io)?;
    for intent in intents {
      if let Db8EditIntent::RegisterAsset { asset } = intent {
        self.projection.assets.assets.insert(asset.id, asset.clone());
      }
    }
    let mut commit = self.finish_source_commit(before_frontier, source)?;
    commit.registered_assets = registered_assets;
    Ok(commit)
  }

  fn flow_edit_for_intent(&self, intent: &Db8EditIntent) -> io::Result<FlowEdit> {
    match intent {
      Db8EditIntent::RegisterAsset { asset } => Ok(FlowEdit::PutAssetReference {
        asset: FlowAssetReference {
          id: FlowAssetId(uuid::Uuid::from_u128(asset.id.0)),
          blake3_hash: blake3_hash(&asset.bytes),
          byte_len: asset.bytes.len() as u64,
          mime_type: asset.mime_type.to_string(),
          original_name: asset.original_name.as_ref().map(ToString::to_string),
        },
      }),
      Db8EditIntent::InsertText { at, text, styles } => {
        let anchor = self.anchor_for_source_position(*at, Side::Right)?;
        Ok(FlowEdit::InsertText {
          at: anchor,
          text: text.clone(),
          marks: flow_marks_for_styles(*styles),
        })
      },
      Db8EditIntent::InsertParagraphFragment {
        at,
        paragraphs,
        new_paragraph_ids,
      } => {
        let Some(first) = paragraphs.first() else {
          return Err(io::Error::new(io::ErrorKind::InvalidInput, "DB8 paragraph fragment is empty"));
        };
        if new_paragraph_ids.len() + 1 != paragraphs.len() {
          return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "DB8 paragraph fragment ID count does not match paragraph count",
          ));
        }
        let anchor = self.anchor_for_source_position(*at, Side::Right)?;
        let additional_paragraphs = paragraphs
          .iter()
          .skip(1)
          .zip(new_paragraph_ids)
          .map(|(paragraph, paragraph_id)| {
            Ok(FlowParagraphInsert {
              paragraph_id: flow_node_id(*paragraph_id),
              metadata: serialize_paragraph_metadata(paragraph.style, &[])?,
              runs: flow_text_inserts(paragraph),
            })
          })
          .collect::<io::Result<Vec<_>>>()?;
        Ok(FlowEdit::InsertParagraphFragment {
          at: anchor,
          first_runs: flow_text_inserts(first),
          additional_paragraphs,
        })
      },
      Db8EditIntent::DeleteText { start, end } => {
        let start = self.anchor_for_source_position(*start, Side::Left)?;
        let end = self.anchor_for_source_position(*end, Side::Right)?;
        Ok(FlowEdit::DeleteDocumentRange { start, end })
      },
      Db8EditIntent::SplitParagraph {
        at,
        new_paragraph_id,
        style,
      } => {
        let anchor = self.anchor_for_source_position(*at, Side::Right)?;
        Ok(FlowEdit::SplitParagraph {
          at: anchor,
          new_paragraph_id: flow_node_id(*new_paragraph_id),
          metadata: serialize_paragraph_metadata(*style, &[])?,
        })
      },
      Db8EditIntent::JoinParagraph { second_paragraph_id } => Ok(FlowEdit::JoinParagraph {
        second_paragraph_id: flow_node_id(*second_paragraph_id),
      }),
      Db8EditIntent::SetParagraphStyle { paragraph_id, style } => {
        Ok(FlowEdit::SetNodeMetadata {
          node_id: flow_node_id(*paragraph_id),
          metadata: serialize_paragraph_metadata(*style, &[])?,
        })
      },
      Db8EditIntent::SetRunStyles {
        paragraph_id,
        range,
        styles,
      } => {
        let start = self.anchor_for_source_position(
          Db8SourcePosition {
            paragraph_id: *paragraph_id,
            byte: range.start,
          },
          Side::Left,
        )?;
        let end = self.anchor_for_source_position(
          Db8SourcePosition {
            paragraph_id: *paragraph_id,
            byte: range.end,
          },
          Side::Right,
        )?;
        Ok(FlowEdit::SetTextMarks {
          start,
          end,
          clear_keys: flow_style_keys(),
          marks: flow_marks_for_styles(*styles),
        })
      },
      Db8EditIntent::InsertBlock {
        block_id,
        block_ix,
        block,
      } => {
        let mut child_flows = Vec::new();
        let object = rich_blocks::seed_object(flow_node_id_from_block(*block_id), block, &mut child_flows)?;
        let at = self
          .source
          .anchor_at_node_index(self.source.root_flow_id(), *block_ix, Side::Left)
          .map_err(collab_to_io)?;
        Ok(FlowEdit::InsertObject {
          at,
          object,
          child_flows,
        })
      },
      Db8EditIntent::DeleteBlock { block_id } => Ok(FlowEdit::DeleteObject {
        object_id: flow_node_id_from_block(*block_id),
      }),
      Db8EditIntent::SetEquationSource { block_id, source } => Ok(FlowEdit::ReplaceParagraphText {
        paragraph_id: rich_blocks::equation_source_paragraph_id(flow_node_id_from_block(*block_id)),
        text: source.clone(),
        marks: Vec::new(),
      }),
      Db8EditIntent::SetImageProperties { block_id, image } => Ok(FlowEdit::SetNodeMetadata {
        node_id: flow_node_id_from_block(*block_id),
        metadata: rich_blocks::image_shell_metadata(image)?,
      }),
      Db8EditIntent::InsertTableRow {
        table_id,
        after_row_id,
        row_id,
        cells,
      } => {
        let at = self.child_object_insert_anchor(*table_id, *after_row_id)?;
        let mut child_flows = Vec::new();
        let cells = cells
          .iter()
          .map(|(cell, paragraph)| (flow_node_id_from_block(*cell), flow_node_id(*paragraph)))
          .collect::<Vec<_>>();
        let object = rich_blocks::seed_table_row(flow_node_id_from_block(*row_id), &cells, &mut child_flows)?;
        Ok(FlowEdit::InsertObject {
          at,
          object,
          child_flows,
        })
      },
      Db8EditIntent::DeleteTableRow { row_id } => Ok(FlowEdit::DeleteObject {
        object_id: flow_node_id_from_block(*row_id),
      }),
      Db8EditIntent::InsertTableCell {
        row_id,
        after_cell_id,
        cell_id,
        paragraph_id,
      } => {
        let at = self.child_object_insert_anchor(*row_id, *after_cell_id)?;
        let mut child_flows = Vec::new();
        let object = rich_blocks::seed_table_cell(
          flow_node_id_from_block(*cell_id),
          flow_node_id(*paragraph_id),
          &mut child_flows,
        )?;
        Ok(FlowEdit::InsertObject {
          at,
          object,
          child_flows,
        })
      },
      Db8EditIntent::DeleteTableCell { cell_id } => Ok(FlowEdit::DeleteObject {
        object_id: flow_node_id_from_block(*cell_id),
      }),
      Db8EditIntent::SetTableProperties {
        table_id,
        column_widths,
        style,
      } => {
        let block_ix = self
          .projection
          .ids
          .block_ids
          .iter()
          .position(|candidate| candidate == table_id)
          .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "DB8 table ID is not in projection"))?;
        let Block::Table(current) = &self.projection.blocks[block_ix] else {
          return Err(io::Error::new(io::ErrorKind::InvalidInput, "DB8 table properties target is not a table"));
        };
        let mut table = current.clone();
        table.column_widths.clone_from(column_widths);
        table.style = style.clone();
        Ok(FlowEdit::SetNodeMetadata {
          node_id: flow_node_id_from_block(*table_id),
          metadata: rich_blocks::table_shell_metadata(&table)?,
        })
      },
    }
  }

  fn child_object_insert_anchor(&self, parent_id: BlockId, after_child_id: Option<BlockId>) -> io::Result<AnchoredPosition> {
    let parent_id = flow_node_id_from_block(parent_id);
    let parent = self.source.node_record(parent_id).map_err(collab_to_io)?;
    let [child_flow_id] = parent.child_flows.as_slice() else {
      return Err(io::Error::new(io::ErrorKind::InvalidInput, "DB8 rich parent does not own one child-order flow"));
    };
    let unicode_offset = match after_child_id {
      Some(after_child_id) => {
        let after_child_id = flow_node_id_from_block(after_child_id);
        let window = self.source.materialize_node_window(after_child_id).map_err(collab_to_io)?;
        if window.id != *child_flow_id {
          return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "DB8 rich insertion predecessor is not in parent flow",
          ));
        }
        window.unicode_range.end
      },
      None => 0,
    };
    self
      .source
      .anchor_at_unicode(*child_flow_id, unicode_offset, Side::Left)
      .map_err(collab_to_io)
  }

  pub fn anchor_selection(&self, selection: &EditorSelection) -> io::Result<AnchoredSelection> {
    let forward = selection.anchor <= selection.head;
    Ok(AnchoredSelection {
      anchor: self.anchor_for_offset_with_side(selection.anchor, if forward { Side::Left } else { Side::Right })?,
      head: self.anchor_for_offset_with_side(selection.head, if forward { Side::Right } else { Side::Left })?,
    })
  }

  pub fn resolve_selection(&self, selection: &AnchoredSelection) -> io::Result<EditorSelection> {
    Ok(EditorSelection {
      anchor: self.resolve_anchored_offset(&selection.anchor)?,
      head: self.resolve_anchored_offset(&selection.head)?,
    })
  }

  pub fn anchor_source_selection(&self, selection: AuthoritativeSourceSelection) -> io::Result<AnchoredSelection> {
    Ok(AnchoredSelection {
      anchor: self
        .source
        .anchor_in_paragraph_utf8(flow_node_id(selection.anchor.paragraph), selection.anchor.byte, Side::Left)
        .map_err(collab_to_io)?,
      head: self
        .source
        .anchor_in_paragraph_utf8(flow_node_id(selection.head.paragraph), selection.head.byte, Side::Right)
        .map_err(collab_to_io)?,
    })
  }

  pub fn resolve_source_selection(&self, selection: &AnchoredSelection) -> io::Result<AuthoritativeSourceSelection> {
    let anchor = self
      .source
      .resolve_anchor_in_paragraph_utf8(&selection.anchor)
      .map_err(collab_to_io)?;
    let head = self
      .source
      .resolve_anchor_in_paragraph_utf8(&selection.head)
      .map_err(collab_to_io)?;
    Ok(AuthoritativeSourceSelection {
      anchor: AuthoritativeSourcePosition {
        paragraph: paragraph_id(anchor.node_id),
        byte: anchor.byte_offset,
      },
      head: AuthoritativeSourcePosition {
        paragraph: paragraph_id(head.node_id),
        byte: head.byte_offset,
      },
    })
  }

  pub fn apply_remote_update(&mut self, update: &[u8], policy: &FlowImportPolicy) -> io::Result<Db8ProjectionDelta> {
    self.typing_burst = None;
    let before_frontier = self.source.frontier().map_err(collab_to_io)?;
    let before_projection = self.projection.clone();
    let FlowImportOutcome { frontier, changes } = self
      .source
      .import_update_checked(update, policy)
      .map_err(collab_to_io)?;
    let (projection, impact) = materialize_db8_projection_for_changes(&self.source, &self.projection, &changes)?;
    self.projection = projection.clone();
    build_projection_delta(
      before_frontier,
      frontier,
      changes,
      None,
      impact,
      &before_projection,
      projection,
    )
  }

  fn reset_undo_lineage(&mut self) {
    self.undo = self.source.new_undo_manager();
    self.typing_burst = None;
  }

  pub fn install_verified_asset(&mut self, asset: crate::AssetRecord) -> io::Result<Document> {
    let reference = self
      .source
      .asset_references()
      .map_err(collab_to_io)?
      .get(&FlowAssetId(uuid::Uuid::from_u128(asset.id.0)))
      .cloned()
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "asset is not referenced by authoritative source"))?;
    if reference.blake3_hash != blake3_hash(&asset.bytes)
      || reference.byte_len != asset.bytes.len() as u64
      || reference.mime_type != asset.mime_type.as_ref()
      || reference.original_name.as_deref() != asset.original_name.as_deref().map(AsRef::as_ref)
    {
      return Err(io::Error::new(io::ErrorKind::InvalidData, "asset bytes or metadata do not match authoritative source reference"));
    }
    self.projection.assets.assets.insert(asset.id, asset);
    Ok(self.projection.clone())
  }

  pub fn install_verified_asset_bytes(&mut self, hash: [u8; 32], bytes: Vec<u8>) -> io::Result<Document> {
    let reference = self
      .source
      .asset_references()
      .map_err(collab_to_io)?
      .into_values()
      .find(|reference| reference.blake3_hash == hash)
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "asset hash is not referenced by authoritative source"))?;
    let mut legacy_hash = DefaultHasher::new();
    bytes.hash(&mut legacy_hash);
    self.install_verified_asset(crate::AssetRecord {
      id: crate::AssetId(reference.id.0.as_u128()),
      mime_type: reference.mime_type.into(),
      original_name: reference.original_name.map(Into::into),
      content_hash: legacy_hash.finish(),
      bytes: Arc::new(bytes),
    })
  }

  pub fn undo(&mut self, role: Role) -> io::Result<Option<Db8ControllerCommit>> {
    self.undo_with_selection(role, None)
  }

  pub fn undo_with_selection(
    &mut self,
    role: Role,
    selection_before: Option<AnchoredSelection>,
  ) -> io::Result<Option<Db8ControllerCommit>> {
    self.typing_burst = None;
    let before_frontier = self.source.frontier().map_err(collab_to_io)?;
    self
      .undo
      .set_selection_for_next_item(selection_before)
      .map_err(collab_to_io)?;
    self.undo.take_popped_selection().map_err(collab_to_io)?;
    let source = self.source.undo(role, &mut self.undo).map_err(collab_to_io);
    self.undo.set_selection_for_next_item(None).map_err(collab_to_io)?;
    let Some(source) = source? else {
      return Ok(None);
    };
    let selection = self.undo.take_popped_selection().map_err(collab_to_io)?;
    let mut commit = self.finish_source_commit(before_frontier, source)?;
    commit.selection = selection.as_ref().and_then(|selection| self.resolve_selection(selection).ok());
    Ok(Some(commit))
  }

  pub fn redo(&mut self, role: Role) -> io::Result<Option<Db8ControllerCommit>> {
    self.redo_with_selection(role, None)
  }

  pub fn redo_with_selection(
    &mut self,
    role: Role,
    selection_before: Option<AnchoredSelection>,
  ) -> io::Result<Option<Db8ControllerCommit>> {
    self.typing_burst = None;
    let before_frontier = self.source.frontier().map_err(collab_to_io)?;
    self
      .undo
      .set_selection_for_next_item(selection_before)
      .map_err(collab_to_io)?;
    self.undo.take_popped_selection().map_err(collab_to_io)?;
    let source = self.source.redo(role, &mut self.undo).map_err(collab_to_io);
    self.undo.set_selection_for_next_item(None).map_err(collab_to_io)?;
    let Some(source) = source? else {
      return Ok(None);
    };
    let selection = self.undo.take_popped_selection().map_err(collab_to_io)?;
    let mut commit = self.finish_source_commit(before_frontier, source)?;
    commit.selection = selection.as_ref().and_then(|selection| self.resolve_selection(selection).ok());
    Ok(Some(commit))
  }

  fn anchor_for_source_position(&self, position: Db8SourcePosition, side: Side) -> io::Result<AnchoredPosition> {
    self
      .source
      .anchor_in_paragraph_utf8(flow_node_id(position.paragraph_id), position.byte, side)
      .map_err(collab_to_io)
  }

  fn continues_typing_burst(&self, intents: &[Db8EditIntent]) -> bool {
    let [Db8EditIntent::InsertText {
      at,
      text,
      styles,
    }] = intents
    else {
      return false;
    };
    !text.is_empty()
      && self.typing_burst.is_some_and(|burst| {
        burst.paragraph_id == at.paragraph_id && burst.next_byte == at.byte && burst.styles == *styles
      })
  }

  fn update_typing_burst(&mut self, intents: &[Db8EditIntent]) {
    self.typing_burst = match intents {
      [Db8EditIntent::InsertText {
        at,
        text,
        styles,
      }] if !text.is_empty() => Some(TypingBurst {
        paragraph_id: at.paragraph_id,
        next_byte: at.byte.saturating_add(text.len()),
        styles: *styles,
      }),
      _ => None,
    };
  }

  fn anchor_for_offset_with_side(&self, offset: DocumentOffset, side: Side) -> io::Result<AnchoredPosition> {
    let paragraph_id = self
      .projection
      .ids
      .paragraph_ids
      .get(offset.paragraph)
      .copied()
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidInput, "DB8 paragraph offset is not in projection"))?;
    self
      .source
      .anchor_in_paragraph_utf8(flow_node_id(paragraph_id), offset.byte, side)
      .map_err(collab_to_io)
  }

  fn resolve_anchored_offset(&self, position: &AnchoredPosition) -> io::Result<DocumentOffset> {
    let resolved = self
      .source
      .resolve_anchor_in_paragraph_utf8(position)
      .map_err(collab_to_io)?;
    let paragraph_id = paragraph_id(resolved.node_id);
    let paragraph = self
      .projection
      .ids
      .paragraph_ids
      .iter()
      .position(|candidate| *candidate == paragraph_id)
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "anchored DB8 paragraph is not visible in projection"))?;
    Ok(DocumentOffset {
      paragraph,
      byte: resolved.byte_offset,
    })
  }

  fn finish_source_commit(&mut self, before_frontier: Vec<u8>, source: FlowCommit) -> io::Result<Db8ControllerCommit> {
    let before_projection = self.projection.clone();
    let (projection, impact) = materialize_db8_projection_for_changes(&self.source, &self.projection, &source.changes)?;
    self.projection = projection.clone();
    Ok(Db8ControllerCommit {
      projection: build_projection_delta(
        before_frontier,
        source.resulting_frontier.clone(),
        source.changes.clone(),
        None,
        impact,
        &before_projection,
        projection,
      )?,
      source,
      registered_assets: Vec::new(),
      selection: None,
    })
  }
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum ProjectionNodeSignature {
  Paragraph {
    block_id: BlockId,
    paragraph_id: ParagraphId,
    text: String,
    style: ParagraphStyle,
    runs: Vec<TextRun>,
  },
  Object {
    block_id: BlockId,
    block: Block,
  },
}

fn build_projection_delta(
  before_frontier: Vec<u8>,
  after_frontier: Vec<u8>,
  changes: FlowChangeSummary,
  source_hash: Option<[u8; 32]>,
  impact: Option<ProjectionImpact>,
  before: &Document,
  projection: Document,
) -> io::Result<Db8ProjectionDelta> {
  let ProjectionImpact {
    replaced_blocks_before,
    replacement_blocks_after,
    affected_paragraphs_before,
    affected_paragraphs_after,
  } = match impact {
    Some(impact) => impact,
    None => projection_impact_from_full_comparison(before, &projection)?,
  };
  Ok(Db8ProjectionDelta {
    before_frontier,
    after_frontier,
    source_hash,
    changes,
    replaced_blocks_before,
    replacement_blocks_after,
    affected_paragraphs_before,
    affected_paragraphs_after,
    projection,
  })
}

fn projection_impact_from_full_comparison(before: &Document, after: &Document) -> io::Result<ProjectionImpact> {
  let before_nodes = projection_node_signatures(before)?;
  let after_nodes = projection_node_signatures(after)?;
  let prefix = before_nodes
    .iter()
    .zip(&after_nodes)
    .take_while(|(before, after)| before == after)
    .count();
  let suffix = before_nodes[prefix..]
    .iter()
    .rev()
    .zip(after_nodes[prefix..].iter().rev())
    .take_while(|(before, after)| before == after)
    .count();
  let replaced_blocks_before = prefix..before_nodes.len().saturating_sub(suffix);
  let replacement_blocks_after = prefix..after_nodes.len().saturating_sub(suffix);
  let affected_paragraphs_before = paragraph_range_for_node_range(&before_nodes, &replaced_blocks_before);
  let affected_paragraphs_after = paragraph_range_for_node_range(&after_nodes, &replacement_blocks_after);
  Ok(ProjectionImpact {
    replaced_blocks_before,
    replacement_blocks_after,
    affected_paragraphs_before,
    affected_paragraphs_after,
  })
}

fn projection_node_signatures(document: &Document) -> io::Result<Vec<ProjectionNodeSignature>> {
  let mut paragraph_ix = 0;
  document
    .blocks
    .iter()
    .enumerate()
    .map(|(block_ix, block)| {
      let block_id = document
        .ids
        .block_ids
        .get(block_ix)
        .copied()
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "projection block ID missing"))?;
      match block {
        Block::Paragraph(paragraph) => {
          let paragraph_id = document
            .ids
            .paragraph_ids
            .get(paragraph_ix)
            .copied()
            .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "projection paragraph ID missing"))?;
          let text = paragraph_text(document, paragraph_ix);
          paragraph_ix += 1;
          Ok(ProjectionNodeSignature::Paragraph {
            block_id,
            paragraph_id,
            text,
            style: paragraph.style,
            runs: paragraph.runs.clone(),
          })
        },
        Block::Image(_) | Block::Equation(_) | Block::Table(_) => Ok(ProjectionNodeSignature::Object {
          block_id,
          block: block.clone(),
        }),
      }
    })
    .collect()
}

fn paragraph_range_for_node_range(nodes: &[ProjectionNodeSignature], range: &Range<usize>) -> Range<usize> {
  let start = nodes[..range.start.min(nodes.len())]
    .iter()
    .filter(|node| matches!(node, ProjectionNodeSignature::Paragraph { .. }))
    .count();
  let count = nodes[range.start.min(nodes.len())..range.end.min(nodes.len())]
    .iter()
    .filter(|node| matches!(node, ProjectionNodeSignature::Paragraph { .. }))
    .count();
  start..start + count
}

pub fn serialize_db8_anchored_selection(selection: &AnchoredSelection) -> io::Result<String> {
  let bytes = postcard::to_stdvec(selection).map_err(invalid_data)?;
  let mut encoded = String::with_capacity(ANCHORED_SELECTION_PREFIX.len() + bytes.len() * 2);
  encoded.push_str(ANCHORED_SELECTION_PREFIX);
  for byte in bytes {
    use std::fmt::Write as _;
    write!(&mut encoded, "{byte:02x}").expect("writing to String cannot fail");
  }
  Ok(encoded)
}

pub fn parse_db8_anchored_selection(encoded: &str) -> io::Result<AnchoredSelection> {
  let hex = encoded
    .strip_prefix(ANCHORED_SELECTION_PREFIX)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "not a DB8 anchored selection"))?;
  if hex.len() % 2 != 0 {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 anchored selection has invalid hex length"));
  }
  let bytes = hex
    .as_bytes()
    .chunks_exact(2)
    .map(|pair| {
      let high = hex_nibble(pair[0])?;
      let low = hex_nibble(pair[1])?;
      Ok(high << 4 | low)
    })
    .collect::<io::Result<Vec<_>>>()?;
  postcard::from_bytes(&bytes).map_err(invalid_data)
}

fn hex_nibble(byte: u8) -> io::Result<u8> {
  match byte {
    b'0'..=b'9' => Ok(byte - b'0'),
    b'a'..=b'f' => Ok(byte - b'a' + 10),
    b'A'..=b'F' => Ok(byte - b'A' + 10),
    _ => Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 anchored selection contains non-hex data")),
  }
}

pub fn db8_flow_seed(document: &Document) -> io::Result<FlowDocumentSeed> {
  validate_document_invariants(document).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
  let root_flow_id = FlowId(uuid::Uuid::from_u128(document.ids.document_id));
  let mut flows = Vec::new();
  let mut nodes = Vec::with_capacity(document.blocks.len());
  let mut paragraph_ix = 0;
  for (block_ix, block) in document.blocks.iter().enumerate() {
    match block {
      Block::Paragraph(paragraph) => {
        let paragraph_id = document
          .ids
          .paragraph_ids
          .get(paragraph_ix)
          .copied()
          .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "DB8 paragraph ID missing during vNext migration"))?;
        let text = document_text_slice(document, paragraph.byte_range.clone());
        nodes.push(FlowSeedNode {
          record: FlowNodeRecord {
            id: flow_node_id(paragraph_id),
            kind: FlowNodeKind::Paragraph,
            metadata: serialize_paragraph_metadata(paragraph.style, &paragraph.runs)?,
            child_flows: Vec::new(),
          },
          text,
          marks: flow_marks_from_runs(&paragraph.runs),
        });
        paragraph_ix += 1;
      },
      Block::Image(_) | Block::Equation(_) | Block::Table(_) => {
        let block_id = document
          .ids
          .block_ids
          .get(block_ix)
          .copied()
          .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "DB8 block ID missing during vNext migration"))?;
        nodes.push(rich_blocks::seed_object(flow_node_id_from_block(block_id), block, &mut flows)?);
      },
    }
  }
  flows.insert(0, FlowSeedFlow { id: root_flow_id, nodes });
  Ok(FlowDocumentSeed {
    root_flow_id,
    document_metadata: postcard::to_stdvec(&Db8FlowMetadata {
      style_manifest: DocumentStyleManifest::from_theme(&document.theme),
    })
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?,
    assets: document
      .assets
      .assets
      .values()
      .map(|asset| FlowAssetReference {
        id: FlowAssetId(uuid::Uuid::from_u128(asset.id.0)),
        blake3_hash: blake3_hash(&asset.bytes),
        byte_len: asset.bytes.len() as u64,
        mime_type: asset.mime_type.to_string(),
        original_name: asset.original_name.as_ref().map(ToString::to_string),
      })
      .collect(),
    flows,
  })
}

fn materialize_db8_projection_for_changes(
  source: &FlowDocument,
  current: &Document,
  changes: &FlowChangeSummary,
) -> io::Result<(Document, Option<ProjectionImpact>)> {
  if let Ok((projection, impact)) = materialize_incremental_root_projection(source, current, changes) {
    return Ok((projection, Some(impact)));
  }
  materialize_db8_flow_document(source, current.assets.clone()).map(|projection| (projection, None))
}

fn materialize_incremental_root_projection(
  source: &FlowDocument,
  current: &Document,
  changes: &FlowChangeSummary,
) -> io::Result<(Document, ProjectionImpact)> {
  let root_flow_id = source.root_flow_id();
  let mut projection = current.clone();
  let mut impacts = Vec::new();
  if let Some(change) = changes.flow_text_changes.get(&root_flow_id) {
    let window = source
      .materialize_flow_window(root_flow_id, change.after_unicode.clone())
      .map_err(collab_to_io)?;
    impacts.push(patch_root_projection_window(
      source,
      &mut projection,
      &window,
      change.before_unicode.clone(),
      change.before_unicode.len() > change.after_unicode.len(),
    )?);
  }
  let mut touched_paragraphs = changes
    .touched_nodes
    .iter()
    .copied()
    .filter(|node_id| projection.ids.paragraph_ids.contains(&paragraph_id(*node_id)))
    .collect::<Vec<_>>();
  touched_paragraphs.sort_unstable();
  for node_id in touched_paragraphs {
    let window = source.materialize_node_window(node_id).map_err(collab_to_io)?;
    impacts.push(patch_root_projection_window(
      source,
      &mut projection,
      &window,
      window.unicode_range.clone(),
      false,
    )?);
  }
  for block_id in affected_root_rich_objects(source, current, &projection, changes)? {
    impacts.push(patch_root_rich_object(source, &mut projection, block_id)?);
  }
  if changes.flow_text_changes.is_empty() && changes.touched_nodes.is_empty() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "change has no incremental root projection"));
  }
  validate_document_invariants(&projection).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
  let impact = if let [impact] = impacts.as_slice() {
    impact.clone()
  } else {
    ProjectionImpact {
      replaced_blocks_before: 0..current.blocks.len(),
      replacement_blocks_after: 0..projection.blocks.len(),
      affected_paragraphs_before: 0..current.paragraphs.len(),
      affected_paragraphs_after: 0..projection.paragraphs.len(),
    }
  };
  Ok((projection, impact))
}

fn patch_root_projection_window(
  source: &FlowDocument,
  document: &mut Document,
  window: &MaterializedFlowWindow,
  before_unicode: Range<usize>,
  include_deleted_blocks: bool,
) -> io::Result<ProjectionImpact> {
  let mapped_range = window
    .nodes
    .iter()
    .filter_map(|node| {
      let block_id = BlockId(node.record().id.0.as_u128());
      document.ids.block_ids.iter().position(|candidate| *candidate == block_id)
    })
    .fold(None::<Range<usize>>, |range, block_ix| {
      Some(match range {
        Some(range) => range.start.min(block_ix)..range.end.max(block_ix + 1),
        None => block_ix..block_ix + 1,
      })
    });
  let changed_range = root_block_range_for_unicode(document, before_unicode)?;
  let block_range = match (mapped_range, include_deleted_blocks) {
    (Some(mapped), true) => mapped.start.min(changed_range.start)..mapped.end.max(changed_range.end),
    (Some(mapped), false) => mapped,
    (None, _) => changed_range,
  };
  let paragraph_start = document.blocks[..block_range.start]
    .iter()
    .filter(|block| matches!(block, Block::Paragraph(_)))
    .count();
  let paragraph_count = document.blocks[block_range.clone()]
    .iter()
    .filter(|block| matches!(block, Block::Paragraph(_)))
    .count();

  let paragraph_inputs = window
    .nodes
    .iter()
    .filter_map(|node| match node {
      FlowNode::Paragraph { record, text, marks } => Some((record, text, marks)),
      FlowNode::Object { .. } => None,
    })
    .map(|(record, text, marks)| {
      let (style, _) = deserialize_paragraph_metadata(&record.metadata)
        .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "DB8 vNext paragraph metadata invalid"))?;
      let runs = db8_runs_from_marks(text.len(), &granular_marks(marks));
      Ok(DocumentParagraphInput {
        style,
        runs: document_run_inputs(text, &runs)?,
      })
    })
    .collect::<io::Result<Vec<_>>>()?;
  let projected = document_from_paragraphs(document.theme.clone(), paragraph_inputs);
  let replacement_paragraphs = projected.paragraphs.as_ref().clone();
  let replacement_text = projected.text.to_string();
  let replacement_paragraph_ids = window
    .nodes
    .iter()
    .filter_map(|node| match node {
      FlowNode::Paragraph { record, .. } => Some(paragraph_id(record.id)),
      FlowNode::Object { .. } => None,
    })
    .collect::<Vec<_>>();
  let replaced_rich_block_ids = document.blocks[block_range.clone()]
    .iter()
    .enumerate()
    .filter(|(_, block)| !matches!(block, Block::Paragraph(_)))
    .map(|(offset, _)| document.ids.block_ids[block_range.start + offset])
    .collect::<Vec<_>>();
  let replacement_rich_block_ids = window
    .nodes
    .iter()
    .filter(|node| matches!(node, FlowNode::Object { .. }))
    .map(|node| BlockId(node.record().id.0.as_u128()))
    .collect::<Vec<_>>();
  for block_id in replaced_rich_block_ids {
    if !replacement_rich_block_ids.contains(&block_id) {
      document.ids.rich_block_ids.remove(&block_id);
    }
  }
  let mut replacement_blocks = Vec::with_capacity(window.nodes.len());
  let mut replacement_block_ids = Vec::with_capacity(window.nodes.len());
  let mut paragraphs = replacement_paragraphs.iter();
  for node in &window.nodes {
    let block_id = BlockId(node.record().id.0.as_u128());
    replacement_block_ids.push(block_id);
    match node {
      FlowNode::Paragraph { .. } => replacement_blocks.push(Block::Paragraph(
        paragraphs
          .next()
          .cloned()
          .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "incremental paragraph projection missing"))?,
      )),
      FlowNode::Object { .. } => {
        if let Some(block_ix) = document
          .ids
          .block_ids
          .iter()
          .position(|candidate| *candidate == block_id)
        {
          replacement_blocks.push(
            document
              .blocks
              .get(block_ix)
              .cloned()
              .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "incremental rich object block missing"))?,
          );
        } else {
          let graph = source
            .materialize_object_graph(flow_node_id_from_block(block_id))
            .map_err(collab_to_io)?;
          let identity = rich_blocks::materialize_object_graph_identity(&graph)?;
          replacement_blocks.push(rich_blocks::materialize_object_graph(&graph)?);
          document.ids.rich_block_ids.insert(block_id, identity);
        }
      },
    }
  }

  let paragraph_end = paragraph_start + paragraph_count;
  let replacement_paragraph_count = replacement_paragraphs.len();
  let replacement_block_count = replacement_blocks.len();
  let replaced_blocks_before = block_range.clone();
  let replacement_block_start = block_range.start;
  let byte_range = paragraph_span_byte_range(document, paragraph_start, paragraph_count);
  document.text.delete(byte_range.clone());
  document.text.insert(byte_range.start, &replacement_text);
  paragraphs_mut(document).splice(paragraph_start..paragraph_end, replacement_paragraphs);
  document
    .ids
    .paragraph_ids
    .splice(paragraph_start..paragraph_end, replacement_paragraph_ids);
  Arc::make_mut(&mut document.blocks).splice(block_range.clone(), replacement_blocks);
  document
    .ids
    .block_ids
    .splice(block_range, replacement_block_ids);
  rebuild_document_offset_index(document);
  rebuild_document_sections(document);
  Ok(ProjectionImpact {
    replaced_blocks_before,
    replacement_blocks_after: replacement_block_start..replacement_block_start + replacement_block_count,
    affected_paragraphs_before: paragraph_start..paragraph_end,
    affected_paragraphs_after: paragraph_start..paragraph_start + replacement_paragraph_count,
  })
}

fn affected_root_rich_objects(
  source: &FlowDocument,
  before: &Document,
  after: &Document,
  changes: &FlowChangeSummary,
) -> io::Result<Vec<BlockId>> {
  let mut candidates = changes.touched_nodes.clone();
  for (flow_id, change) in &changes.flow_text_changes {
    if *flow_id == source.root_flow_id() {
      continue;
    }
    let window = source
      .materialize_flow_window(*flow_id, change.after_unicode.clone())
      .map_err(collab_to_io)?;
    candidates.extend(window.nodes.iter().map(|node| node.record().id));
  }

  let mut roots = Vec::new();
  let mut unresolved = false;
  for candidate in candidates {
    let raw = candidate.0.as_u128();
    if after.ids.paragraph_ids.contains(&ParagraphId(raw)) || before.ids.paragraph_ids.contains(&ParagraphId(raw)) {
      continue;
    }
    let candidate_block = BlockId(raw);
    if after.ids.rich_block_ids.contains_key(&candidate_block) {
      if !roots.contains(&candidate_block) {
        roots.push(candidate_block);
      }
      continue;
    }
    let root = after
      .ids
      .rich_block_ids
      .iter()
      .chain(before.ids.rich_block_ids.iter())
      .find_map(|(block_id, identity)| rich_identity_contains_node(identity, raw).then_some(*block_id));
    if let Some(root) = root {
      if !roots.contains(&root) {
        roots.push(root);
      }
      continue;
    }
    if after.ids.block_ids.contains(&candidate_block) || before.ids.block_ids.contains(&candidate_block) {
      return Err(io::Error::new(
        io::ErrorKind::InvalidData,
        "root rich object is missing its stable identity graph",
      ));
    }
    unresolved = true;
  }
  if unresolved && roots.is_empty() {
    return Err(io::Error::new(
      io::ErrorKind::InvalidData,
      "changed rich child is not reachable from a projected root object",
    ));
  }
  Ok(roots)
}

fn rich_identity_contains_node(identity: &crate::RichBlockIdentity, raw: u128) -> bool {
  match identity {
    crate::RichBlockIdentity::Image { caption } => caption.is_some_and(|paragraph| paragraph.0 == raw),
    crate::RichBlockIdentity::Equation { source } => source.0 == raw,
    crate::RichBlockIdentity::Table(table) => table.rows.iter().any(|row| {
      row.id.0 == raw
        || row.cells.iter().any(|cell| {
          cell.id.0 == raw
            || cell.blocks.iter().any(|block| match block {
              crate::TableCellBlockIdentity::Paragraph(paragraph) => paragraph.0 == raw,
              crate::TableCellBlockIdentity::Table { id, identity } => id.0 == raw || rich_table_identity_contains_node(identity, raw),
            })
        })
    }),
  }
}

fn rich_table_identity_contains_node(identity: &crate::TableIdentity, raw: u128) -> bool {
  identity.rows.iter().any(|row| {
    row.id.0 == raw
      || row.cells.iter().any(|cell| {
        cell.id.0 == raw
          || cell.blocks.iter().any(|block| match block {
            crate::TableCellBlockIdentity::Paragraph(paragraph) => paragraph.0 == raw,
            crate::TableCellBlockIdentity::Table { id, identity } => id.0 == raw || rich_table_identity_contains_node(identity, raw),
          })
      })
  })
}

fn patch_root_rich_object(source: &FlowDocument, document: &mut Document, block_id: BlockId) -> io::Result<ProjectionImpact> {
  let block_ix = document
    .ids
    .block_ids
    .iter()
    .position(|candidate| *candidate == block_id)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "changed rich root object is not in projection"))?;
  let graph = source
    .materialize_object_graph(flow_node_id_from_block(block_id))
    .map_err(collab_to_io)?;
  let block = rich_blocks::materialize_object_graph(&graph)?;
  let identity = rich_blocks::materialize_object_graph_identity(&graph)?;
  Arc::make_mut(&mut document.blocks)[block_ix] = block;
  document.ids.rich_block_ids.insert(block_id, identity);
  let paragraph = document.blocks[..block_ix]
    .iter()
    .filter(|block| matches!(block, Block::Paragraph(_)))
    .count();
  Ok(ProjectionImpact {
    replaced_blocks_before: block_ix..block_ix + 1,
    replacement_blocks_after: block_ix..block_ix + 1,
    affected_paragraphs_before: paragraph..paragraph,
    affected_paragraphs_after: paragraph..paragraph,
  })
}

fn root_block_range_for_unicode(document: &Document, changed: Range<usize>) -> io::Result<Range<usize>> {
  if document.blocks.is_empty() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 projection has no root blocks"));
  }
  let mut starts = Vec::with_capacity(document.blocks.len());
  let mut unicode = 0usize;
  let mut paragraph_ix = 0usize;
  for block in document.blocks.iter() {
    starts.push(unicode);
    unicode += 1;
    if matches!(block, Block::Paragraph(_)) {
      unicode += paragraph_text(document, paragraph_ix).chars().count();
      paragraph_ix += 1;
    }
  }
  let nearest = changed.start.min(unicode.saturating_sub(1));
  let containing = starts.partition_point(|start| *start <= nearest).saturating_sub(1);
  let starts_at_token = starts.binary_search(&changed.start).is_ok();
  let start = if starts_at_token {
    containing.saturating_sub(1)
  } else {
    containing
  };
  let search_end = changed.end.min(unicode).max(starts[containing] + 1);
  let end = starts.partition_point(|block_start| *block_start < search_end).max(start + 1);
  Ok(start..end.min(document.blocks.len()))
}

pub fn materialize_db8_flow_document(source: &FlowDocument, assets: AssetStore) -> io::Result<Document> {
  let materialized = source.materialize().map_err(collab_to_io)?;
  let metadata: Db8FlowMetadata = postcard::from_bytes(&materialized.document_metadata)
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
  let root = materialized
    .flows
    .get(&materialized.root_flow_id)
    .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "DB8 vNext root flow missing"))?;
  let mut paragraph_inputs = Vec::new();
  let mut paragraph_ids = Vec::new();
  for node in &root.nodes {
    let FlowNode::Paragraph { record, text, marks } = node else {
      continue;
    };
    let (style, _) = deserialize_paragraph_metadata(&record.metadata)
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "DB8 vNext paragraph metadata invalid"))?;
    let runs = db8_runs_from_marks(text.len(), &granular_marks(marks));
    paragraph_inputs.push(DocumentParagraphInput {
      style,
      runs: document_run_inputs(text, &runs)?,
    });
    paragraph_ids.push(paragraph_id(record.id));
  }
  if paragraph_inputs.is_empty() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 vNext root flow has no paragraphs"));
  }

  let mut theme = DocumentTheme::default();
  metadata.style_manifest.apply_to_theme(&mut theme);
  let mut document = document_from_paragraphs(theme, paragraph_inputs);
  document.assets = assets;
  document.ids.document_id = materialized.document_id.0.as_u128();
  document.ids.paragraph_ids = paragraph_ids;

  let mut blocks = Vec::with_capacity(root.nodes.len());
  let mut block_ids = Vec::with_capacity(root.nodes.len());
  let mut rich_block_ids = rustc_hash::FxHashMap::default();
  let mut paragraph_iter = document.paragraphs.iter();
  for node in &root.nodes {
    match node {
      FlowNode::Paragraph { record, .. } => {
        let paragraph = paragraph_iter
          .next()
          .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "DB8 vNext paragraph projection missing"))?;
        blocks.push(Block::Paragraph(paragraph.clone()));
        block_ids.push(BlockId(record.id.0.as_u128()));
      },
      FlowNode::Object { record } => {
        let block_id = BlockId(record.id.0.as_u128());
        blocks.push(rich_blocks::materialize_object(record, &materialized)?);
        rich_block_ids.insert(block_id, rich_blocks::materialize_object_identity(record, &materialized)?);
        block_ids.push(block_id);
      },
    }
  }
  document.blocks = Arc::new(blocks);
  document.ids = DocumentIds {
    document_id: materialized.document_id.0.as_u128(),
    paragraph_ids: document.ids.paragraph_ids,
    block_ids,
    rich_block_ids,
  };
  rebuild_document_sections(&mut document);
  validate_document_invariants(&document).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
  Ok(document)
}

fn flow_marks_from_runs(runs: &[TextRun]) -> Vec<FlowInlineMark> {
  let mut marks = Vec::new();
  let mut start = 0;
  for run in runs {
    let end = start + run.len;
    if run.styles.semantic != RunSemanticStyle::Plain {
      marks.push(mark(start..end, MARK_SEMANTIC, FlowMarkValue::I64(semantic_code(run.styles.semantic))));
    }
    if run.styles.direct_underline {
      marks.push(mark(start..end, MARK_DIRECT_UNDERLINE, FlowMarkValue::Bool(true)));
    }
    if run.styles.strikethrough {
      marks.push(mark(start..end, MARK_STRIKETHROUGH, FlowMarkValue::Bool(true)));
    }
    if let Some(highlight) = run.styles.highlight {
      marks.push(mark(start..end, MARK_HIGHLIGHT, FlowMarkValue::I64(i64::from(highlight_code(highlight)))));
    }
    start = end;
  }
  marks
}

fn flow_marks_for_styles(styles: RunStyles) -> Vec<(String, FlowMarkValue)> {
  let mut marks = Vec::with_capacity(4);
  if styles.semantic != RunSemanticStyle::Plain {
    marks.push((MARK_SEMANTIC.to_string(), FlowMarkValue::I64(semantic_code(styles.semantic))));
  }
  if styles.direct_underline {
    marks.push((MARK_DIRECT_UNDERLINE.to_string(), FlowMarkValue::Bool(true)));
  }
  if styles.strikethrough {
    marks.push((MARK_STRIKETHROUGH.to_string(), FlowMarkValue::Bool(true)));
  }
  if let Some(highlight) = styles.highlight {
    marks.push((
      MARK_HIGHLIGHT.to_string(),
      FlowMarkValue::I64(i64::from(highlight_code(highlight))),
    ));
  }
  marks
}

fn flow_text_inserts(paragraph: &InputParagraph) -> Vec<FlowTextInsert> {
  paragraph
    .runs
    .iter()
    .filter(|run| !run.text.is_empty())
    .map(|run| FlowTextInsert {
      text: run.text.clone(),
      marks: flow_marks_for_styles(run.styles),
    })
    .collect()
}

fn flow_style_keys() -> Vec<String> {
  [MARK_SEMANTIC, MARK_DIRECT_UNDERLINE, MARK_STRIKETHROUGH, MARK_HIGHLIGHT]
    .into_iter()
    .map(str::to_string)
    .collect()
}

fn granular_marks(marks: &[FlowInlineMark]) -> Vec<flowstate_collab::GranularTextMark> {
  marks
    .iter()
    .map(|mark| flowstate_collab::GranularTextMark {
      start_utf8: mark.range_utf8.start,
      end_utf8: mark.range_utf8.end,
      key: mark.key.clone(),
      value: match &mark.value {
        FlowMarkValue::Bool(value) => flowstate_collab::GranularValue::Bool(*value),
        FlowMarkValue::I64(value) => flowstate_collab::GranularValue::I64(*value),
        FlowMarkValue::String(value) => flowstate_collab::GranularValue::String(value.clone()),
      },
    })
    .collect()
}

fn document_run_inputs(text: &str, runs: &[TextRun]) -> io::Result<Vec<DocumentRunInput>> {
  let mut offset = 0;
  let mut inputs = Vec::with_capacity(runs.len());
  for run in runs {
    let end = offset + run.len;
    let run_text = text
      .get(offset..end)
      .ok_or_else(|| io::Error::new(io::ErrorKind::InvalidData, "DB8 vNext run range invalid"))?;
    inputs.push(DocumentRunInput {
      text: run_text.to_string(),
      styles: run.styles,
    });
    offset = end;
  }
  if offset != text.len() {
    return Err(io::Error::new(io::ErrorKind::InvalidData, "DB8 vNext runs do not cover paragraph text"));
  }
  Ok(inputs)
}

fn mark(range_utf8: std::ops::Range<usize>, key: &str, value: FlowMarkValue) -> FlowInlineMark {
  FlowInlineMark {
    range_utf8,
    key: key.to_string(),
    value,
  }
}

const fn flow_node_id(id: ParagraphId) -> FlowNodeId {
  FlowNodeId(uuid::Uuid::from_u128(id.0))
}

const fn flow_node_id_from_block(id: BlockId) -> FlowNodeId {
  FlowNodeId(uuid::Uuid::from_u128(id.0))
}

const fn paragraph_id(id: FlowNodeId) -> ParagraphId {
  ParagraphId(id.0.as_u128())
}

fn semantic_code(style: RunSemanticStyle) -> i64 {
  match style {
    RunSemanticStyle::Plain => 0,
    RunSemanticStyle::Custom(value) => i64::from(value) + 1,
  }
}

const fn highlight_code(style: crate::HighlightStyle) -> u8 {
  match style {
    crate::HighlightStyle::Custom(value) => value,
  }
}

fn collab_to_io(error: flowstate_collab::CollabError) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, error)
}

fn invalid_data(error: impl std::error::Error + Send + Sync + 'static) -> io::Error {
  io::Error::new(io::ErrorKind::InvalidData, error)
}

#[cfg(test)]
mod incremental_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod undo_tests;
