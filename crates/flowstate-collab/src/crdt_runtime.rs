use std::{
  collections::BTreeMap,
  io,
  path::{Path, PathBuf},
  sync::{Arc, Mutex},
};

use anyhow::{Context as _, Result};
use flowstate_document::{
  AssetId, AssetRecord, BLOCKS_BY_ID, Block, CollabPatch, CollabStructuralBlock, DEFAULT_UPDATE_SEGMENT_COMPACTION_THRESHOLD, DocumentProjection, DocumentPackage,
  FLOW_ATTRS_KEY, FLOW_ID_KEY, FLOW_KIND_KEY, FLOW_TEXT_KEY, FLOWS_BY_ID, InputBlock, InputBlockAlignment, InputEquationDisplay,
  InputImageSizing, InputParagraph, InputTableBlock, InputTableCell, InputTableCellBlock, InputTableColumnWidth, InputTableRow,
  MAIN_BODY_BLOCK_ID, MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE, MARK_PARAGRAPH_STYLE, MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH,
  OBJECT_REPLACEMENT, PARAGRAPHS_BY_ID, ParagraphStyle, ROOT, ROOT_BODY_FLOW_ID, ROOT_FIRST_PARAGRAPH_ID, RunSemanticStyle, RunStyles,
  SENTINEL_NEWLINE, document_from_loro, document_to_loro,
  loro_import::assets_from_document,
  loro_schema::body_text,
  new_loro_document,
};
use gpui_flowtext::SemanticEditCommand as EditorSemanticCommand;
use loro::{
  Container, ExportMode, Frontiers, ImportStatus, LoroDoc, LoroMap, LoroMovableList, LoroText, LoroValue, Subscription, UndoItemMeta, UndoManager,
  ValueOrContainer, VersionRange, VersionVector,
  cursor::{Cursor, Side},
  event::{Diff, DiffEvent},
};
use rustc_hash::FxHashMap;
use uuid::Uuid;

#[path = "crdt_runtime/types.rs"]
mod types;
#[path = "crdt_runtime/projection_patch.rs"]
mod projection_patch;
pub use types::{
  ProjectionFallbackStats, ProjectionInvalidation, ProjectionTextRange, RuntimeAssetMetadata, RuntimeEvent, RuntimePresenceCaretRequest,
  RuntimePresenceCarets, RuntimeRevisionInfo, SemanticCommand, UndoSelectionAffinity, UndoSelectionDirection, UndoSelectionSnapshot,
};
use projection_patch::{
  body_input_paragraph, projection_patches_between, remote_body_projection_patches, remote_nonstructural_projection_patches,
};
use types::UndoSelectionState;
use crate::presence::{
  PresenceSelection, SelectionAffinity, SelectionDirection, SelectionEndpoint, VisualGravity,
};
use gpui_flowtext::{
  DocumentOffset, EditorSelection, ExternalCaret, apply_projection_patches, global_byte, global_to_document_offset,
};
use loro::{
  ContainerTrait as _,
  cursor::PosType,
};

#[derive(Debug)]
pub struct CrdtRuntime {
  doc: LoroDoc,
  projection: DocumentProjection,
  projection_index: ProjectionRuntimeIndex,
  undo: UndoManager,
  defer_undo_checkpoints: bool,
  undo_checkpoint_pending: bool,
  package: Option<DocumentPackage>,
  package_path: Option<PathBuf>,
  package_journal_prepared: bool,
  last_persisted_frontier: Frontiers,
  last_persisted_vv: VersionVector,
  undo_selection: Arc<Mutex<UndoSelectionState>>,
  subscription_events: Arc<Mutex<Vec<SubscriptionEventSummary>>>,
  local_subscription_updates: Arc<Mutex<Vec<Vec<u8>>>>,
  projection_fallback_counts: Mutex<BTreeMap<String, u64>>,
  _root_subscription: Subscription,
  _local_update_subscription: Subscription,
}

#[derive(Debug, Default)]
struct ProjectionRuntimeIndex {
  paragraph_body_unicode_starts: Vec<usize>,
  paragraph_boundary_positions: Vec<usize>,
  object_placeholder_positions: Vec<usize>,
}

impl ProjectionRuntimeIndex {
  fn from_projection(projection: &DocumentProjection) -> Self {
    let mut index = Self::default();
    let mut body_unicode = 1usize;
    let mut paragraph_ix = 0usize;
    let mut has_body_content = false;

    for block in projection.blocks.iter() {
      match block {
        Block::Paragraph(_) => {
          if has_body_content {
            index.paragraph_boundary_positions.push(body_unicode);
            body_unicode = body_unicode.saturating_add(1);
          } else {
            index.paragraph_boundary_positions.push(0);
          }
          index.paragraph_body_unicode_starts.push(body_unicode);
          body_unicode = body_unicode.saturating_add(
            flowstate_document::paragraph_text(projection, paragraph_ix)
              .chars()
              .count(),
          );
          paragraph_ix = paragraph_ix.saturating_add(1);
          has_body_content = true;
        },
        Block::Image(_) | Block::Equation(_) | Block::Table(_) => {
          index.object_placeholder_positions.push(body_unicode);
          body_unicode = body_unicode.saturating_add(1);
          has_body_content = true;
        },
      }
    }
    index
  }

  fn body_unicode_for_offset(&self, projection: &DocumentProjection, offset: DocumentOffset) -> Option<usize> {
    let paragraph = projection.paragraphs.get(offset.paragraph)?;
    let paragraph_text = flowstate_document::paragraph_text(projection, offset.paragraph);
    let byte = offset.byte.min(flowstate_document::paragraph_text_len(paragraph));
    if !paragraph_text.is_char_boundary(byte) {
      return None;
    }
    Some(*self.paragraph_body_unicode_starts.get(offset.paragraph)? + paragraph_text[..byte].chars().count())
  }

  fn paragraphs_for_changed_ranges(&self, ranges: &[ProjectionTextRange], paragraph_count: usize) -> Vec<usize> {
    let mut touched = std::collections::BTreeSet::new();
    for range in ranges.iter().filter(|range| range.flow_id == ROOT_BODY_FLOW_ID) {
      let start = self.paragraph_at_body_unicode(range.unicode_start, paragraph_count);
      let end = self.paragraph_at_body_unicode(range.unicode_start.saturating_add(range.unicode_len), paragraph_count);
      if let Some(start) = start {
        touched.insert(start);
      }
      if let Some(end) = end {
        touched.insert(end);
      }
      if let (Some(start), Some(end)) = (start, end) {
        touched.extend(start.min(end)..=start.max(end));
      }
    }
    touched.into_iter().collect()
  }

  fn paragraph_at_body_unicode(&self, unicode: usize, paragraph_count: usize) -> Option<usize> {
    if paragraph_count == 0 || self.paragraph_body_unicode_starts.is_empty() {
      return None;
    }
    match self.paragraph_body_unicode_starts.binary_search(&unicode) {
      Ok(ix) => Some(ix.min(paragraph_count - 1)),
      Err(0) => Some(0),
      Err(ix) => Some((ix - 1).min(paragraph_count - 1)),
    }
  }

  fn deleted_range_contains_structure(&self, start: usize, len: usize) -> bool {
    if len == 0 {
      return false;
    }
    let end = start.saturating_add(len);
    self
      .paragraph_boundary_positions
      .iter()
      .chain(&self.object_placeholder_positions)
      .any(|position| (start..end).contains(position))
  }

  fn update_for_patches(&mut self, projection: &DocumentProjection, patches: &[CollabPatch]) -> bool {
    let mut text_deltas = Vec::new();
    let mut rebuild = false;
    for patch in patches {
      match patch {
        CollabPatch::ParagraphText { row, new, .. } => {
          let Some(paragraph_ix) = paragraph_index_for_block_row(projection, *row) else {
            rebuild = true;
            break;
          };
          let old_len = flowstate_document::paragraph_text(projection, paragraph_ix).chars().count();
          let new_len = new.runs.iter().map(|run| run.text.chars().count()).sum::<usize>();
          text_deltas.push((paragraph_ix, new_len as isize - old_len as isize));
        },
        CollabPatch::InsertBlocks { .. } | CollabPatch::DeleteBlocks { .. } | CollabPatch::MoveBlock { .. } => {
          rebuild = true;
          break;
        },
        CollabPatch::ParagraphStyle { .. }
        | CollabPatch::ParagraphRuns { .. }
        | CollabPatch::ReplaceObjectBlock { .. }
        | CollabPatch::AssetArrived { .. } => {},
      }
    }
    if rebuild {
      return true;
    }
    for (paragraph_ix, delta) in text_deltas {
      if delta == 0 {
        continue;
      }
      for start in self.paragraph_body_unicode_starts.iter_mut().skip(paragraph_ix.saturating_add(1)) {
        *start = start.saturating_add_signed(delta);
      }
      for boundary in self.paragraph_boundary_positions.iter_mut().skip(paragraph_ix.saturating_add(1)) {
        *boundary = boundary.saturating_add_signed(delta);
      }
      let threshold = self
        .paragraph_body_unicode_starts
        .get(paragraph_ix)
        .copied()
        .unwrap_or_default();
      for placeholder in self.object_placeholder_positions.iter_mut().filter(|position| **position > threshold) {
        *placeholder = placeholder.saturating_add_signed(delta);
      }
    }
    false
  }
}

fn paragraph_index_for_block_row(projection: &DocumentProjection, row: usize) -> Option<usize> {
  matches!(projection.blocks.get(row), Some(Block::Paragraph(_))).then(|| {
    projection
      .blocks
      .iter()
      .take(row)
      .filter(|block| matches!(block, Block::Paragraph(_)))
      .count()
  })
}

impl CrdtRuntime {
  pub fn new_empty(title: &str) -> Result<Self> {
    let doc = new_loro_document(title).context("initializing Loro document")?;
    Self::from_doc(doc, None, None)
  }

  pub fn open_package(path: impl AsRef<Path>) -> Result<Self> {
    let path = path.as_ref();
    let package = DocumentPackage::read(path).with_context(|| format!("reading Flowstate package {}", path.display()))?;
    let projection = package
      .current_projection_document()
      .context("reading frontier-matched package projection cache")?;
    let doc = package.load_loro_doc().context("loading Loro document from package")?;
    let mut runtime = Self::from_doc_with_projection(doc, Some(package), Some(path.to_path_buf()), projection)?;
    runtime.package_journal_prepared = true;
    Ok(runtime)
  }

  pub fn from_package(package: DocumentPackage, package_path: Option<PathBuf>) -> Result<Self> {
    let projection = package
      .current_projection_document()
      .context("reading frontier-matched package projection cache")?;
    let doc = package.load_loro_doc().context("loading Loro document from package")?;
    Self::from_doc_with_projection(doc, Some(package), package_path, projection)
  }

  pub fn from_document_projection(document: &DocumentProjection, title: &str) -> Result<Self> {
    let doc = document_to_loro(document, title).context("importing projected document into canonical Loro runtime")?;
    let package = DocumentPackage::from_loro_snapshot_with_assets(&doc, title, assets_from_document(document))
      .context("creating Loro-native package from projected document")?;
    Self::from_doc(doc, Some(package), None)
  }

  pub fn from_doc(doc: LoroDoc, package: Option<DocumentPackage>, package_path: Option<PathBuf>) -> Result<Self> {
    Self::from_doc_with_projection(doc, package, package_path, None)
  }

