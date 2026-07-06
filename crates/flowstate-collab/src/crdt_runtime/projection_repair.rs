//! §P2a projection repair.
//!
//! Projecting a Loro document reports one [`ProjectionDefect`] per malformed
//! canonical-state anomaly instead of silently laundering it (see
//! `flowstate_document::projection_defects`). This module turns each defect into
//! **one idempotent, convergent canonical Loro mutation**. Every mutation is
//! *check-before-write*, keyed on the stable container / cursor the defect names,
//! so two peers repairing the same defect concurrently converge on identical
//! state through Loro map/mark LWW semantics rather than racing to insert
//! divergent content.
//!
//! The runtime commits the batch of repairs under a dedicated `repair` origin
//! (excluded from undo), persists the resulting update segment, and re-projects.
//! A per-`stable_key` attempt cap in the runtime guarantees a defect that cannot
//! be repaired is eventually quarantined instead of looping.
//!
//! Convergence design notes (why these choices and not others):
//! * Content-*inserting* repairs (e.g. re-inserting a U+FFFC placeholder for an
//!   unresolved/displaced object anchor) cannot be made convergent — two peers
//!   would each insert a placeholder and duplicate the object. So anchor defects
//!   resolve by *deletion* of the dangling block record, keyed on the block's
//!   stable map key: concurrent deletes of the same key converge.
//! * Metadata / mark / field *writes* are keyed on a stable location (paragraph
//!   boundary cursor, asset content hash), so concurrent peers compute the same
//!   value and Loro LWW converges them.

use anyhow::{Context as _, Result};
use flowstate_document::ProjectionDefect;
use flowstate_fidelity as fidelity;
use loro::{ContainerTrait as _, LoroDoc, LoroMap, LoroText, cursor::Cursor};

use super::{
  BLOCKS_BY_ID, FLOWS_BY_ID, MAIN_BODY_BLOCK_ID, MARK_PARAGRAPH_STYLE, OBJECT_REPLACEMENT, ParagraphStyle, ROOT, ROOT_BODY_FLOW_ID,
  ROOT_FIRST_PARAGRAPH_ID, body_text, child_map, ensure_paragraph_metadata_at_boundary_with_keys, flow_text, map_binary_opt, map_keys,
  map_string_opt, paragraph_style_value,
};

/// Apply the single canonical repair for one projection defect.
///
/// Returns `Ok(true)` when a canonical mutation was written (the caller then
/// commits the batch under the `repair` origin), `Ok(false)` when the defect no
/// longer applies (already repaired, healed by a concurrent edit, or a peer got
/// there first — all convergent no-ops). Never commits; the caller owns the
/// commit and its origin.
pub(super) fn apply_projection_repair(doc: &LoroDoc, defect: &ProjectionDefect) -> Result<bool> {
  let result = match defect {
    ProjectionDefect::MissingParagraphMetadata {
      flow_id, boundary_unicode, ..
    }
    | ProjectionDefect::MissingParagraphBlock {
      flow_id, boundary_unicode, ..
    } => repair_missing_paragraph_metadata(doc, flow_id, *boundary_unicode),
    ProjectionDefect::MissingParagraphStyleMark { flow_id, boundary_unicode } => {
      repair_missing_paragraph_style_mark(doc, flow_id, *boundary_unicode)
    },
    ProjectionDefect::UnresolvedObjectAnchor { block_key, flow_id, .. } => repair_unresolved_object_anchor(doc, flow_id, block_key),
    ProjectionDefect::CollidingObjectAnchors {
      flow_id,
      kept_block_key,
      displaced_block_key,
      ..
    } => repair_displaced_object_block(doc, flow_id, kept_block_key, displaced_block_key),
    ProjectionDefect::OrphanObjectPlaceholder { flow_id, unicode_pos } => repair_orphan_object_placeholder(doc, flow_id, *unicode_pos),
    ProjectionDefect::InvalidAssetId { block_key, .. } => repair_invalid_asset_id(doc, block_key),
  };
  // §fidelity: record the canonical mutation this defect produced (Ok(true)); a
  // convergent no-op (Ok(false)) or error is left to the caller's telemetry.
  if matches!(&result, Ok(true)) {
    fidelity::event(super::fidelity_class_for_defect(defect), "repair-applied", || {
      format!("class={} key={}", defect.class(), defect.stable_key())
    });
  }
  result
}

