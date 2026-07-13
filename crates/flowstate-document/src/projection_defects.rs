//! §5 projection defect reporting.
//!
//! Projecting a Loro document must never silently normalize malformed canonical
//! state: dropped blocks, fabricated identities, and coerced ids are data
//! corruption laundering. Instead, the projector records one
//! [`ProjectionDefect`] per anomaly while still producing a deterministic
//! projection (quarantined content is appended in stable order, fabricated
//! identities are deterministic for that projection only). The CRDT runtime
//! consumes the defects and applies idempotent canonical repair mutations with a
//! dedicated commit origin, so peers converge on the repaired state.

/// One malformed-canonical-state anomaly discovered while projecting a Loro
/// document. Each variant carries enough context for the runtime to apply (or
/// refuse) a canonical repair keyed by the stable location of the defect.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ProjectionDefect {
  /// FS-002: an object block whose `anchor_cursor` is missing, undecodable, or
  /// no longer resolves to a live U+FFFC placeholder in its flow. The block is
  /// projected in a quarantine position instead of vanishing.
  UnresolvedObjectAnchor {
    block_key: String,
    flow_id: String,
    anchor_cursor: Option<Vec<u8>>,
  },
  /// FS-003: two object blocks resolved their anchors to the same placeholder
  /// position; only one can own it. The displaced block is projected in a
  /// quarantine position instead of being overwritten out of existence.
  CollidingObjectAnchors {
    flow_id: String,
    anchor_unicode: usize,
    kept_block_key: String,
    displaced_block_key: String,
  },
  /// FS-004: a paragraph boundary with no durable paragraph metadata record.
  /// The projection keeps a deterministic fabricated placeholder id for this
  /// projection only; the runtime must write a real durable record.
  /// `boundary_unicode` is `None` when no live boundary newline exists (e.g. an
  /// entirely empty/uninitialized flow).
  MissingParagraphMetadata {
    flow_id: String,
    boundary_unicode: Option<usize>,
    fabricated_id: u128,
  },
  /// FS-005: a paragraph boundary with no durable paragraph *block* record.
  /// Mirrors [`Self::MissingParagraphMetadata`] for the block registry.
  MissingParagraphBlock {
    flow_id: String,
    boundary_unicode: Option<usize>,
    fabricated_id: u128,
  },
  /// FS-011: an image block whose `asset_id` is missing or unparseable. The
  /// projection uses a placeholder `AssetId(0)` and reports the defect so the
  /// runtime can recover the id (e.g. from the block's `content_hash`).
  InvalidAssetId { block_key: String, raw_asset_id: Option<String> },
  /// adjustmentplan:224: a paragraph boundary newline without a
  /// paragraph-style mark. The projection defaults it to `Normal` and the
  /// runtime schedules the canonical mark repair.
  MissingParagraphStyleMark { flow_id: String, boundary_unicode: usize },
  /// FS-036 backstop: a U+FFFC placeholder character with no object block
  /// anchored to it. Projecting skips the character; the runtime deletes the
  /// orphan character canonically once no block claims it.
  OrphanObjectPlaceholder { flow_id: String, unicode_pos: usize },
  /// FS-010: a table grid that is not a well-formed full rectangle — a missing
  /// `(row, column)` cell (the concurrent add-row × add-column gap), a duplicate
  /// coordinate, an out-of-bounds cell span, or a cell orphaned from the
  /// `row_order` / `column_order`. The projector normalizes the grid
  /// deterministically (synthesize / clamp / drop) so every peer reads the same
  /// full grid, and the runtime applies the matching idempotent canonical repair
  /// (ensure the coordinate cell / clamp the span / delete the orphan). The
  /// `row_id` / `column_id` are the durable u128 coordinate ids (`None` when the
  /// defect is not tied to a single coordinate).
  TableTopology {
    table_block_key: String,
    row_id: Option<u128>,
    column_id: Option<u128>,
    kind: TableTopologyKind,
  },
}