  fn from_doc_with_projection(
    doc: LoroDoc,
    mut package: Option<DocumentPackage>,
    package_path: Option<PathBuf>,
    projection: Option<DocumentProjection>,
  ) -> Result<Self> {
    persist_body_paragraph_style_mark_repair(&doc, package.as_mut(), package_path.as_deref())?;
    let current_frontier = doc.state_frontiers().encode();
    let projection_cache_matches = package
      .as_ref()
      .and_then(|package| package.manifest.projection_cache_frontier.as_deref())
      == Some(current_frontier.as_slice());
    let mut projection = match projection {
      Some(projection) if projection_cache_matches => projection,
      None => document_from_loro(&doc).context("building initial projection from canonical Loro state")?,
      Some(_) => document_from_loro(&doc).context("rebuilding stale package projection cache")?,
    };
    if let Some(package) = &package {
      attach_package_assets(&mut projection, package);
    }
    let last_persisted_frontier = doc.state_frontiers();
    let last_persisted_vv = doc.state_vv();
    let subscription_events = Arc::new(Mutex::new(Vec::new()));
    let subscription_events_for_callback = Arc::clone(&subscription_events);
    let root_subscription = doc.subscribe_root(Arc::new(move |event: DiffEvent<'_>| {
      let summary = summarize_subscription_event(&event);
      tracing::trace!(origin = %summary.origin, trigger = %summary.triggered_by, changes = summary.changes.len(), "Flowstate Loro root event");
      if let Ok(mut events) = subscription_events_for_callback.lock() {
        events.push(summary);
      }
    }));
    let local_subscription_updates = Arc::new(Mutex::new(Vec::new()));
    let local_updates_for_callback = Arc::clone(&local_subscription_updates);
    let local_update_subscription = doc.subscribe_local_update(Box::new(move |bytes| {
      tracing::trace!(bytes = bytes.len(), "Flowstate Loro local update");
      if let Ok(mut updates) = local_updates_for_callback.lock() {
        updates.push(bytes.clone());
      }
      true
    }));
    let mut undo = UndoManager::new(&doc);
    undo.set_merge_interval(600);
    undo.set_max_undo_steps(300);
    undo.add_exclude_origin_prefix("remote");
    let undo_selection = Arc::new(Mutex::new(UndoSelectionState::default()));
    install_undo_selection_callbacks(&mut undo, &undo_selection);
    let projection_index = ProjectionRuntimeIndex::from_projection(&projection);
    Ok(Self {
      doc,
      projection,
      projection_index,
      undo,
      defer_undo_checkpoints: false,
      undo_checkpoint_pending: false,
      package,
      package_path,
      package_journal_prepared: false,
      last_persisted_frontier,
      last_persisted_vv,
      undo_selection,
      subscription_events,
      local_subscription_updates,
      projection_fallback_counts: Mutex::new(BTreeMap::new()),
      _root_subscription: root_subscription,
      _local_update_subscription: local_update_subscription,
    })
  }

  pub(crate) fn doc(&self) -> &LoroDoc {
    &self.doc
  }

  pub fn set_pending_undo_selection(&mut self, selection: Option<UndoSelectionSnapshot>) -> Result<()> {
    let pending_selection = selection
      .map(|selection| postcard::to_stdvec(&selection).context("encoding undo selection snapshot failed"))
      .transpose()?;
    if let Ok(mut state) = self.undo_selection.lock() {
      state.pending_selection = pending_selection;
    }
    Ok(())
  }

  pub fn take_restored_undo_selection(&mut self) -> Option<UndoSelectionSnapshot> {
    self
      .undo_selection
      .lock()
      .ok()
      .and_then(|mut state| state.restored_selection.take())
  }

  fn record_undo_checkpoint(&mut self) -> Result<()> {
    if self.defer_undo_checkpoints {
      self.undo_checkpoint_pending = true;
      return Ok(());
    }
    self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")
  }

  fn undo_selection_for_editor(&self, selection: &EditorSelection) -> Option<UndoSelectionSnapshot> {
    let direction = selection_direction(selection.anchor, selection.head);
    let (anchor_affinity, head_affinity, _, _) = endpoint_intent(direction);
    let body = body_text(&self.doc);
    let anchor = clamp_projection_offset(&self.projection, selection.anchor);
    let head = clamp_projection_offset(&self.projection, selection.head);
    let anchor_pos = self.projection_index.body_unicode_for_offset(&self.projection, anchor)?;
    let head_pos = self.projection_index.body_unicode_for_offset(&self.projection, head)?;
    let anchor_cursor = body.get_cursor(anchor_pos, side_for_affinity(anchor_affinity))?.encode();
    let head_cursor = body.get_cursor(head_pos, side_for_affinity(head_affinity))?.encode();
    Some(UndoSelectionSnapshot {
      anchor_cursor,
      head_cursor,
      anchor_affinity: undo_affinity(anchor_affinity),
      head_affinity: undo_affinity(head_affinity),
      direction: match direction {
        SelectionDirection::Forward => UndoSelectionDirection::Forward,
        SelectionDirection::Backward => UndoSelectionDirection::Backward,
        SelectionDirection::None => UndoSelectionDirection::None,
      },
    })
  }

  pub fn apply_editor_semantic_command(&mut self, projection: &DocumentProjection, command: &EditorSemanticCommand) -> Result<Vec<RuntimeEvent>> {
    self.apply_editor_semantic_command_with_projection(projection, command, true)
  }

  pub fn apply_editor_semantic_command_without_projection(
    &mut self,
    projection: &DocumentProjection,
    command: &EditorSemanticCommand,
  ) -> Result<Vec<RuntimeEvent>> {
    self.apply_editor_semantic_command_with_projection(projection, command, false)
  }

  pub fn try_apply_editor_semantic_command_without_projection(
    &mut self,
    command: &EditorSemanticCommand,
  ) -> Result<Option<Vec<RuntimeEvent>>> {
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    if apply_editor_semantic_command_body_fast_path(&self.doc, &self.projection, &self.projection_index, command)? {
      self.record_undo_checkpoint()?;
      let mut invalidation = ProjectionInvalidation::body_text(
        from_frontier.encode(),
        self.doc.state_frontiers().encode(),
        0,
        body_text(&self.doc).len_unicode(),
      );
      self.merge_subscription_invalidation(&mut invalidation);
      let mut events = self.events_after_local_change(from_frontier, from_vv, invalidation.clone(), false)?;
      if let Some(patches) = incremental_projection_patches_for_command(&self.projection, &self.doc, command) {
        self.apply_projection_patch_set(&patches);
        self.projection.frontier = self.doc.state_frontiers().encode();
        events.push(self.projection_patched_event(patches, invalidation));
      } else {
        let before_projection = self.projection.clone();
        self.refresh_projection()?;
        events.push(self.projection_change_event(&before_projection, invalidation)?);
      }
      return Ok(Some(events));
    }
    Ok(None)
  }

  fn apply_editor_semantic_command_with_projection(
    &mut self,
    projection: &DocumentProjection,
    command: &EditorSemanticCommand,
    emit_projection: bool,
  ) -> Result<Vec<RuntimeEvent>> {
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    if apply_editor_semantic_command(&self.doc, projection, command)? {
      self.record_undo_checkpoint()?;
      let mut invalidation = editor_command_invalidation(
        projection,
        command,
        from_frontier.encode(),
        self.doc.state_frontiers().encode(),
      );
      self.merge_subscription_invalidation(&mut invalidation);
      let mut events = self.events_after_local_change(from_frontier, from_vv, invalidation.clone(), false)?;
      if emit_projection {
        if let Some(patches) = incremental_projection_patches_for_command(&self.projection, &self.doc, command) {
          self.apply_projection_patch_set(&patches);
          self.projection.frontier = self.doc.state_frontiers().encode();
          events.push(self.projection_patched_event(patches, invalidation));
        } else {
          let before_projection = self.projection.clone();
          self.refresh_projection()?;
          events.push(self.projection_change_event(&before_projection, invalidation)?);
        }
      } else {
        self.refresh_projection()?;
      }
      Ok(events)
    } else {
      Ok(Vec::new())
    }
  }

  pub fn projection_snapshot(&self) -> Result<DocumentProjection> {
    Ok(self.projection.clone())
  }

  pub fn asset_metadata(&self) -> Result<Vec<RuntimeAssetMetadata>> {
    let root = self.doc.get_map(ROOT);
    let Some(ValueOrContainer::Container(Container::Map(assets_by_id))) = root.get(flowstate_document::loro_schema::ASSETS_BY_ID) else {
      return Ok(Vec::new());
    };
    let mut assets = Vec::new();
    for key in assets_by_id.keys() {
      let Some(ValueOrContainer::Container(Container::Map(map))) = assets_by_id.get(&key) else {
        continue;
      };
      let Some(asset_id) = map_string_opt(&map, "asset_id").and_then(|value| value.parse::<u128>().ok()) else {
        continue;
      };
      let byte_length = map_i64_opt(&map, "byte_length").unwrap_or_default().max(0) as u64;
      let Some(content_hash) = map_string_opt(&map, "content_hash").and_then(|hash| parse_blake3_hex(&hash)) else {
        tracing::warn!(asset_id, "ignoring asset metadata with an invalid BLAKE3 digest");
        continue;
      };
      if byte_length == 0 {
        continue;
      }
      assets.push(RuntimeAssetMetadata {
        asset_id,
        content_hash,
        mime_type: map_string_opt(&map, "mime_type").unwrap_or_else(|| "application/octet-stream".to_string()),
        original_name: map_string_opt(&map, "original_name"),
        byte_length,
      });
    }
    Ok(assets)
  }

  pub fn revisions(&self) -> Vec<RuntimeRevisionInfo> {
    self
      .package
      .as_ref()
      .map(|package| {
        package
          .revisions
          .iter()
          .rev()
          .map(|revision| RuntimeRevisionInfo {
            revision_id: revision.revision_id,
            title: revision.title.clone(),
            summary: revision.summary.clone(),
            created_at_unix_secs: revision.created_at_unix_secs,
          })
          .collect()
      })
      .unwrap_or_default()
  }

  pub fn presence_selection(&self, selection: &EditorSelection) -> Option<PresenceSelection> {
    let direction = selection_direction(selection.anchor, selection.head);
    let (anchor_affinity, head_affinity, anchor_gravity, head_gravity) = endpoint_intent(direction);
    Some(PresenceSelection {
      anchor: self.presence_endpoint(selection.anchor, anchor_affinity, anchor_gravity)?,
      head: self.presence_endpoint(selection.head, head_affinity, head_gravity)?,
      direction,
    })
  }

  pub fn resolve_presence_carets(&self, requests: Vec<RuntimePresenceCaretRequest>) -> RuntimePresenceCarets {
    let text = body_text(&self.doc);
    let carets = requests
      .into_iter()
      .filter_map(|request| {
        let cursor = Cursor::decode(&request.selection.head.cursor).ok()?;
        if cursor.container != text.id() {
          return None;
        }
        let resolved = self.doc.get_cursor_pos(&cursor).ok()?;
        let byte = text.convert_pos(resolved.current.pos, PosType::Unicode, PosType::Bytes)?;
        Some(ExternalCaret {
          offset: global_to_document_offset(&self.projection, byte),
          color_rgb: request.color_rgb,
        })
      })
      .collect();
    RuntimePresenceCarets { carets }
  }

  fn presence_endpoint(
    &self,
    offset: DocumentOffset,
    affinity: SelectionAffinity,
    visual_gravity: VisualGravity,
  ) -> Option<SelectionEndpoint> {
    let text = body_text(&self.doc);
    let byte = global_byte(&self.projection, offset).min(text.len_utf8());
    let pos = text.convert_pos(byte, PosType::Bytes, PosType::Unicode)?;
    text
      .get_cursor(pos, side_for_affinity(affinity))
      .map(|cursor| SelectionEndpoint {
        cursor: cursor.encode(),
        affinity,
        visual_gravity,
      })
  }

  pub fn merge_asset_records(&mut self, records: Vec<AssetRecord>) -> Result<Vec<RuntimeEvent>> {
    if records.is_empty() {
      return Ok(Vec::new());
    }
    let before = self.projection.clone();
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    let frontier_before = from_frontier.encode();
    for record in records {
      self.projection.assets.assets.insert(record.id, record);
    }
    flowstate_document::touch_document_metadata(&self.doc).context("updating canonical document metadata for asset change")?;
    flowstate_document::loro_import::import_assets(&self.doc, &self.projection).context("recording asset metadata in canonical Loro state")?;
    refresh_image_asset_metadata(&self.doc).context("refreshing image asset integrity metadata")?;
    self.doc.commit();
    if let Some(package) = &mut self.package {
      package.replace_assets_from_document(&self.projection)?;
      if let Some(path) = &self.package_path {
        package.append_assets_to_path(path)?;
      }
    }
    let mut invalidation = ProjectionInvalidation {
      frontier_before,
      frontier_after: self.doc.state_frontiers().encode(),
      changed_assets: self
        .projection
        .assets
        .assets
        .keys()
        .map(|id| id.0.to_string())
        .collect(),
      ..ProjectionInvalidation::default()
    };
    self.merge_subscription_invalidation(&mut invalidation);
    let mut events = self.events_after_local_change(from_frontier, from_vv, invalidation.clone(), false)?;
    events.push(self.projection_change_event(&before, invalidation)?);
    Ok(events)
  }

  pub fn apply_editor_commands(
    &mut self,
    commands: &[EditorSemanticCommand],
    selection_after: Option<&EditorSelection>,
  ) -> Result<Vec<RuntimeEvent>> {
    if commands.is_empty() {
      return Ok(Vec::new());
    }
    if let Some(selection) = selection_after.and_then(|selection| self.undo_selection_for_editor(selection)) {
      self.set_pending_undo_selection(Some(selection))?;
    }
    self.defer_undo_checkpoints = true;
    self.undo_checkpoint_pending = false;
    let result = (|| {
      let mut events = Vec::new();
      flowstate_document::touch_document_metadata(&self.doc)
        .context("updating canonical document metadata for editor command batch")?;
      for command in commands {
        let command_events = if let Some(events) = self.try_apply_editor_semantic_command_without_projection(command)? {
          events
        } else {
          let projection = self.projection.clone();
          self.apply_editor_semantic_command_with_projection(&projection, command, true)?
        };
        events.extend(command_events);
      }
      Ok(events)
    })();
    self.defer_undo_checkpoints = false;
    if result.is_ok() && self.undo_checkpoint_pending {
      self.undo.record_new_checkpoint().context("recording grouped Loro undo checkpoint")?;
    }
    self.undo_checkpoint_pending = false;
    result
  }

  pub fn command(&mut self, command: SemanticCommand) -> Result<Vec<RuntimeEvent>> {
    let restore_undo_selection = matches!(&command, SemanticCommand::Undo | SemanticCommand::Redo);
    let before_projection = self.projection.clone();
    let before_body = body_text(&self.doc).to_string();
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    let mutates_document = match &command {
      SemanticCommand::InsertText { text, .. } => !text.is_empty(),
      SemanticCommand::DeleteRange { unicode_len, .. } => *unicode_len > 0,
      SemanticCommand::OpenRevision { .. }
      | SemanticCommand::ForkRevision { .. }
      | SemanticCommand::Undo
      | SemanticCommand::Redo => false,
      _ => true,
    };
    if mutates_document {
      flowstate_document::touch_document_metadata(&self.doc).context("updating canonical document metadata for semantic command")?;
    }
    let projection_invalidation;
    match command {
      SemanticCommand::InsertText {
        unicode_index,
        text,
        styles,
      } => {
        if text.is_empty() {
          return Ok(Vec::new());
        }
        let body = body_text(&self.doc);
        let newline_boundaries = inserted_newline_boundaries(unicode_index, &text);
        body.insert(unicode_index, &text).context("inserting text into Loro body flow")?;
        let inserted_len = text.chars().count();
        if inserted_len > 0 {
          mark_run_styles(&body, unicode_index..unicode_index + inserted_len, styles).context("marking inserted run styles")?;
        }
        repair_paragraph_metadata_after_text_flow_edit(&self.doc, &body, &newline_boundaries, "semantic_insert_text")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation = ProjectionInvalidation::body_text(
          from_frontier.encode(),
          self.doc.state_frontiers().encode(),
          unicode_index,
          inserted_len,
        );
      }
      SemanticCommand::DeleteRange {
        unicode_index,
        unicode_len,
      } => {
        if unicode_len > 0 {
          let body = body_text(&self.doc);
          body
            .delete(unicode_index, unicode_len)
            .context("deleting text from Loro body flow")?;
          repair_paragraph_metadata_after_text_flow_edit(&self.doc, &body, &[], "semantic_delete_range")?;
          self.doc.commit();
          self.record_undo_checkpoint()?;
          projection_invalidation = ProjectionInvalidation::body_text(
            from_frontier.encode(),
            self.doc.state_frontiers().encode(),
            unicode_index,
            unicode_len,
          );
        } else {
          return Ok(Vec::new());
        }
      }
      SemanticCommand::SplitParagraph {
        unicode_index,
        inherited_style,
      } => {
        let body = body_text(&self.doc);
        body.insert(unicode_index, "\n").context("splitting Loro body paragraph")?;
        body
          .mark(unicode_index..unicode_index + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(inherited_style))
          .context("marking split paragraph boundary")?;
        repair_paragraph_metadata_after_text_flow_edit(&self.doc, &body, &[unicode_index], "semantic_split_paragraph")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation = ProjectionInvalidation::body_text(
          from_frontier.encode(),
          self.doc.state_frontiers().encode(),
          unicode_index,
          1,
        );
      }
      SemanticCommand::SetParagraphStyle {
        boundary_unicode_index,
        style,
      } => {
        let body = body_text(&self.doc);
        body
          .mark(
            boundary_unicode_index..boundary_unicode_index + 1,
            MARK_PARAGRAPH_STYLE,
            paragraph_style_value(style),
          )
          .context("marking paragraph style in Loro body flow")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation = ProjectionInvalidation::body_style(
          from_frontier.encode(),
          self.doc.state_frontiers().encode(),
          boundary_unicode_index,
          1,
        );
      }
      SemanticCommand::SetRunStyles { unicode_range, styles } => {
        if unicode_range.is_empty() {
          return Ok(Vec::new());
        }
        let unicode_start = unicode_range.start;
        let unicode_len = unicode_range.end.saturating_sub(unicode_range.start);
        mark_run_styles(&body_text(&self.doc), unicode_range, styles).context("marking run styles in Loro body flow")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation = ProjectionInvalidation::body_style(
          from_frontier.encode(),
          self.doc.state_frontiers().encode(),
          unicode_start,
          unicode_len,
        );
      }
      SemanticCommand::InsertImage {
        unicode_index,
        asset_id,
        alt_text,
        caption,
        sizing,
        alignment,
      } => {
        insert_image_block(&self.doc, unicode_index, asset_id, &alt_text, caption.as_deref(), sizing, alignment)
          .context("inserting image block into Loro document")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation = ProjectionInvalidation::body_object(
          from_frontier.encode(),
          self.doc.state_frontiers().encode(),
          unicode_index,
          "image",
        );
      }
      SemanticCommand::InsertEquation {
        unicode_index,
        source,
        display,
      } => {
        insert_equation_block(&self.doc, unicode_index, &source, display).context("inserting equation block into Loro document")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation = ProjectionInvalidation::body_object(
          from_frontier.encode(),
          self.doc.state_frontiers().encode(),
          unicode_index,
          "equation",
        );
      }
      SemanticCommand::InsertTable {
        unicode_index,
        rows,
        columns,
        column_widths,
        header_row,
      } => {
        insert_table_block(&self.doc, unicode_index, rows, columns, &column_widths, header_row)
          .context("inserting table block into Loro document")?;
        self.doc.commit();
        self.record_undo_checkpoint()?;
        projection_invalidation = ProjectionInvalidation::body_object(
          from_frontier.encode(),
          self.doc.state_frontiers().encode(),
          unicode_index,
          "table",
        );
      }
      SemanticCommand::OpenRevision { revision_id } => {
        let document = self.revision_projection(revision_id)?;
        return Ok(vec![RuntimeEvent::RevisionOpened {
          revision_id,
          document: Box::new(document),
        }]);
      }
      SemanticCommand::ForkRevision { revision_id } => {
        let (document, package) = self.fork_revision(revision_id)?;
        return Ok(vec![RuntimeEvent::RevisionForked {
          revision_id,
          document: Box::new(document),
          package: Box::new(package),
        }]);
      }
      SemanticCommand::Undo => {
        if !self.undo.undo().context("applying Loro undo")? {
          return Ok(Vec::new());
        }
        projection_invalidation = ProjectionInvalidation {
          frontier_before: from_frontier.encode(),
          frontier_after: self.doc.state_frontiers().encode(),
          changed_flows: vec![ROOT_BODY_FLOW_ID.to_string()],
          ..ProjectionInvalidation::default()
        };
      }
      SemanticCommand::Redo => {
        if !self.undo.redo().context("applying Loro redo")? {
          return Ok(Vec::new());
        }
        projection_invalidation = ProjectionInvalidation {
          frontier_before: from_frontier.encode(),
          frontier_after: self.doc.state_frontiers().encode(),
          changed_flows: vec![ROOT_BODY_FLOW_ID.to_string()],
          ..ProjectionInvalidation::default()
        };
      }
    }
    let mut projection_invalidation = projection_invalidation;
    self.merge_subscription_invalidation(&mut projection_invalidation);
    let mut events = self.events_after_local_change(from_frontier, from_vv, projection_invalidation.clone(), false)?;
    let after_body = body_text(&self.doc).to_string();
    if let Some(patches) = remote_body_projection_patches(
      &before_projection,
      &before_body,
      &after_body,
      &self.doc,
      &projection_invalidation,
    ) {
      self.apply_projection_patch_set(&patches);
      self.projection.frontier = self.doc.state_frontiers().encode();
      events.push(self.projection_patched_event(patches, projection_invalidation));
    } else {
      self.refresh_projection()?;
      let reason = if restore_undo_selection {
        "undo_redo_structural_projection_fallback"
      } else {
        "semantic_command_structural_projection_fallback"
      };
      events.push(self.projection_change_event(
        &before_projection,
        ProjectionInvalidation::full_rebuild(
          projection_invalidation.frontier_before,
          projection_invalidation.frontier_after,
          reason,
        ),
      )?);
    }
    if restore_undo_selection
      && let Some(snapshot) = self.take_restored_undo_selection()
    {
      if let Some(selection) = self.resolve_undo_selection(&snapshot) {
        events.push(RuntimeEvent::SelectionRestored { selection });
      } else if let Ok(mut state) = self.undo_selection.lock() {
        state.restored_selection = Some(snapshot);
      }
    }
    Ok(events)
  }

  fn resolve_undo_selection(&self, snapshot: &UndoSelectionSnapshot) -> Option<EditorSelection> {
    Some(EditorSelection {
      anchor: self.resolve_undo_cursor(&snapshot.anchor_cursor)?,
      head: self.resolve_undo_cursor(&snapshot.head_cursor)?,
    })
  }

  fn resolve_undo_cursor(&self, encoded: &[u8]) -> Option<DocumentOffset> {
    let cursor = Cursor::decode(encoded).ok()?;
    let body = body_text(&self.doc);
    if cursor.container != body.id() {
      return None;
    }
    let resolved = self.doc.get_cursor_pos(&cursor).ok()?;
    let byte = body.convert_pos(resolved.current.pos, PosType::Unicode, PosType::Bytes)?;
    Some(global_to_document_offset(&self.projection, byte))
  }

  pub fn revision_projection(&self, revision_id: u128) -> Result<DocumentProjection> {
    let revision_doc = self
      .package
      .as_ref()
      .context("cannot open revision without a package-backed runtime")?
      .load_revision_loro_doc(revision_id)
      .context("loading revision Loro snapshot")?;
    let mut document = document_from_loro(&revision_doc).context("projecting revision document")?;
    if let Some(package) = &self.package {
      attach_package_assets(&mut document, package);
    }
    Ok(document)
  }

  pub fn fork_revision(&self, revision_id: u128) -> Result<(DocumentProjection, DocumentPackage)> {
    let package = self.package.as_ref().context("cannot fork revision without a package-backed runtime")?;
    let revision_doc = package
      .load_revision_loro_doc(revision_id)
      .context("loading revision Loro snapshot for fork")?;
    let forked_doc = revision_doc.fork();
    flowstate_document::fork_document_lineage(&forked_doc).context("assigning forked document lineage")?;
    let forked_package = DocumentPackage::from_loro_snapshot_with_assets(&forked_doc, "Forked revision", package.assets.clone())
      .context("creating forked revision package")?;
    let mut document = document_from_loro(&forked_doc).context("projecting forked revision")?;
    attach_package_assets(&mut document, &forked_package);
    Ok((document, forked_package))
  }

  pub fn import_remote_update(&mut self, bytes: &[u8]) -> Result<Vec<RuntimeEvent>> {
    let from_frontier = self.doc.state_frontiers();
    let status = self.doc.import_with(bytes, "remote").context("importing remote Loro update")?;
    let after_remote_vv = self.doc.state_vv();
    let repair_update = if status.pending.is_none() && repair_missing_paragraph_style_marks(&self.doc)? {
      self.local_update_bytes(&after_remote_vv)?
    } else {
      Vec::new()
    };
    let frontier_after = self.doc.state_frontiers();
    let version_vector = self.doc.state_vv();
    let mut events = vec![RuntimeEvent::RemoteUpdateApplied {
      pending: status.pending.clone(),
      frontier: frontier_after.encode(),
      version_vector: version_vector.encode(),
    }];
    if !repair_update.is_empty() {
      events.push(RuntimeEvent::LocalUpdate {
        bytes: repair_update,
        frontier: frontier_after.encode(),
        version_vector: version_vector.encode(),
      });
    }
    let frontier_before = from_frontier.encode();
    let frontier_after = frontier_after.encode();
    if status.pending.is_none() {
      let mut invalidation = ProjectionInvalidation {
        frontier_before,
        frontier_after,
        changed_flows: vec![ROOT_BODY_FLOW_ID.to_string()],
        ..ProjectionInvalidation::default()
      };
      self.merge_subscription_invalidation(&mut invalidation);
      let touched_paragraphs = self
        .projection_index
        .paragraphs_for_changed_ranges(&invalidation.changed_text_ranges, self.projection.paragraphs.len());
      if let Some(patches) = remote_nonstructural_projection_patches(
        &self.projection,
        &self.doc,
        &invalidation,
        &touched_paragraphs,
      ) {
        self.apply_projection_patch_set(&patches);
        self.projection.frontier = self.doc.state_frontiers().encode();
        events.push(self.projection_patched_event(patches, invalidation));
      } else {
        let before_projection = self.projection.clone();
        self.refresh_projection()?;
        events.push(self.projection_change_event(&before_projection, invalidation)?);
      }
    } else {
      let mut invalidation = ProjectionInvalidation::full_rebuild(
        frontier_before,
        frontier_after,
        "remote_update_pending_projection_fallback",
      );
      self.merge_subscription_invalidation(&mut invalidation);
      self.refresh_projection()?;
      events.push(self.projection_event(invalidation)?);
    }
    if status.pending.is_none() {
      if let Some(package) = &mut self.package {
        package.sync_revisions_from_loro(&self.doc)?;
      }
      self.persist_update_from_last_frontier()?;
    }
    Ok(events)
  }

  fn projection_event(&self, invalidation: ProjectionInvalidation) -> Result<RuntimeEvent> {
    self.record_projection_fallback(&invalidation);
    Ok(RuntimeEvent::ProjectionUpdated {
      document: Box::new(self.projection_snapshot()?),
      invalidation,
      frontier: self.doc.state_frontiers().encode(),
      version_vector: self.doc.state_vv().encode(),
    })
  }

  pub fn export_updates_for(&self, remote_vv: &VersionVector) -> Result<Vec<u8>> {
    self
      .doc
      .export(ExportMode::updates(remote_vv))
      .context("exporting Loro updates for anti-entropy")
  }

  pub fn missing_dependency_request(status: &ImportStatus) -> Option<&VersionRange> {
    status.pending.as_ref()
  }

  pub fn save_package(&mut self) -> io::Result<()> {
    let Some(package) = &self.package else {
      return Ok(());
    };
    let Some(path) = &self.package_path else {
      return Ok(());
    };
    package.write(path)?;
    self.package_journal_prepared = true;
    Ok(())
  }

  fn projection_change_event(&self, before: &DocumentProjection, invalidation: ProjectionInvalidation) -> Result<RuntimeEvent> {
    if let Some(patches) = projection_patches_between(before, &self.projection) {
      self.record_projection_fallback(&invalidation);
      return Ok(RuntimeEvent::ProjectionPatched {
        patches,
        invalidation,
        frontier: self.doc.state_frontiers().encode(),
        version_vector: self.doc.state_vv().encode(),
      });
    }
    self.projection_event(ProjectionInvalidation::full_rebuild(
      invalidation.frontier_before,
      invalidation.frontier_after,
      "projection_diff_ambiguous",
    ))
  }

  fn projection_patched_event(&self, patches: Vec<flowstate_document::CollabPatch>, invalidation: ProjectionInvalidation) -> RuntimeEvent {
    RuntimeEvent::ProjectionPatched {
      patches,
      invalidation,
      frontier: self.doc.state_frontiers().encode(),
      version_vector: self.doc.state_vv().encode(),
    }
  }

  fn record_projection_fallback(&self, invalidation: &ProjectionInvalidation) {
    if !invalidation.rebuild_required {
      return;
    }
    let reason = invalidation.fallback_reason.unwrap_or("unspecified_projection_fallback");
    if let Ok(mut counts) = self.projection_fallback_counts.lock() {
      *counts.entry(reason.to_string()).or_default() += 1;
    }
    tracing::warn!(reason, "Flowstate projection used a full rebuild fallback");
  }

  pub fn projection_fallback_stats(&self) -> ProjectionFallbackStats {
    let by_reason = self
      .projection_fallback_counts
      .lock()
      .map(|counts| counts.clone())
      .unwrap_or_default();
    ProjectionFallbackStats {
      total: by_reason.values().copied().sum(),
      by_reason,
    }
  }

  fn refresh_projection(&mut self) -> Result<()> {
    let mut projection = document_from_loro(&self.doc).context("refreshing projection from canonical Loro state")?;
    if let Some(package) = &self.package {
      attach_package_assets(&mut projection, package);
    }
    projection.theme = self.projection.theme.clone();
    self.projection = projection;
    self.projection_index = ProjectionRuntimeIndex::from_projection(&self.projection);
    Ok(())
  }

  fn apply_projection_patch_set(&mut self, patches: &[CollabPatch]) {
    let rebuild_index = self.projection_index.update_for_patches(&self.projection, patches);
    apply_projection_patches(&mut self.projection, patches);
    if rebuild_index {
      self.projection_index = ProjectionRuntimeIndex::from_projection(&self.projection);
    }
  }

  pub fn save_package_to(&mut self, path: impl AsRef<Path>) -> io::Result<()> {
    self.package_path = Some(path.as_ref().to_path_buf());
    self.package_journal_prepared = false;
    self.save_package()
  }

  pub fn checkpoint_package(&mut self, title: &str, path: Option<PathBuf>) -> io::Result<()> {
    let revision_id = Uuid::new_v4().as_u128();
    let revision_frontiers = self.doc.state_frontiers();
    let revision_frontier = revision_frontiers.encode();
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    flowstate_document::touch_document_metadata(&self.doc)
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    flowstate_document::record_revision(
      &self.doc,
      revision_id,
      revision_frontier,
      title,
      "Explicit save",
      None,
    )
    .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    let mut revision_invalidation = ProjectionInvalidation::default();
    self.merge_subscription_invalidation(&mut revision_invalidation);
    let update = self
      .local_update_bytes(&from_vv)
      .map_err(|error| io::Error::other(error.to_string()))?;
    if !update.is_empty() {
      self
        .persist_update_segment(from_frontier, from_vv, update)
        .map_err(|error| io::Error::other(error.to_string()))?;
    }
    if self.package.is_none() {
      self.package = Some(DocumentPackage::from_loro_snapshot_with_assets(
        &self.doc,
        title,
        assets_from_document(&self.projection),
      )?);
    }
    let Some(package) = &mut self.package else {
      return Ok(());
    };
    package.replace_assets_from_document(&self.projection)?;
    package.rebuild_projection_cache_from_loro(&self.doc)?;
    package.rebuild_search_units_from_loro(&self.doc)?;
    package.compact_to_snapshot(&self.doc)?;
    package.create_named_revision_at_with_id(
      &self.doc,
      revision_id,
      &revision_frontiers,
      title,
      "Explicit save",
      None,
      Some(self.doc.peer_id() as u128),
    )?;
    if let Some(path) = path {
      self.package_path = Some(path);
      self.package_journal_prepared = false;
    }
    self.save_package()
  }

  pub fn package_bytes(&mut self, title: &str) -> io::Result<Vec<u8>> {
    if self.package.is_none() {
      self.package = Some(DocumentPackage::from_loro_snapshot_with_assets(
        &self.doc,
        title,
        assets_from_document(&self.projection),
      )?);
    }
    let Some(package) = &mut self.package else {
      return Err(io::Error::other("runtime package was not initialized"));
    };
    package.replace_assets_from_document(&self.projection)?;
    package.rebuild_projection_cache_from_loro(&self.doc)?;
    package.rebuild_search_units_from_loro(&self.doc)?;
    package.to_bytes()
  }

  fn events_after_local_change(
    &mut self,
    from_frontier: Frontiers,
    from_vv: VersionVector,
    invalidation: ProjectionInvalidation,
    emit_projection: bool,
  ) -> Result<Vec<RuntimeEvent>> {
    let update = self.local_update_bytes(&from_vv)?;
    let mut events = Vec::new();
    if !update.is_empty() {
      self.persist_update_segment(from_frontier, from_vv, update.clone())?;
      events.push(RuntimeEvent::LocalUpdate {
        bytes: update,
        frontier: self.doc.state_frontiers().encode(),
        version_vector: self.doc.state_vv().encode(),
      });
    }
    if emit_projection {
      events.push(self.projection_event(invalidation)?);
    }
    Ok(events)
  }

  fn local_update_bytes(&self, from_vv: &VersionVector) -> Result<Vec<u8>> {
    let mut subscribed = self
      .local_subscription_updates
      .lock()
      .map(|mut updates| std::mem::take(&mut *updates))
      .unwrap_or_default();
    if subscribed.len() == 1 {
      return Ok(subscribed.pop().unwrap_or_default());
    }
    self
      .doc
      .export(ExportMode::updates(from_vv))
      .context("exporting local Loro update fallback")
  }

  fn merge_subscription_invalidation(&self, invalidation: &mut ProjectionInvalidation) {
    let summaries = self
      .subscription_events
      .lock()
      .map(|mut events| std::mem::take(&mut *events))
      .unwrap_or_default();
    let body_target = body_text(&self.doc).id().to_string();
    for summary in summaries {
      for change in summary.changes {
        match change {
          SubscriptionChange::Text {
            target,
            unicode_start,
            unicode_len,
            deleted_len,
            inserted_structure,
          } if target == body_target => {
            if inserted_structure || self.projection_index.deleted_range_contains_structure(unicode_start, deleted_len) {
              invalidation.rebuild_required = true;
              invalidation.fallback_reason = Some("structural_body_text_change");
            }
            invalidation.changed_flows.push(ROOT_BODY_FLOW_ID.to_string());
            invalidation.changed_text_ranges.push(ProjectionTextRange {
              flow_id: ROOT_BODY_FLOW_ID.to_string(),
              unicode_start,
              unicode_len,
            });
          },
          SubscriptionChange::Text { target, .. } => invalidation.changed_flows.push(target),
          SubscriptionChange::Map { target, keys } => classify_map_invalidation(invalidation, &target, &keys),
          SubscriptionChange::List { target } => invalidation.changed_blocks.push(target),
          SubscriptionChange::Unknown { target } => {
            invalidation.rebuild_required = true;
            invalidation.fallback_reason = Some("unknown_loro_subscription_diff");
            invalidation.changed_blocks.push(target);
          },
        }
      }
    }
    invalidation.changed_flows.sort();
    invalidation.changed_flows.dedup();
    invalidation.changed_blocks.sort();
    invalidation.changed_blocks.dedup();
    invalidation.changed_tables.sort();
    invalidation.changed_tables.dedup();
    invalidation.changed_assets.sort();
    invalidation.changed_assets.dedup();
    invalidation.changed_sections.sort();
    invalidation.changed_sections.dedup();
  }

  fn persist_update_from_last_frontier(&mut self) -> Result<()> {
    let from_frontier = self.last_persisted_frontier.clone();
    let from_vv = self.last_persisted_vv.clone();
    let update = self
      .doc
      .export(ExportMode::updates(&from_vv))
      .context("exporting accepted remote Loro update for persistence")?;
    if update.is_empty() {
      return Ok(());
    }
    self.persist_update_segment(from_frontier, from_vv, update)
  }

  fn persist_update_segment(&mut self, from_frontier: Frontiers, from_vv: VersionVector, update: Vec<u8>) -> Result<()> {
    if let Some(package) = &mut self.package {
      package.append_update_segment(&from_frontier, &from_vv, &self.doc.state_frontiers(), &self.doc.state_vv(), update)?;
      let compacted = package.compact_update_segments_if_needed(&self.doc, DEFAULT_UPDATE_SEGMENT_COMPACTION_THRESHOLD)?;
      if let Some(path) = &self.package_path {
        if compacted.is_some() {
          package.write(path)?;
          self.package_journal_prepared = true;
        } else if self.package_journal_prepared {
          package.append_latest_update_to_prepared_path(path)?;
        } else {
          package.append_latest_update_to_path(path)?;
          self.package_journal_prepared = true;
        }
      }
    }
    self.last_persisted_frontier = self.doc.state_frontiers();
    self.last_persisted_vv = self.doc.state_vv();
    Ok(())
  }
}

fn summarize_subscription_event(event: &DiffEvent<'_>) -> SubscriptionEventSummary {
  let mut changes = Vec::new();
  for container in &event.events {
    let target = container.target.to_string();
    match &container.diff {
      Diff::Text(delta) => {
        let mut cursor = 0usize;
        for item in delta {
          match item {
            loro::TextDelta::Retain { retain, attributes } => {
              if attributes.is_some() {
                changes.push(SubscriptionChange::Text {
                  target: target.clone(),
                  unicode_start: cursor,
                  unicode_len: *retain,
                  deleted_len: 0,
                  inserted_structure: false,
                });
              }
              cursor = cursor.saturating_add(*retain);
            },
            loro::TextDelta::Insert { insert, .. } => {
              let len = insert.chars().count();
              changes.push(SubscriptionChange::Text {
                target: target.clone(),
                unicode_start: cursor,
                unicode_len: len,
                deleted_len: 0,
                inserted_structure: insert.chars().any(|ch| ch == '\n' || ch == OBJECT_REPLACEMENT),
              });
              cursor = cursor.saturating_add(len);
            },
            loro::TextDelta::Delete { delete } => {
              changes.push(SubscriptionChange::Text {
                target: target.clone(),
                unicode_start: cursor,
                unicode_len: *delete,
                deleted_len: *delete,
                inserted_structure: false,
              });
            },
          }
        }
      },
      Diff::Map(delta) => changes.push(SubscriptionChange::Map {
        target,
        keys: delta.updated.keys().map(|key| key.to_string()).collect(),
      }),
      Diff::List(_) => changes.push(SubscriptionChange::List { target }),
      Diff::Tree(_) | Diff::Unknown => changes.push(SubscriptionChange::Unknown { target }),
      Diff::Counter(_) => changes.push(SubscriptionChange::Unknown { target }),
    }
  }
  SubscriptionEventSummary {
    origin: event.origin.to_string(),
    triggered_by: format!("{:?}", event.triggered_by),
    changes,
  }
}