/// FS-004 / FS-005: write a durable paragraph metadata + paragraph-block record
/// for a boundary that had none. Only the body flow reports these (the projector
/// tracks durable ids for the body alone), so other flows are ignored.
///
/// * `Some(boundary)` — write the durable record via the runtime's boundary-keyed
///   metadata writer, forcing *deterministic* map keys derived from the flow +
///   boundary so concurrent peers write the same keys and converge (a random uuid
///   would diverge). The writer re-checks that the boundary is a live newline.
/// * `None` — the body is empty/uninitialized: converge on the *same* canonical
///   seed that fresh-document creation uses (`seed_document_body`).
fn repair_missing_paragraph_metadata(doc: &LoroDoc, flow_id: &str, boundary_unicode: Option<usize>) -> Result<bool> {
  if flow_id != ROOT_BODY_FLOW_ID {
    return Ok(false);
  }
  match boundary_unicode {
    Some(boundary) => {
      let body = body_text(doc);
      // Deterministic, convergent map keys: two peers repairing the same boundary
      // write the SAME paragraph/block keys, so Loro map LWW converges them (the
      // default random-uuid id would diverge). Boundary 0 uses the canonical seed
      // identities; other boundaries derive a stable key from flow + boundary.
      let (paragraph_key, block_key) = if boundary == 0 {
        (ROOT_FIRST_PARAGRAPH_ID.to_string(), MAIN_BODY_BLOCK_ID.to_string())
      } else {
        (
          format!("paragraph.repair.{flow_id}.{boundary}"),
          format!("paragraph_block.repair.{flow_id}.{boundary}"),
        )
      };
      ensure_paragraph_metadata_at_boundary_with_keys(doc, &body, boundary, Some(paragraph_key), Some(block_key))
        .context("writing durable paragraph metadata for MissingParagraph* defect")?;
      Ok(true)
    },
    None => {
      flowstate_document::loro_schema::seed_document_body(doc).context("seeding empty body for MissingParagraph* defect")?;
      Ok(true)
    },
  }
}

/// adjustmentplan:224 — a paragraph boundary newline missing its paragraph-style
/// mark. Mirror the canonical mark write used by
/// `super::repair_missing_paragraph_style_marks` (the existing style-mark repair),
/// but scoped to the one boundary this defect names so it folds into the single
/// `repair`-origin commit instead of committing on its own.
///
/// Convergent: marking the boundary `Normal` is LWW — two peers writing the same
/// value converge. Check-before-write only skips when the boundary is no longer a
/// live newline.
fn repair_missing_paragraph_style_mark(doc: &LoroDoc, flow_id: &str, boundary_unicode: usize) -> Result<bool> {
  let Some(text) = flow_text_for_id(doc, flow_id) else {
    return Ok(false);
  };
  if text.to_string().chars().nth(boundary_unicode) != Some('\n') {
    return Ok(false);
  }
  text
    .mark(
      boundary_unicode..boundary_unicode + 1,
      MARK_PARAGRAPH_STYLE,
      paragraph_style_value(ParagraphStyle::Normal),
    )
    .context("repairing missing paragraph style mark")?;
  Ok(true)
}

