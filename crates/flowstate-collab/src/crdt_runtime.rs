use std::{
  io,
  path::{Path, PathBuf},
  sync::Arc,
};

use anyhow::{Context as _, Result};
use flowstate_document::{
  AssetId, AssetRecord, BLOCKS_BY_ID, Document, DocumentPackage, FLOW_ATTRS_KEY, FLOW_ID_KEY, FLOW_KIND_KEY, FLOW_TEXT_KEY, FLOWS_BY_ID,
  InputBlockAlignment, InputEquationDisplay, InputImageSizing, InputTableColumnWidth, MARK_DIRECT_UNDERLINE, MARK_HIGHLIGHT_STYLE,
  MARK_PARAGRAPH_STYLE, MARK_RUN_SEMANTIC_STYLE, MARK_STRIKETHROUGH, OBJECT_REPLACEMENT, ParagraphStyle, ROOT, ROOT_BODY_FLOW_ID,
  RunSemanticStyle, RunStyles, SENTINEL_NEWLINE, document_from_loro,
  loro_schema::body_text,
  new_loro_document,
};
use gpui_flowtext::SemanticEditCommand as EditorSemanticCommand;
use loro::{ExportMode, Frontiers, ImportStatus, LoroDoc, LoroMap, Subscription, UndoManager, VersionRange, VersionVector, cursor::Side, event::DiffEvent};
use uuid::Uuid;