/// The specific table-grid anomaly carried by [`ProjectionDefect::TableTopology`]
/// (§P2b / FS-010). Mirrors `table_topology::TableTopologyDefect` but stays a
/// flat, id-type-free enum so `projection_defects` does not depend on the id
/// newtypes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum TableTopologyKind {
  MissingCell,
  DuplicateCoordinate,
  InvalidSpan,
  OrphanCell,
}

impl TableTopologyKind {
  /// Short stable discriminator used in the defect's stable key.
  #[must_use]
  pub const fn as_str(self) -> &'static str {
    match self {
      Self::MissingCell => "missing_cell",
      Self::DuplicateCoordinate => "duplicate_coordinate",
      Self::InvalidSpan => "invalid_span",
      Self::OrphanCell => "orphan_cell",
    }
  }
}

impl ProjectionDefect {
  /// Short stable class name for telemetry counters.
  #[must_use]
  pub fn class(&self) -> &'static str {
    match self {
      Self::UnresolvedObjectAnchor { .. } => "unresolved_object_anchor",
      Self::CollidingObjectAnchors { .. } => "colliding_object_anchors",
      Self::MissingParagraphMetadata { .. } => "missing_paragraph_metadata",
      Self::MissingParagraphBlock { .. } => "missing_paragraph_block",
      Self::InvalidAssetId { .. } => "invalid_asset_id",
      Self::MissingParagraphStyleMark { .. } => "missing_paragraph_style_mark",
      Self::OrphanObjectPlaceholder { .. } => "orphan_object_placeholder",
      Self::TableTopology { .. } => "table_topology",
    }
  }

  /// Stable key identifying the canonical location of the defect. The runtime
  /// caps repair attempts per key, so a defect that persists across repair
  /// passes is quarantined instead of spinning.
  #[must_use]
  pub fn stable_key(&self) -> String {
    match self {
      Self::UnresolvedObjectAnchor { block_key, flow_id, .. } => {
        format!("{}:{flow_id}:{block_key}", self.class())
      },
      Self::CollidingObjectAnchors {
        flow_id,
        displaced_block_key,
        ..
      } => format!("{}:{flow_id}:{displaced_block_key}", self.class()),
      Self::MissingParagraphMetadata {
        flow_id, boundary_unicode, ..
      }
      | Self::MissingParagraphBlock {
        flow_id, boundary_unicode, ..
      } => match boundary_unicode {
        Some(boundary) => format!("{}:{flow_id}:{boundary}", self.class()),
        None => format!("{}:{flow_id}:none", self.class()),
      },
      Self::InvalidAssetId { block_key, .. } => format!("{}:{block_key}", self.class()),
      Self::MissingParagraphStyleMark { flow_id, boundary_unicode } => {
        format!("{}:{flow_id}:{boundary_unicode}", self.class())
      },
      Self::OrphanObjectPlaceholder { flow_id, unicode_pos } => {
        format!("{}:{flow_id}:{unicode_pos}", self.class())
      },
      Self::TableTopology {
        table_block_key,
        row_id,
        column_id,
        kind,
      } => {
        let row = row_id.map_or_else(|| "none".to_string(), |id| id.to_string());
        let column = column_id.map_or_else(|| "none".to_string(), |id| id.to_string());
        format!("{}:{table_block_key}:{}:{row}:{column}", self.class(), kind.as_str())
      },
    }
  }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn stable_keys_are_stable_per_canonical_location() {
    let defect = ProjectionDefect::MissingParagraphMetadata {
      flow_id: "body".to_string(),
      boundary_unicode: Some(6),
      fabricated_id: 1,
    };
    let same_location_other_fabrication = ProjectionDefect::MissingParagraphMetadata {
      flow_id: "body".to_string(),
      boundary_unicode: Some(6),
      fabricated_id: 2,
    };
    assert_eq!(defect.stable_key(), same_location_other_fabrication.stable_key());
    assert_eq!(defect.stable_key(), "missing_paragraph_metadata:body:6");
  }
}
