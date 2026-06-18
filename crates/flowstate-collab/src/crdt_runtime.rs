use std::{
  collections::BTreeMap,
  io,
  path::{Path, PathBuf},
  sync::{Arc, Mutex},
};

use anyhow::{Context as _, Result};
use flowstate_document::{
  AssetId, AssetRecord, BLOCKS_BY_ID, Document, DocumentPackage, FLOW_ATTRS_KEY, FLOW_ID_KEY, FLOW_KIND_KEY, FLOW_TEXT_KEY, FLOWS_BY_ID,
  InputBlock, InputBlockAlignment, InputEquationDisplay, InputImageSizing, InputParagraph, InputTableBlock, InputTableCellBlock,
  InputTableColumnWidth, MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE, MAIN_BODY_BLOCK_ID, MARK_PARAGRAPH_STYLE,
  MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH, OBJECT_REPLACEMENT, PARAGRAPHS_BY_ID, ParagraphStyle, ROOT, ROOT_BODY_FLOW_ID,
  ROOT_FIRST_PARAGRAPH_ID, RunSemanticStyle, RunStyles, SENTINEL_NEWLINE, document_from_loro,
  loro_schema::body_text,
  new_loro_document,
};
use gpui_flowtext::SemanticEditCommand as EditorSemanticCommand;
use loro::{
  ExportMode, Frontiers, ImportStatus, LoroDoc, LoroMap, LoroMovableList, LoroText, LoroValue, Subscription, UndoItemMeta, UndoManager,
  ValueOrContainer, VersionRange, VersionVector,
  cursor::{Cursor, Side},
  event::DiffEvent,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Debug)]
