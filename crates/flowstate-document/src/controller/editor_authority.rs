use std::collections::VecDeque;

use std::io;

use flowstate_collab::{AnchoredSelection, DocumentId, FlowCommit, FlowImportPolicy, ReplicaId, Role};
use loro::PeerID;

use super::{Db8ControllerCommit, Db8DocumentController, Db8EditIntent, Db8SourcePosition};
use crate::{
  AssetStore, AuthoritativeEditController, AuthoritativeEditResponse, AuthoritativeProjectionOrigin, AuthoritativeProjectionUpdate,
  AuthoritativeSourceEditRequest, AuthoritativeSourceOperation, AuthoritativeSourceSelection, Document, DocumentOffset, EditorSelection,
  ParagraphId, paragraph_text_len,
};

pub trait Db8CommitOutbox: std::fmt::Debug {
  fn accept(&mut self, commit: &FlowCommit) -> io::Result<()>;

  fn retained_updates(&self) -> io::Result<Vec<Vec<u8>>> {
    Ok(Vec::new())
  }
}

pub struct Db8EditorAuthority {
  controller: Db8DocumentController,
  role: Role,
  pending_commits: VecDeque<Db8ControllerCommit>,
  commit_outbox: Option<Box<dyn Db8CommitOutbox>>,
  commit_outbox_error: Option<String>,
}

impl std::fmt::Debug for Db8EditorAuthority {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("Db8EditorAuthority")
      .field("controller", &self.controller)
      .field("role", &self.role)
      .field("pending_commits", &self.pending_commits.len())
      .field("commit_outbox", &self.commit_outbox)
      .field("commit_outbox_error", &self.commit_outbox_error)
      .finish()
  }
}

impl Db8EditorAuthority {
  #[must_use]
  pub fn new(controller: Db8DocumentController, role: Role) -> Self {
    Self {
      controller,
      role,
      pending_commits: VecDeque::new(),
      commit_outbox: None,
      commit_outbox_error: None,
    }
  }

  #[must_use]
  pub const fn controller(&self) -> &Db8DocumentController {
    &self.controller
  }

  pub fn from_snapshot(
    snapshot: &[u8],
    document_id: DocumentId,
    replica_id: ReplicaId,
    assets: AssetStore,
    role: Role,
  ) -> io::Result<Self> {
    Ok(Self::new(
      Db8DocumentController::from_snapshot(snapshot, document_id, replica_id, assets)?,
      role,
    ))
  }

  #[must_use]
  pub fn peer_id(&self) -> PeerID {
    self.controller.source().peer_id()
  }

  pub fn snapshot(&self) -> io::Result<Vec<u8>> {
    self
      .controller
      .source()
      .export_snapshot()
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
  }

