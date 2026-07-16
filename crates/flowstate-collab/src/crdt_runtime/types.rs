use flowstate_document::{
  DocumentPackage, DocumentProjection, InputBlockAlignment, InputEquationDisplay, InputImageSizing, InputTableColumnWidth, ParagraphStyle,
  ProjectionPatchBatch, ROOT_BODY_FLOW_ID, RunStyles,
};
use gpui_flowtext::{EditorSelection, ExternalCaret, ExternalSelection};
use loro::VersionRange;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

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
  /// H-S1: the minting tier (named pin / session save / autosave grain).
  pub kind: flowstate_document::RevisionKind,
  /// H-S2: the encoded frontier this record points at (tape checkouts).
  pub frontier: Vec<u8>,
  pub created_at_unix_secs: i64,
  pub author_user_id: Option<u128>,
  pub author_display_name: Option<String>,
  pub replica_id: Option<u128>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct RuntimeCommentMessage {
  pub message_id: u128,
  pub author_user_id: u128,
  pub author_display_name: String,
  pub body: String,
  pub created_at_unix_secs: i64,
  pub updated_at_unix_secs: i64,
  /// C-S1: tombstoned by its author — render "message deleted", keep shape.
  pub deleted: bool,
}

#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeCommentThread {
  pub comment_id: u128,
  /// The one authorship authority (enforcement uses the same resolution).
  pub author_user_id: Option<u128>,
  pub quoted_text: String,
  pub resolved: bool,
  /// A general (unanchored) note — never an orphan, pinned by the panel.
  pub general: bool,
  pub created_at_unix_secs: i64,
  pub updated_at_unix_secs: i64,
  /// The frontier this thread was born at (C-S6 history-jump checks it out).
  pub created_frontier: Option<Vec<u8>>,
  pub anchor: Option<(gpui_flowtext::DocumentOffset, gpui_flowtext::DocumentOffset)>,
  pub messages: Vec<RuntimeCommentMessage>,
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
  /// H-K0 keystone: a read-only projection of the document at an arbitrary
  /// encoded frontier — serves history preview/tape/restore and comment
  /// orphan history-jump. Unlike `OpenRevision` it needs no named revision.
  OpenFrontier {
    frontier: Vec<u8>,
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
  FrontierViewOpened {
    frontier: Vec<u8>,
    document: Box<DocumentProjection>,
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
    batch: ProjectionPatchBatch,
    invalidation: ProjectionInvalidation,
    version_vector: Vec<u8>,
  },
  /// §A12.1.3 slice 4: a remote payload depended on history below this
  /// shallow session's root. It was durably merged into the PACKAGE (full
  /// history) but the in-memory doc cannot hold it — the session must reopen
  /// the document to see the merged state. Rare by construction (requires a
  /// peer offline since before the shallow root reconnecting with unsynced
  /// edits); until the reopen, the local view lacks those foreign edits.
  HistoryRebaseRequired {
    /// Merged PACKAGE tip (not the in-memory doc's frontier).
    merged_frontier: Vec<u8>,
  },
}

impl RuntimeEvent {
  #[must_use]
  pub fn frontier(&self) -> Option<&[u8]> {
    match self {
      Self::LocalUpdate { frontier, .. } | Self::RemoteUpdateApplied { frontier, .. } | Self::ProjectionUpdated { frontier, .. } => {
        Some(frontier)
      },
      Self::ProjectionPatched { batch, .. } => Some(&batch.new_frontier),
      Self::RevisionOpened { document, .. } | Self::RevisionForked { document, .. } => Some(&document.frontier),
      // A frontier view reports the HISTORICAL frontier it was opened at,
      // which is exactly what its consumers key on.
      Self::FrontierViewOpened { frontier, .. } => Some(frontier),
      // The merged frontier is the PACKAGE tip, not this runtime's — never
      // report it as a runtime frontier.
      Self::SelectionRestored { .. } | Self::HistoryRebaseRequired { .. } => None,
    }
  }
}

/// H-S5: one differing span, in the BASE (historical) projection's
/// coordinates, attributed to the author whose op wrote it.
#[derive(Clone, Debug, PartialEq)]
pub struct RuntimeDiffSpan {
  pub start: gpui_flowtext::DocumentOffset,
  pub end: gpui_flowtext::DocumentOffset,
  pub author_user_id: Option<u128>,
  pub author_display_name: Option<String>,
}

/// H-S5: a two-frontier diff, shaped for the history preview. `removed_since`
/// paints on the historical view (that text no longer exists at the newer
/// frontier); insertions have no home in the historical view, so they are
/// summarized as counts.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct RuntimeFrontierDiff {
  pub removed_since: Vec<RuntimeDiffSpan>,
  pub inserted_chars: usize,
  pub removed_chars: usize,
}

#[derive(Clone, Debug)]
pub struct RuntimePresenceCaretRequest {
  pub selection: crate::presence::PresenceSelection,
  pub color_rgb: u32,
}

#[derive(Clone, Debug)]
pub struct RuntimePresenceCarets {
  pub carets: Vec<ExternalCaret>,
  pub selections: Vec<ExternalSelection>,
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
  /// §23: set when at least one drained, in-epoch subscription summary carried a
  /// remote origin (e.g. an imported update). Lets downstream consumers bias
  /// toward conservative handling/telemetry without forcing a full rebuild for
  /// ordinary remote text edits (the incremental remote fast paths still apply).
  /// Defaults to `false` via `Default` for every existing constructor.
  pub has_remote_origin: bool,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct ProjectionTextRange {
  pub flow_id: String,
  pub unicode_start: usize,
  pub unicode_len: usize,
}

impl ProjectionInvalidation {
  pub(crate) fn body_text(frontier_before: Vec<u8>, frontier_after: Vec<u8>, unicode_start: usize, unicode_len: usize) -> Self {
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

  pub(crate) fn body_style(frontier_before: Vec<u8>, frontier_after: Vec<u8>, unicode_start: usize, unicode_len: usize) -> Self {
    Self::body_text(frontier_before, frontier_after, unicode_start, unicode_len)
  }

  /// §act-ten A10.2: multi-range body invalidation. A mass op (replace-all,
  /// sparse restyle) touches MANY disjoint spans; a single covering span forces
  /// the derive ladder to rematerialize every paragraph in between (O(doc) for
  /// a whole-document replace-all). One range per touched span keeps the
  /// readback proportional to the actual change set.
  pub(crate) fn body_text_ranges(frontier_before: Vec<u8>, frontier_after: Vec<u8>, ranges: &[(usize, usize)]) -> Self {
    Self {
      frontier_before,
      frontier_after,
      changed_flows: vec![ROOT_BODY_FLOW_ID.to_string()],
      changed_text_ranges: ranges
        .iter()
        .map(|&(unicode_start, unicode_len)| ProjectionTextRange {
          flow_id: ROOT_BODY_FLOW_ID.to_string(),
          unicode_start,
          unicode_len,
        })
        .collect(),
      ..Self::default()
    }
  }

  pub(crate) fn body_object(frontier_before: Vec<u8>, frontier_after: Vec<u8>, unicode_index: usize, block_kind: &'static str) -> Self {
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
      changed_tables: (block_kind == "table")
        .then(|| block_kind.to_string())
        .into_iter()
        .collect(),
      ..Self::default()
    }
  }

  pub(crate) fn full_rebuild(frontier_before: Vec<u8>, frontier_after: Vec<u8>, reason: &'static str) -> Self {
    Self {
      frontier_before,
      frontier_after,
      rebuild_required: true,
      fallback_reason: Some(reason),
      ..Self::default()
    }
  }
}