pub struct CrdtRuntime {
  doc: LoroDoc,
  undo: UndoManager,
  package: Option<DocumentPackage>,
  package_path: Option<PathBuf>,
  last_persisted_frontier: Frontiers,
  last_persisted_vv: VersionVector,
  undo_selection: Arc<Mutex<UndoSelectionState>>,
  _root_subscription: Subscription,
  _local_update_subscription: Subscription,
}

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct UndoSelectionSnapshot {
  pub anchor_cursor: Vec<u8>,
  pub head_cursor: Vec<u8>,
  pub anchor_affinity: UndoSelectionAffinity,
  pub head_affinity: UndoSelectionAffinity,
  pub direction: UndoSelectionDirection,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum UndoSelectionAffinity {
  Before,
  After,
  #[default]
  Neutral,
}

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub enum UndoSelectionDirection {
  Forward,
  Backward,
  #[default]
  None,
}

#[derive(Debug, Default)]
struct UndoSelectionState {
  pending_selection: Option<Vec<u8>>,
  restored_selection: Option<UndoSelectionSnapshot>,
}

#[derive(Clone, Debug)]
pub enum SemanticCommand {
  InsertText {
    unicode_index: usize,
    text: String,
  },
  DeleteRange {
    unicode_index: usize,
    unicode_len: usize,
  },
  SplitParagraph {
    unicode_index: usize,
    inherited_style: ParagraphStyle,
  },
  SetParagraphStyle {
    boundary_unicode_index: usize,
    style: ParagraphStyle,
  },
  SetRunStyles {
    unicode_range: std::ops::Range<usize>,
    styles: RunStyles,
  },
  InsertImage {
    unicode_index: usize,
    asset_id: u128,
    alt_text: String,
    caption: Option<String>,
    sizing: InputImageSizing,
    alignment: InputBlockAlignment,
  },
  InsertEquation {
    unicode_index: usize,
    source: String,
    display: InputEquationDisplay,
  },
  InsertTable {
    unicode_index: usize,
    rows: usize,
    columns: usize,
    column_widths: Vec<InputTableColumnWidth>,
    header_row: bool,
  },
  OpenRevision {
    revision_id: u128,
  },
  ForkRevision {
    revision_id: u128,
  },
  Undo,
  Redo,
}

#[derive(Debug)]
pub enum RuntimeEvent {
  LocalUpdate {
    bytes: Vec<u8>,
    frontier: Vec<u8>,
    version_vector: Vec<u8>,
  },
  RemoteUpdateApplied {
    pending: Option<VersionRange>,
    frontier: Vec<u8>,
    version_vector: Vec<u8>,
  },
  RevisionOpened {
    revision_id: u128,
    document: Box<Document>,
  },
  RevisionForked {
    revision_id: u128,
    runtime: Box<CrdtRuntime>,
  },
  ProjectionUpdated {
    document: Box<Document>,
    invalidation: ProjectionInvalidation,
    frontier: Vec<u8>,
    version_vector: Vec<u8>,
  },
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectionInvalidation {
  pub frontier_before: Vec<u8>,
  pub frontier_after: Vec<u8>,
  pub changed_flows: Vec<String>,
  pub changed_text_ranges: Vec<ProjectionTextRange>,
  pub changed_blocks: Vec<String>,
  pub changed_tables: Vec<String>,
  pub changed_assets: Vec<String>,
  pub changed_sections: Vec<String>,
  pub rebuild_required: bool,
  pub fallback_reason: Option<&'static str>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionTextRange {
  pub flow_id: String,
  pub unicode_start: usize,
  pub unicode_len: usize,
}

impl ProjectionInvalidation {
  fn body_text(frontier_before: Vec<u8>, frontier_after: Vec<u8>, unicode_start: usize, unicode_len: usize) -> Self {
    Self {
      frontier_before,
      frontier_after,
      changed_flows: vec![ROOT_BODY_FLOW_ID.to_string()],
      changed_text_ranges: vec![ProjectionTextRange {
        flow_id: ROOT_BODY_FLOW_ID.to_string(),
        unicode_start,
        unicode_len,
      }],
      ..Self::default()
    }
  }

  fn body_style(frontier_before: Vec<u8>, frontier_after: Vec<u8>, unicode_start: usize, unicode_len: usize) -> Self {
    Self::body_text(frontier_before, frontier_after, unicode_start, unicode_len)
  }

  fn body_object(frontier_before: Vec<u8>, frontier_after: Vec<u8>, unicode_index: usize, block_kind: &'static str) -> Self {
    Self {
      frontier_before,
      frontier_after,
      changed_flows: vec![ROOT_BODY_FLOW_ID.to_string()],
      changed_text_ranges: vec![ProjectionTextRange {
        flow_id: ROOT_BODY_FLOW_ID.to_string(),
        unicode_start: unicode_index,
        unicode_len: 1,
      }],
      changed_blocks: vec![block_kind.to_string()],
      changed_tables: (block_kind == "table").then(|| block_kind.to_string()).into_iter().collect(),
      ..Self::default()
    }
  }

  fn full_rebuild(frontier_before: Vec<u8>, frontier_after: Vec<u8>, reason: &'static str) -> Self {
    tracing::warn!(reason, "Flowstate Loro projection requested full rebuild fallback");
    Self {
      frontier_before,
      frontier_after,
      rebuild_required: true,
      fallback_reason: Some(reason),
      ..Self::default()
    }
  }
}

impl CrdtRuntime {
  pub fn new_empty(title: &str) -> Result<Self> {
    let doc = new_loro_document(title).context("initializing Loro document")?;
    Self::from_doc(doc, None, None)
  }

  pub fn open_package(path: impl AsRef<Path>) -> Result<Self> {
    let path = path.as_ref();
    let package = DocumentPackage::read(path).with_context(|| format!("reading Flowstate package {}", path.display()))?;
    let doc = package.load_loro_doc().context("loading Loro document from package")?;
    Self::from_doc(doc, Some(package), Some(path.to_path_buf()))
  }

  pub fn from_doc(doc: LoroDoc, package: Option<DocumentPackage>, package_path: Option<PathBuf>) -> Result<Self> {
    let last_persisted_frontier = doc.state_frontiers();
    let last_persisted_vv = doc.state_vv();
    let root_subscription = doc.subscribe_root(Arc::new(|event: DiffEvent<'_>| {
      tracing::trace!(origin = ?event.origin, trigger = ?event.triggered_by, "Flowstate Loro root event");
    }));
    let local_update_subscription = doc.subscribe_local_update(Box::new(|bytes| {
      tracing::trace!(bytes = bytes.len(), "Flowstate Loro local update");
      true
    }));
    let mut undo = UndoManager::new(&doc);
    undo.set_merge_interval(600);
    undo.set_max_undo_steps(300);
    undo.add_exclude_origin_prefix("remote");
    let undo_selection = Arc::new(Mutex::new(UndoSelectionState::default()));
    install_undo_selection_callbacks(&mut undo, &undo_selection);
    Ok(Self {
      doc,
      undo,
      package,
      package_path,
      last_persisted_frontier,
      last_persisted_vv,
      undo_selection,
      _root_subscription: root_subscription,
      _local_update_subscription: local_update_subscription,
    })
  }

  pub fn doc(&self) -> &LoroDoc {
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

  pub fn apply_editor_semantic_command(&mut self, projection: &Document, command: &EditorSemanticCommand) -> Result<Vec<RuntimeEvent>> {
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    if apply_editor_semantic_command(&self.doc, projection, command)? {
      self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")?;
      let invalidation = ProjectionInvalidation::full_rebuild(
        from_frontier.encode(),
        self.doc.state_frontiers().encode(),
        "editor_semantic_command_bridge",
      );
      self.events_after_local_change(from_frontier, from_vv, invalidation)
    } else {
      Ok(Vec::new())
    }
  }

  pub fn projection_snapshot(&self) -> Result<Document> {
    let mut document = document_from_loro(&self.doc).context("projecting Flowstate document from canonical Loro state")?;
    if let Some(package) = &self.package {
      attach_package_assets(&mut document, package);
    }
    Ok(document)
  }

  pub fn command(&mut self, command: SemanticCommand) -> Result<Vec<RuntimeEvent>> {
    let from_frontier = self.doc.state_frontiers();
    let from_vv = self.doc.state_vv();
    let projection_invalidation;
    match command {
      SemanticCommand::InsertText { unicode_index, text } => {
        if text.is_empty() {
          return Ok(Vec::new());
        }
        let body = body_text(&self.doc);
        let newline_boundaries = inserted_newline_boundaries(unicode_index, &text);
        body.insert(unicode_index, &text).context("inserting text into Loro body flow")?;
        repair_paragraph_metadata_after_text_flow_edit(&self.doc, &body, &newline_boundaries, "semantic_insert_text")?;
        self.doc.commit();
        self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")?;
        projection_invalidation = ProjectionInvalidation::body_text(
          from_frontier.encode(),
          self.doc.state_frontiers().encode(),
          unicode_index,
          text.chars().count(),
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
          self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")?;
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
        self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")?;
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
        self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")?;
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
        self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")?;
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
        self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")?;
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
        self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")?;
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
        self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")?;
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
        let fork = self.fork_revision_runtime(revision_id)?;
        return Ok(vec![RuntimeEvent::RevisionForked {
          revision_id,
          runtime: Box::new(fork),
        }]);
      }
      SemanticCommand::Undo => {
        if !self.undo.undo().context("applying Loro undo")? {
          return Ok(Vec::new());
        }
        projection_invalidation = ProjectionInvalidation::full_rebuild(
          from_frontier.encode(),
          self.doc.state_frontiers().encode(),
          "undo_projection_fallback",
        );
      }
      SemanticCommand::Redo => {
        if !self.undo.redo().context("applying Loro redo")? {
          return Ok(Vec::new());
        }
        projection_invalidation = ProjectionInvalidation::full_rebuild(
          from_frontier.encode(),
          self.doc.state_frontiers().encode(),
          "redo_projection_fallback",
        );
      }
    }
    self.events_after_local_change(from_frontier, from_vv, projection_invalidation)
  }

  pub fn revision_projection(&self, revision_id: u128) -> Result<Document> {
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

  pub fn fork_revision_runtime(&self, revision_id: u128) -> Result<Self> {
    let package = self.package.as_ref().context("cannot fork revision without a package-backed runtime")?;
    let revision_doc = package
      .load_revision_loro_doc(revision_id)
      .context("loading revision Loro snapshot for fork")?;
    let forked_doc = revision_doc.fork();
    let forked_package = DocumentPackage::from_loro_snapshot_with_assets(&forked_doc, "Forked revision", package.assets.clone())
      .context("creating forked revision package")?;
    Self::from_doc(forked_doc, Some(forked_package), None)
  }

  pub fn import_remote_update(&mut self, bytes: &[u8]) -> Result<Vec<RuntimeEvent>> {
    let from_frontier = self.doc.state_frontiers();
    let status = self.doc.import_with(bytes, "remote").context("importing remote Loro update")?;
    let mut events = vec![RuntimeEvent::RemoteUpdateApplied {
      pending: status.pending.clone(),
      frontier: self.doc.state_frontiers().encode(),
      version_vector: self.doc.state_vv().encode(),
    }];
    let invalidation = ProjectionInvalidation::full_rebuild(
      from_frontier.encode(),
      self.doc.state_frontiers().encode(),
      "remote_update_projection_fallback",
    );
    events.push(self.projection_event(invalidation)?);
    if status.pending.is_none() {
      self.persist_update_from_last_frontier()?;
    }
    Ok(events)
  }

  fn projection_event(&self, invalidation: ProjectionInvalidation) -> Result<RuntimeEvent> {
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

  pub fn save_package(&self) -> io::Result<()> {
    let Some(package) = &self.package else {
      return Ok(());
    };
    let Some(path) = &self.package_path else {
      return Ok(());
    };
    package.write(path)
  }

  fn events_after_local_change(
    &mut self,
    from_frontier: Frontiers,
    from_vv: VersionVector,
    invalidation: ProjectionInvalidation,
  ) -> Result<Vec<RuntimeEvent>> {
    let update = self
      .doc
      .export(ExportMode::updates(&from_vv))
      .context("exporting local Loro update")?;
    self.persist_update_segment(from_frontier, from_vv, update.clone())?;
    Ok(vec![
      RuntimeEvent::LocalUpdate {
        bytes: update,
        frontier: self.doc.state_frontiers().encode(),
        version_vector: self.doc.state_vv().encode(),
      },
      self.projection_event(invalidation)?,
    ])
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
      package.rebuild_search_units_from_loro(&self.doc)?;
      if let Some(path) = &self.package_path {
        package.write(path)?;
      }
    }
    self.last_persisted_frontier = self.doc.state_frontiers();
    self.last_persisted_vv = self.doc.state_vv();
    Ok(())
  }
}

pub fn apply_editor_semantic_command(doc: &LoroDoc, projection: &Document, command: &EditorSemanticCommand) -> Result<bool> {
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
    EditorSemanticCommand::ReplaceDocument => {
      replace_entire_body_from_projection_defensively(doc, projection).context("defensively replacing Loro body from projected document")
    }
    EditorSemanticCommand::InsertBlock { block, block_ix } => {
      tracing::warn!(
        ?block,
        block_ix,
        "skipping editor InsertBlock command because the runtime bridge needs a structured block payload or stable Loro/editor block-id mapping",
      );
      Ok(false)
    }
    EditorSemanticCommand::DeleteBlock { block } => {
      tracing::warn!(
        ?block,
        "skipping editor DeleteBlock command because current Loro block ids are not the editor block ids",
      );
      Ok(false)
    }
    EditorSemanticCommand::MoveBlock { block, new_block_ix } => {
      tracing::warn!(
        ?block,
        new_block_ix,
        "skipping editor MoveBlock command because current Loro block ids are not the editor block ids",
      );
      Ok(false)
    }
    EditorSemanticCommand::ReplaceBlock { block, block_ix, after } => {
      replace_projection_object_block(doc, projection, *block_ix, after).with_context(|| {
        format!("replacing object block from editor semantic command at projection block {block_ix} ({block:?})")
      })
    }
  }
}

fn replace_projection_object_block(doc: &LoroDoc, projection: &Document, block_ix: usize, after: &InputBlock) -> Result<bool> {
  if matches!(after, InputBlock::Paragraph(_)) {
    tracing::warn!(block_ix, "skipping ReplaceBlock for paragraph payload; paragraph edits must use text/paragraph semantic commands");
    return Ok(false);
  }
  if projection.blocks.get(block_ix).is_none() {
    tracing::warn!(block_ix, "skipping ReplaceBlock because the projection block index is out of range");
    return Ok(false);
  }

  let body = body_text(doc);
  let Some(anchor_pos) = object_unicode_pos_for_projection_block(&body, block_ix) else {
    tracing::warn!(block_ix, "skipping ReplaceBlock because no object placeholder maps to the projection block index");
    return Ok(false);
  };
  let Some(block) = object_loro_block_at_unicode_pos(doc, &body, anchor_pos) else {
    tracing::warn!(block_ix, anchor_pos, "skipping ReplaceBlock because no Loro block registry entry is anchored at the placeholder");
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

fn replace_image_block_from_input(doc: &LoroDoc, block: &LoroMap, image: &flowstate_document::InputImageBlock) -> Result<()> {
  block.insert("kind", "image")?;
  block.insert("asset_id", image.asset_id.0.to_string())?;

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
    column.ensure_mergeable_map("attrs")?;
    write_table_column_width(&column, table.column_widths.get(column_ix).unwrap_or(&InputTableColumnWidth::Auto))?;
  }

  for (row_ix, row) in table.rows.iter().enumerate() {
    let row_id = format!("{prefix}.row.{row_ix}");
    row_order.push(row_id.as_str())?;
    let row_map = rows_by_id.ensure_mergeable_map(&row_id)?;
    row_map.insert("id", row_id.as_str())?;
    row_map.ensure_mergeable_map("attrs")?;
    let mut column_ix = 0_usize;
    for (cell_ix, cell) in row.cells.iter().enumerate() {
      let Some(column_id) = column_ids.get(column_ix) else {
        break;
      };
      let cell_id = format!("{row_id}.cell.{cell_ix}");
      let cell_map = cells_by_id.ensure_mergeable_map(&cell_id)?;
      cell_map.insert("id", cell_id.as_str())?;
      cell_map.insert("row_id", row_id.as_str())?;
      cell_map.insert("column_id", column_id.as_str())?;
      cell_map.insert("row_span", i64::from(cell.row_span))?;
      cell_map.insert("column_span", i64::from(cell.col_span))?;
      cell_map.ensure_mergeable_map("attrs")?;
      let nested_table_ids = cell_map.ensure_mergeable_movable_list("nested_table_ids")?;
      let nested_tables_by_id = cell_map.ensure_mergeable_map("nested_tables_by_id")?;
      clear_movable_list(&nested_table_ids)?;
      clear_map(&nested_tables_by_id)?;
      let flow_id = format!("{cell_id}.flow");
      cell_map.insert("flow_id", flow_id.as_str())?;
      let flow = ensure_flow(doc, &flow_id, "table_cell")?;
      let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
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
            nested_map.insert("kind", "table")?;
            if let Some(cursor) = text.get_cursor(pos, Side::Left) {
              nested_map.insert("anchor_cursor", cursor.encode())?;
            }
            nested_map.ensure_mergeable_map("attrs")?;
            write_table_map_from_input(doc, &nested_map.ensure_mergeable_map("table")?, nested, &format!("{cell_id}.nested.{block_ix}"))?;
          },
        }
      }
      column_ix += usize::from(cell.col_span.max(1));
    }
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

fn projection_offset_to_body_unicode_index(projection: &Document, offset: flowstate_document::DocumentOffset) -> usize {
  let mut unicode_index = 1;
  for paragraph_ix in 0..offset.paragraph.min(projection.paragraphs.len()) {
    unicode_index += flowstate_document::paragraph_text(projection, paragraph_ix)
      .chars()
      .count()
      + 1;
  }
  if let Some(paragraph) = projection.paragraphs.get(offset.paragraph) {
    let text = flowstate_document::paragraph_text(projection, offset.paragraph);
    unicode_index += text[..offset.byte.min(paragraph.byte_range.len())].chars().count();
  }
  unicode_index
}

fn paragraph_boundary_unicode_index(projection: &Document, paragraph_ix: usize) -> usize {
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
  projection: &Document,
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

fn projection_paragraph_blocks_are_adjacent(projection: &Document, first_ix: usize, second_ix: usize) -> bool {
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
  projection: &Document,
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
  let before_len = before.text.chars().count();
  let paragraph_texts = span_paragraph_texts(after);
  let replacement = paragraph_texts.join("\n");
  let body = body_text(doc);
  let start = start.min(body.len_unicode());
  let end = start.saturating_add(before_len).min(body.len_unicode());
  if end > start {
    body.delete(start, end - start)?;
  }
  if !replacement.is_empty() {
    body.insert(start, &replacement)?;
  }
  let first_boundary = start.saturating_sub(1);
  mark_replacement_span(&body, first_boundary, start, after, &paragraph_texts)?;
  let boundaries = replacement_span_boundaries(first_boundary, start, &paragraph_texts);
  repair_paragraph_metadata_after_text_flow_edit(doc, &body, &boundaries, "editor_replace_paragraph_span")?;
  doc.commit();
  Ok(true)
}

fn replace_entire_body_from_projection_defensively(doc: &LoroDoc, projection: &Document) -> Result<bool> {
  let current = document_from_loro(doc).context("projecting current Loro document before defensive ReplaceDocument")?;
  let current_object_blocks = object_block_count(&current);
  let target_object_blocks = object_block_count(projection);
  if current_object_blocks > 0 || target_object_blocks > 0 {
    tracing::warn!(
      current_blocks = current.blocks.len(),
      target_blocks = projection.blocks.len(),
      current_object_blocks,
      target_object_blocks,
      "skipping editor ReplaceDocument because object/table edits require structured block commands",
    );
    return Ok(false);
  }

  tracing::warn!(
    current_paragraphs = current.paragraphs.len(),
    target_paragraphs = projection.paragraphs.len(),
    "applying narrow paragraph-only defensive ReplaceDocument path",
  );
  replace_entire_body_from_projection(doc, projection)
}

fn object_block_count(document: &Document) -> usize {
  document
    .blocks
    .iter()
    .filter(|block| !matches!(block, flowstate_document::Block::Paragraph(_)))
    .count()
}

fn replace_entire_body_from_projection(doc: &LoroDoc, projection: &Document) -> Result<bool> {
  let body = body_text(doc);
  let len = body.len_unicode();
  if len > 1 {
    body.delete(1, len - 1)?;
  }
  let paragraph_texts = (0..projection.paragraphs.len())
    .map(|paragraph_ix| flowstate_document::paragraph_text(projection, paragraph_ix))
    .collect::<Vec<_>>();
  let replacement = paragraph_texts.join("\n");
  if !replacement.is_empty() {
    body.insert(1, &replacement)?;
  }
  body.mark(0..1, MARK_PARAGRAPH_STYLE, paragraph_style_value(projection.paragraphs.first().map_or(ParagraphStyle::Normal, |paragraph| paragraph.style)))?;
  mark_projected_paragraphs(&body, 1, &projection.paragraphs, &paragraph_texts)?;
  let boundaries = replacement_span_boundaries(0, 1, &paragraph_texts);
  repair_paragraph_metadata_after_text_flow_edit(doc, &body, &boundaries, "defensive_replace_document")?;
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
  paragraph.insert("flow_id", ROOT_BODY_FLOW_ID)?;
  if let Some(cursor) = body.get_cursor(boundary, Side::Left) {
    paragraph.insert("start_cursor", cursor.encode())?;
  }
  if let Some(cursor) = body.get_cursor(boundary, Side::Right) {
    paragraph.insert("boundary_cursor", cursor.encode())?;
  }
  paragraph.ensure_mergeable_map("attrs")?;

  let block_id = paragraph_block_key_at_boundary(doc, &body_snapshot, &blocks, boundary).unwrap_or_else(|| new_paragraph_block_id(boundary));
  let block = blocks.ensure_mergeable_map(&block_id)?;
  block.insert("id", block_id.as_str())?;
  block.insert("kind", "paragraph")?;
  block.insert("flow_id", ROOT_BODY_FLOW_ID)?;
  block.insert("paragraph_id", paragraph_id.as_str())?;
  if let Some(cursor) = body.get_cursor(boundary, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  block.ensure_mergeable_map("attrs")?;
  block.ensure_mergeable_map("nested_refs")?;
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

fn attach_package_assets(document: &mut Document, package: &DocumentPackage) {
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

fn paragraph_style_value(style: ParagraphStyle) -> i64 {
  match style {
    ParagraphStyle::Normal => 0,
    ParagraphStyle::Custom(slot) => i64::from(slot),
  }
}

fn mark_run_styles(text: &loro::LoroText, range: std::ops::Range<usize>, styles: RunStyles) -> loro::LoroResult<()> {
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

  let alt_flow_id = nested_flow_id("image_alt");
  block.insert("alt_text_flow_id", alt_flow_id.as_str())?;
  let alt_flow = ensure_flow(doc, &alt_flow_id, "alt_text")?;
  replace_text(&alt_flow.ensure_mergeable_text(FLOW_TEXT_KEY)?, alt_text)?;

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
    row.ensure_mergeable_map("attrs")?;
    for (column_ix, column_id) in column_ids.iter().enumerate() {
      let cell_id = format!("{row_id}.cell.{column_ix}");
      let cell = cells_by_id.ensure_mergeable_map(&cell_id)?;
      cell.insert("id", cell_id.as_str())?;
      cell.insert("row_id", row_id.as_str())?;
      cell.insert("column_id", column_id.as_str())?;
      cell.insert("row_span", 1_i64)?;
      cell.insert("column_span", 1_i64)?;
      cell.ensure_mergeable_map("attrs")?;
      cell.ensure_mergeable_movable_list("nested_table_ids")?;
      cell.ensure_mergeable_map("nested_tables_by_id")?;
      let flow_id = format!("{cell_id}.flow");
      cell.insert("flow_id", flow_id.as_str())?;
      let flow = ensure_flow(doc, &flow_id, "table_cell")?;
      let text = flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
      replace_text(&text, SENTINEL_NEWLINE)?;
      text.mark(0..1, MARK_PARAGRAPH_STYLE, 0_i64)?;
    }
  }
  Ok(())
}

fn ensure_flow(doc: &LoroDoc, flow_id: &str, kind: &str) -> loro::LoroResult<LoroMap> {
  let root = doc.get_map(ROOT);
  let flows = root.ensure_mergeable_map(FLOWS_BY_ID)?;
  let flow = flows.ensure_mergeable_map(flow_id)?;
  flow.insert(FLOW_ID_KEY, flow_id)?;
  flow.insert(FLOW_KIND_KEY, kind)?;
  flow.ensure_mergeable_text(FLOW_TEXT_KEY)?;
  flow.ensure_mergeable_map(FLOW_ATTRS_KEY)?;
  Ok(flow)
}

fn ensure_block(doc: &LoroDoc, kind: &str, flow_id: &str, text: &loro::LoroText, pos: usize) -> loro::LoroResult<LoroMap> {
  let root = doc.get_map(ROOT);
  let blocks = root.ensure_mergeable_map(BLOCKS_BY_ID)?;
  let id = format!("{kind}.{}", Uuid::new_v4().as_u128());
  let block = blocks.ensure_mergeable_map(&id)?;
  block.insert("id", id.as_str())?;
  block.insert("kind", kind)?;
  block.insert("flow_id", flow_id)?;
  if let Some(cursor) = text.get_cursor(pos, Side::Left) {
    block.insert("anchor_cursor", cursor.encode())?;
  }
  block.ensure_mergeable_map("attrs")?;
  block.ensure_mergeable_map("nested_refs")?;
  Ok(block)
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
  use flowstate_document::{DocumentPackage, loro_schema::body_text};

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
          cells: row
            .into_iter()
            .map(|text| flowstate_document::InputTableCell {
              blocks: vec![InputTableCellBlock::Paragraph(input_paragraph(text))],
              row_span: 1,
              col_span: 1,
            })
            .collect(),
        })
        .collect(),
      column_widths,
      style: flowstate_document::InputTableStyle { header_row },
    }
  }

  #[test]
  fn local_insert_exports_update_and_invalidates_projection() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    let events = runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello".to_string(),
    })?;
    assert!(matches!(events.first(), Some(RuntimeEvent::LocalUpdate { bytes, .. }) if !bytes.is_empty()));
    assert!(events.iter().any(|event| matches!(
      event,
      RuntimeEvent::ProjectionUpdated {
        document,
        ..
      } if flowstate_document::paragraph_text(document, 0) == "hello"
    )));
    assert_eq!(body_text(runtime.doc()).to_string(), "\nhello");
    Ok(())
  }

  #[test]
  fn split_paragraph_creates_live_paragraph_metadata_and_block_anchor() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello".to_string(),
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
  fn join_paragraphs_deletes_boundary_and_prunes_stale_metadata() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello".to_string(),
    })?;
    runtime.command(SemanticCommand::SplitParagraph {
      unicode_index: 6,
      inherited_style: flowstate_document::ParagraphStyle::Normal,
    })?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 7,
      text: "world".to_string(),
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
    })?;
    let package = DocumentPackage::read(&path)?;
    assert_eq!(package.loro_update_segments.len(), 1);
    let loaded = package.load_loro_doc()?;
    assert_eq!(body_text(&loaded).to_string(), "\npersisted");
    Ok(())
  }

  #[test]
  fn semantic_text_commands_mutate_loro_body_flow() -> Result<()> {
    let mut runtime = CrdtRuntime::new_empty("Runtime")?;
    runtime.command(SemanticCommand::InsertText {
      unicode_index: 1,
      text: "hello world".to_string(),
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
  fn editor_replace_document_with_object_blocks_is_not_silent_body_replacement() -> Result<()> {
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
    let target = flowstate_document::document_from_input_blocks(
      flowstate_document::flowstate_document_theme(),
      vec![
        flowstate_document::InputBlock::Paragraph(flowstate_document::InputParagraph {
          style: flowstate_document::ParagraphStyle::Normal,
          runs: vec![flowstate_document::InputRun {
            text: "old".to_string(),
            styles: flowstate_document::RunStyles::default(),
          }],
        }),
        flowstate_document::InputBlock::Image(flowstate_document::InputImageBlock {
          asset_id: flowstate_document::AssetId(42),
          alt_text: "alt".to_string(),
          caption: None,
          sizing: flowstate_document::InputImageSizing::FitWidth,
          alignment: flowstate_document::InputBlockAlignment::Left,
        }),
      ],
    );
    let doc = flowstate_document::document_to_loro(&source, "Object ReplaceDocument")?;
    let mut runtime = CrdtRuntime::from_doc(doc, None, None)?;

    let events = runtime.apply_editor_semantic_command(&target, &EditorSemanticCommand::ReplaceDocument)?;

    assert!(events.is_empty());
    assert_eq!(body_text(runtime.doc()).to_string(), "\nold");
    let projection = runtime.projection_snapshot()?;
    assert_eq!(projection.blocks.len(), 1);
    assert!(matches!(&projection.blocks[0], flowstate_document::Block::Paragraph(_)));
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
    let [RuntimeEvent::RevisionForked { runtime: fork, .. }] = forked.as_slice() else {
      panic!("expected fork event");
    };
    assert_eq!(body_text(fork.doc()).to_string(), "\n");
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