  pub fn frontier(&self) -> io::Result<Vec<u8>> {
    self
      .controller
      .source()
      .frontier()
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))
  }

  pub fn anchor_selection(&self, selection: &EditorSelection) -> io::Result<AnchoredSelection> {
    self.controller.anchor_selection(selection)
  }

  pub fn resolve_selection(&self, selection: &AnchoredSelection) -> io::Result<EditorSelection> {
    self.controller.resolve_selection(selection)
  }

  pub fn apply_remote_update(&mut self, peer_id: PeerID, update: &[u8]) -> io::Result<AuthoritativeProjectionUpdate> {
    self
      .controller
      .apply_remote_update(update, &FlowImportPolicy::from_peer(Role::Editor, peer_id))
      .map(|delta| delta.into_editor_update(AuthoritativeProjectionOrigin::Remote, None))
  }

  pub fn replay_retained_updates(&mut self, peer_id: PeerID, updates: &[Vec<u8>]) -> io::Result<()> {
    for update in updates {
      self
        .controller
        .apply_remote_update(update, &FlowImportPolicy::from_peer(Role::Owner, peer_id))?;
    }
    self.controller.reset_undo_lineage();
    Ok(())
  }

  pub fn install_verified_asset_bytes(&mut self, hash: [u8; 32], bytes: Vec<u8>) -> io::Result<AuthoritativeProjectionUpdate> {
    let paragraph_count = self.controller.projection().paragraphs.len();
    let document = self.controller.install_verified_asset_bytes(hash, bytes)?;
    Ok(AuthoritativeProjectionUpdate {
      document,
      affected_paragraphs_before: 0..paragraph_count,
      affected_paragraphs_after: 0..paragraph_count,
      selection: None,
      origin: AuthoritativeProjectionOrigin::Remote,
    })
  }

  pub fn set_role(&mut self, role: Role) {
    self.role = role;
  }

  pub fn set_commit_outbox(&mut self, outbox: Box<dyn Db8CommitOutbox>) {
    self.commit_outbox = Some(outbox);
    self.commit_outbox_error = None;
  }

  pub fn clear_commit_outbox(&mut self) {
    self.commit_outbox = None;
  }

  pub fn block_local_edits(&mut self, reason: impl Into<String>) {
    self.commit_outbox = None;
    self.commit_outbox_error = Some(reason.into());
  }

  #[must_use]
  pub fn commit_outbox_error(&self) -> Option<&str> {
    self.commit_outbox_error.as_deref()
  }

  pub fn drain_commits(&mut self) -> impl Iterator<Item = Db8ControllerCommit> + '_ {
    self.pending_commits.drain(..)
  }

  fn apply_source_request(&mut self, request: AuthoritativeSourceEditRequest) -> Result<AuthoritativeEditResponse, String> {
    if let Some(error) = &self.commit_outbox_error {
      return Ok(self.recovery_response(Some(format!("local source edits are blocked because durable outbox acceptance failed: {error}"))));
    }
    let intents = intents_from_source_operations(&request.operations)?;
    let undo_selection = self
      .controller
      .anchor_source_selection(request.selection_before)
      .map_err(|error| error.to_string())?;
    let commit = self
      .controller
      .apply_intents_with_undo_selection(self.role, &intents, Some(undo_selection))
      .map_err(|error| error.to_string())?;
    let resolved_selection = resolve_source_selection(self.controller.projection(), request.planned_selection);
    let mut response = AuthoritativeEditResponse {
      projection: commit
        .projection
        .clone()
        .into_editor_update(AuthoritativeProjectionOrigin::LocalInput, resolved_selection),
      error: None,
    };
    if let Err(error) = self.accept_commit(&commit) {
      let error = error.to_string();
      self.commit_outbox_error = Some(error.clone());
      response.error = Some(format!("durable outbox acceptance failed; further local source edits are blocked: {error}"));
    } else {
      self.pending_commits.push_back(commit);
    }
    Ok(response)
  }

  fn history_response(
    &mut self,
    undo: bool,
    selection_before: AuthoritativeSourceSelection,
  ) -> Result<AuthoritativeEditResponse, String> {
    if let Some(error) = &self.commit_outbox_error {
      return Ok(self.recovery_response(Some(format!("local source history edits are blocked because durable outbox acceptance failed: {error}"))));
    }
    let selection_before = self
      .controller
      .anchor_source_selection(selection_before)
      .map_err(|error| error.to_string())?;
    let commit = if undo {
      self.controller.undo_with_selection(self.role, Some(selection_before))
    } else {
      self.controller.redo_with_selection(self.role, Some(selection_before))
    }
    .map_err(|error| error.to_string())?;
    let Some(commit) = commit else {
      return Ok(self.recovery_response(None));
    };
    let origin = if undo {
      AuthoritativeProjectionOrigin::Undo
    } else {
      AuthoritativeProjectionOrigin::Redo
    };
    let mut response = AuthoritativeEditResponse {
      projection: commit
        .projection
        .clone()
        .into_editor_update(origin, commit.selection.clone()),
      error: None,
    };
    if let Err(error) = self.accept_commit(&commit) {
      let error = error.to_string();
      self.commit_outbox_error = Some(error.clone());
      response.error = Some(format!("durable outbox acceptance failed; further local source edits are blocked: {error}"));
    } else {
      self.pending_commits.push_back(commit);
    }
    Ok(response)
  }

  fn accept_commit(&mut self, commit: &Db8ControllerCommit) -> io::Result<()> {
    if let Some(outbox) = &mut self.commit_outbox {
      outbox.accept(&commit.source)?;
    }
    Ok(())
  }

  fn recovery_response(&self, error: Option<String>) -> AuthoritativeEditResponse {
    let paragraph_count = self.controller.projection().paragraphs.len();
    AuthoritativeEditResponse {
      projection: AuthoritativeProjectionUpdate {
        document: self.controller.projection().clone(),
        affected_paragraphs_before: 0..paragraph_count,
        affected_paragraphs_after: 0..paragraph_count,
        selection: None,
        origin: AuthoritativeProjectionOrigin::Recovery,
      },
      error,
    }
  }
}