fn classify_map_invalidation(invalidation: &mut ProjectionInvalidation, target: &str, keys: &[String]) {
  if keys.iter().any(|key| {
    matches!(
      key.as_str(),
      "asset_id" | "content_hash" | "mime_type" | "byte_length" | "dimensions" | "original_name"
    )
  }) {
    invalidation.changed_assets.push(target.to_string());
  }
  if keys.iter().any(|key| {
    matches!(
      key.as_str(),
      "row_order" | "rows_by_id" | "column_order" | "columns_by_id" | "cells_by_id" | "row_span" | "column_span"
    )
  }) {
    invalidation.changed_tables.push(target.to_string());
  }
  if keys.iter().any(|key| {
    matches!(
      key.as_str(),
      "kind" | "flow_id" | "anchor_cursor" | "attrs" | "nested_refs"
    )
  }) {
    invalidation.changed_blocks.push(target.to_string());
  }
  if keys.iter().any(|key| key == "section_id" || key == "sections_by_id") {
    invalidation.changed_sections.push(target.to_string());
  }
}

pub fn apply_editor_semantic_command(doc: &LoroDoc, projection: &DocumentProjection, command: &EditorSemanticCommand) -> Result<bool> {
  match command {
    EditorSemanticCommand::InsertText { at, text, styles } => {
      let unicode_index = projection_offset_to_body_unicode_index(projection, *at);
      let body = body_text(doc);
      let newline_boundaries = inserted_newline_boundaries(unicode_index, text);
      body
        .insert(unicode_index, text)
        .context("inserting projection-scoped text command into Loro body flow")?;
      let inserted_len = text.chars().count();
      if inserted_len > 0 {
        mark_run_styles(&body, unicode_index..unicode_index + inserted_len, *styles).context("marking inserted run styles")?;
      }
      repair_paragraph_metadata_after_text_flow_edit(doc, &body, &newline_boundaries, "editor_insert_text")?;
      doc.commit();
      Ok(true)
    }
    EditorSemanticCommand::DeleteRange { range } => {
      let start = projection_offset_to_body_unicode_index(projection, range.start);
      let end = projection_offset_to_body_unicode_index(projection, range.end);
      if end > start {
        let body = body_text(doc);
        body
          .delete(start, end - start)
          .context("deleting projection-scoped text range from Loro body flow")?;
        repair_paragraph_metadata_after_text_flow_edit(doc, &body, &[], "editor_delete_range")?;
        doc.commit();
        return Ok(true);
      }
      Ok(false)
    }
    EditorSemanticCommand::SplitParagraph {
      at,
      inherited_style,
    } => {
      let unicode_index = projection_offset_to_body_unicode_index(projection, *at);
      let body = body_text(doc);
      body
        .insert(unicode_index, "\n")
        .context("splitting paragraph in Loro body flow")?;
      body
        .mark(
          unicode_index..unicode_index + 1,
          MARK_PARAGRAPH_STYLE,
          paragraph_style_value(*inherited_style),
        )
        .context("marking split paragraph style")?;
      repair_paragraph_metadata_after_text_flow_edit(doc, &body, &[unicode_index], "editor_split_paragraph")?;
      doc.commit();
      Ok(true)
    }
    EditorSemanticCommand::SetParagraphStyle { paragraph, style } => {
      if let Some(paragraph_ix) = projection.ids.paragraph_ids.iter().position(|id| id == paragraph) {
        let boundary = paragraph_boundary_unicode_index(projection, paragraph_ix);
        body_text(doc)
          .mark(boundary..boundary + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(*style))
          .context("marking paragraph style from editor semantic command")?;
        doc.commit();
        return Ok(true);
      }
      Ok(false)
    }
    EditorSemanticCommand::SetRunStyles {
      paragraph,
      range,
      styles,
    } => {
      if let Some(paragraph_ix) = projection.ids.paragraph_ids.iter().position(|id| id == paragraph) {
        let start = projection_offset_to_body_unicode_index(
          projection,
          flowstate_document::DocumentOffset {
            paragraph: paragraph_ix,
            byte: range.start,
          },
        );
        let end = projection_offset_to_body_unicode_index(
          projection,
          flowstate_document::DocumentOffset {
            paragraph: paragraph_ix,
            byte: range.end,
          },
        );
        if end > start {
          mark_run_styles(&body_text(doc), start..end, *styles).context("marking run styles from editor semantic command")?;
          doc.commit();
          return Ok(true);
        }
      }
      Ok(false)
    }
    EditorSemanticCommand::JoinParagraphs { first, second } => {
      join_projection_paragraphs(doc, projection, *first, *second).context("joining paragraphs from editor semantic command")
    }
    EditorSemanticCommand::ReplaceParagraphSpan { start, before, after } => {
      replace_body_paragraph_span(doc, projection, *start, before, after).context("replacing paragraph span from editor semantic command")
    }
    EditorSemanticCommand::InsertBlock {
      block,
      block_ix,
      after,
    } => {
      insert_projection_object_block(doc, *block, *block_ix, after).with_context(|| {
        format!("inserting object block from editor semantic command at projection block {block_ix} ({block:?})")
      })
    }
    EditorSemanticCommand::DeleteBlock { block } => {
      delete_projection_object_block(doc, *block).context("deleting object block from editor semantic command")
    }
    EditorSemanticCommand::MoveBlock { block, new_block_ix } => {
      move_projection_object_block(doc, *block, *new_block_ix).context("moving object block from editor semantic command")
    }
    EditorSemanticCommand::ReplaceBlock { block, block_ix, after } => {
      replace_projection_object_block(doc, projection, *block, *block_ix, after).with_context(|| {
        format!("replacing object block from editor semantic command at projection block {block_ix} ({block:?})")
      })
    }
    EditorSemanticCommand::InsertTableRow { table, row_ix, row } => {
      insert_projection_table_row(doc, *table, *row_ix, row).with_context(|| {
        format!("inserting table row from editor semantic command at table {table:?}, row {row_ix}")
      })
    }
    EditorSemanticCommand::DeleteTableRow { table, row_ix } => {
      delete_projection_table_row(doc, *table, *row_ix).with_context(|| {
        format!("deleting table row from editor semantic command at table {table:?}, row {row_ix}")
      })
    }
    EditorSemanticCommand::MoveTableRow {
      table,
      from_row_ix,
      to_row_ix,
    } => move_projection_table_axis(doc, *table, "row_order", *from_row_ix, *to_row_ix).with_context(|| {
      format!("moving table row from {from_row_ix} to {to_row_ix} at table {table:?}")
    }),
    EditorSemanticCommand::InsertTableColumn {
      table,
      column_ix,
      width,
      cells,
    } => insert_projection_table_column(doc, *table, *column_ix, width, cells).with_context(|| {
      format!("inserting table column from editor semantic command at table {table:?}, column {column_ix}")
    }),
    EditorSemanticCommand::DeleteTableColumn { table, column_ix } => {
      delete_projection_table_column(doc, *table, *column_ix).with_context(|| {
        format!("deleting table column from editor semantic command at table {table:?}, column {column_ix}")
      })
    }
    EditorSemanticCommand::MoveTableColumn {
      table,
      from_column_ix,
      to_column_ix,
    } => move_projection_table_axis(doc, *table, "column_order", *from_column_ix, *to_column_ix).with_context(|| {
      format!("moving table column from {from_column_ix} to {to_column_ix} at table {table:?}")
    }),
    EditorSemanticCommand::ReplaceTableCell {
      table,
      row_ix,
      cell_ix,
      cell,
    } => replace_projection_table_cell(doc, *table, *row_ix, *cell_ix, cell).with_context(|| {
      format!("replacing table cell from editor semantic command at table {table:?}, row {row_ix}, cell {cell_ix}")
    }),
    EditorSemanticCommand::SetTableCellSpan {
      table,
      row_ix,
      cell_ix,
      row_span,
      column_span,
    } => set_projection_table_cell_span(doc, *table, *row_ix, *cell_ix, *row_span, *column_span).with_context(|| {
      format!("setting table cell span at table {table:?}, row {row_ix}, cell {cell_ix}")
    }),
    EditorSemanticCommand::ReplaceEquationSourceRange { equation, range, text } => {
      replace_projection_equation_source_range(doc, *equation, range, text).with_context(|| {
        format!("replacing equation source range from editor semantic command at equation {equation:?}, range {range:?}")
      })
    }
    EditorSemanticCommand::ReplaceImageAltText { image, text } => {
      replace_projection_image_alt_text(doc, *image, text).with_context(|| {
        format!("replacing image alt text from editor semantic command at image {image:?}")
      })
    }
    EditorSemanticCommand::ReplaceImageCaption { image, caption } => {
      replace_projection_image_caption(doc, *image, caption.as_ref()).with_context(|| {
        format!("replacing image caption from editor semantic command at image {image:?}")
      })
    }
    EditorSemanticCommand::SetImageLayout { image, sizing, alignment } => {
      set_projection_image_layout(doc, *image, sizing, *alignment).with_context(|| {
        format!("setting image layout from editor semantic command at image {image:?}")
      })
    }
    EditorSemanticCommand::SetTableColumnWidth { table, column_ix, width } => {
      set_projection_table_column_width(doc, *table, *column_ix, width).with_context(|| {
        format!("setting table column width from editor semantic command at table {table:?}, column {column_ix}")
      })
    }
  }
}

#[derive(Clone, Debug)]
struct SubscriptionEventSummary {
  origin: String,
  triggered_by: String,
  changes: Vec<SubscriptionChange>,
}

#[derive(Clone, Debug)]
enum SubscriptionChange {
  Text {
    target: String,
    unicode_start: usize,
    unicode_len: usize,
    deleted_len: usize,
    inserted_structure: bool,
  },
  Map {
    target: String,
    keys: Vec<String>,
  },
  List {
    target: String,
  },
  Unknown {
    target: String,
  },
}

fn apply_editor_semantic_command_body_fast_path(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  projection_index: &ProjectionRuntimeIndex,
  command: &EditorSemanticCommand,
) -> Result<bool> {
  match command {
    EditorSemanticCommand::InsertText { at, text, styles } => {
      let body = body_text(doc);
      let Some(unicode_index) = projection_index.body_unicode_for_offset(projection, *at) else {
        return Ok(false);
      };
      let newline_boundaries = inserted_newline_boundaries(unicode_index, text);
      body
        .insert(unicode_index, text)
        .context("inserting text into Loro body flow without projection snapshot")?;
      let inserted_len = text.chars().count();
      if inserted_len > 0 {
        mark_run_styles(&body, unicode_index..unicode_index + inserted_len, *styles).context("marking inserted run styles")?;
      }
      repair_paragraph_metadata_after_text_flow_edit(doc, &body, &newline_boundaries, "editor_insert_text_fast_path")?;
      doc.commit();
      Ok(true)
    },
    EditorSemanticCommand::DeleteRange { range } => {
      let body = body_text(doc);
      let Some(start) = projection_index.body_unicode_for_offset(projection, range.start) else {
        return Ok(false);
      };
      let Some(end) = projection_index.body_unicode_for_offset(projection, range.end) else {
        return Ok(false);
      };
      if end > start {
        body
          .delete(start, end - start)
          .context("deleting text from Loro body flow without projection snapshot")?;
        repair_paragraph_metadata_after_text_flow_edit(doc, &body, &[], "editor_delete_range_fast_path")?;
        doc.commit();
        return Ok(true);
      }
      Ok(false)
    },
    EditorSemanticCommand::SplitParagraph {
      at,
      inherited_style,
    } => {
      let body = body_text(doc);
      let Some(unicode_index) = projection_index.body_unicode_for_offset(projection, *at) else {
        return Ok(false);
      };
      body
        .insert(unicode_index, "\n")
        .context("splitting paragraph in Loro body flow without projection snapshot")?;
      body
        .mark(
          unicode_index..unicode_index + 1,
          MARK_PARAGRAPH_STYLE,
          paragraph_style_value(*inherited_style),
        )
        .context("marking split paragraph style")?;
      repair_paragraph_metadata_after_text_flow_edit(doc, &body, &[unicode_index], "editor_split_paragraph_fast_path")?;
      doc.commit();
      Ok(true)
    },
    EditorSemanticCommand::SetParagraphStyle { .. }
    | EditorSemanticCommand::SetRunStyles { .. }
    | EditorSemanticCommand::JoinParagraphs { .. }
    | EditorSemanticCommand::ReplaceParagraphSpan { .. }
    | EditorSemanticCommand::InsertBlock { .. }
    | EditorSemanticCommand::DeleteBlock { .. }
    | EditorSemanticCommand::MoveBlock { .. }
    | EditorSemanticCommand::ReplaceBlock { .. }
    | EditorSemanticCommand::InsertTableRow { .. }
    | EditorSemanticCommand::DeleteTableRow { .. }
    | EditorSemanticCommand::MoveTableRow { .. }
    | EditorSemanticCommand::InsertTableColumn { .. }
    | EditorSemanticCommand::DeleteTableColumn { .. }
    | EditorSemanticCommand::MoveTableColumn { .. }
    | EditorSemanticCommand::ReplaceTableCell { .. }
    | EditorSemanticCommand::SetTableCellSpan { .. }
    | EditorSemanticCommand::ReplaceEquationSourceRange { .. }
    | EditorSemanticCommand::ReplaceImageAltText { .. }
    | EditorSemanticCommand::ReplaceImageCaption { .. }
    | EditorSemanticCommand::SetImageLayout { .. }
    | EditorSemanticCommand::SetTableColumnWidth { .. }
    => Ok(false),
  }
}

fn incremental_projection_patches_for_command(
  projection: &DocumentProjection,
  doc: &LoroDoc,
  command: &EditorSemanticCommand,
) -> Option<Vec<flowstate_document::CollabPatch>> {
  match command {
    EditorSemanticCommand::InsertText { at, text, .. }
      if !text.contains('\n') && !text.contains(OBJECT_REPLACEMENT) =>
    {
      let row = flowstate_document::block_ix_for_paragraph(projection, at.paragraph)?;
      let old_len = flowstate_document::paragraph_text_len(projection.paragraphs.get(at.paragraph)?);
      let new = body_input_paragraph(doc, at.paragraph)?;
      Some(vec![flowstate_document::CollabPatch::ParagraphText {
        row,
        new,
        delta_utf8: projection_text_delta(
          at.byte.min(old_len),
          0,
          text.len(),
          old_len.saturating_sub(at.byte.min(old_len)),
        ),
      }])
    },
    EditorSemanticCommand::DeleteRange { range } if range.start.paragraph == range.end.paragraph => {
      let paragraph_ix = range.start.paragraph;
      let row = flowstate_document::block_ix_for_paragraph(projection, paragraph_ix)?;
      let old_len = flowstate_document::paragraph_text_len(projection.paragraphs.get(paragraph_ix)?);
      let start = range.start.byte.min(old_len);
      let end = range.end.byte.min(old_len).max(start);
      let new = body_input_paragraph(doc, paragraph_ix)?;
      Some(vec![flowstate_document::CollabPatch::ParagraphText {
        row,
        new,
        delta_utf8: projection_text_delta(start, end - start, 0, old_len.saturating_sub(end)),
      }])
    },
    EditorSemanticCommand::SetParagraphStyle { paragraph, style } => {
      let paragraph_ix = projection.ids.paragraph_ids.iter().position(|id| id == paragraph)?;
      let row = flowstate_document::block_ix_for_paragraph(projection, paragraph_ix)?;
      Some(vec![flowstate_document::CollabPatch::ParagraphStyle {
        row,
        style: *style,
      }])
    },
    EditorSemanticCommand::SetRunStyles { paragraph, .. } => {
      let paragraph_ix = projection.ids.paragraph_ids.iter().position(|id| id == paragraph)?;
      let row = flowstate_document::block_ix_for_paragraph(projection, paragraph_ix)?;
      let new = body_input_paragraph(doc, paragraph_ix)?;
      Some(vec![flowstate_document::CollabPatch::ParagraphRuns {
        row,
        runs: flowstate_document::document_from_input_blocks(
          projection.theme.clone(),
          vec![InputBlock::Paragraph(new)],
        )
        .paragraphs
        .first()?
        .runs
        .clone(),
      }])
    },
    _ => structured_projection_patches_for_command(projection, command),
  }
}