/// FS-002: an object block whose anchor no longer resolves to a live U+FFFC
/// placeholder. Its placeholder is gone, so re-anchoring would have to *insert*
/// content (non-convergent under concurrent repair). Instead delete the dangling
/// block record, keyed on its stable map key — concurrent deletes converge.
///
/// Check-before-write: skip if the anchor now resolves to a live placeholder
/// (a concurrent edit healed it) or the record is already gone.
fn repair_unresolved_object_anchor(doc: &LoroDoc, flow_id: &str, block_key: &str) -> Result<bool> {
  let root = doc.get_map(ROOT);
  let Some(blocks) = child_map(&root, BLOCKS_BY_ID) else {
    return Ok(false);
  };
  let Some(block) = child_map(&blocks, block_key) else {
    return Ok(false);
  };
  if map_string_opt(&block, "kind").as_deref() == Some("paragraph") {
    return Ok(false);
  }
  if let Some(text) = flow_text_for_id(doc, flow_id)
    && anchor_placeholder_pos(doc, &block, &text).is_some()
  {
    return Ok(false);
  }
  blocks
    .delete(block_key)
    .context("deleting orphaned object block for UnresolvedObjectAnchor defect")?;
  Ok(true)
}

/// FS-003: two object blocks resolved their anchors to the *same* placeholder;
/// only one can own it. Delete the displaced record so a single owner remains —
/// convergent (deletion keyed on the displaced block's stable map key) where
/// giving it a "distinct anchor" (a fresh inserted placeholder) would not be.
///
/// Check-before-write: only act on a still-live collision (both anchors resolve
/// to the same placeholder position); otherwise the collision already dissolved.
fn repair_displaced_object_block(doc: &LoroDoc, flow_id: &str, kept_block_key: &str, displaced_block_key: &str) -> Result<bool> {
  let root = doc.get_map(ROOT);
  let Some(blocks) = child_map(&root, BLOCKS_BY_ID) else {
    return Ok(false);
  };
  let (Some(kept), Some(displaced)) = (child_map(&blocks, kept_block_key), child_map(&blocks, displaced_block_key)) else {
    return Ok(false);
  };
  let Some(text) = flow_text_for_id(doc, flow_id) else {
    return Ok(false);
  };
  match (anchor_placeholder_pos(doc, &kept, &text), anchor_placeholder_pos(doc, &displaced, &text)) {
    (Some(kept_pos), Some(displaced_pos)) if kept_pos == displaced_pos => {
      blocks
        .delete(displaced_block_key)
        .context("deleting displaced object block for CollidingObjectAnchors defect")?;
      Ok(true)
    },
    _ => Ok(false),
  }
}

/// FS-036 backstop: a U+FFFC placeholder that no object block claims. Delete the
/// stray character canonically. Only the body flow reports this.
///
/// Check-before-write: skip unless the character is still a U+FFFC and still
/// unclaimed (a block may have been anchored to it since projection).
fn repair_orphan_object_placeholder(doc: &LoroDoc, flow_id: &str, unicode_pos: usize) -> Result<bool> {
  if flow_id != ROOT_BODY_FLOW_ID {
    return Ok(false);
  }
  let text = body_text(doc);
  if text.to_string().chars().nth(unicode_pos) != Some(OBJECT_REPLACEMENT) {
    return Ok(false);
  }
  if placeholder_is_claimed(doc, &text, unicode_pos) {
    return Ok(false);
  }
  text
    .delete(unicode_pos, 1)
    .context("deleting orphan object placeholder for OrphanObjectPlaceholder defect")?;
  Ok(true)
}