fn intents_from_source_operations(operations: &[AuthoritativeSourceOperation]) -> Result<Vec<Db8EditIntent>, String> {
  if operations.is_empty() {
    return Err("authoritative DB8 source edit contained no typed operations".to_string());
  }
  operations
    .iter()
    .map(|operation| match operation {
      AuthoritativeSourceOperation::RegisterAsset { asset } => Ok(Db8EditIntent::RegisterAsset { asset: asset.clone() }),
      AuthoritativeSourceOperation::InsertText { at, text, styles } => Ok(Db8EditIntent::InsertText {
        at: Db8SourcePosition {
          paragraph_id: at.paragraph,
          byte: at.byte,
        },
        text: text.clone(),
        styles: *styles,
      }),
      AuthoritativeSourceOperation::InsertParagraphFragment {
        at,
        paragraphs,
        new_paragraphs,
      } => Ok(Db8EditIntent::InsertParagraphFragment {
        at: Db8SourcePosition {
          paragraph_id: at.paragraph,
          byte: at.byte,
        },
        paragraphs: paragraphs.clone(),
        new_paragraph_ids: new_paragraphs.clone(),
      }),
      AuthoritativeSourceOperation::DeleteText { start, end } => Ok(Db8EditIntent::DeleteText {
        start: Db8SourcePosition {
          paragraph_id: start.paragraph,
          byte: start.byte,
        },
        end: Db8SourcePosition {
          paragraph_id: end.paragraph,
          byte: end.byte,
        },
      }),
      AuthoritativeSourceOperation::SplitParagraph {
        at,
        new_paragraph,
        style,
      } => Ok(Db8EditIntent::SplitParagraph {
        at: Db8SourcePosition {
          paragraph_id: at.paragraph,
          byte: at.byte,
        },
        new_paragraph_id: *new_paragraph,
        style: *style,
      }),
      AuthoritativeSourceOperation::JoinParagraph { second_paragraph } => Ok(Db8EditIntent::JoinParagraph {
        second_paragraph_id: *second_paragraph,
      }),
      AuthoritativeSourceOperation::SetParagraphStyle { paragraph, style } => Ok(Db8EditIntent::SetParagraphStyle {
        paragraph_id: *paragraph,
        style: *style,
      }),
      AuthoritativeSourceOperation::SetRunStyles {
        paragraph,
        range,
        styles,
      } => Ok(Db8EditIntent::SetRunStyles {
        paragraph_id: *paragraph,
        range: range.clone(),
        styles: *styles,
      }),
      AuthoritativeSourceOperation::InsertBlock {
        block_id,
        block_ix,
        block,
      } => Ok(Db8EditIntent::InsertBlock {
        block_id: *block_id,
        block_ix: *block_ix,
        block: block.clone(),
      }),
      AuthoritativeSourceOperation::DeleteBlock { block_id } => Ok(Db8EditIntent::DeleteBlock { block_id: *block_id }),
      AuthoritativeSourceOperation::SetEquationSource { block_id, source } => Ok(Db8EditIntent::SetEquationSource {
        block_id: *block_id,
        source: source.clone(),
      }),
      AuthoritativeSourceOperation::SetImageProperties { block_id, image } => Ok(Db8EditIntent::SetImageProperties {
        block_id: *block_id,
        image: image.clone(),
      }),
      AuthoritativeSourceOperation::InsertTableRow {
        table_id,
        after_row_id,
        row_id,
        cells,
      } => Ok(Db8EditIntent::InsertTableRow {
        table_id: *table_id,
        after_row_id: *after_row_id,
        row_id: *row_id,
        cells: cells.clone(),
      }),
      AuthoritativeSourceOperation::DeleteTableRow { row_id } => Ok(Db8EditIntent::DeleteTableRow { row_id: *row_id }),
      AuthoritativeSourceOperation::InsertTableCell {
        row_id,
        after_cell_id,
        cell_id,
        paragraph_id,
      } => Ok(Db8EditIntent::InsertTableCell {
        row_id: *row_id,
        after_cell_id: *after_cell_id,
        cell_id: *cell_id,
        paragraph_id: *paragraph_id,
      }),
      AuthoritativeSourceOperation::DeleteTableCell { cell_id } => Ok(Db8EditIntent::DeleteTableCell { cell_id: *cell_id }),
      AuthoritativeSourceOperation::SetTableProperties {
        table_id,
        column_widths,
        style,
      } => Ok(Db8EditIntent::SetTableProperties {
        table_id: *table_id,
        column_widths: column_widths.clone(),
        style: style.clone(),
      }),
    })
    .collect()
}