fn structured_projection_patches_for_command(
  projection: &DocumentProjection,
  command: &EditorSemanticCommand,
) -> Option<Vec<CollabPatch>> {
  match command {
    EditorSemanticCommand::InsertBlock {
      block,
      block_ix,
      after,
    } => Some(vec![CollabPatch::InsertBlocks {
      row: (*block_ix).min(projection.blocks.len()),
      blocks: vec![CollabStructuralBlock {
        block_id: *block,
        paragraph_id: None,
        block: after.clone(),
      }],
    }]),
    EditorSemanticCommand::DeleteBlock { block } => Some(vec![CollabPatch::DeleteBlocks {
      row: projection.ids.block_ids.iter().position(|id| id == block)?,
      count: 1,
    }]),
    EditorSemanticCommand::MoveBlock { block, new_block_ix } => Some(vec![CollabPatch::MoveBlock {
      from: projection.ids.block_ids.iter().position(|id| id == block)?,
      to: (*new_block_ix).min(projection.blocks.len().saturating_sub(1)),
    }]),
    EditorSemanticCommand::ReplaceBlock {
      block,
      block_ix,
      after,
    } => object_replacement_patch(
      projection,
      block
        .and_then(|id| projection.ids.block_ids.iter().position(|candidate| *candidate == id))
        .unwrap_or(*block_ix),
      after.clone(),
    ),
    EditorSemanticCommand::InsertTableRow { table, row_ix, row } => {
      let (block_ix, mut table_input) = projected_table_input(projection, *table)?;
      table_input.rows.insert((*row_ix).min(table_input.rows.len()), row.clone());
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::DeleteTableRow { table, row_ix } => {
      let (block_ix, mut table_input) = projected_table_input(projection, *table)?;
      if *row_ix >= table_input.rows.len() {
        return None;
      }
      table_input.rows.remove(*row_ix);
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::MoveTableRow {
      table,
      from_row_ix,
      to_row_ix,
    } => {
      let (block_ix, mut table_input) = projected_table_input(projection, *table)?;
      if *from_row_ix >= table_input.rows.len() || *to_row_ix >= table_input.rows.len() {
        return None;
      }
      let row = table_input.rows.remove(*from_row_ix);
      table_input.rows.insert(*to_row_ix, row);
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::InsertTableColumn {
      table,
      column_ix,
      width,
      cells,
    } => {
      let (block_ix, mut table_input) = projected_table_input(projection, *table)?;
      let column_ix = (*column_ix).min(table_input.column_widths.len());
      table_input.column_widths.insert(column_ix, width.clone());
      for (row_ix, row) in table_input.rows.iter_mut().enumerate() {
        row
          .cells
          .insert(column_ix.min(row.cells.len()), cells.get(row_ix).cloned().unwrap_or_else(empty_input_table_cell));
      }
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::DeleteTableColumn { table, column_ix } => {
      let (block_ix, mut table_input) = projected_table_input(projection, *table)?;
      if *column_ix >= table_input.column_widths.len() {
        return None;
      }
      table_input.column_widths.remove(*column_ix);
      for row in &mut table_input.rows {
        if *column_ix < row.cells.len() {
          row.cells.remove(*column_ix);
        }
      }
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::MoveTableColumn {
      table,
      from_column_ix,
      to_column_ix,
    } => {
      let (block_ix, mut table_input) = projected_table_input(projection, *table)?;
      if *from_column_ix >= table_input.column_widths.len() || *to_column_ix >= table_input.column_widths.len() {
        return None;
      }
      let width = table_input.column_widths.remove(*from_column_ix);
      table_input.column_widths.insert(*to_column_ix, width);
      for row in &mut table_input.rows {
        if *from_column_ix < row.cells.len() && *to_column_ix < row.cells.len() {
          let cell = row.cells.remove(*from_column_ix);
          row.cells.insert(*to_column_ix, cell);
        }
      }
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::ReplaceTableCell {
      table,
      row_ix,
      cell_ix,
      cell,
    } => {
      let (block_ix, mut table_input) = projected_table_input(projection, *table)?;
      let target = table_input.rows.get_mut(*row_ix)?.cells.get_mut(*cell_ix)?;
      *target = cell.clone();
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::SetTableCellSpan {
      table,
      row_ix,
      cell_ix,
      row_span,
      column_span,
    } => {
      let (block_ix, mut table_input) = projected_table_input(projection, *table)?;
      let cell = table_input.rows.get_mut(*row_ix)?.cells.get_mut(*cell_ix)?;
      cell.row_span = (*row_span).max(1);
      cell.col_span = (*column_span).max(1);
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::SetTableColumnWidth {
      table,
      column_ix,
      width,
    } => {
      let (block_ix, mut table_input) = projected_table_input(projection, *table)?;
      *table_input.column_widths.get_mut(*column_ix)? = width.clone();
      object_replacement_patch(projection, block_ix, InputBlock::Table(table_input))
    },
    EditorSemanticCommand::ReplaceEquationSourceRange {
      equation,
      range,
      text,
    } => {
      let block_ix = projection.ids.block_ids.iter().position(|id| id == equation)?;
      let InputBlock::Equation(mut equation_input) = flowstate_document::input_block_from_block(projection.blocks.get(block_ix)?) else {
        return None;
      };
      if range.start > range.end
        || range.end > equation_input.source.len()
        || !equation_input.source.is_char_boundary(range.start)
        || !equation_input.source.is_char_boundary(range.end)
      {
        return None;
      }
      equation_input.source.replace_range(range.clone(), text);
      object_replacement_patch(projection, block_ix, InputBlock::Equation(equation_input))
    },
    EditorSemanticCommand::ReplaceImageAltText { image, text } => {
      let block_ix = projection.ids.block_ids.iter().position(|id| id == image)?;
      let InputBlock::Image(mut image_input) = flowstate_document::input_block_from_block(projection.blocks.get(block_ix)?) else {
        return None;
      };
      image_input.alt_text = text.clone();
      object_replacement_patch(projection, block_ix, InputBlock::Image(image_input))
    },
    EditorSemanticCommand::ReplaceImageCaption { image, caption } => {
      let block_ix = projection.ids.block_ids.iter().position(|id| id == image)?;
      let InputBlock::Image(mut image_input) = projection.blocks.get(block_ix).map(flowstate_document::input_block_from_block)? else {
        return None;
      };
      image_input.caption = caption.clone();
      object_replacement_patch(projection, block_ix, InputBlock::Image(image_input))
    },
    EditorSemanticCommand::SetImageLayout {
      image,
      sizing,
      alignment,
    } => {
      let block_ix = projection.ids.block_ids.iter().position(|id| id == image)?;
      let InputBlock::Image(mut image_input) = flowstate_document::input_block_from_block(projection.blocks.get(block_ix)?) else {
        return None;
      };
      image_input.sizing = sizing.clone();
      image_input.alignment = *alignment;
      object_replacement_patch(projection, block_ix, InputBlock::Image(image_input))
    },
    EditorSemanticCommand::InsertText { .. }
    | EditorSemanticCommand::DeleteRange { .. }
    | EditorSemanticCommand::SplitParagraph { .. }
    | EditorSemanticCommand::JoinParagraphs { .. }
    | EditorSemanticCommand::SetParagraphStyle { .. }
    | EditorSemanticCommand::SetRunStyles { .. }
    | EditorSemanticCommand::ReplaceParagraphSpan { .. } => None,
  }
}

fn projected_table_input(
  projection: &DocumentProjection,
  table: flowstate_document::BlockId,
) -> Option<(usize, InputTableBlock)> {
  let block_ix = projection.ids.block_ids.iter().position(|id| *id == table)?;
  let InputBlock::Table(table) = flowstate_document::input_block_from_block(projection.blocks.get(block_ix)?) else {
    return None;
  };
  Some((block_ix, table))
}

fn object_replacement_patch(
  projection: &DocumentProjection,
  block_ix: usize,
  block: InputBlock,
) -> Option<Vec<CollabPatch>> {
  Some(vec![CollabPatch::ReplaceObjectBlock {
    row: block_ix,
    block: CollabStructuralBlock {
      block_id: *projection.ids.block_ids.get(block_ix)?,
      paragraph_id: None,
      block,
    },
  }])
}

fn projection_text_delta(
  prefix_retain: usize,
  delete_len: usize,
  insert_len: usize,
  trailing_retain: usize,
) -> Vec<flowstate_document::CollabTextDelta> {
  let mut delta = Vec::new();
  if prefix_retain > 0 {
    delta.push(flowstate_document::CollabTextDelta::Retain(prefix_retain));
  }
  if delete_len > 0 {
    delta.push(flowstate_document::CollabTextDelta::Delete(delete_len));
  }
  if insert_len > 0 {
    delta.push(flowstate_document::CollabTextDelta::Insert(insert_len));
  }
  if trailing_retain > 0 {
    delta.push(flowstate_document::CollabTextDelta::Retain(trailing_retain));
  }
  delta
}

fn editor_command_invalidation(
  projection: &DocumentProjection,
  command: &EditorSemanticCommand,
  frontier_before: Vec<u8>,
  frontier_after: Vec<u8>,
) -> ProjectionInvalidation {
  match command {
    EditorSemanticCommand::InsertText { at, text, .. } => ProjectionInvalidation::body_text(
      frontier_before,
      frontier_after,
      projection_offset_to_body_unicode_index(projection, *at),
      text.chars().count(),
    ),
    EditorSemanticCommand::DeleteRange { range } => {
      let start = projection_offset_to_body_unicode_index(projection, range.start);
      let end = projection_offset_to_body_unicode_index(projection, range.end);
      ProjectionInvalidation::body_text(frontier_before, frontier_after, start, end.saturating_sub(start))
    },
    EditorSemanticCommand::SetParagraphStyle { paragraph, .. } => {
      let paragraph_ix = projection
        .ids
        .paragraph_ids
        .iter()
        .position(|id| id == paragraph)
        .unwrap_or_default();
      ProjectionInvalidation::body_style(
        frontier_before,
        frontier_after,
        paragraph_boundary_unicode_index(projection, paragraph_ix),
        1,
      )
    },
    EditorSemanticCommand::SetRunStyles { paragraph, range, .. } => {
      let paragraph_ix = projection
        .ids
        .paragraph_ids
        .iter()
        .position(|id| id == paragraph)
        .unwrap_or_default();
      let start = projection_offset_to_body_unicode_index(
        projection,
        DocumentOffset {
          paragraph: paragraph_ix,
          byte: range.start,
        },
      );
      ProjectionInvalidation::body_style(frontier_before, frontier_after, start, range.end.saturating_sub(range.start))
    },
    _ => ProjectionInvalidation::full_rebuild(frontier_before, frontier_after, "editor_structural_projection_fallback"),
  }
}

fn insert_projection_object_block(
  doc: &LoroDoc,
  block_id: flowstate_document::BlockId,
  block_ix: usize,
  input: &InputBlock,
) -> Result<bool> {
  if matches!(input, InputBlock::Paragraph(_)) {
    tracing::warn!(block_ix, ?block_id, "skipping InsertBlock for paragraph payload; paragraph edits must use text/paragraph semantic commands");
    return Ok(false);
  }

  let body = body_text(doc);
  if object_loro_block_by_projected_id(doc, &body, block_id).is_some() {
    tracing::warn!(block_ix, ?block_id, "skipping InsertBlock because the Loro object block already exists");
    return Ok(false);
  }
  let Some(unicode_index) = object_insert_unicode_pos_for_projection_block(&body, block_ix) else {
    tracing::warn!(block_ix, ?block_id, "skipping InsertBlock because no Loro insertion point maps to the projection block index");
    return Ok(false);
  };
  insert_input_object_block(doc, unicode_index, block_id, input)?;
  doc.commit();
  Ok(true)
}

fn insert_input_object_block(doc: &LoroDoc, unicode_index: usize, block_id: flowstate_document::BlockId, input: &InputBlock) -> Result<()> {
  match input {
    InputBlock::Image(image) => insert_image_block_with_id(doc, unicode_index, block_id, image),
    InputBlock::Equation(equation) => insert_equation_block_with_id(doc, unicode_index, block_id, equation),
    InputBlock::Table(table) => insert_table_block_with_id(doc, unicode_index, block_id, table),
    InputBlock::Paragraph(_) => Ok(()),
  }
}

fn replace_projection_object_block(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  block_id: Option<flowstate_document::BlockId>,
  block_ix: usize,
  after: &InputBlock,
) -> Result<bool> {
  if matches!(after, InputBlock::Paragraph(_)) {
    tracing::warn!(block_ix, "skipping ReplaceBlock for paragraph payload; paragraph edits must use text/paragraph semantic commands");
    return Ok(false);
  }
  if block_id.is_none() && projection.blocks.get(block_ix).is_none() {
    tracing::warn!(block_ix, "skipping ReplaceBlock because the projection block index is out of range");
    return Ok(false);
  }

  let body = body_text(doc);
  let block = block_id
    .and_then(|block_id| object_loro_block_by_projected_id(doc, &body, block_id).map(|(_, block, _)| block))
    .or_else(|| {
      projection
        .ids
        .block_ids
        .get(block_ix)
        .and_then(|block_id| object_loro_block_by_projected_id(doc, &body, *block_id).map(|(_, block, _)| block))
    })
    .or_else(|| {
      let anchor_pos = object_unicode_pos_for_projection_block(&body, block_ix)?;
      object_loro_block_at_unicode_pos(doc, &body, anchor_pos)
    });
  let Some(block) = block else {
    tracing::warn!(block_ix, "skipping ReplaceBlock because no Loro object block maps to the projected block");
    return Ok(false);
  };

  match after {
    InputBlock::Image(image) => replace_image_block_from_input(doc, &block, image)?,
    InputBlock::Equation(equation) => replace_equation_block_from_input(doc, &block, equation)?,
    InputBlock::Table(table) => {
      tracing::warn!(block_ix, "applying coarse structured table ReplaceBlock; editor should emit finer table operations later");
      replace_table_block_from_input(doc, &block, table)?;
    },
    InputBlock::Paragraph(_) => unreachable!("paragraph payload was handled above"),
  }
  doc.commit();
  Ok(true)
}

fn set_projection_table_column_width(
  doc: &LoroDoc,
  table_block_id: flowstate_document::BlockId,
  column_ix: usize,
  width: &InputTableColumnWidth,
) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, column_ix, "skipping table column width command because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let Some(column_order) = child_movable_list(&table, "column_order") else {
    tracing::warn!(?table_block_id, column_ix, "skipping table column width command because the table has no column order");
    return Ok(false);
  };
  let column_ids = movable_list_strings(&column_order);
  let Some(column_id) = column_ids.get(column_ix) else {
    tracing::warn!(?table_block_id, column_ix, "skipping table column width command because the column index is out of range");
    return Ok(false);
  };
  let Some(columns_by_id) = child_map(&table, "columns_by_id") else {
    tracing::warn!(?table_block_id, column_ix, "skipping table column width command because the table has no columns map");
    return Ok(false);
  };
  let column = columns_by_id.ensure_mergeable_map(column_id)?;
  write_table_column_width(&column, width)?;
  doc.commit();
  Ok(true)
}

fn insert_projection_table_row(
  doc: &LoroDoc,
  table_block_id: flowstate_document::BlockId,
  row_ix: usize,
  row: &InputTableRow,
) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, row_ix, "skipping table row insert because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let row_order = table.ensure_mergeable_movable_list("row_order")?;
  let column_order = table.ensure_mergeable_movable_list("column_order")?;
  let rows_by_id = table.ensure_mergeable_map("rows_by_id")?;
  let cells_by_id = table.ensure_mergeable_map("cells_by_id")?;
  let column_ids = movable_list_strings(&column_order);
  if column_ids.is_empty() {
    tracing::warn!(?table_block_id, row_ix, "skipping table row insert because the table has no columns");
    return Ok(false);
  }

  let row_id = format!("row.{}", Uuid::new_v4().as_u128());
  row_order.insert(row_ix.min(row_order.len()), row_id.as_str())?;
  let row_map = rows_by_id.ensure_mergeable_map(&row_id)?;
  row_map.insert("id", row_id.as_str())?;
  row_map.insert("container_id", row_map.id().to_string())?;
  row_map.ensure_mergeable_map("attrs")?;

  let empty_cell = empty_input_table_cell();
  for (column_ix, column_id) in column_ids.iter().enumerate() {
    let cell_id = format!("{row_id}.cell.{}", Uuid::new_v4().as_u128());
    let cell_map = cells_by_id.ensure_mergeable_map(&cell_id)?;
    let cell = row.cells.get(column_ix).unwrap_or(&empty_cell);
    write_table_cell_map_from_input(doc, &cell_map, &cell_id, &row_id, column_id, cell)?;
  }
  doc.commit();
  Ok(true)
}

fn delete_projection_table_row(doc: &LoroDoc, table_block_id: flowstate_document::BlockId, row_ix: usize) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, row_ix, "skipping table row delete because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let Some(row_order) = child_movable_list(&table, "row_order") else {
    tracing::warn!(?table_block_id, row_ix, "skipping table row delete because the table has no row order");
    return Ok(false);
  };
  let row_ids = movable_list_strings(&row_order);
  let Some(row_id) = row_ids.get(row_ix) else {
    tracing::warn!(?table_block_id, row_ix, "skipping table row delete because the row index is out of range");
    return Ok(false);
  };
  let row_id = row_id.clone();
  row_order.delete(row_ix, 1)?;
  if let Some(rows_by_id) = child_map(&table, "rows_by_id") {
    rows_by_id.delete(&row_id)?;
  }
  if let Some(cells_by_id) = child_map(&table, "cells_by_id") {
    for cell_id in map_keys(&cells_by_id) {
      let delete_cell = child_map(&cells_by_id, &cell_id)
        .and_then(|cell| map_string_opt(&cell, "row_id"))
        .as_deref()
        == Some(row_id.as_str());
      if delete_cell {
        cells_by_id.delete(&cell_id)?;
      }
    }
  }
  doc.commit();
  Ok(true)
}

fn move_projection_table_axis(
  doc: &LoroDoc,
  table_block_id: flowstate_document::BlockId,
  order_key: &'static str,
  from_ix: usize,
  to_ix: usize,
) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, order_key, from_ix, to_ix, "skipping table move because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let Some(order) = child_movable_list(&table, order_key) else {
    tracing::warn!(?table_block_id, order_key, from_ix, to_ix, "skipping table move because its order list is missing");
    return Ok(false);
  };
  if from_ix >= order.len() || to_ix >= order.len() || from_ix == to_ix {
    return Ok(false);
  }
  order.mov(from_ix, to_ix)?;
  doc.commit();
  Ok(true)
}

fn insert_projection_table_column(
  doc: &LoroDoc,
  table_block_id: flowstate_document::BlockId,
  column_ix: usize,
  width: &InputTableColumnWidth,
  cells: &[InputTableCell],
) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, column_ix, "skipping table column insert because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let row_order = table.ensure_mergeable_movable_list("row_order")?;
  let column_order = table.ensure_mergeable_movable_list("column_order")?;
  let rows_by_id = table.ensure_mergeable_map("rows_by_id")?;
  let columns_by_id = table.ensure_mergeable_map("columns_by_id")?;
  let cells_by_id = table.ensure_mergeable_map("cells_by_id")?;
  table.insert("container_id", table.id().to_string())?;
  table.insert("row_order_container_id", row_order.id().to_string())?;
  table.insert("column_order_container_id", column_order.id().to_string())?;
  table.insert("rows_container_id", rows_by_id.id().to_string())?;
  table.insert("columns_container_id", columns_by_id.id().to_string())?;
  table.insert("cells_container_id", cells_by_id.id().to_string())?;
  let row_ids = movable_list_strings(&row_order);
  if row_ids.is_empty() {
    tracing::warn!(?table_block_id, column_ix, "skipping table column insert because the table has no rows");
    return Ok(false);
  }

  let column_id = format!("column.{}", Uuid::new_v4().as_u128());
  column_order.insert(column_ix.min(column_order.len()), column_id.as_str())?;
  let column = columns_by_id.ensure_mergeable_map(&column_id)?;
  column.insert("id", column_id.as_str())?;
  column.insert("container_id", column.id().to_string())?;
  column.ensure_mergeable_map("attrs")?;
  write_table_column_width(&column, width)?;

  let empty_cell = empty_input_table_cell();
  for (row_ix, row_id) in row_ids.iter().enumerate() {
    let cell_id = format!("{row_id}.cell.{}", Uuid::new_v4().as_u128());
    let cell_map = cells_by_id.ensure_mergeable_map(&cell_id)?;
    let cell = cells.get(row_ix).unwrap_or(&empty_cell);
    write_table_cell_map_from_input(doc, &cell_map, &cell_id, row_id, &column_id, cell)?;
  }
  doc.commit();
  Ok(true)
}

fn delete_projection_table_column(doc: &LoroDoc, table_block_id: flowstate_document::BlockId, column_ix: usize) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, column_ix, "skipping table column delete because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let Some(column_order) = child_movable_list(&table, "column_order") else {
    tracing::warn!(?table_block_id, column_ix, "skipping table column delete because the table has no column order");
    return Ok(false);
  };
  let column_ids = movable_list_strings(&column_order);
  let Some(column_id) = column_ids.get(column_ix) else {
    tracing::warn!(?table_block_id, column_ix, "skipping table column delete because the column index is out of range");
    return Ok(false);
  };
  let column_id = column_id.clone();
  column_order.delete(column_ix, 1)?;
  if let Some(columns_by_id) = child_map(&table, "columns_by_id") {
    columns_by_id.delete(&column_id)?;
  }
  if let Some(cells_by_id) = child_map(&table, "cells_by_id") {
    for cell_id in map_keys(&cells_by_id) {
      let delete_cell = child_map(&cells_by_id, &cell_id)
        .and_then(|cell| map_string_opt(&cell, "column_id"))
        .as_deref()
        == Some(column_id.as_str());
      if delete_cell {
        cells_by_id.delete(&cell_id)?;
      }
    }
  }
  doc.commit();
  Ok(true)
}

fn replace_projection_table_cell(
  doc: &LoroDoc,
  table_block_id: flowstate_document::BlockId,
  row_ix: usize,
  cell_ix: usize,
  cell: &InputTableCell,
) -> Result<bool> {
  let Some(table) = projection_table_map_by_block_id(doc, table_block_id) else {
    tracing::warn!(?table_block_id, row_ix, cell_ix, "skipping table cell replace because no Loro table maps to the projected block id");
    return Ok(false);
  };
  let Some(row_order) = child_movable_list(&table, "row_order") else {
    tracing::warn!(?table_block_id, row_ix, cell_ix, "skipping table cell replace because the table has no row order");
    return Ok(false);
  };
  let Some(column_order) = child_movable_list(&table, "column_order") else {
    tracing::warn!(?table_block_id, row_ix, cell_ix, "skipping table cell replace because the table has no column order");
    return Ok(false);
  };
  let row_ids = movable_list_strings(&row_order);
  let column_ids = movable_list_strings(&column_order);
  let Some(row_id) = row_ids.get(row_ix) else {
    tracing::warn!(?table_block_id, row_ix, cell_ix, "skipping table cell replace because the row index is out of range");
    return Ok(false);
  };
  let Some(column_id) = column_ids.get(cell_ix) else {
    tracing::warn!(?table_block_id, row_ix, cell_ix, "skipping table cell replace because the cell column index is out of range");
    return Ok(false);
  };
  let cells_by_id = table.ensure_mergeable_map("cells_by_id")?;
  let cell_id = table_cell_id_by_row_column(&cells_by_id, row_id, column_id)
    .unwrap_or_else(|| format!("{row_id}.cell.{}", Uuid::new_v4().as_u128()));
  let cell_map = cells_by_id.ensure_mergeable_map(&cell_id)?;
  update_table_cell_map_from_input(doc, &cell_map, &cell_id, row_id, column_id, cell)?;
  doc.commit();
  Ok(true)
}

fn set_projection_table_cell_span(
  doc: &LoroDoc,
  table_block_id: flowstate_document::BlockId,
  row_ix: usize,
  cell_ix: usize,
  row_span: u16,
  column_span: u16,
) -> Result<bool> {
  let Some(cell) = projection_table_cell_map(doc, table_block_id, row_ix, cell_ix) else {
    tracing::warn!(?table_block_id, row_ix, cell_ix, "skipping table span command because the Loro cell is missing");
    return Ok(false);
  };
  cell.insert("row_span", i64::from(row_span.max(1)))?;
  cell.insert("column_span", i64::from(column_span.max(1)))?;
  doc.commit();
  Ok(true)
}

fn projection_table_cell_map(
  doc: &LoroDoc,
  table_block_id: flowstate_document::BlockId,
  row_ix: usize,
  cell_ix: usize,
) -> Option<LoroMap> {
  let table = projection_table_map_by_block_id(doc, table_block_id)?;
  let row_id = movable_list_strings(&child_movable_list(&table, "row_order")?).get(row_ix)?.clone();
  let column_id = movable_list_strings(&child_movable_list(&table, "column_order")?).get(cell_ix)?.clone();
  let cells_by_id = child_map(&table, "cells_by_id")?;
  let cell_id = table_cell_id_by_row_column(&cells_by_id, &row_id, &column_id)?;
  child_map(&cells_by_id, &cell_id)
}

fn replace_projection_equation_source_range(
  doc: &LoroDoc,
  equation_block_id: flowstate_document::BlockId,
  range: &std::ops::Range<usize>,
  replacement: &str,
) -> Result<bool> {
  let body = body_text(doc);
  let Some((_, block, _)) = object_loro_block_by_projected_id(doc, &body, equation_block_id) else {
    tracing::warn!(?equation_block_id, ?range, "skipping equation source edit because no Loro equation maps to the projected block id");
    return Ok(false);
  };
  if map_string_opt(&block, "kind").as_deref() != Some("equation") {
    tracing::warn!(?equation_block_id, ?range, "skipping equation source edit because the projected block is not an equation");
    return Ok(false);
  }
  let source_flow_id = map_string_opt(&block, "source_flow_id").unwrap_or_else(|| nested_flow_id("equation_source"));
  block.insert("source_flow_id", source_flow_id.as_str())?;
  let source_flow = ensure_flow(doc, &source_flow_id, "equation_source")?;
  let source_text = source_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  let before = source_text.to_string();
  let Some(start) = byte_index_to_unicode_index(&before, range.start) else {
    tracing::warn!(?equation_block_id, ?range, "skipping equation source edit because the start byte is not a source boundary");
    return Ok(false);
  };
  let Some(end) = byte_index_to_unicode_index(&before, range.end) else {
    tracing::warn!(?equation_block_id, ?range, "skipping equation source edit because the end byte is not a source boundary");
    return Ok(false);
  };
  if end < start {
    tracing::warn!(?equation_block_id, ?range, "skipping equation source edit because the range is inverted");
    return Ok(false);
  }
  if end > start {
    source_text.delete(start, end - start)?;
  }
  if !replacement.is_empty() {
    source_text.insert(start, replacement)?;
  }
  doc.commit();
  Ok(true)
}

fn replace_projection_image_alt_text(doc: &LoroDoc, image_block_id: flowstate_document::BlockId, text: &str) -> Result<bool> {
  let body = body_text(doc);
  let Some((_, block, _)) = object_loro_block_by_projected_id(doc, &body, image_block_id) else {
    tracing::warn!(?image_block_id, "skipping image alt text edit because no Loro image maps to the projected block id");
    return Ok(false);
  };
  if map_string_opt(&block, "kind").as_deref() != Some("image") {
    tracing::warn!(?image_block_id, "skipping image alt text edit because the projected block is not an image");
    return Ok(false);
  }
  let alt_flow_id = map_string_opt(&block, "alt_text_flow_id").unwrap_or_else(|| nested_flow_id("image_alt"));
  block.insert("alt_text_flow_id", alt_flow_id.as_str())?;
  let alt_flow = ensure_flow(doc, &alt_flow_id, "alt_text")?;
  replace_text_incrementally(&alt_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, text)?;
  doc.commit();
  Ok(true)
}

fn replace_projection_image_caption(
  doc: &LoroDoc,
  image_block_id: flowstate_document::BlockId,
  caption: Option<&InputParagraph>,
) -> Result<bool> {
  let body = body_text(doc);
  let Some((_, block, _)) = object_loro_block_by_projected_id(doc, &body, image_block_id) else {
    tracing::warn!(?image_block_id, "skipping image caption edit because no Loro image maps to the projected block id");
    return Ok(false);
  };
  if map_string_opt(&block, "kind").as_deref() != Some("image") {
    return Ok(false);
  }
  if let Some(caption) = caption {
    let caption_flow_id = map_string_opt(&block, "caption_flow_id").unwrap_or_else(|| nested_flow_id("image_caption"));
    block.insert("caption_flow_id", caption_flow_id.as_str())?;
    let caption_flow = ensure_flow(doc, &caption_flow_id, "caption")?;
    let text = caption_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
    let desired = format!("{SENTINEL_NEWLINE}{}", caption.runs.iter().map(|run| run.text.as_str()).collect::<String>());
    replace_text_incrementally(&text, &desired)?;
    let len = text.len_unicode();
    for key in [
      MARK_PARAGRAPH_STYLE,
      MARK_RUN_SEMANTIC_STYLE,
      MARK_HIGHLIGHT_STYLE,
      MARK_DIRECT_UNDERLINE,
      MARK_STRIKETHROUGH,
    ] {
      text.unmark(0..len, key)?;
    }
    text.mark(0..1, MARK_PARAGRAPH_STYLE, paragraph_style_value(caption.style))?;
    let mut cursor = 1usize;
    for run in &caption.runs {
      let run_len = run.text.chars().count();
      if run_len > 0 {
        mark_run_styles(&text, cursor..cursor + run_len, run.styles)?;
      }
      cursor += run_len;
    }
  } else {
    block.delete("caption_flow_id")?;
  }
  doc.commit();
  Ok(true)
}

fn set_projection_image_layout(
  doc: &LoroDoc,
  image_block_id: flowstate_document::BlockId,
  sizing: &InputImageSizing,
  alignment: InputBlockAlignment,
) -> Result<bool> {
  let body = body_text(doc);
  let Some((_, block, _)) = object_loro_block_by_projected_id(doc, &body, image_block_id) else {
    tracing::warn!(?image_block_id, "skipping image layout edit because no Loro image maps to the projected block id");
    return Ok(false);
  };
  if map_string_opt(&block, "kind").as_deref() != Some("image") {
    tracing::warn!(?image_block_id, "skipping image layout edit because the projected block is not an image");
    return Ok(false);
  }
  let attrs = block.ensure_mergeable_map("attrs")?;
  attrs.insert("alignment", alignment_name(alignment))?;
  write_image_sizing_attrs(&attrs, sizing)?;
  doc.commit();
  Ok(true)
}

fn byte_index_to_unicode_index(value: &str, byte: usize) -> Option<usize> {
  (byte <= value.len() && value.is_char_boundary(byte)).then(|| value[..byte].chars().count())
}

fn table_cell_id_by_row_column(cells_by_id: &LoroMap, row_id: &str, column_id: &str) -> Option<String> {
  map_keys(cells_by_id).into_iter().find(|cell_id| {
    child_map(cells_by_id, cell_id).is_some_and(|cell| {
      map_string_opt(&cell, "row_id").as_deref() == Some(row_id)
        && map_string_opt(&cell, "column_id").as_deref() == Some(column_id)
    })
  })
}

fn empty_input_table_cell() -> InputTableCell {
  InputTableCell {
    blocks: vec![InputTableCellBlock::Paragraph(InputParagraph {
      style: ParagraphStyle::Normal,
      runs: Vec::new(),
    })],
    row_span: 1,
    col_span: 1,
  }
}

fn projection_table_map_by_block_id(doc: &LoroDoc, table_block_id: flowstate_document::BlockId) -> Option<LoroMap> {
  let body = body_text(doc);
  let (_, block, _) = object_loro_block_by_projected_id(doc, &body, table_block_id)?;
  if map_string_opt(&block, "kind").as_deref() != Some("table") {
    return None;
  }
  child_map(&block, "table")
}

fn delete_projection_object_block(doc: &LoroDoc, block_id: flowstate_document::BlockId) -> Result<bool> {
  let body = body_text(doc);
  let Some((key, _, anchor_pos)) = object_loro_block_by_projected_id(doc, &body, block_id) else {
    tracing::warn!(?block_id, "skipping DeleteBlock because no Loro object block maps to the projected block id");
    return Ok(false);
  };
  if body.to_string().chars().nth(anchor_pos) != Some(OBJECT_REPLACEMENT) {
    tracing::warn!(?block_id, anchor_pos, "skipping DeleteBlock because the Loro object anchor is no longer live");
    return Ok(false);
  }
  body.delete(anchor_pos, 1).context("deleting object placeholder from body flow")?;
  doc
    .get_map(ROOT)
    .ensure_mergeable_map(BLOCKS_BY_ID)?
    .delete(&key)
    .context("deleting object block metadata")?;
  doc.commit();
  Ok(true)
}

fn move_projection_object_block(doc: &LoroDoc, block_id: flowstate_document::BlockId, new_block_ix: usize) -> Result<bool> {
  let body = body_text(doc);
  let Some((_, block, anchor_pos)) = object_loro_block_by_projected_id(doc, &body, block_id) else {
    tracing::warn!(?block_id, new_block_ix, "skipping MoveBlock because no Loro object block maps to the projected block id");
    return Ok(false);
  };
  if body.to_string().chars().nth(anchor_pos) != Some(OBJECT_REPLACEMENT) {
    tracing::warn!(?block_id, anchor_pos, "skipping MoveBlock because the Loro object anchor is no longer live");
    return Ok(false);
  }
  body.delete(anchor_pos, 1).context("deleting object placeholder before move")?;
  let insert_pos = object_insert_unicode_pos_for_projection_block(&body, new_block_ix).unwrap_or_else(|| body.len_unicode());
  body
    .insert(insert_pos, &OBJECT_REPLACEMENT.to_string())
    .context("reinserting object placeholder after move")?;
  if let Some(cursor) = body.get_cursor(insert_pos, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  doc.commit();
  Ok(true)
}

fn object_unicode_pos_for_projection_block(body: &LoroText, target_block_ix: usize) -> Option<usize> {
  let mut block_ix = 0_usize;
  let mut current_paragraph_has_text = false;
  let mut seen_sentinel = false;

  for (unicode_pos, ch) in body.to_string().chars().enumerate() {
    match ch {
      '\n' => {
        if seen_sentinel {
          if block_ix == target_block_ix {
            return None;
          }
          block_ix += 1;
        } else {
          seen_sentinel = true;
        }
        current_paragraph_has_text = false;
      },
      OBJECT_REPLACEMENT => {
        if current_paragraph_has_text {
          if block_ix == target_block_ix {
            return None;
          }
          block_ix += 1;
          current_paragraph_has_text = false;
        }
        if block_ix == target_block_ix {
          return Some(unicode_pos);
        }
        block_ix += 1;
      },
      _ => current_paragraph_has_text = true,
    }
  }
  None
}

fn object_insert_unicode_pos_for_projection_block(body: &LoroText, target_block_ix: usize) -> Option<usize> {
  let mut block_ix = 0_usize;
  let mut current_paragraph_has_text = false;
  let mut seen_sentinel = false;
  let mut last_pos = 0_usize;

  for (unicode_pos, ch) in body.to_string().chars().enumerate() {
    last_pos = unicode_pos + 1;
    match ch {
      '\n' => {
        if seen_sentinel {
          if block_ix >= target_block_ix {
            return Some(unicode_pos);
          }
          block_ix += 1;
        } else {
          seen_sentinel = true;
        }
        current_paragraph_has_text = false;
      },
      OBJECT_REPLACEMENT => {
        if current_paragraph_has_text {
          if block_ix >= target_block_ix {
            return Some(unicode_pos);
          }
          block_ix += 1;
          current_paragraph_has_text = false;
        }
        if block_ix >= target_block_ix {
          return Some(unicode_pos);
        }
        block_ix += 1;
      },
      _ => current_paragraph_has_text = true,
    }
  }

  if current_paragraph_has_text {
    if block_ix >= target_block_ix {
      return Some(last_pos);
    }
    block_ix += 1;
  }
  (block_ix <= target_block_ix).then_some(last_pos)
}

fn object_loro_block_by_projected_id(doc: &LoroDoc, body: &LoroText, block_id: flowstate_document::BlockId) -> Option<(String, LoroMap, usize)> {
  let root = doc.get_map(ROOT);
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID).ok()?;
  let body_snapshot = body.to_string();
  for key in map_keys(&blocks) {
    if loro_id_u128(&key) != block_id.0 {
      continue;
    }
    let block = child_map(&blocks, &key)?;
    if map_string_opt(&block, "kind").as_deref() == Some("paragraph") {
      return None;
    }
    let anchor_pos = live_object_cursor_pos(doc, &body_snapshot, &block, "anchor_cursor")?;
    return Some((key, block, anchor_pos));
  }
  for key in map_keys(&blocks) {
    let block = child_map(&blocks, &key)?;
    if map_string_opt(&block, "kind").as_deref() == Some("paragraph") {
      continue;
    }
    if map_string_opt(&block, "id").is_some_and(|id| loro_id_u128(&id) == block_id.0) {
      let anchor_pos = live_object_cursor_pos(doc, &body_snapshot, &block, "anchor_cursor")?;
      return Some((key, block, anchor_pos));
    }
  }
  None
}