#[derive(Debug)]
pub struct CrdtRuntime {
  doc: LoroDoc,
  undo: UndoManager,
  package: Option<DocumentPackage>,
  package_path: Option<PathBuf>,
  last_persisted_frontier: Frontiers,
  last_persisted_vv: VersionVector,
  _root_subscription: Subscription,
  _local_update_subscription: Subscription,
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
    frontier: Vec<u8>,
    version_vector: Vec<u8>,
  },
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
    undo.add_exclude_origin_prefix("remote");
    Ok(Self {
      doc,
      undo,
      package,
      package_path,
      last_persisted_frontier,
      last_persisted_vv,
      _root_subscription: root_subscription,
      _local_update_subscription: local_update_subscription,
    })
  }

  pub fn doc(&self) -> &LoroDoc {
    &self.doc
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
    match command {
      SemanticCommand::InsertText { unicode_index, text } => {
        let body = body_text(&self.doc);
        body.insert(unicode_index, &text).context("inserting text into Loro body flow")?;
        self.doc.commit();
        self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")?;
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
          self.doc.commit();
          self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")?;
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
        self.doc.commit();
        self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")?;
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
      }
      SemanticCommand::SetRunStyles { unicode_range, styles } => {
        mark_run_styles(&body_text(&self.doc), unicode_range, styles).context("marking run styles in Loro body flow")?;
        self.doc.commit();
        self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")?;
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
      }
      SemanticCommand::InsertEquation {
        unicode_index,
        source,
        display,
      } => {
        insert_equation_block(&self.doc, unicode_index, &source, display).context("inserting equation block into Loro document")?;
        self.doc.commit();
        self.undo.record_new_checkpoint().context("recording Loro undo checkpoint")?;
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
        self.undo.undo().context("applying Loro undo")?;
      }
      SemanticCommand::Redo => {
        self.undo.redo().context("applying Loro redo")?;
      }
    }
    self.events_after_local_change(from_frontier, from_vv)
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
    let status = self.doc.import_with(bytes, "remote").context("importing remote Loro update")?;
    let mut events = vec![RuntimeEvent::RemoteUpdateApplied {
      pending: status.pending.clone(),
      frontier: self.doc.state_frontiers().encode(),
      version_vector: self.doc.state_vv().encode(),
    }];
    events.push(self.projection_event()?);
    if status.pending.is_none() {
      self.persist_update_from_last_frontier()?;
    }
    Ok(events)
  }

  fn projection_event(&self) -> Result<RuntimeEvent> {
    Ok(RuntimeEvent::ProjectionUpdated {
      document: Box::new(self.projection_snapshot()?),
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

  fn events_after_local_change(&mut self, from_frontier: Frontiers, from_vv: VersionVector) -> Result<Vec<RuntimeEvent>> {
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
      self.projection_event()?,
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
      body
        .insert(unicode_index, text)
        .context("inserting projection-scoped text command into Loro body flow")?;
      let inserted_len = text.chars().count();
      if inserted_len > 0 {
        mark_run_styles(&body, unicode_index..unicode_index + inserted_len, *styles).context("marking inserted run styles")?;
      }
      doc.commit();
      Ok(true)
    }
    EditorSemanticCommand::DeleteRange { range } => {
      let start = projection_offset_to_body_unicode_index(projection, range.start);
      let end = projection_offset_to_body_unicode_index(projection, range.end);
      if end > start {
        body_text(doc)
          .delete(start, end - start)
          .context("deleting projection-scoped text range from Loro body flow")?;
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
    EditorSemanticCommand::ReplaceParagraphSpan { before, after, .. } => {
      replace_body_paragraph_span(doc, projection, before, after).context("replacing paragraph span from editor semantic command")
    }
    EditorSemanticCommand::ReplaceDocument => {
      replace_entire_body_from_projection(doc, projection).context("replacing Loro body from projected document")
    }
    EditorSemanticCommand::JoinParagraphs { .. }
    | EditorSemanticCommand::InsertBlock { .. }
    | EditorSemanticCommand::DeleteBlock { .. }
    | EditorSemanticCommand::MoveBlock { .. }
    | EditorSemanticCommand::ReplaceBlock { .. } => Ok(false),
  }
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

fn replace_body_paragraph_span(doc: &LoroDoc, projection: &Document, before: &flowstate_document::DocumentSpan, after: &flowstate_document::DocumentSpan) -> Result<bool> {
  let start = projection_offset_to_body_unicode_index(
    projection,
    flowstate_document::DocumentOffset {
      paragraph: before.start_paragraph,
      byte: 0,
    },
  );
  let end_paragraph = before
    .start_paragraph
    .saturating_add(before.paragraphs.len())
    .min(projection.paragraphs.len());
  let end = if end_paragraph == 0 {
    start
  } else {
    projection_offset_to_body_unicode_index(
      projection,
      flowstate_document::DocumentOffset {
        paragraph: end_paragraph - 1,
        byte: flowstate_document::paragraph_text(projection, end_paragraph - 1).len(),
      },
    )
  };
  let replacement = after.text.clone();
  let body = body_text(doc);
  if end > start {
    body.delete(start, end - start)?;
  }
  if !replacement.is_empty() {
    body.insert(start, &replacement)?;
  }
  doc.commit();
  Ok(true)
}

fn replace_entire_body_from_projection(doc: &LoroDoc, projection: &Document) -> Result<bool> {
  let body = body_text(doc);
  let len = body.len_unicode();
  if len > 1 {
    body.delete(1, len - 1)?;
  }
  let replacement = (0..projection.paragraphs.len())
    .map(|paragraph_ix| flowstate_document::paragraph_text(projection, paragraph_ix))
    .collect::<Vec<_>>()
    .join("\n");
  if !replacement.is_empty() {
    body.insert(1, &replacement)?;
  }
  doc.commit();
  Ok(true)
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
  let row_order = table.ensure_mergeable_list("row_order")?;
  let column_order = table.ensure_mergeable_list("column_order")?;
  let rows_by_id = table.ensure_mergeable_map("rows_by_id")?;
  let columns_by_id = table.ensure_mergeable_map("columns_by_id")?;
  let cells_by_id = table.ensure_mergeable_map("cells_by_id")?;
  let table_id = table_id();

  for column_ix in 0..columns {
    let column_id = format!("{table_id}.column.{column_ix}");
    column_order.push(column_id.as_str())?;
    let column = columns_by_id.ensure_mergeable_map(&column_id)?;
    column.insert("id", column_id.as_str())?;
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
    for column_ix in 0..columns {
      let cell_id = format!("{row_id}.cell.{column_ix}");
      let cell = cells_by_id.ensure_mergeable_map(&cell_id)?;
      cell.insert("id", cell_id.as_str())?;
      cell.insert("row_id", row_id.as_str())?;
      cell.insert("column_index", i64::try_from(column_ix).unwrap_or(i64::MAX))?;
      cell.insert("row_span", 1_i64)?;
      cell.insert("column_span", 1_i64)?;
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