impl AuthoritativeEditController for Db8EditorAuthority {
  fn apply_source(&mut self, request: AuthoritativeSourceEditRequest) -> AuthoritativeEditResponse {
    self
      .apply_source_request(request)
      .unwrap_or_else(|error| self.recovery_response(Some(error)))
  }

  fn undo(&mut self, selection_before: AuthoritativeSourceSelection) -> AuthoritativeEditResponse {
    self
      .history_response(true, selection_before)
      .unwrap_or_else(|error| self.recovery_response(Some(error)))
  }

  fn redo(&mut self, selection_before: AuthoritativeSourceSelection) -> AuthoritativeEditResponse {
    self
      .history_response(false, selection_before)
      .unwrap_or_else(|error| self.recovery_response(Some(error)))
  }

  fn recover_projection(&mut self, error: String) -> AuthoritativeEditResponse {
    self.recovery_response(Some(error))
  }

  fn capture_source_selection_anchor(&self, selection: AuthoritativeSourceSelection) -> io::Result<Option<Vec<u8>>> {
    self
      .controller
      .anchor_source_selection(selection)
      .and_then(|selection| postcard::to_stdvec(&selection).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error)))
      .map(Some)
  }

  fn resolve_source_selection_anchor(&self, anchor: &[u8]) -> io::Result<Option<AuthoritativeSourceSelection>> {
    let selection =
      postcard::from_bytes(anchor).map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error))?;
    self.controller.resolve_source_selection(&selection).map(Some)
  }

  fn native_snapshot_bytes(&self) -> io::Result<Option<Vec<u8>>> {
    let source = self.controller.source();
    let created_by_actor = source
      .created_by_actor()
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let snapshot = source
      .export_snapshot()
      .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    let recent_updates = self
      .commit_outbox
      .as_ref()
      .map_or_else(|| Ok(Vec::new()), |outbox| outbox.retained_updates())?;
    crate::db8_vnext_bytes_with_updates(self.controller.projection(), snapshot, recent_updates, created_by_actor).map(Some)
  }
}

fn resolve_source_selection(document: &Document, selection: AuthoritativeSourceSelection) -> Option<EditorSelection> {
  Some(EditorSelection {
    anchor: resolve_stable_offset(document, (selection.anchor.paragraph, selection.anchor.byte))?,
    head: resolve_stable_offset(document, (selection.head.paragraph, selection.head.byte))?,
  })
}

fn resolve_stable_offset(document: &Document, offset: (ParagraphId, usize)) -> Option<DocumentOffset> {
  let paragraph = document
    .ids
    .paragraph_ids
    .iter()
    .position(|candidate| *candidate == offset.0)?;
  let byte = offset.1.min(paragraph_text_len(document.paragraphs.get(paragraph)?));
  Some(DocumentOffset { paragraph, byte })
}