fn object_loro_block_at_unicode_pos(doc: &LoroDoc, body: &LoroText, unicode_pos: usize) -> Option<LoroMap> {
  let root = doc.get_map(ROOT);
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID).ok()?;
  let body_snapshot = body.to_string();
  for key in map_keys(&blocks) {
    let block = child_map(&blocks, &key)?;
    if map_string_opt(&block, "kind").as_deref() == Some("paragraph") {
      continue;
    }
    if live_object_cursor_pos(doc, &body_snapshot, &block, "anchor_cursor") == Some(unicode_pos) {
      return Some(block);
    }
  }
  None
}

fn loro_id_u128(id: &str) -> u128 {
  if let Some(value) = id.rsplit('.').next().and_then(|suffix| suffix.parse::<u128>().ok()) {
    return value;
  }
  Uuid::new_v5(&Uuid::NAMESPACE_OID, id.as_bytes()).as_u128()
}

fn replace_image_block_from_input(doc: &LoroDoc, block: &LoroMap, image: &flowstate_document::InputImageBlock) -> Result<()> {
  block.insert("kind", "image")?;
  block.insert("asset_id", image.asset_id.0.to_string())?;
  copy_asset_metadata_to_image_block(doc, block, image.asset_id.0)?;

  let alt_flow_id = map_string_opt(block, "alt_text_flow_id").unwrap_or_else(|| nested_flow_id("image_alt"));
  block.insert("alt_text_flow_id", alt_flow_id.as_str())?;
  let alt_flow = ensure_flow(doc, &alt_flow_id, "alt_text")?;
  replace_text(&alt_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, image.alt_text.as_ref())?;

  if let Some(caption) = &image.caption {
    let caption_flow_id = map_string_opt(block, "caption_flow_id").unwrap_or_else(|| nested_flow_id("image_caption"));
    block.insert("caption_flow_id", caption_flow_id.as_str())?;
    let caption_flow = ensure_flow(doc, &caption_flow_id, "caption")?;
    let caption_text = caption_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
    replace_text(&caption_text, SENTINEL_NEWLINE)?;
    append_input_paragraph_text_only(&caption_text, caption)?;
  } else {
    block.delete("caption_flow_id")?;
  }

  let attrs = block.ensure_mergeable_map("attrs")?;
  attrs.insert("alignment", alignment_name(image.alignment))?;
  write_image_sizing_attrs(&attrs, &image.sizing)?;
  Ok(())
}

fn copy_asset_metadata_to_image_block(doc: &LoroDoc, block: &LoroMap, asset_id: u128) -> Result<()> {
  let root = doc.get_map(ROOT);
  let Some(assets) = child_map(&root, flowstate_document::loro_schema::ASSETS_BY_ID) else {
    return Ok(());
  };
  let Some(asset) = child_map(&assets, &asset_id.to_string()) else {
    return Ok(());
  };
  for field in ["content_hash", "mime_type", "byte_length"] {
    if let Some(ValueOrContainer::Value(value)) = asset.get(field) {
      block.insert(field, value)?;
    }
  }
  Ok(())
}

fn refresh_image_asset_metadata(doc: &LoroDoc) -> Result<()> {
  let root = doc.get_map(ROOT);
  let Some(blocks) = child_map(&root, BLOCKS_BY_ID) else {
    return Ok(());
  };
  for key in map_keys(&blocks) {
    let Some(block) = child_map(&blocks, &key) else {
      continue;
    };
    if map_string_opt(&block, "kind").as_deref() != Some("image") {
      continue;
    }
    let Some(asset_id) = map_string_opt(&block, "asset_id").and_then(|id| id.parse().ok()) else {
      continue;
    };
    copy_asset_metadata_to_image_block(doc, &block, asset_id)?;
  }
  Ok(())
}

fn replace_equation_block_from_input(doc: &LoroDoc, block: &LoroMap, equation: &flowstate_document::InputEquationBlock) -> Result<()> {
  block.insert("kind", "equation")?;
  let source_flow_id = map_string_opt(block, "source_flow_id").unwrap_or_else(|| nested_flow_id("equation_source"));
  block.insert("source_flow_id", source_flow_id.as_str())?;
  let source_flow = ensure_flow(doc, &source_flow_id, "equation_source")?;
  replace_text(&source_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, &equation.source)?;
  let attrs = block.ensure_mergeable_map("attrs")?;
  attrs.insert("syntax", "latex")?;
  attrs.insert("display", equation_display_name(equation.display))?;
  Ok(())
}

fn replace_table_block_from_input(doc: &LoroDoc, block: &LoroMap, table: &InputTableBlock) -> Result<()> {
  block.insert("kind", "table")?;
  let table_map = block.ensure_mergeable_map("table")?;
  write_table_map_from_input(doc, &table_map, table, &table_id())
}

fn write_image_sizing_attrs(attrs: &LoroMap, sizing: &InputImageSizing) -> Result<()> {
  attrs.delete("width_px")?;
  attrs.delete("height_px")?;
  match sizing {
    InputImageSizing::Intrinsic => attrs.insert("sizing", "intrinsic")?,
    InputImageSizing::FitWidth => attrs.insert("sizing", "fit_width")?,
    InputImageSizing::Fixed { width_px, height_px } => {
      attrs.insert("sizing", "fixed")?;
      attrs.insert("width_px", i64::from(*width_px))?;
      if let Some(height_px) = *height_px {
        attrs.insert("height_px", i64::from(height_px))?;
      }
    },
  };
  Ok(())
}

fn write_table_map_from_input(doc: &LoroDoc, table_map: &LoroMap, table: &InputTableBlock, prefix: &str) -> Result<()> {
  table_map.insert("header_row", table.style.header_row)?;
  let row_order = table_map.ensure_mergeable_movable_list("row_order")?;
  let column_order = table_map.ensure_mergeable_movable_list("column_order")?;
  let rows_by_id = table_map.ensure_mergeable_map("rows_by_id")?;
  let columns_by_id = table_map.ensure_mergeable_map("columns_by_id")?;
  let cells_by_id = table_map.ensure_mergeable_map("cells_by_id")?;
  table_map.insert("container_id", table_map.id().to_string())?;
  table_map.insert("row_order_container_id", row_order.id().to_string())?;
  table_map.insert("column_order_container_id", column_order.id().to_string())?;
  table_map.insert("rows_container_id", rows_by_id.id().to_string())?;
  table_map.insert("columns_container_id", columns_by_id.id().to_string())?;
  table_map.insert("cells_container_id", cells_by_id.id().to_string())?;
  clear_movable_list(&row_order)?;
  clear_movable_list(&column_order)?;
  clear_map(&rows_by_id)?;
  clear_map(&columns_by_id)?;
  clear_map(&cells_by_id)?;

  let column_count = table.column_widths.len().max(
    table
      .rows
      .iter()
      .map(|row| row.cells.iter().map(|cell| usize::from(cell.col_span.max(1))).sum())
      .max()
      .unwrap_or(0),
  );
  let mut column_ids = Vec::with_capacity(column_count);
  for column_ix in 0..column_count {
    let column_id = format!("{prefix}.column.{column_ix}");
    column_order.push(column_id.as_str())?;
    column_ids.push(column_id.clone());
    let column = columns_by_id.ensure_mergeable_map(&column_id)?;
    column.insert("id", column_id.as_str())?;
    column.insert("container_id", column.id().to_string())?;
    column.insert("container_id", column.id().to_string())?;
    column.ensure_mergeable_map("attrs")?;
    write_table_column_width(&column, table.column_widths.get(column_ix).unwrap_or(&InputTableColumnWidth::Auto))?;
  }

  for (row_ix, row) in table.rows.iter().enumerate() {
    let row_id = format!("{prefix}.row.{row_ix}");
    row_order.push(row_id.as_str())?;
    let row_map = rows_by_id.ensure_mergeable_map(&row_id)?;
    row_map.insert("id", row_id.as_str())?;
    row_map.insert("container_id", row_map.id().to_string())?;
    row_map.ensure_mergeable_map("attrs")?;
    let mut column_ix = 0_usize;
    for (cell_ix, cell) in row.cells.iter().enumerate() {
      let Some(column_id) = column_ids.get(column_ix) else {
        break;
      };
      let cell_id = format!("{row_id}.cell.{cell_ix}");
      let cell_map = cells_by_id.ensure_mergeable_map(&cell_id)?;
      write_table_cell_map_from_input(doc, &cell_map, &cell_id, &row_id, column_id, cell)?;
      column_ix += usize::from(cell.col_span.max(1));
    }
  }
  Ok(())
}

fn write_table_cell_map_from_input(
  doc: &LoroDoc,
  cell_map: &LoroMap,
  cell_id: &str,
  row_id: &str,
  column_id: &str,
  cell: &InputTableCell,
) -> Result<()> {
  cell_map.insert("id", cell_id)?;
  cell_map.insert("container_id", cell_map.id().to_string())?;
  cell_map.insert("row_id", row_id)?;
  cell_map.insert("column_id", column_id)?;
  cell_map.insert("row_span", i64::from(cell.row_span))?;
  cell_map.insert("column_span", i64::from(cell.col_span))?;
  cell_map.ensure_mergeable_map("attrs")?;
  let nested_table_ids = cell_map.ensure_mergeable_movable_list("nested_table_ids")?;
  let nested_tables_by_id = cell_map.ensure_mergeable_map("nested_tables_by_id")?;
  cell_map.insert("nested_table_order_container_id", nested_table_ids.id().to_string())?;
  cell_map.insert("nested_tables_container_id", nested_tables_by_id.id().to_string())?;
  clear_movable_list(&nested_table_ids)?;
  clear_map(&nested_tables_by_id)?;
  let flow_id = format!("{cell_id}.flow");
  cell_map.insert("flow_id", flow_id.as_str())?;
  let flow = ensure_flow(doc, &flow_id, "table_cell")?;
  let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  cell_map.insert("flow_container_id", flow.id().to_string())?;
  cell_map.insert("text_container_id", text.id().to_string())?;
  replace_text(&text, SENTINEL_NEWLINE)?;
  text.mark(0..1, MARK_PARAGRAPH_STYLE, 0_i64)?;
  for (block_ix, cell_block) in cell.blocks.iter().enumerate() {
    match cell_block {
      InputTableCellBlock::Paragraph(paragraph) => append_input_paragraph_text_only(&text, paragraph)?,
      InputTableCellBlock::Table(nested) => {
        let pos = text.len_unicode();
        text.insert(pos, &OBJECT_REPLACEMENT.to_string())?;
        let nested_table_id = format!("{cell_id}.nested_table.{block_ix}");
        nested_table_ids.push(nested_table_id.as_str())?;
        let nested_map = nested_tables_by_id.ensure_mergeable_map(&nested_table_id)?;
        nested_map.insert("id", nested_table_id.as_str())?;
        nested_map.insert("container_id", nested_map.id().to_string())?;
        nested_map.insert("kind", "table")?;
        if let Some(cursor) = text.get_cursor(pos, Side::Left) {
          nested_map.insert("anchor_cursor", cursor.encode())?;
        }
        nested_map.ensure_mergeable_map("attrs")?;
        write_table_map_from_input(doc, &nested_map.ensure_mergeable_map("table")?, nested, &format!("{cell_id}.nested.{block_ix}"))?;
      },
    }
  }
  Ok(())
}

fn update_table_cell_map_from_input(
  doc: &LoroDoc,
  cell_map: &LoroMap,
  cell_id: &str,
  row_id: &str,
  column_id: &str,
  cell: &InputTableCell,
) -> Result<()> {
  if cell
    .blocks
    .iter()
    .any(|block| matches!(block, InputTableCellBlock::Table(_)))
  {
    tracing::warn!(cell_id, "using full table-cell rebuild fallback for nested table structure");
    return write_table_cell_map_from_input(doc, cell_map, cell_id, row_id, column_id, cell);
  }
  cell_map.insert("id", cell_id)?;
  cell_map.insert("container_id", cell_map.id().to_string())?;
  cell_map.insert("row_id", row_id)?;
  cell_map.insert("column_id", column_id)?;
  cell_map.insert("row_span", i64::from(cell.row_span))?;
  cell_map.insert("column_span", i64::from(cell.col_span))?;
  cell_map.ensure_mergeable_map("attrs")?;
  let flow_id = map_string_opt(cell_map, "flow_id").unwrap_or_else(|| format!("{cell_id}.flow"));
  cell_map.insert("flow_id", flow_id.as_str())?;
  let flow = ensure_flow(doc, &flow_id, "table_cell")?;
  let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  cell_map.insert("flow_container_id", flow.id().to_string())?;
  cell_map.insert("text_container_id", text.id().to_string())?;

  let paragraphs = cell
    .blocks
    .iter()
    .filter_map(|block| match block {
      InputTableCellBlock::Paragraph(paragraph) => Some(paragraph),
      InputTableCellBlock::Table(_) => None,
    })
    .collect::<Vec<_>>();
  let desired = if paragraphs.is_empty() {
    SENTINEL_NEWLINE.to_string()
  } else {
    let mut desired = String::from(SENTINEL_NEWLINE);
    for (paragraph_ix, paragraph) in paragraphs.iter().enumerate() {
      if paragraph_ix > 0 {
        desired.push('\n');
      }
      for run in &paragraph.runs {
        desired.push_str(&run.text);
      }
    }
    desired
  };
  replace_text_incrementally(&text, &desired)?;
  let len = text.len_unicode();
  for key in [
    MARK_PARAGRAPH_STYLE,
    MARK_RUN_SEMANTIC_STYLE,
    MARK_HIGHLIGHT_STYLE,
    MARK_DIRECT_UNDERLINE,
    MARK_STRIKETHROUGH,
  ] {
    text.unmark(0..len, key)?;
  }
  if paragraphs.is_empty() {
    text.mark(0..1, MARK_PARAGRAPH_STYLE, paragraph_style_value(ParagraphStyle::Normal))?;
    return Ok(());
  }
  let mut cursor = 0usize;
  for paragraph in paragraphs {
    text.mark(cursor..cursor + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(paragraph.style))?;
    cursor += 1;
    for run in &paragraph.runs {
      let run_len = run.text.chars().count();
      if run_len > 0 {
        mark_run_styles(&text, cursor..cursor + run_len, run.styles)?;
      }
      cursor += run_len;
    }
  }
  Ok(())
}

fn replace_text_incrementally(text: &LoroText, desired: &str) -> loro::LoroResult<()> {
  let current = text.to_string();
  if current == desired {
    return Ok(());
  }
  let current_chars = current.chars().collect::<Vec<_>>();
  let desired_chars = desired.chars().collect::<Vec<_>>();
  let prefix = current_chars
    .iter()
    .zip(&desired_chars)
    .take_while(|(left, right)| left == right)
    .count();
  let suffix = current_chars
    .iter()
    .skip(prefix)
    .rev()
    .zip(desired_chars.iter().skip(prefix).rev())
    .take_while(|(left, right)| left == right)
    .count();
  let delete_len = current_chars.len().saturating_sub(prefix + suffix);
  if delete_len > 0 {
    text.delete(prefix, delete_len)?;
  }
  let insert_end = desired_chars.len().saturating_sub(suffix);
  if insert_end > prefix {
    let insert = desired_chars[prefix..insert_end].iter().collect::<String>();
    text.insert(prefix, &insert)?;
  }
  Ok(())
}

fn write_table_column_width(column: &LoroMap, width: &InputTableColumnWidth) -> Result<()> {
  column.delete("width_px")?;
  column.delete("fraction")?;
  match width {
    InputTableColumnWidth::Auto => column.insert("width_kind", "auto")?,
    InputTableColumnWidth::FixedPx(px) => {
      column.insert("width_kind", "fixed_px")?;
      column.insert("width_px", i64::from(*px))?;
    },
    InputTableColumnWidth::Fraction(fraction) => {
      column.insert("width_kind", "fraction")?;
      column.insert("fraction", i64::from(*fraction))?;
    },
  };
  Ok(())
}

fn append_input_paragraph_text_only(text: &LoroText, paragraph: &InputParagraph) -> Result<()> {
  let use_existing_sentinel = text.len_unicode() == 1 && text.to_string() == SENTINEL_NEWLINE;
  let boundary_pos = if use_existing_sentinel {
    0
  } else {
    let pos = text.len_unicode();
    text.insert(pos, "\n")?;
    pos
  };
  text.mark(boundary_pos..boundary_pos + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(paragraph.style))?;
  for run in &paragraph.runs {
    if run.text.is_empty() {
      continue;
    }
    let start = text.len_unicode();
    text.insert(start, &run.text)?;
    let len = run.text.chars().count();
    mark_run_styles(text, start..start + len, run.styles)?;
  }
  Ok(())
}

fn clear_map(map: &LoroMap) -> loro::LoroResult<()> {
  for key in map_keys(map) {
    map.delete(&key)?;
  }
  Ok(())
}

fn clear_movable_list(list: &LoroMovableList) -> loro::LoroResult<()> {
  let len = list.len();
  if len > 0 {
    list.delete(0, len)?;
  }
  Ok(())
}

fn projection_offset_to_body_unicode_index(projection: &DocumentProjection, offset: flowstate_document::DocumentOffset) -> usize {
  ProjectionRuntimeIndex::from_projection(projection)
    .body_unicode_for_offset(projection, offset)
    .unwrap_or(1)
}

fn clamp_projection_offset(projection: &DocumentProjection, offset: DocumentOffset) -> DocumentOffset {
  let paragraph = offset
    .paragraph
    .min(projection.paragraphs.len().saturating_sub(1));
  let byte = projection
    .paragraphs
    .get(paragraph)
    .map(flowstate_document::paragraph_text_len)
    .unwrap_or_default()
    .min(offset.byte);
  DocumentOffset { paragraph, byte }
}

fn paragraph_boundary_unicode_index(projection: &DocumentProjection, paragraph_ix: usize) -> usize {
  if paragraph_ix == 0 {
    return 0;
  }
  projection_offset_to_body_unicode_index(
    projection,
    flowstate_document::DocumentOffset {
      paragraph: paragraph_ix,
      byte: 0,
    },
  ) - 1
}

fn join_projection_paragraphs(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  first: flowstate_document::ParagraphId,
  second: flowstate_document::ParagraphId,
) -> Result<bool> {
  let Some(first_ix) = projection.ids.paragraph_ids.iter().position(|id| *id == first) else {
    tracing::warn!(?first, ?second, "skipping JoinParagraphs because the first paragraph id is absent from the supplied projection");
    return Ok(false);
  };
  let Some(second_ix) = projection.ids.paragraph_ids.iter().position(|id| *id == second) else {
    tracing::warn!(?first, ?second, "skipping JoinParagraphs because the second paragraph id is absent from the supplied projection");
    return Ok(false);
  };
  if first_ix + 1 != second_ix {
    tracing::warn!(?first, ?second, first_ix, second_ix, "skipping JoinParagraphs for non-adjacent paragraphs");
    return Ok(false);
  }
  if !projection_paragraph_blocks_are_adjacent(projection, first_ix, second_ix) {
    tracing::warn!(
      ?first,
      ?second,
      first_ix,
      second_ix,
      "skipping JoinParagraphs because an object block separates the paragraphs and projection offsets are not object-aware",
    );
    return Ok(false);
  }

  let boundary = paragraph_boundary_unicode_index(projection, second_ix);
  let body = body_text(doc);
  if !boundary_is_live(&body.to_string(), boundary) {
    tracing::warn!(
      ?first,
      ?second,
      boundary,
      "skipping JoinParagraphs because the computed Loro boundary is not a live paragraph newline",
    );
    return Ok(false);
  }
  body.delete(boundary, 1).context("deleting joined paragraph boundary from Loro body flow")?;
  repair_paragraph_metadata_after_text_flow_edit(doc, &body, &[], "editor_join_paragraphs")?;
  doc.commit();
  Ok(true)
}

fn projection_paragraph_blocks_are_adjacent(projection: &DocumentProjection, first_ix: usize, second_ix: usize) -> bool {
  let Some(first_block_ix) = flowstate_document::block_ix_for_paragraph(projection, first_ix) else {
    return false;
  };
  let Some(second_block_ix) = flowstate_document::block_ix_for_paragraph(projection, second_ix) else {
    return false;
  };
  second_block_ix == first_block_ix + 1
}

fn replace_body_paragraph_span(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  start: Option<flowstate_document::DocumentOffset>,
  before: &flowstate_document::DocumentSpan,
  after: &flowstate_document::DocumentSpan,
) -> Result<bool> {
  if before.paragraphs.is_empty() && after.paragraphs.is_empty() {
    return Ok(false);
  }
  let start = projection_offset_to_body_unicode_index(
    projection,
    start.unwrap_or(flowstate_document::DocumentOffset {
      paragraph: before.start_paragraph,
      byte: 0,
    }),
  );
  let paragraph_texts = span_paragraph_texts(after);
  let replacement = paragraph_texts.join("\n");
  let before_chars = before.text.chars().collect::<Vec<_>>();
  let replacement_chars = replacement.chars().collect::<Vec<_>>();
  let common_prefix = before_chars
    .iter()
    .zip(&replacement_chars)
    .take_while(|(before, after)| before == after)
    .count();
  let max_suffix = before_chars.len().min(replacement_chars.len()).saturating_sub(common_prefix);
  let common_suffix = before_chars
    .iter()
    .rev()
    .zip(replacement_chars.iter().rev())
    .take(max_suffix)
    .take_while(|(before, after)| before == after)
    .count();
  let before_changed_len = before_chars.len().saturating_sub(common_prefix + common_suffix);
  let replacement_changed_end = replacement_chars.len().saturating_sub(common_suffix);
  let replacement_changed = replacement_chars[common_prefix..replacement_changed_end]
    .iter()
    .collect::<String>();
  let body = body_text(doc);
  let start = start.min(body.len_unicode());
  let change_start = start.saturating_add(common_prefix).min(body.len_unicode());
  let change_end = change_start.saturating_add(before_changed_len).min(body.len_unicode());
  if change_end > change_start {
    body.delete(change_start, change_end - change_start)?;
  }
  if !replacement_changed.is_empty() {
    body.insert(change_start, &replacement_changed)?;
  }
  let first_boundary = start.saturating_sub(1);
  mark_replacement_span(&body, first_boundary, start, after, &paragraph_texts)?;
  let boundaries = replacement_span_boundaries(first_boundary, start, &paragraph_texts);
  repair_paragraph_metadata_after_text_flow_edit(doc, &body, &boundaries, "editor_replace_paragraph_span")?;
  doc.commit();
  Ok(true)
}

fn span_paragraph_texts(span: &flowstate_document::DocumentSpan) -> Vec<String> {
  let mut offset = 0_usize;
  span
    .paragraphs
    .iter()
    .enumerate()
    .map(|(paragraph_ix, paragraph)| {
      if paragraph_ix > 0 && span.text.get(offset..).is_some_and(|text| text.starts_with('\n')) {
        offset += '\n'.len_utf8();
      }
      let len = flowstate_document::paragraph_text_len(paragraph);
      let end = offset.saturating_add(len).min(span.text.len());
      let text = span.text.get(offset..end).unwrap_or_default().to_string();
      offset = end;
      text
    })
    .collect()
}

fn mark_replacement_span(
  body: &loro::LoroText,
  first_boundary_unicode: usize,
  text_start_unicode: usize,
  span: &flowstate_document::DocumentSpan,
  paragraph_texts: &[String],
) -> loro::LoroResult<()> {
  if let Some(first) = span.paragraphs.first() {
    body.mark(
      first_boundary_unicode..first_boundary_unicode + 1,
      MARK_PARAGRAPH_STYLE,
      paragraph_style_value(first.style),
    )?;
  }
  mark_projected_paragraphs(body, text_start_unicode, &span.paragraphs, paragraph_texts)
}

