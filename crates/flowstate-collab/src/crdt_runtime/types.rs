use flowstate_document::{
  CollabPatch, DocumentProjection, DocumentPackage, InputBlockAlignment, InputEquationDisplay, InputImageSizing, InputTableColumnWidth,
  ParagraphStyle, ROOT_BODY_FLOW_ID, RunStyles,
};
use std::collections::BTreeMap;
use gpui_flowtext::{EditorSelection, ExternalCaret};
use loro::VersionRange;
use serde::{Deserialize, Serialize};

#[derive(Clone, Debug)]
pub struct RuntimeAssetMetadata {
  pub asset_id: u128,
  pub content_hash: [u8; 32],
  pub mime_type: String,
  pub original_name: Option<String>,
  pub byte_length: u64,
}

#[derive(Clone, Debug)]
pub struct RuntimeRevisionInfo {
  pub revision_id: u128,
  pub title: String,
  pub summary: String,
  pub created_at_unix_secs: i64,
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
pub(super) struct UndoSelectionState {
  pub(super) pending_selection: Option<Vec<u8>>,
  pub(super) restored_selection: Option<UndoSelectionSnapshot>,
}

#[derive(Clone, Debug)]
pub enum SemanticCommand {
  InsertText {
    unicode_index: usize,
    text: String,
    styles: RunStyles,
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
    document: Box<DocumentProjection>,
  },
  RevisionForked {
    revision_id: u128,
    document: Box<DocumentProjection>,
    package: Box<DocumentPackage>,
  },
  SelectionRestored {
    selection: EditorSelection,
  },
  ProjectionUpdated {
    document: Box<DocumentProjection>,
    invalidation: ProjectionInvalidation,
    frontier: Vec<u8>,
    version_vector: Vec<u8>,
  },
  ProjectionPatched {
    patches: Vec<CollabPatch>,
    invalidation: ProjectionInvalidation,
    frontier: Vec<u8>,
    version_vector: Vec<u8>,
  },
}

#[derive(Clone, Debug)]
pub struct RuntimePresenceCaretRequest {
  pub selection: crate::presence::PresenceSelection,
  pub color_rgb: u32,
}

#[derive(Clone, Debug)]
pub struct RuntimePresenceCarets {
  pub carets: Vec<ExternalCaret>,
}

#[derive(Clone, Debug, Default, Eq, PartialEq)]
pub struct ProjectionFallbackStats {
  pub total: u64,
  pub by_reason: BTreeMap<String, u64>,
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
  pub(super) fn body_text(frontier_before: Vec<u8>, frontier_after: Vec<u8>, unicode_start: usize, unicode_len: usize) -> Self {
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

  pub(super) fn body_style(frontier_before: Vec<u8>, frontier_after: Vec<u8>, unicode_start: usize, unicode_len: usize) -> Self {
    Self::body_text(frontier_before, frontier_after, unicode_start, unicode_len)
  }

  pub(super) fn body_object(frontier_before: Vec<u8>, frontier_after: Vec<u8>, unicode_index: usize, block_kind: &'static str) -> Self {
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

  pub(super) fn full_rebuild(frontier_before: Vec<u8>, frontier_after: Vec<u8>, reason: &'static str) -> Self {
    Self {
      frontier_before,
      frontier_after,
      rebuild_required: true,
      fallback_reason: Some(reason),
      ..Self::default()
    }
  }
}