/// FS-011: an image block whose `asset_id` is missing/unparseable. Recover it
/// from the block's durable `content_hash` (copied onto the block at insert time)
/// by matching an asset in `assets_by_id`. Convergent: every peer computes the
/// same recovered id from the same content hash and Loro LWW converges the write.
///
/// Check-before-write: skip when the block already has a valid id, has no content
/// hash, or no asset matches.
fn repair_invalid_asset_id(doc: &LoroDoc, block_key: &str) -> Result<bool> {
  let root = doc.get_map(ROOT);
  let Some(blocks) = child_map(&root, BLOCKS_BY_ID) else {
    return Ok(false);
  };
  let Some(block) = find_block_by_id(&blocks, block_key) else {
    return Ok(false);
  };
  if map_string_opt(&block, "kind").as_deref() != Some("image") {
    return Ok(false);
  }
  if map_string_opt(&block, "asset_id")
    .and_then(|id| id.parse::<u128>().ok())
    .is_some()
  {
    return Ok(false);
  }
  let Some(content_hash) = map_string_opt(&block, "content_hash") else {
    return Ok(false);
  };
  let Some(assets) = child_map(&root, flowstate_document::loro_schema::ASSETS_BY_ID) else {
    return Ok(false);
  };
  let Some(asset_id) = asset_id_for_content_hash(&assets, &content_hash) else {
    return Ok(false);
  };
  block
    .insert("asset_id", asset_id.as_str())
    .context("recovering asset id from content hash for InvalidAssetId defect")?;
  Ok(true)
}

/// Resolve a flow's canonical text container by flow id (via `flows_by_id`).
fn flow_text_for_id(doc: &LoroDoc, flow_id: &str) -> Option<LoroText> {
  let root = doc.get_map(ROOT);
  let flows = child_map(&root, FLOWS_BY_ID)?;
  let flow = child_map(&flows, flow_id)?;
  flow_text(doc, &flow).ok()
}

/// The unicode position of the live U+FFFC placeholder a block's `anchor_cursor`
/// resolves to *within `text`*, or `None` if it does not resolve to one there.
/// Mirrors the projector's object-anchor resolution (container-checked) so a
/// repair's check-before-write agrees with what projection would see.
fn anchor_placeholder_pos(doc: &LoroDoc, block: &LoroMap, text: &LoroText) -> Option<usize> {
  let bytes = map_binary_opt(block, "anchor_cursor")?;
  let cursor = Cursor::decode(&bytes).ok()?;
  if cursor.container != text.id() {
    return None;
  }
  let pos = doc.get_cursor_pos(&cursor).ok()?.current.pos;
  (text.to_string().chars().nth(pos) == Some(OBJECT_REPLACEMENT)).then_some(pos)
}

/// Whether any non-paragraph block's anchor resolves to `pos` in `text`.
fn placeholder_is_claimed(doc: &LoroDoc, text: &LoroText, pos: usize) -> bool {
  let root = doc.get_map(ROOT);
  let Some(blocks) = child_map(&root, BLOCKS_BY_ID) else {
    return false;
  };
  for key in map_keys(&blocks) {
    let Some(block) = child_map(&blocks, &key) else {
      continue;
    };
    if map_string_opt(&block, "kind").as_deref() == Some("paragraph") {
      continue;
    }
    if anchor_placeholder_pos(doc, &block, text) == Some(pos) {
      return true;
    }
  }
  false
}

/// Find a block record by the projector-reported block id, which is the block's
/// `id` field (equal to its map key for runtime-created blocks). Try a direct key
/// lookup first, then scan for a matching `id` field.
fn find_block_by_id(blocks: &LoroMap, block_key: &str) -> Option<LoroMap> {
  if let Some(block) = child_map(blocks, block_key) {
    return Some(block);
  }
  for key in map_keys(blocks) {
    let Some(block) = child_map(blocks, &key) else {
      continue;
    };
    if map_string_opt(&block, "id").as_deref() == Some(block_key) {
      return Some(block);
    }
  }
  None
}

/// The asset id (decimal string) of the asset in `assets_by_id` whose stored
/// `content_hash` equals `content_hash`, preferring the asset's own `asset_id`
/// field and falling back to its map key.
fn asset_id_for_content_hash(assets: &LoroMap, content_hash: &str) -> Option<String> {
  for key in map_keys(assets) {
    let Some(asset) = child_map(assets, &key) else {
      continue;
    };
    if map_string_opt(&asset, "content_hash").as_deref() == Some(content_hash) {
      return Some(map_string_opt(&asset, "asset_id").unwrap_or(key));
    }
  }
  None
}