fn mark_projected_paragraphs(
  body: &loro::LoroText,
  text_start_unicode: usize,
  paragraphs: &[flowstate_document::Paragraph],
  paragraph_texts: &[String],
) -> loro::LoroResult<()> {
  let mut paragraph_start = text_start_unicode;
  for (paragraph_ix, (paragraph, paragraph_text)) in paragraphs.iter().zip(paragraph_texts).enumerate() {
    if paragraph_ix > 0 {
      let boundary = paragraph_start.saturating_sub(1);
      body.mark(boundary..boundary + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(paragraph.style))?;
    }
    mark_paragraph_runs(body, paragraph_start, paragraph_text, &paragraph.runs)?;
    paragraph_start += paragraph_text.chars().count() + 1;
  }
  Ok(())
}

fn mark_paragraph_runs(
  body: &loro::LoroText,
  paragraph_start_unicode: usize,
  paragraph_text: &str,
  runs: &[flowstate_document::TextRun],
) -> loro::LoroResult<()> {
  let mut byte_offset = 0_usize;
  for run in runs {
    let end = byte_offset.saturating_add(run.len).min(paragraph_text.len());
    let Some(run_text) = paragraph_text.get(byte_offset..end) else {
      break;
    };
    let run_len = run_text.chars().count();
    if run_len > 0 {
      let run_start = paragraph_start_unicode + paragraph_text.get(..byte_offset).unwrap_or_default().chars().count();
      mark_run_styles(body, run_start..run_start + run_len, run.styles)?;
    }
    byte_offset = end;
  }
  Ok(())
}

fn inserted_newline_boundaries(start_unicode: usize, text: &str) -> Vec<usize> {
  text
    .chars()
    .enumerate()
    .filter_map(|(offset, ch)| (ch == '\n').then_some(start_unicode + offset))
    .collect()
}

fn persist_body_paragraph_style_mark_repair(
  doc: &LoroDoc,
  package: Option<&mut DocumentPackage>,
  package_path: Option<&Path>,
) -> Result<()> {
  let from_frontier = doc.state_frontiers();
  let from_vv = doc.state_vv();
  let replica_registered = flowstate_document::register_replica(doc, None)?;
  let paragraph_marks_repaired = repair_missing_paragraph_style_marks(doc)?;
  if !replica_registered && !paragraph_marks_repaired {
    return Ok(());
  }
  let Some(package) = package else {
    return Ok(());
  };
  package.sync_revisions_from_loro(doc)?;
  let update = doc
    .export(ExportMode::updates(&from_vv))
    .context("exporting paragraph style repair update")?;
  if !update.is_empty() {
    package.append_update_segment(&from_frontier, &from_vv, &doc.state_frontiers(), &doc.state_vv(), update)?;
    package.compact_update_segments_if_needed(doc, DEFAULT_UPDATE_SEGMENT_COMPACTION_THRESHOLD)?;
  }
  package.rebuild_search_units_from_loro(doc)?;
  if let Some(path) = package_path {
    package.write(path)?;
  }
  Ok(())
}

fn repair_missing_paragraph_style_marks(doc: &LoroDoc) -> Result<bool> {
  let root = doc.get_map(ROOT);
  let Some(flows) = child_map(&root, FLOWS_BY_ID) else {
    return Ok(false);
  };
  let mut repaired = false;
  for flow_id in map_keys(&flows) {
    let Some(flow) = child_map(&flows, &flow_id) else {
      continue;
    };
    if !matches!(
      map_string_opt(&flow, FLOW_KIND_KEY).as_deref(),
      Some("body" | "table_cell" | "caption" | "header" | "footer")
    ) {
      continue;
    }
    let Some(ValueOrContainer::Container(Container::Text(text))) = flow.get(FLOW_TEXT_KEY) else {
      continue;
    };
    for boundary in body_paragraph_boundaries_missing_style_mark(&text) {
      text
        .mark(boundary..boundary + 1, MARK_PARAGRAPH_STYLE, paragraph_style_value(ParagraphStyle::Normal))
        .context("repairing missing paragraph style mark")?;
      repaired = true;
    }
  }
  if repaired {
    doc.commit();
  }
  Ok(repaired)
}

fn body_paragraph_boundaries_missing_style_mark(body: &loro::LoroText) -> Vec<usize> {
  let mut missing = Vec::new();
  let mut unicode_pos = 0_usize;
  for item in body.to_delta() {
    let loro::TextDelta::Insert { insert, attributes } = item else {
      continue;
    };
    let has_paragraph_style = paragraph_style_from_attrs(attributes.as_ref()).is_some();
    for ch in insert.chars() {
      if ch == '\n' && !has_paragraph_style {
        missing.push(unicode_pos);
      }
      unicode_pos += 1;
    }
  }
  missing
}

fn replacement_span_boundaries(first_boundary_unicode: usize, text_start_unicode: usize, paragraph_texts: &[String]) -> Vec<usize> {
  if paragraph_texts.is_empty() {
    return Vec::new();
  }
  let mut boundaries = Vec::with_capacity(paragraph_texts.len());
  boundaries.push(first_boundary_unicode);
  let mut paragraph_start = text_start_unicode;
  for (paragraph_ix, paragraph_text) in paragraph_texts.iter().enumerate() {
    if paragraph_ix > 0 {
      boundaries.push(paragraph_start.saturating_sub(1));
    }
    paragraph_start += paragraph_text.chars().count() + 1;
  }
  boundaries
}

fn repair_paragraph_metadata_after_text_flow_edit(
  doc: &LoroDoc,
  body: &loro::LoroText,
  live_boundaries: &[usize],
  reason: &'static str,
) -> loro::LoroResult<()> {
  for boundary in live_boundaries {
    ensure_paragraph_metadata_at_boundary(doc, body, *boundary)?;
  }
  let pruned = prune_stale_paragraph_metadata(doc, body)?;
  if pruned.changed() {
    tracing::warn!(
      reason,
      stale_paragraphs = pruned.stale_paragraphs,
      duplicate_paragraphs = pruned.duplicate_paragraphs,
      stale_blocks = pruned.stale_blocks,
      duplicate_blocks = pruned.duplicate_blocks,
      "pruned stale Loro paragraph metadata after text-flow edit",
    );
  }
  Ok(())
}

fn ensure_paragraph_metadata_at_boundary(doc: &LoroDoc, body: &loro::LoroText, boundary: usize) -> loro::LoroResult<()> {
  let body_snapshot = body.to_string();
  if !boundary_is_live(&body_snapshot, boundary) {
    tracing::warn!(boundary, "cannot create paragraph metadata because boundary is not a live paragraph newline");
    return Ok(());
  }

  let root = doc.get_map(ROOT);
  let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  let paragraph_id = paragraph_metadata_key_at_boundary(doc, &body_snapshot, &paragraphs, boundary).unwrap_or_else(|| new_paragraph_metadata_id(boundary));
  let paragraph = paragraphs.ensure_mergeable_map(&paragraph_id)?;
  paragraph.insert("id", paragraph_id.as_str())?;
  paragraph.insert("container_id", paragraph.id().to_string())?;
  paragraph.insert("flow_id", ROOT_BODY_FLOW_ID)?;
  if let Some(cursor) = body.get_cursor(boundary, Side::Left) {
    paragraph.insert("start_cursor", cursor.encode())?;
  }
  if let Some(cursor) = body.get_cursor(boundary, Side::Right) {
    paragraph.insert("boundary_cursor", cursor.encode())?;
  }
  let paragraph_attrs = paragraph.ensure_mergeable_map("attrs")?;
  paragraph.insert("attrs_container_id", paragraph_attrs.id().to_string())?;

  let block_id = paragraph_block_key_at_boundary(doc, &body_snapshot, &blocks, boundary).unwrap_or_else(|| new_paragraph_block_id(boundary));
  let block = blocks.ensure_mergeable_map(&block_id)?;
  block.insert("id", block_id.as_str())?;
  block.insert("container_id", block.id().to_string())?;
  block.insert("kind", "paragraph")?;
  block.insert("flow_id", ROOT_BODY_FLOW_ID)?;
  block.insert("paragraph_id", paragraph_id.as_str())?;
  if let Some(cursor) = body.get_cursor(boundary, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  let block_attrs = block.ensure_mergeable_map("attrs")?;
  let nested_refs = block.ensure_mergeable_map("nested_refs")?;
  block.insert("attrs_container_id", block_attrs.id().to_string())?;
  block.insert("nested_refs_container_id", nested_refs.id().to_string())?;
  Ok(())
}

fn paragraph_metadata_key_at_boundary(doc: &LoroDoc, body_snapshot: &str, paragraphs: &LoroMap, boundary: usize) -> Option<String> {
  let mut keys = metadata_keys_at_boundary(doc, body_snapshot, paragraphs, "boundary_cursor", boundary);
  if boundary == 0
    && let Some(root_ix) = keys.iter().position(|key| key == ROOT_FIRST_PARAGRAPH_ID)
  {
    return Some(keys.swap_remove(root_ix));
  }
  keys.into_iter().next()
}

fn paragraph_block_key_at_boundary(doc: &LoroDoc, body_snapshot: &str, blocks: &LoroMap, boundary: usize) -> Option<String> {
  let mut keys = Vec::new();
  for key in map_keys(blocks) {
    let Some(block) = child_map(blocks, &key) else {
      continue;
    };
    if map_string_opt(&block, "kind").as_deref() != Some("paragraph") {
      continue;
    }
    if live_cursor_pos(doc, body_snapshot, &block, "anchor_cursor") == Some(boundary) {
      keys.push(key);
    }
  }
  if boundary == 0
    && let Some(main_ix) = keys.iter().position(|key| key == MAIN_BODY_BLOCK_ID)
  {
    return Some(keys.swap_remove(main_ix));
  }
  keys.into_iter().next()
}

fn metadata_keys_at_boundary(doc: &LoroDoc, body_snapshot: &str, maps: &LoroMap, cursor_key: &str, boundary: usize) -> Vec<String> {
  map_keys(maps)
    .into_iter()
    .filter(|key| {
      child_map(maps, key)
        .as_ref()
        .and_then(|map| live_cursor_pos(doc, body_snapshot, map, cursor_key))
        == Some(boundary)
    })
    .collect()
}

#[derive(Default)]
struct ParagraphMetadataPrune {
  stale_paragraphs: usize,
  duplicate_paragraphs: usize,
  stale_blocks: usize,
  duplicate_blocks: usize,
}

impl ParagraphMetadataPrune {
  fn changed(&self) -> bool {
    self.stale_paragraphs > 0 || self.duplicate_paragraphs > 0 || self.stale_blocks > 0 || self.duplicate_blocks > 0
  }
}

fn prune_stale_paragraph_metadata(doc: &LoroDoc, body: &loro::LoroText) -> loro::LoroResult<ParagraphMetadataPrune> {
  let body_snapshot = body.to_string();
  let root = doc.get_map(ROOT);
  let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID)?;
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  let mut pruned = ParagraphMetadataPrune::default();

  let mut paragraph_by_boundary = BTreeMap::<usize, String>::new();
  let mut paragraphs_to_delete = Vec::new();
  for key in map_keys(&paragraphs) {
    let Some(paragraph) = child_map(&paragraphs, &key) else {
      paragraphs_to_delete.push(key);
      pruned.stale_paragraphs += 1;
      continue;
    };
    let Some(boundary) = live_cursor_pos(doc, &body_snapshot, &paragraph, "boundary_cursor")
      .or_else(|| live_cursor_pos(doc, &body_snapshot, &paragraph, "start_cursor"))
    else {
      paragraphs_to_delete.push(key);
      pruned.stale_paragraphs += 1;
      continue;
    };
    if let Some(existing) = paragraph_by_boundary.get(&boundary) {
      if prefer_paragraph_metadata_key(boundary, existing, &key) {
        paragraphs_to_delete.push(existing.clone());
        paragraph_by_boundary.insert(boundary, key);
      } else {
        paragraphs_to_delete.push(key);
      }
      pruned.duplicate_paragraphs += 1;
    } else {
      paragraph_by_boundary.insert(boundary, key);
    }
  }
  for key in paragraphs_to_delete {
    paragraphs.delete(&key)?;
  }

  let mut block_by_boundary = BTreeMap::<usize, String>::new();
  let mut blocks_to_delete = Vec::new();
  for key in map_keys(&blocks) {
    let Some(block) = child_map(&blocks, &key) else {
      continue;
    };
    if map_string_opt(&block, "kind").as_deref() != Some("paragraph") {
      continue;
    }
    let Some(boundary) = live_cursor_pos(doc, &body_snapshot, &block, "anchor_cursor") else {
      blocks_to_delete.push(key);
      pruned.stale_blocks += 1;
      continue;
    };
    if let Some(existing) = block_by_boundary.get(&boundary) {
      if prefer_paragraph_block_key(boundary, existing, &key) {
        blocks_to_delete.push(existing.clone());
        block_by_boundary.insert(boundary, key);
      } else {
        blocks_to_delete.push(key);
      }
      pruned.duplicate_blocks += 1;
    } else {
      block_by_boundary.insert(boundary, key);
    }
  }
  for key in blocks_to_delete {
    blocks.delete(&key)?;
  }

  Ok(pruned)
}

fn prefer_paragraph_metadata_key(boundary: usize, existing: &str, candidate: &str) -> bool {
  boundary == 0 && candidate == ROOT_FIRST_PARAGRAPH_ID && existing != ROOT_FIRST_PARAGRAPH_ID
}

fn prefer_paragraph_block_key(boundary: usize, existing: &str, candidate: &str) -> bool {
  boundary == 0 && candidate == MAIN_BODY_BLOCK_ID && existing != MAIN_BODY_BLOCK_ID
}

fn live_cursor_pos(doc: &LoroDoc, body_snapshot: &str, map: &LoroMap, cursor_key: &str) -> Option<usize> {
  let cursor = Cursor::decode(&map_binary_opt(map, cursor_key)?).ok()?;
  let pos = doc.get_cursor_pos(&cursor).ok()?.current.pos;
  boundary_is_live(body_snapshot, pos).then_some(pos)
}

fn live_object_cursor_pos(doc: &LoroDoc, body_snapshot: &str, map: &LoroMap, cursor_key: &str) -> Option<usize> {
  let cursor = Cursor::decode(&map_binary_opt(map, cursor_key)?).ok()?;
  let pos = doc.get_cursor_pos(&cursor).ok()?.current.pos;
  (body_snapshot.chars().nth(pos) == Some(OBJECT_REPLACEMENT)).then_some(pos)
}

fn boundary_is_live(body_snapshot: &str, boundary: usize) -> bool {
  body_snapshot.chars().nth(boundary) == Some('\n')
}

fn new_paragraph_metadata_id(boundary: usize) -> String {
  if boundary == 0 {
    ROOT_FIRST_PARAGRAPH_ID.to_string()
  } else {
    format!("paragraph.{}", Uuid::new_v4().as_u128())
  }
}

fn new_paragraph_block_id(boundary: usize) -> String {
  if boundary == 0 {
    MAIN_BODY_BLOCK_ID.to_string()
  } else {
    format!("paragraph_block.{}", Uuid::new_v4().as_u128())
  }
}

fn map_keys(map: &LoroMap) -> Vec<String> {
  let mut keys = map.keys().map(|key| key.to_string()).collect::<Vec<_>>();
  keys.sort();
  keys
}

fn child_map(parent: &LoroMap, key: &str) -> Option<LoroMap> {
  parent.get(key).and_then(|value| match value {
    ValueOrContainer::Container(container) => container.into_map().ok(),
    ValueOrContainer::Value(_) => None,
  })
}

fn child_movable_list(parent: &LoroMap, key: &str) -> Option<LoroMovableList> {
  parent.get(key).and_then(|value| match value {
    ValueOrContainer::Container(Container::MovableList(list)) => Some(list),
    _ => None,
  })
}

fn movable_list_strings(list: &LoroMovableList) -> Vec<String> {
  (0..list.len())
    .filter_map(|ix| match list.get(ix) {
      Some(ValueOrContainer::Value(LoroValue::String(value))) => Some(value.to_string()),
      _ => None,
    })
    .collect()
}

fn map_string_opt(map: &LoroMap, key: &str) -> Option<String> {
  map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::String(value)) => Some(value.to_string()),
    _ => None,
  })
}

fn map_binary_opt(map: &LoroMap, key: &str) -> Option<Vec<u8>> {
  map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::Binary(value)) => Some(value.to_vec()),
    _ => None,
  })
}

fn attach_package_assets(document: &mut DocumentProjection, package: &DocumentPackage) {
  for asset in &package.assets {
    let bytes = asset.bytes.clone();
    document.assets.assets.insert(
      AssetId(asset.asset_id),
      AssetRecord {
        id: AssetId(asset.asset_id),
        mime_type: asset.mime_type.clone().into(),
        original_name: None,
        content_hash: AssetRecord::stable_content_hash(&bytes),
        bytes: Arc::new(bytes),
      },
    );
  }
}

fn install_undo_selection_callbacks(undo: &mut UndoManager, state: &Arc<Mutex<UndoSelectionState>>) {
  let push_state = Arc::clone(state);
  undo.set_on_push(Some(Box::new(move |_, _, _| {
    let mut meta = UndoItemMeta::new();
    if let Ok(state) = push_state.lock()
      && let Some(selection) = &state.pending_selection
    {
      meta.set_value(LoroValue::Binary(selection.clone().into()));
    }
    meta
  })));

  let pop_state = Arc::clone(state);
  undo.set_on_pop(Some(Box::new(move |_, _, meta| {
    let LoroValue::Binary(bytes) = meta.value else {
      return;
    };
    match postcard::from_bytes::<UndoSelectionSnapshot>(bytes.as_ref()) {
      Ok(selection) => {
        if let Ok(mut state) = pop_state.lock() {
          state.restored_selection = Some(selection);
        }
      },
      Err(error) => {
        tracing::warn!(error = %error, "decoding Loro undo selection metadata failed");
      },
    }
  })));
}

fn map_i64_opt(map: &LoroMap, key: &str) -> Option<i64> {
  map.get(key).and_then(|value| match value {
    ValueOrContainer::Value(LoroValue::I64(value)) => Some(value),
    _ => None,
  })
}

fn parse_blake3_hex(value: &str) -> Option<[u8; 32]> {
  if value.len() != 64 {
    return None;
  }
  let mut bytes = [0u8; 32];
  for (index, byte) in bytes.iter_mut().enumerate() {
    *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16).ok()?;
  }
  Some(bytes)
}

fn selection_direction(anchor: DocumentOffset, head: DocumentOffset) -> SelectionDirection {
  match anchor.cmp(&head) {
    std::cmp::Ordering::Less => SelectionDirection::Forward,
    std::cmp::Ordering::Greater => SelectionDirection::Backward,
    std::cmp::Ordering::Equal => SelectionDirection::None,
  }
}

fn endpoint_intent(
  direction: SelectionDirection,
) -> (SelectionAffinity, SelectionAffinity, VisualGravity, VisualGravity) {
  match direction {
    SelectionDirection::Forward => (
      SelectionAffinity::Before,
      SelectionAffinity::After,
      VisualGravity::Upstream,
      VisualGravity::Downstream,
    ),
    SelectionDirection::Backward => (
      SelectionAffinity::After,
      SelectionAffinity::Before,
      VisualGravity::Downstream,
      VisualGravity::Upstream,
    ),
    SelectionDirection::None => (
      SelectionAffinity::After,
      SelectionAffinity::After,
      VisualGravity::Downstream,
      VisualGravity::Downstream,
    ),
  }
}

fn side_for_affinity(affinity: SelectionAffinity) -> Side {
  match affinity {
    SelectionAffinity::Before => Side::Left,
    SelectionAffinity::After => Side::Right,
    SelectionAffinity::Neutral => Side::Middle,
  }
}

fn undo_affinity(affinity: SelectionAffinity) -> UndoSelectionAffinity {
  match affinity {
    SelectionAffinity::Before => UndoSelectionAffinity::Before,
    SelectionAffinity::After => UndoSelectionAffinity::After,
    SelectionAffinity::Neutral => UndoSelectionAffinity::Neutral,
  }
}

pub(super) fn paragraph_style_from_attrs(attrs: Option<&FxHashMap<String, LoroValue>>) -> Option<ParagraphStyle> {
  let value = attrs?.get(MARK_PARAGRAPH_STYLE)?;
  match value {
    LoroValue::I64(0) => Some(ParagraphStyle::Normal),
    LoroValue::I64(slot) if *slot > 0 => u8::try_from(*slot - 1).ok().map(ParagraphStyle::Custom),
    _ => None,
  }
}

fn paragraph_style_value(style: ParagraphStyle) -> i64 {
  match style {
    ParagraphStyle::Normal => 0,
    ParagraphStyle::Custom(slot) => i64::from(slot) + 1,
  }
}

fn mark_run_styles(text: &loro::LoroText, range: std::ops::Range<usize>, styles: RunStyles) -> loro::LoroResult<()> {
  for key in [
    MARK_RUN_SEMANTIC_STYLE,
    MARK_HIGHLIGHT_STYLE,
    MARK_DIRECT_UNDERLINE,
    MARK_STRIKETHROUGH,
  ] {
    text.unmark(range.clone(), key)?;
  }
  if let RunSemanticStyle::Custom(slot) = styles.semantic {
    text.mark(range.clone(), MARK_RUN_SEMANTIC_STYLE, i64::from(slot))?;
  }
  if let Some(flowstate_document::HighlightStyle::Custom(slot)) = styles.highlight {
    text.mark(range.clone(), MARK_HIGHLIGHT_STYLE, i64::from(slot))?;
  }
  if styles.direct_underline {
    text.mark(range.clone(), MARK_DIRECT_UNDERLINE, true)?;
  }
  if styles.strikethrough {
    text.mark(range, MARK_STRIKETHROUGH, true)?;
  }
  Ok(())
}

fn insert_image_block(
  doc: &LoroDoc,
  unicode_index: usize,
  asset_id: u128,
  alt_text: &str,
  caption: Option<&str>,
  sizing: InputImageSizing,
  alignment: InputBlockAlignment,
) -> Result<()> {
  let body = body_text(doc);
  body.insert(unicode_index, &OBJECT_REPLACEMENT.to_string())?;
  let block = ensure_block(doc, "image", ROOT_BODY_FLOW_ID, &body, unicode_index)?;
  block.insert("asset_id", asset_id.to_string())?;
  copy_asset_metadata_to_image_block(doc, &block, asset_id)?;

  let alt_flow_id = nested_flow_id("image_alt");
  block.insert("alt_text_flow_id", alt_flow_id.as_str())?;
  let alt_flow = ensure_flow(doc, &alt_flow_id, "alt_text")?;
  replace_text_incrementally(&alt_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, alt_text)?;

  if let Some(caption) = caption {
    let caption_flow_id = nested_flow_id("image_caption");
    block.insert("caption_flow_id", caption_flow_id.as_str())?;
    let caption_flow = ensure_flow(doc, &caption_flow_id, "caption")?;
    let caption_text = caption_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
    replace_text(&caption_text, SENTINEL_NEWLINE)?;
    caption_text.mark(0..1, MARK_PARAGRAPH_STYLE, 0_i64)?;
    if !caption.is_empty() {
      caption_text.insert(1, caption)?;
    }
  }

  let attrs = block.ensure_mergeable_map("attrs")?;
  attrs.insert("alignment", alignment_name(alignment))?;
  match sizing {
    InputImageSizing::Intrinsic => attrs.insert("sizing", "intrinsic")?,
    InputImageSizing::FitWidth => attrs.insert("sizing", "fit_width")?,
    InputImageSizing::Fixed { width_px, height_px } => {
      attrs.insert("sizing", "fixed")?;
      attrs.insert("width_px", i64::from(width_px))?;
      if let Some(height_px) = height_px {
        attrs.insert("height_px", i64::from(height_px))?;
      }
    }
  };
  Ok(())
}

fn insert_image_block_with_id(
  doc: &LoroDoc,
  unicode_index: usize,
  block_id: flowstate_document::BlockId,
  image: &flowstate_document::InputImageBlock,
) -> Result<()> {
  let body = body_text(doc);
  body.insert(unicode_index, &OBJECT_REPLACEMENT.to_string())?;
  let block_key = object_block_key("image", block_id);
  let block = ensure_block_with_id(doc, &block_key, "image", ROOT_BODY_FLOW_ID, &body, unicode_index)?;
  replace_image_block_from_input(doc, &block, image)
}

fn insert_equation_block(doc: &LoroDoc, unicode_index: usize, source: &str, display: InputEquationDisplay) -> Result<()> {
  let body = body_text(doc);
  body.insert(unicode_index, &OBJECT_REPLACEMENT.to_string())?;
  let block = ensure_block(doc, "equation", ROOT_BODY_FLOW_ID, &body, unicode_index)?;
  let source_flow_id = nested_flow_id("equation_source");
  block.insert("source_flow_id", source_flow_id.as_str())?;
  let source_flow = ensure_flow(doc, &source_flow_id, "equation_source")?;
  replace_text(&source_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, source)?;
  let attrs = block.ensure_mergeable_map("attrs")?;
  attrs.insert("syntax", "latex")?;
  attrs.insert("display", equation_display_name(display))?;
  Ok(())
}

fn insert_equation_block_with_id(
  doc: &LoroDoc,
  unicode_index: usize,
  block_id: flowstate_document::BlockId,
  equation: &flowstate_document::InputEquationBlock,
) -> Result<()> {
  let body = body_text(doc);
  body.insert(unicode_index, &OBJECT_REPLACEMENT.to_string())?;
  let block_key = object_block_key("equation", block_id);
  let block = ensure_block_with_id(doc, &block_key, "equation", ROOT_BODY_FLOW_ID, &body, unicode_index)?;
  replace_equation_block_from_input(doc, &block, equation)
}

fn insert_table_block(
  doc: &LoroDoc,
  unicode_index: usize,
  rows: usize,
  columns: usize,
  column_widths: &[InputTableColumnWidth],
  header_row: bool,
) -> Result<()> {
  let body = body_text(doc);
  body.insert(unicode_index, &OBJECT_REPLACEMENT.to_string())?;
  let block = ensure_block(doc, "table", ROOT_BODY_FLOW_ID, &body, unicode_index)?;
  let table = block.ensure_mergeable_map("table")?;
  table.insert("header_row", header_row)?;
  let row_order = table.ensure_mergeable_movable_list("row_order")?;
  let column_order = table.ensure_mergeable_movable_list("column_order")?;
  let rows_by_id = table.ensure_mergeable_map("rows_by_id")?;
  let columns_by_id = table.ensure_mergeable_map("columns_by_id")?;
  let cells_by_id = table.ensure_mergeable_map("cells_by_id")?;
  let table_id = table_id();
  let mut column_ids = Vec::with_capacity(columns);

  for column_ix in 0..columns {
    let column_id = format!("{table_id}.column.{column_ix}");
    column_order.push(column_id.as_str())?;
    column_ids.push(column_id.clone());
    let column = columns_by_id.ensure_mergeable_map(&column_id)?;
    column.insert("id", column_id.as_str())?;
    column.ensure_mergeable_map("attrs")?;
    let width = column_widths.get(column_ix).unwrap_or(&InputTableColumnWidth::Auto);
    match *width {
      InputTableColumnWidth::Auto => column.insert("width_kind", "auto")?,
      InputTableColumnWidth::FixedPx(px) => {
        column.insert("width_kind", "fixed_px")?;
        column.insert("width_px", i64::from(px))?;
      }
      InputTableColumnWidth::Fraction(fraction) => {
        column.insert("width_kind", "fraction")?;
        column.insert("fraction", i64::from(fraction))?;
      }
    };
  }

  for row_ix in 0..rows {
    let row_id = format!("{table_id}.row.{row_ix}");
    row_order.push(row_id.as_str())?;
    let row = rows_by_id.ensure_mergeable_map(&row_id)?;
    row.insert("id", row_id.as_str())?;
    row.insert("container_id", row.id().to_string())?;
    row.ensure_mergeable_map("attrs")?;
    for (column_ix, column_id) in column_ids.iter().enumerate() {
      let cell_id = format!("{row_id}.cell.{column_ix}");
      let cell = cells_by_id.ensure_mergeable_map(&cell_id)?;
      cell.insert("id", cell_id.as_str())?;
      cell.insert("container_id", cell.id().to_string())?;
      cell.insert("row_id", row_id.as_str())?;
      cell.insert("column_id", column_id.as_str())?;
      cell.insert("row_span", 1_i64)?;
      cell.insert("column_span", 1_i64)?;
      cell.ensure_mergeable_map("attrs")?;
      let nested_table_ids = cell.ensure_mergeable_movable_list("nested_table_ids")?;
      let nested_tables_by_id = cell.ensure_mergeable_map("nested_tables_by_id")?;
      cell.insert("nested_table_order_container_id", nested_table_ids.id().to_string())?;
      cell.insert("nested_tables_container_id", nested_tables_by_id.id().to_string())?;
      let flow_id = format!("{cell_id}.flow");
      cell.insert("flow_id", flow_id.as_str())?;
      let flow = ensure_flow(doc, &flow_id, "table_cell")?;
      let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
      cell.insert("flow_container_id", flow.id().to_string())?;
      cell.insert("text_container_id", text.id().to_string())?;
      replace_text(&text, SENTINEL_NEWLINE)?;
      text.mark(0..1, MARK_PARAGRAPH_STYLE, 0_i64)?;
    }
  }
  Ok(())
}

fn insert_table_block_with_id(
  doc: &LoroDoc,
  unicode_index: usize,
  block_id: flowstate_document::BlockId,
  table: &InputTableBlock,
) -> Result<()> {
  let body = body_text(doc);
  body.insert(unicode_index, &OBJECT_REPLACEMENT.to_string())?;
  let block_key = object_block_key("table", block_id);
  let block = ensure_block_with_id(doc, &block_key, "table", ROOT_BODY_FLOW_ID, &body, unicode_index)?;
  replace_table_block_from_input(doc, &block, table)
}

fn ensure_flow(doc: &LoroDoc, flow_id: &str, kind: &str) -> loro::LoroResult<LoroMap> {
  let root = doc.get_map(ROOT);
  let flows = root.ensure_mergeable_map(FLOWS_BY_ID)?;
  let flow = flows.ensure_mergeable_map(flow_id)?;
  flow.insert(FLOW_ID_KEY, flow_id)?;
  flow.insert(FLOW_KIND_KEY, kind)?;
  let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  let attrs = flow.ensure_mergeable_map(FLOW_ATTRS_KEY)?;
  flow.insert("container_id", flow.id().to_string())?;
  flow.insert("text_container_id", text.id().to_string())?;
  flow.insert("attrs_container_id", attrs.id().to_string())?;
  Ok(flow)
}

fn ensure_block(doc: &LoroDoc, kind: &str, flow_id: &str, text: &loro::LoroText, pos: usize) -> loro::LoroResult<LoroMap> {
  let id = format!("{kind}.{}", Uuid::new_v4().as_u128());
  ensure_block_with_id(doc, &id, kind, flow_id, text, pos)
}

fn ensure_block_with_id(
  doc: &LoroDoc,
  id: &str,
  kind: &str,
  flow_id: &str,
  text: &loro::LoroText,
  pos: usize,
) -> loro::LoroResult<LoroMap> {
  let root = doc.get_map(ROOT);
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  let block = blocks.ensure_mergeable_map(id)?;
  block.insert("id", id)?;
  block.insert("container_id", block.id().to_string())?;
  block.insert("kind", kind)?;
  block.insert("flow_id", flow_id)?;
  if let Some(cursor) = text.get_cursor(pos, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  let attrs = block.ensure_mergeable_map("attrs")?;
  let nested_refs = block.ensure_mergeable_map("nested_refs")?;
  block.insert("attrs_container_id", attrs.id().to_string())?;
  block.insert("nested_refs_container_id", nested_refs.id().to_string())?;
  Ok(block)
}

fn object_block_key(kind: &str, block_id: flowstate_document::BlockId) -> String {
  format!("{kind}.{}", block_id.0)
}

fn replace_text(text: &loro::LoroText, value: &str) -> loro::LoroResult<()> {
  let len = text.len_unicode();
  if len > 0 {
    text.delete(0, len)?;
  }
  if !value.is_empty() {
    text.insert(0, value)?;
  }
  Ok(())
}

fn nested_flow_id(kind: &str) -> String {
  format!("{kind}.{}", Uuid::new_v4().as_u128())
}

fn table_id() -> String {
  format!("table.{}", Uuid::new_v4().as_u128())
}

fn alignment_name(alignment: InputBlockAlignment) -> &'static str {
  match alignment {
    InputBlockAlignment::Left => "left",
    InputBlockAlignment::Center => "center",
    InputBlockAlignment::Right => "right",
  }
}

fn equation_display_name(display: InputEquationDisplay) -> &'static str {
  match display {
    InputEquationDisplay::Display => "display",
    InputEquationDisplay::InlineLikeParagraph => "inline_like_paragraph",
  }
}

#[cfg(test)]
mod tests {
  use super::*;
  use flowstate_document::{CollabPatch, CollabTextDelta, DocumentPackage, InputRun, loro_schema::body_text};

  fn live_paragraph_metadata_boundaries(doc: &LoroDoc) -> Vec<usize> {
    let body = body_text(doc);
    let snapshot = body.to_string();
    let root = doc.get_map(ROOT);
    let paragraphs = root.ensure_mergeable_map(PARAGRAPHS_BY_ID).expect("paragraph registry");
    let mut boundaries = map_keys(&paragraphs)
      .into_iter()
      .filter_map(|key| child_map(&paragraphs, &key))
      .filter_map(|paragraph| live_cursor_pos(doc, &snapshot, &paragraph, "boundary_cursor"))
      .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries
  }

  fn live_paragraph_block_boundaries(doc: &LoroDoc) -> Vec<usize> {
    let body = body_text(doc);
    let snapshot = body.to_string();
    let root = doc.get_map(ROOT);
    let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID).expect("block registry");
    let mut boundaries = map_keys(&blocks)
      .into_iter()
      .filter_map(|key| child_map(&blocks, &key))
      .filter(|block| map_string_opt(block, "kind").as_deref() == Some("paragraph"))
      .filter_map(|block| live_cursor_pos(doc, &snapshot, &block, "anchor_cursor"))
      .collect::<Vec<_>>();
    boundaries.sort_unstable();
    boundaries
  }

  fn input_paragraph(text: &str) -> flowstate_document::InputParagraph {
    flowstate_document::InputParagraph {
      style: flowstate_document::ParagraphStyle::Normal,
      runs: vec![flowstate_document::InputRun {
        text: text.to_string(),
        styles: flowstate_document::RunStyles::default(),
      }],
    }
  }

  fn input_table(rows: Vec<Vec<&str>>, column_widths: Vec<flowstate_document::InputTableColumnWidth>, header_row: bool) -> InputTableBlock {
    InputTableBlock {
      rows: rows
        .into_iter()
        .map(|row| flowstate_document::InputTableRow {
          cells: row.into_iter().map(input_table_cell).collect(),
        })
        .collect(),
      column_widths,
      style: flowstate_document::InputTableStyle { header_row },
    }
  }

  fn input_table_cell(text: &str) -> flowstate_document::InputTableCell {
    flowstate_document::InputTableCell {
      blocks: vec![InputTableCellBlock::Paragraph(input_paragraph(text))],
      row_span: 1,
      col_span: 1,
    }
  }

  fn projected_table_cell_text(table: &flowstate_document::TableBlock, row_ix: usize, cell_ix: usize) -> &str {
    let flowstate_document::TableCellBlock::Paragraph(paragraph) = &table.rows[row_ix].cells[cell_ix].blocks[0] else {
      panic!("expected paragraph table cell");
    };
    &paragraph.text
  }

  fn local_update_bytes(events: &[RuntimeEvent]) -> Vec<u8> {
    events
      .iter()
      .find_map(|event| match event {
        RuntimeEvent::LocalUpdate { bytes, .. } => Some(bytes.clone()),
        RuntimeEvent::RemoteUpdateApplied { .. }
        | RuntimeEvent::RevisionOpened { .. }
        | RuntimeEvent::RevisionForked { .. }
        | RuntimeEvent::ProjectionUpdated { .. }
        | RuntimeEvent::ProjectionPatched { .. }
        | RuntimeEvent::SelectionRestored { .. } => None,
      })
      .expect("local update bytes")
  }

  #[test]
  fn local_insert_exports_update_and_invalidates_projection() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    let events = runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello".to_string(),
      styles: RunStyles::default(),
    })?;
    assert!(matches!(events.first(), Some(RuntimeEvent::LocalUpdate { bytes, .. }) if !bytes.is_empty()));
    assert!(events.iter().any(|event| matches!(event, RuntimeEvent::ProjectionPatched { .. })));
    assert_eq!(flowstate_document::paragraph_text(&runtime.projection_snapshot()?, 0), "hello");
    assert_eq!(body_text(runtime.doc()).to_string(), "\nhello");
    Ok(())
  }

  #[test]
  fn semantic_insert_text_projects_inserted_run_styles() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    let styles = RunStyles {
      semantic: RunSemanticStyle::Custom(2),
      ..RunStyles::default()
    };
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "styled".to_string(),
      styles,
    })?;

    let projection = runtime.projection_snapshot()?;
    assert_eq!(flowstate_document::paragraph_text(&projection, 0), "styled");
    assert_eq!(projection.paragraphs[0].runs.len(), 1);
    assert_eq!(projection.paragraphs[0].runs[0].styles, styles);
    Ok(())
  }

  #[test]
  fn editor_insert_text_preserves_paragraph_style_mark() -> Result<()> {
    let source = flowstate_document::document_from_input(
      flowstate_document::flowstate_document_theme(),
      vec![InputParagraph {
        style: ParagraphStyle::Custom(0),
        runs: vec![InputRun {
          text: "pocket".to_string(),
          styles: RunStyles::default(),
        }],
      }],
    );
    let doc = flowstate_document::document_to_loro(&source, "Styled")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;

    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::InsertText {
        at: flowstate_document::DocumentOffset { paragraph: 0, byte: 3 },
        text: "x".to_string(),
        styles: RunStyles::default(),
      },
    )?;

    let updated = runtime.projection_snapshot()?;
    assert_eq!(flowstate_document::paragraph_text(&updated, 0), "pocxket");
    assert_eq!(updated.paragraphs[0].style, ParagraphStyle::Custom(0));
    Ok(())
  }

  #[test]
  fn split_paragraph_creates_live_paragraph_metadata_and_block_anchor() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello".to_string(),
      styles: RunStyles::default(),
    })?;
    runtime.command(SemanticCommand::SplitParagraph {
      unicode_index: 3,
      inherited_style: flowstate_document::ParagraphStyle::Normal,
    })?;

    assert_eq!(body_text(runtime.doc()).to_string(), "\nhe\nllo");
    assert_eq!(live_paragraph_metadata_boundaries(runtime.doc()), vec![0, 3]);
    assert_eq!(live_paragraph_block_boundaries(runtime.doc()), vec![0, 3]);
    let projection = runtime.projection_snapshot()?;
    assert_eq!(projection.paragraphs.len(), 2);
    assert_eq!(flowstate_document::paragraph_text(&projection, 0), "he");
    assert_eq!(flowstate_document::paragraph_text(&projection, 1), "llo");
    Ok(())
  }

  #[test]
  fn runtime_repairs_missing_paragraph_style_marks_on_takeover() -> Result<()> {
    let doc = new_loro_document("Malformed")?;
    let body = body_text(&doc);
    body.insert(1, "bad\nnext")?;
    doc.commit();
    assert_eq!(body_paragraph_boundaries_missing_style_mark(&body), vec![4]);

    let runtime = CrdtRuntime::from_doc(doc, None, None)?;

    assert!(body_paragraph_boundaries_missing_style_mark(&body_text(runtime.doc())).is_empty());
    let projection = runtime.projection_snapshot()?;
    assert_eq!(projection.paragraphs.len(), 2);
    assert_eq!(flowstate_document::paragraph_text(&projection, 0), "bad");
    assert_eq!(flowstate_document::paragraph_text(&projection, 1), "next");
    assert_eq!(projection.paragraphs[1].style, ParagraphStyle::Normal);
    Ok(())
  }

  #[test]
  fn package_open_persists_missing_paragraph_style_mark_repair() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("malformed.db8");
    let doc = new_loro_document("Malformed")?;
    let body = body_text(&doc);
    body.insert(1, "bad\nnext")?;
    doc.commit();
    assert_eq!(body_paragraph_boundaries_missing_style_mark(&body), vec![4]);
    DocumentPackage::from_loro_snapshot(&doc, "Malformed")?.write(&path)?;

    let _runtime = CrdtRuntime::open_package(&path)?;
    let package = DocumentPackage::read(&path)?;
    let loaded = package.load_loro_doc()?;

    assert_eq!(body_text(&loaded).to_string(), "\nbad\nnext");
    assert!(body_paragraph_boundaries_missing_style_mark(&body_text(&loaded)).is_empty());
    assert_eq!(package.loro_update_segments.len(), 1);
    Ok(())
  }

  #[test]
  fn remote_import_repairs_and_publishes_missing_paragraph_style_marks() -> Result<()> {
    let base = new_loro_document("Malformed")?;
    let source = base.fork();
    let from_vv = base.state_vv();
    body_text(&source).insert(1, "bad\nnext")?;
    source.commit();
    let update = source.export(ExportMode::updates(&from_vv))?;

    let mut target = CrdtRuntime::from_doc(base, None, None)?;
    let events = target.import_remote_update(&update)?;

    assert!(body_paragraph_boundaries_missing_style_mark(&body_text(target.doc())).is_empty());
    assert!(events.iter().any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty())));
    Ok(())
  }

  #[test]
  fn join_paragraphs_deletes_boundary_and_prunes_stale_metadata() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello".to_string(),
      styles: RunStyles::default(),
    })?;
    runtime.command(SemanticCommand::SplitParagraph {
      unicode_index: 6,
      inherited_style: flowstate_document::ParagraphStyle::Normal,
    })?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 7,
      text: "world".to_string(),
      styles: RunStyles::default(),
    })?;
    assert_eq!(live_paragraph_metadata_boundaries(runtime.doc()), vec![0, 6]);

    let before_join = runtime.projection_snapshot()?;
    let first = before_join.ids.paragraph_ids[0];
    let second = before_join.ids.paragraph_ids[1];
    let events = runtime.apply_editor_semantic_command(&before_join, &EditorSemanticCommand::JoinParagraphs { first, second })?;

    assert!(events.iter().any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty())));
    assert_eq!(body_text(runtime.doc()).to_string(), "\nhelloworld");
    assert_eq!(live_paragraph_metadata_boundaries(runtime.doc()), vec![0]);
    assert_eq!(live_paragraph_block_boundaries(runtime.doc()), vec![0]);
    Ok(())
  }

  #[test]
  fn runtime_persists_local_update_segments() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("runtime.db8");
    let doc = flowstate_document::new_loro_document("Runtime")?;
    DocumentPackage::from_loro_snapshot(&doc, "Runtime")?.write(&path)?;
    let mut runtime = CrdtRuntime::open_package(&path)?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "persisted".to_string(),
      styles: RunStyles::default(),
    })?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 10,
      text: " twice".to_string(),
      styles: RunStyles::default(),
    })?;
    let package = DocumentPackage::read(&path)?;
    assert!(package.loro_update_segments.len() >= 3);
    assert!(package.current_search_units().is_empty());
    let loaded = package.load_loro_doc()?;
    assert_eq!(body_text(&loaded).to_string(), "\npersisted twice");
    Ok(())
  }

  #[test]
  fn semantic_text_commands_mutate_loro_body_flow() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello world".to_string(),
      styles: RunStyles::default(),
    })?;
    runtime.command(SemanticCommand::DeleteRange {
      unicode_index: 6,
      unicode_len: 1,
    })?;
    runtime.command(SemanticCommand::SplitParagraph {
      unicode_index: 6,
      inherited_style: flowstate_document::ParagraphStyle::Custom(2),
    })?;
    runtime.command(SemanticCommand::SetRunStyles {
      unicode_range: 1..6,
      styles: flowstate_document::RunStyles {
        semantic: flowstate_document::RunSemanticStyle::Custom(3),
        direct_underline: true,
        strikethrough: false,
        highlight: Some(flowstate_document::HighlightStyle::Custom(4)),
      },
    })?;

    assert_eq!(body_text(runtime.doc()).to_string(), "\nhello\nworld");
    let delta = body_text(runtime.doc()).to_delta();
    assert!(delta.iter().any(|item| matches!(
      item,
      loro::TextDelta::Insert {
        attributes: Some(attributes),
        ..
      } if attributes.get(flowstate_document::MARK_RUN_SEMANTIC_STYLE).is_some()
    )));
    assert!(delta.iter().any(|item| matches!(
      item,
      loro::TextDelta::Insert {
        insert,
        attributes: Some(attributes),
      } if insert == "\n" && attributes.get(flowstate_document::MARK_PARAGRAPH_STYLE).is_some()
    )));
    Ok(())
  }

  #[test]
  fn editor_replace_paragraph_span_preserves_boundaries_and_marks() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![flowstate_document::InputBlock::Paragraph(flowstate_document::InputParagraph {
        style: flowstate_document::ParagraphStyle::Normal,
        runs: vec![flowstate_document::InputRun {
          text: "old".to_string(),
          styles: flowstate_document::RunStyles::default(),
        }],
      })],
    );
    let replacement_styles = flowstate_document::RunStyles {
      semantic: flowstate_document::RunSemanticStyle::Custom(3),
      direct_underline: true,
      strikethrough: false,
      highlight: Some(flowstate_document::HighlightStyle::Custom(4)),
    };
    let replacement = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(flowstate_document::InputParagraph {
          style: flowstate_document::ParagraphStyle::Custom(2),
          runs: vec![flowstate_document::InputRun {
            text: "Hello".to_string(),
            styles: replacement_styles,
          }],
        }),
        flowstate_document::InputBlock::Paragraph(flowstate_document::InputParagraph {
          style: flowstate_document::ParagraphStyle::Normal,
          runs: vec![flowstate_document::InputRun {
            text: "World".to_string(),
            styles: flowstate_document::RunStyles::default(),
          }],
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Span")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;

    let events = runtime.apply_editor_semantic_command(
      &source,
      &EditorSemanticCommand::ReplaceParagraphSpan {
        start: None,
        before: flowstate_document::capture_document_span(&source, 0..1),
        after: flowstate_document::capture_document_span(&replacement, 0..2),
      },
    )?;

    assert!(events.iter().any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty())));
    assert_eq!(body_text(runtime.doc()).to_string(), "\nHello\nWorld");
    let projection = runtime.projection_snapshot()?;
    assert_eq!(flowstate_document::paragraph_text(&projection, 0), "Hello");
    assert_eq!(flowstate_document::paragraph_text(&projection, 1), "World");
    assert_eq!(projection.paragraphs[0].style, flowstate_document::ParagraphStyle::Custom(2));
    assert_eq!(projection.paragraphs[0].runs[0].styles, replacement_styles);
    assert_eq!(live_paragraph_metadata_boundaries(runtime.doc()), vec![0, 6]);
    assert_eq!(live_paragraph_block_boundaries(runtime.doc()), vec![0, 6]);
    Ok(())
  }

  #[test]
  fn editor_replace_block_updates_image_metadata() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "old".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Replace Image")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;

    let events = runtime.apply_editor_semantic_command(
      &source,
      &EditorSemanticCommand::ReplaceBlock {
        block: Some(source.ids.block_ids[1]),
        block_ix: 1,
        after: flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(9),
          alt_text: "new alt".to_string(),
          caption: Some(input_paragraph("caption")),
          sizing: flowstate_document::InputImageSizing::Fixed {
            width_px: 640,
            height_px: Some(480),
          },
          alignment: flowstate_document::InputBlockAlignment::Right,
        }),
      },
    )?;

    assert!(events.iter().any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty())));
    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Image(image) = &projection.blocks[1] else {
      panic!("expected image block after ReplaceBlock");
    };
    assert_eq!(image.asset_id, flowstate_document::AssetId(9));
    assert_eq!(image.alt_text.as_ref(), "new alt");
    assert!(image.caption.is_some());
    assert_eq!(
      image.sizing,
      flowstate_document::ImageSizing::Fixed {
        width_px: 640,
        height_px: Some(480),
      }
    );
    assert_eq!(image.alignment, flowstate_document::BlockAlignment::Right);
    Ok(())
  }

  #[test]
  fn editor_replace_block_prefers_projected_loro_id_over_stale_index() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "old".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Replace Image")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let image_block = projection.ids.block_ids[1];

    let events = runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::ReplaceBlock {
        block: Some(image_block),
        block_ix: 99,
        after: flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(9),
          alt_text: "new alt".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::FitWidth,
          alignment: flowstate_document::InputBlockAlignment::Right,
        }),
      },
    )?;

    assert!(events.iter().any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty())));
    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Image(image) = &projection.blocks[1] else {
      panic!("expected image block after ReplaceBlock");
    };
    assert_eq!(image.asset_id, flowstate_document::AssetId(9));
    assert_eq!(image.alt_text.as_ref(), "new alt");
    assert_eq!(image.alignment, flowstate_document::BlockAlignment::Right);
    Ok(())
  }

  #[test]
  fn editor_replace_image_alt_text_updates_alt_flow() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "old".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Image Alt")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let image_block_id = projection.ids.block_ids[1];
    let body = body_text(runtime.doc());
    let (_, block, _) = object_loro_block_by_projected_id(runtime.doc(), &body, image_block_id).expect("image block");
    let before_flow = map_string_opt(&block, "alt_text_flow_id").expect("alt flow id");

    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::ReplaceImageAltText {
        image: image_block_id,
        text: "new alt".to_string(),
      },
    )?;

    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Image(image) = &projection.blocks[1] else {
      panic!("expected image block after alt text edit");
    };
    assert_eq!(image.alt_text.as_ref(), "new alt");
    let body = body_text(runtime.doc());
    let (_, block, _) = object_loro_block_by_projected_id(runtime.doc(), &body, image_block_id).expect("image block");
    assert_eq!(map_string_opt(&block, "alt_text_flow_id").as_deref(), Some(before_flow.as_str()));
    Ok(())
  }

  #[test]
  fn editor_set_image_layout_updates_image_attrs() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "alt".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Image Layout")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let image_block_id = projection.ids.block_ids[1];

    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::SetImageLayout {
        image: image_block_id,
        sizing: flowstate_document::InputImageSizing::Fixed {
          width_px: 444,
          height_px: None,
        },
        alignment: flowstate_document::InputBlockAlignment::Center,
      },
    )?;

    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Image(image) = &projection.blocks[1] else {
      panic!("expected image block after layout edit");
    };
    assert_eq!(image.asset_id, flowstate_document::AssetId(1));
    assert_eq!(image.alt_text.as_ref(), "alt");
    assert_eq!(image.alignment, flowstate_document::BlockAlignment::Center);
    assert_eq!(
      image.sizing,
      flowstate_document::ImageSizing::Fixed {
        width_px: 444,
        height_px: None,
      }
    );
    Ok(())
  }

  #[test]
  fn editor_insert_block_creates_loro_object_from_projection_payload() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![flowstate_document::InputBlock::Paragraph(input_paragraph("body"))],
    );
    let target = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(7),
          alt_text: "inserted".to_string(),
          caption: Some(input_paragraph("caption")),
          sizing: flowstate_document::InputImageSizing::FitWidth,
          alignment: flowstate_document::InputBlockAlignment::Center,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Insert Image")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let image_block = target.ids.block_ids[1];

    let events = runtime.apply_editor_semantic_command(
      &target,
      &EditorSemanticCommand::InsertBlock {
        block: image_block,
        block_ix: 1,
        after: flowstate_document::input_block_from_block(&target.blocks[1]),
      },
    )?;

    assert!(events.iter().any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty())));
    assert!(body_text(runtime.doc()).to_string().contains(OBJECT_REPLACEMENT));
    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Image(image) = &projection.blocks[1] else {
      panic!("expected inserted image block");
    };
    assert_eq!(image.asset_id, flowstate_document::AssetId(7));
    assert_eq!(image.alt_text.as_ref(), "inserted");
    assert!(image.caption.is_some());
    assert_eq!(image.alignment, flowstate_document::BlockAlignment::Center);
    assert_eq!(projection.ids.block_ids[1], image_block);
    Ok(())
  }

  #[test]
  fn editor_delete_block_removes_loro_object_by_projected_id() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "old".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Delete Image")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let image_block = projection.ids.block_ids[1];

    let events = runtime.apply_editor_semantic_command(&projection, &EditorSemanticCommand::DeleteBlock { block: image_block })?;

    assert!(events.iter().any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty())));
    assert!(!body_text(runtime.doc()).to_string().contains(OBJECT_REPLACEMENT));
    let projection = runtime.projection_snapshot()?;
    assert_eq!(projection.blocks.len(), 1);
    assert!(matches!(&projection.blocks[0], flowstate_document::Block::Paragraph(_)));
    Ok(())
  }

  #[test]
  fn editor_delete_object_and_replace_paragraph_span_apply_together() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("alpha")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "alt".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
        flowstate_document::InputBlock::Paragraph(input_paragraph("omega")),
      ],
    );
    let target = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![flowstate_document::InputBlock::Paragraph(input_paragraph("alega"))],
    );
    let doc = flowstate_document::document_to_loro(&source, "Mixed Delete")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let image_block_id = projection.ids.block_ids[1];
    let before = flowstate_document::capture_document_span(&source, 0..2);
    let after = flowstate_document::capture_document_span(&target, 0..1);

    runtime.apply_editor_semantic_command(&projection, &EditorSemanticCommand::DeleteBlock { block: image_block_id })?;
    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::ReplaceParagraphSpan {
        start: Some(flowstate_document::DocumentOffset { paragraph: 0, byte: 0 }),
        before,
        after,
      },
    )?;

    let projection = runtime.projection_snapshot()?;
    assert_eq!(projection.blocks.len(), 1);
    assert_eq!(flowstate_document::paragraph_text(&projection, 0), "alega");
    assert!(!body_text(runtime.doc()).to_string().contains(OBJECT_REPLACEMENT));
    Ok(())
  }

  #[test]
  fn editor_move_block_reorders_loro_object_placeholder_by_projected_id() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(1),
          alt_text: "first".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(2),
          alt_text: "second".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::Intrinsic,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Move Image")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let second_image = projection.ids.block_ids[2];

    let events = runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::MoveBlock {
        block: second_image,
        new_block_ix: 1,
      },
    )?;

    assert!(events.iter().any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty())));
    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Image(image) = &projection.blocks[1] else {
      panic!("expected moved image at block 1");
    };
    assert_eq!(image.alt_text.as_ref(), "second");
    assert_eq!(projection.ids.block_ids[1], second_image);
    let flowstate_document::Block::Image(image) = &projection.blocks[2] else {
      panic!("expected first image at block 2");
    };
    assert_eq!(image.alt_text.as_ref(), "first");
    Ok(())
  }

  #[test]
  fn editor_replace_block_updates_equation_source() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Equation(flowstate_document::InputEquationBlock {
          source: "x".to_string(),
          syntax: flowstate_document::InputEquationSyntax::Latex,
          display: flowstate_document::InputEquationDisplay::Display,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Replace Equation")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;

    runtime.apply_editor_semantic_command(
      &source,
      &EditorSemanticCommand::ReplaceBlock {
        block: Some(source.ids.block_ids[1]),
        block_ix: 1,
        after: flowstate_document::InputBlock::Equation(flowstate_document::InputEquationBlock {
          source: "x+1".to_string(),
          syntax: flowstate_document::InputEquationSyntax::Latex,
          display: flowstate_document::InputEquationDisplay::InlineLikeParagraph,
        }),
      },
    )?;

    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Equation(equation) = &projection.blocks[1] else {
      panic!("expected equation block after ReplaceBlock");
    };
    assert_eq!(equation.source.as_ref(), "x+1");
    assert_eq!(equation.display, flowstate_document::EquationDisplay::InlineLikeParagraph);
    Ok(())
  }

  #[test]
  fn editor_replace_equation_source_range_edits_source_flow() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Equation(flowstate_document::InputEquationBlock {
          source: "x+y".to_string(),
          syntax: flowstate_document::InputEquationSyntax::Latex,
          display: flowstate_document::InputEquationDisplay::Display,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Edit Equation Source")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let equation_block_id = projection.ids.block_ids[1];
    let body = body_text(runtime.doc());
    let (_, block, _) = object_loro_block_by_projected_id(runtime.doc(), &body, equation_block_id).expect("equation block");
    let before_flow = map_string_opt(&block, "source_flow_id").expect("source flow id");

    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::ReplaceEquationSourceRange {
        equation: equation_block_id,
        range: 1..2,
        text: "*".to_string(),
      },
    )?;

    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Equation(equation) = &projection.blocks[1] else {
      panic!("expected equation block after source range edit");
    };
    assert_eq!(equation.source.as_ref(), "x*y");
    let body = body_text(runtime.doc());
    let (_, block, _) = object_loro_block_by_projected_id(runtime.doc(), &body, equation_block_id).expect("equation block");
    assert_eq!(map_string_opt(&block, "source_flow_id").as_deref(), Some(before_flow.as_str()));
    Ok(())
  }

  #[test]
  fn editor_replace_block_rebuilds_table_structure() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Table(input_table(
          vec![vec!["old"]],
          vec![flowstate_document::InputTableColumnWidth::Auto],
          false,
        )),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Replace Table")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;

    runtime.apply_editor_semantic_command(
      &source,
      &EditorSemanticCommand::ReplaceBlock {
        block: Some(source.ids.block_ids[1]),
        block_ix: 1,
        after: flowstate_document::InputBlock::Table(input_table(
          vec![vec!["a", "b"], vec!["c", "d"]],
          vec![
            flowstate_document::InputTableColumnWidth::FixedPx(90),
            flowstate_document::InputTableColumnWidth::Fraction(1),
          ],
          true,
        )),
      },
    )?;

    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Table(table) = &projection.blocks[1] else {
      panic!("expected table block after ReplaceBlock");
    };
    assert_eq!(table.rows.len(), 2);
    assert_eq!(table.rows[0].cells.len(), 2);
    assert!(table.style.header_row);
    assert!(matches!(
      table.column_widths.as_slice(),
      [flowstate_document::TableColumnWidth::FixedPx(90), flowstate_document::TableColumnWidth::Fraction(1)]
    ));
    let flowstate_document::TableCellBlock::Paragraph(cell) = &table.rows[1].cells[0].blocks[0] else {
      panic!("expected paragraph cell after ReplaceBlock");
    };
    assert_eq!(cell.text, "c");
    Ok(())
  }

  #[test]
  fn editor_set_table_column_width_preserves_table_identity() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Table(input_table(
          vec![vec!["a", "b"], vec!["c", "d"]],
          vec![
            flowstate_document::InputTableColumnWidth::Auto,
            flowstate_document::InputTableColumnWidth::Fraction(1),
          ],
          false,
        )),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Resize Table")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let table_block_id = projection.ids.block_ids[1];
    let table = projection_table_map_by_block_id(runtime.doc(), table_block_id).expect("table map");
    let before_rows = movable_list_strings(&child_movable_list(&table, "row_order").expect("row order"));
    let before_columns = movable_list_strings(&child_movable_list(&table, "column_order").expect("column order"));

    let events = runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::SetTableColumnWidth {
        table: table_block_id,
        column_ix: 1,
        width: flowstate_document::InputTableColumnWidth::FixedPx(222),
      },
    )?;

    assert!(events.iter().any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty())));
    let table = projection_table_map_by_block_id(runtime.doc(), table_block_id).expect("table map");
    assert_eq!(
      movable_list_strings(&child_movable_list(&table, "row_order").expect("row order")),
      before_rows
    );
    assert_eq!(
      movable_list_strings(&child_movable_list(&table, "column_order").expect("column order")),
      before_columns
    );
    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Table(table) = &projection.blocks[1] else {
      panic!("expected table block after column width command");
    };
    assert!(matches!(
      table.column_widths.as_slice(),
      [flowstate_document::TableColumnWidth::Auto, flowstate_document::TableColumnWidth::FixedPx(222)]
    ));
    Ok(())
  }

  #[test]
  fn editor_table_structure_commands_mutate_loro_table_incrementally() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Table(input_table(
          vec![vec!["a", "b"], vec!["c", "d"]],
          vec![
            flowstate_document::InputTableColumnWidth::Auto,
            flowstate_document::InputTableColumnWidth::Fraction(1),
          ],
          false,
        )),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Structure Table")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let mut projection = runtime.projection_snapshot()?;
    let table_block_id = projection.ids.block_ids[1];
    let table = projection_table_map_by_block_id(runtime.doc(), table_block_id).expect("table map");
    let initial_rows = movable_list_strings(&child_movable_list(&table, "row_order").expect("row order"));
    let initial_columns = movable_list_strings(&child_movable_list(&table, "column_order").expect("column order"));

    let inserted_row = input_table(
      vec![vec!["new-a", "new-b"]],
      Vec::new(),
      false,
    )
    .rows
    .into_iter()
    .next()
    .expect("row");
    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::InsertTableRow {
        table: table_block_id,
        row_ix: 1,
        row: inserted_row,
      },
    )?;
    projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Table(table_projection) = &projection.blocks[1] else {
      panic!("expected table block after row insert");
    };
    assert_eq!(table_projection.rows.len(), 3);
    assert_eq!(projected_table_cell_text(table_projection, 1, 0), "new-a");
    let table = projection_table_map_by_block_id(runtime.doc(), table_block_id).expect("table map");
    assert_eq!(
      movable_list_strings(&child_movable_list(&table, "column_order").expect("column order")),
      initial_columns
    );
    assert_eq!(
      movable_list_strings(&child_movable_list(&table, "row_order").expect("row order")).len(),
      initial_rows.len() + 1
    );

    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::InsertTableColumn {
        table: table_block_id,
        column_ix: 1,
        width: flowstate_document::InputTableColumnWidth::FixedPx(88),
        cells: vec![input_table_cell("x"), input_table_cell("y"), input_table_cell("z")],
      },
    )?;
    projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Table(table_projection) = &projection.blocks[1] else {
      panic!("expected table block after column insert");
    };
    assert_eq!(table_projection.column_widths.len(), 3);
    assert!(matches!(
      table_projection.column_widths.as_slice(),
      [
        flowstate_document::TableColumnWidth::Auto,
        flowstate_document::TableColumnWidth::FixedPx(88),
        flowstate_document::TableColumnWidth::Fraction(1)
      ]
    ));
    assert_eq!(projected_table_cell_text(table_projection, 0, 1), "x");
    assert_eq!(projected_table_cell_text(table_projection, 1, 1), "y");
    let table = projection_table_map_by_block_id(runtime.doc(), table_block_id).expect("table map");
    assert_eq!(
      movable_list_strings(&child_movable_list(&table, "row_order").expect("row order")).len(),
      initial_rows.len() + 1
    );

    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::DeleteTableRow {
        table: table_block_id,
        row_ix: 1,
      },
    )?;
    projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Table(table_projection) = &projection.blocks[1] else {
      panic!("expected table block after row delete");
    };
    assert_eq!(table_projection.rows.len(), 2);
    assert_eq!(projected_table_cell_text(table_projection, 1, 0), "c");

    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::DeleteTableColumn {
        table: table_block_id,
        column_ix: 1,
      },
    )?;
    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Table(table_projection) = &projection.blocks[1] else {
      panic!("expected table block after column delete");
    };
    assert_eq!(table_projection.column_widths.len(), 2);
    assert_eq!(projected_table_cell_text(table_projection, 0, 1), "b");
    assert_eq!(projected_table_cell_text(table_projection, 1, 1), "d");
    Ok(())
  }

  #[test]
  fn editor_replace_table_cell_preserves_table_structure() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(input_paragraph("body")),
        flowstate_document::InputBlock::Table(input_table(
          vec![vec!["a", "b"], vec!["c", "d"]],
          vec![
            flowstate_document::InputTableColumnWidth::Auto,
            flowstate_document::InputTableColumnWidth::Fraction(1),
          ],
          false,
        )),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Replace Cell")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;
    let projection = runtime.projection_snapshot()?;
    let table_block_id = projection.ids.block_ids[1];
    let table = projection_table_map_by_block_id(runtime.doc(), table_block_id).expect("table map");
    let before_rows = movable_list_strings(&child_movable_list(&table, "row_order").expect("row order"));
    let before_columns = movable_list_strings(&child_movable_list(&table, "column_order").expect("column order"));

    runtime.apply_editor_semantic_command(
      &projection,
      &EditorSemanticCommand::ReplaceTableCell {
        table: table_block_id,
        row_ix: 1,
        cell_ix: 0,
        cell: input_table_cell("changed"),
      },
    )?;

    let table = projection_table_map_by_block_id(runtime.doc(), table_block_id).expect("table map");
    assert_eq!(
      movable_list_strings(&child_movable_list(&table, "row_order").expect("row order")),
      before_rows
    );
    assert_eq!(
      movable_list_strings(&child_movable_list(&table, "column_order").expect("column order")),
      before_columns
    );
    let projection = runtime.projection_snapshot()?;
    let flowstate_document::Block::Table(table_projection) = &projection.blocks[1] else {
      panic!("expected table block after cell replace");
    };
    assert_eq!(projected_table_cell_text(table_projection, 1, 0), "changed");
    assert_eq!(projected_table_cell_text(table_projection, 1, 1), "d");
    Ok(())
  }

  #[test]
  fn undo_manager_restores_selection_metadata() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    let selection = UndoSelectionSnapshot {
      anchor_cursor: vec![1, 2, 3],
      head_cursor: vec![4, 5, 6],
      anchor_affinity: UndoSelectionAffinity::Before,
      head_affinity: UndoSelectionAffinity::After,
      direction: UndoSelectionDirection::Forward,
    };

    runtime.set_pending_undo_selection(Some(selection.clone()))?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "abc".to_string(),
      styles: RunStyles::default(),
    })?;
    runtime.command(SemanticCommand::Undo)?;

    assert_eq!(runtime.take_restored_undo_selection(), Some(selection.clone()));
    runtime.command(SemanticCommand::Redo)?;
    assert_eq!(runtime.take_restored_undo_selection(), Some(selection));
    Ok(())
  }

  #[test]
  fn semantic_object_commands_project_structured_blocks() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertImage {
      unicode_index: 1,
      asset_id: 7,
      alt_text: "alt".to_string(),
      caption: Some("caption".to_string()),
      sizing: flowstate_document::InputImageSizing::Fixed {
        width_px: 320,
        height_px: Some(180),
      },
      alignment: flowstate_document::InputBlockAlignment::Center,
    })?;
    runtime.command(SemanticCommand::InsertEquation {
      unicode_index: 2,
      source: "x^2".to_string(),
      display: flowstate_document::InputEquationDisplay::InlineLikeParagraph,
    })?;
    runtime.command(SemanticCommand::InsertTable {
      unicode_index: 3,
      rows: 2,
      columns: 2,
      column_widths: vec![
        flowstate_document::InputTableColumnWidth::FixedPx(120),
        flowstate_document::InputTableColumnWidth::Fraction(1),
      ],
      header_row: true,
    })?;

    let projection = runtime.projection_snapshot()?;
    assert!(matches!(
      &projection.blocks[0],
      flowstate_document::Block::Image(image)
        if image.asset_id == flowstate_document::AssetId(7)
          && image.alt_text.as_ref() == "alt"
          && image.caption.is_some()
    ));
    assert!(matches!(
      &projection.blocks[1],
      flowstate_document::Block::Equation(equation)
        if equation.source.as_ref() == "x^2"
          && equation.display == flowstate_document::EquationDisplay::InlineLikeParagraph
    ));
    assert!(matches!(
      &projection.blocks[2],
      flowstate_document::Block::Table(table)
        if table.rows.len() == 2
          && table.rows[0].cells.len() == 2
          && table.style.header_row
          && matches!(table.column_widths.as_slice(), [
            flowstate_document::TableColumnWidth::FixedPx(120),
            flowstate_document::TableColumnWidth::Fraction(1)
          ])
    ));
    Ok(())
  }

  #[test]
  fn runtime_opens_and_forks_named_revisions() -> Result<()> {
    let dir = tempfile::tempdir()?;
    let path = dir.path().join("revisions.db8");
    let doc = flowstate_document::new_loro_document("Runtime")?;
    let mut package = DocumentPackage::from_loro_snapshot(&doc, "Runtime")?;
    let blank_revision = package.create_named_revision(&doc, "Blank", "Blank document", None, None)?;
    body_text(&doc).insert(1, "latest")?;
    doc.commit();
    package.compact_to_named_snapshot(&doc, "Latest", "Latest document", None, None)?;
    package.write(&path)?;

    let mut runtime = CrdtRuntime::open_package(&path)?;
    let opened = runtime.command(SemanticCommand::OpenRevision {
      revision_id: blank_revision,
    })?;
    assert!(matches!(
      opened.as_slice(),
      [RuntimeEvent::RevisionOpened { document, .. }] if document.paragraphs.first().is_some_and(|paragraph| paragraph.byte_range.is_empty())
    ));

    let forked = runtime.command(SemanticCommand::ForkRevision {
      revision_id: blank_revision,
    })?;
    let [RuntimeEvent::RevisionForked { document, package, .. }] = forked.as_slice() else {
      panic!("expected fork event");
    };
    assert_eq!(document.paragraphs.first().map(|paragraph| paragraph.byte_range.clone()), Some(0..0));
    assert!(!package.loro_snapshots.is_empty());
    Ok(())
  }

  #[test]
  fn remote_text_insert_emits_incremental_paragraph_patch() -> Result<()> {
    let base = flowstate_document::new_loro_document("Shared")?;
    let mut source = CrdtRuntime::from_doc(base.fork(), None, None)?;
    let mut target = CrdtRuntime::from_doc(base, None, None)?;
    let setup = source.export_updates_for(&target.doc().oplog_vv())?;
    target.import_remote_update(&setup)?;
    let update = local_update_bytes(&source.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello".to_string(),
      styles: RunStyles::default(),
    })?);

    let events = target.import_remote_update(&update)?;
    let patches = events
      .iter()
      .find_map(|event| match event {
        RuntimeEvent::ProjectionPatched { patches, .. } => Some(patches),
        RuntimeEvent::LocalUpdate { .. }
        | RuntimeEvent::RemoteUpdateApplied { .. }
        | RuntimeEvent::RevisionOpened { .. }
        | RuntimeEvent::RevisionForked { .. }
        | RuntimeEvent::ProjectionUpdated { .. }
        | RuntimeEvent::SelectionRestored { .. } => None,
      })
      .expect("remote import should emit a projection patch");

    let [CollabPatch::ParagraphText { row, new, delta_utf8 }] = patches.as_slice() else {
      panic!("expected one paragraph text patch");
    };
    assert_eq!(*row, 0);
    assert_eq!(new.runs.iter().map(|run| run.text.as_str()).collect::<String>(), "hello");
    assert_eq!(delta_utf8, &[CollabTextDelta::Insert("hello".len())]);
    assert!(events.iter().all(|event| !matches!(
      event,
      RuntimeEvent::ProjectionUpdated {
        invalidation: ProjectionInvalidation {
          fallback_reason: Some("remote_update_projection_fallback"),
          ..
        },
        ..
      }
    )));
    Ok(())
  }

  #[test]
  fn remote_text_insert_in_object_document_still_emits_incremental_patch() -> Result<()> {
    let source_projection = flowstate_document::document_from_input_blocks(
      flowstate_document::DocumentTheme::default(),
      vec![
        InputBlock::Paragraph(input_paragraph("alpha")),
        InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(7),
          alt_text: "figure".to_string(),
          caption: None,
          sizing: InputImageSizing::Intrinsic,
          alignment: InputBlockAlignment::Center,
        }),
        InputBlock::Paragraph(input_paragraph("omega")),
      ],
    );
    let base = flowstate_document::document_to_loro(&source_projection, "Mixed")?;
    let mut source = CrdtRuntime::from_doc(base.fork(), None, None)?;
    let mut target = CrdtRuntime::from_doc(base, None, None)?;
    let setup = source.export_updates_for(&target.doc().oplog_vv())?;
    target.import_remote_update(&setup)?;
    let unicode_index = body_text(source.doc()).len_unicode();
    let update = local_update_bytes(&source.command(SemanticCommand::InsertText {
      unicode_index,
      text: "!".to_string(),
      styles: RunStyles::default(),
    })?);

    let events = target.import_remote_update(&update)?;
    let patches = events
      .iter()
      .find_map(|event| match event {
        RuntimeEvent::ProjectionPatched { patches, .. } => Some(patches),
        RuntimeEvent::LocalUpdate { .. }
        | RuntimeEvent::RemoteUpdateApplied { .. }
        | RuntimeEvent::RevisionOpened { .. }
        | RuntimeEvent::RevisionForked { .. }
        | RuntimeEvent::ProjectionUpdated { .. }
        | RuntimeEvent::SelectionRestored { .. } => None,
      })
      .expect("remote import should emit a projection patch");

    let [CollabPatch::ParagraphText { row, new, delta_utf8 }] = patches.as_slice() else {
      panic!("expected one paragraph text patch");
    };
    assert_eq!(*row, 2);
    assert_eq!(new.runs.iter().map(|run| run.text.as_str()).collect::<String>(), "omega!");
    assert_eq!(delta_utf8, &[CollabTextDelta::Retain("omega".len()), CollabTextDelta::Insert("!".len())]);
    assert!(events.iter().all(|event| !matches!(event, RuntimeEvent::ProjectionUpdated { .. })));
    Ok(())
  }

  #[test]
  fn local_text_insert_can_apply_without_projection_snapshot() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    let events = runtime
      .try_apply_editor_semantic_command_without_projection(&EditorSemanticCommand::InsertText {
        at: flowstate_document::DocumentOffset { paragraph: 0, byte: 0 },
        text: "hello".to_string(),
        styles: RunStyles::default(),
      })?
      .expect("text insert should use body fast path");

    assert!(events.iter().any(|event| matches!(event, RuntimeEvent::LocalUpdate { bytes, .. } if !bytes.is_empty())));
    assert!(events.iter().all(|event| !matches!(event, RuntimeEvent::ProjectionUpdated { .. })));
    assert_eq!(body_text(runtime.doc()).to_string(), "\nhello");
    Ok(())
  }

  #[test]
  fn editor_insert_after_object_uses_canonical_body_index() -> Result<()> {
    let source = flowstate_document::document_from_input_blocks(
      flowstate_document::DocumentTheme::default(),
      vec![
        InputBlock::Paragraph(input_paragraph("before")),
        InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(9),
          alt_text: "figure".to_string(),
          caption: None,
          sizing: InputImageSizing::Intrinsic,
          alignment: InputBlockAlignment::Center,
        }),
        InputBlock::Paragraph(input_paragraph("after")),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Mixed editor offsets")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;

    runtime.apply_editor_commands(
      &[EditorSemanticCommand::InsertText {
        at: DocumentOffset { paragraph: 1, byte: 2 },
        text: "!".to_string(),
        styles: RunStyles::default(),
      }],
      None,
    )?;

    let projection = runtime.projection_snapshot()?;
    assert_eq!(flowstate_document::paragraph_text(&projection, 0), "before");
    assert_eq!(flowstate_document::paragraph_text(&projection, 1), "af!ter");
    Ok(())
  }

  #[test]
  fn remote_import_reports_pending_dependencies() -> Result<()> {
    let source = flowstate_document::new_loro_document("Source")?;
    let empty_vv = VersionVector::default();
    body_text(&source).insert(1, "first")?;
    source.commit();
    let mid_vv = source.state_vv();
    body_text(&source).insert(6, " second")?;
    source.commit();
    let second_only = source.export(ExportMode::updates(&mid_vv))?;

    let mut target = CrdtRuntime::new_empty("Target")?;
    let events = target.import_remote_update(&second_only)?;
    assert!(matches!(
      events.first(),
      Some(RuntimeEvent::RemoteUpdateApplied {
        pending: Some(_),
        ..
      })
    ));

    let first_update = source.export(ExportMode::updates(&empty_vv))?;
    let events = target.import_remote_update(&first_update)?;
    assert!(matches!(
      events.first(),
      Some(RuntimeEvent::RemoteUpdateApplied {
        pending: None,
        ..
      })
    ));
    Ok(())
  }
}
