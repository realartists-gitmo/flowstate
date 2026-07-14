//! The resolution law (spec §4): identity + cursor over hint, reject before
//! mutation.
//!
//! Runs strictly inside the write gate, where the maintained projection and
//! index are in-sync with the live doc by construction — so identity lookups
//! through them are lookups against canonical Loro state, not against a
//! possibly-stale render model. Every resolution is O(touched paragraph), never
//! O(document): paragraph starts come from the durable paragraph-record cursor
//! (Loro space) or the maintained index, and the only per-call scan is the
//! byte→codepoint conversion within the one anchored paragraph (spec I-8).
//!
//! Resolution order for a [`TextAnchor`]:
//! 1. If an encoded cursor is present and resolves inside the body flow, the
//!    cursor wins — it is finer-grained identity than the paragraph id and
//!    survives structural moves (e.g. a remote split relocating the anchored
//!    character into a new paragraph).
//! 2. Otherwise the paragraph id resolves through the maintained index and the
//!    byte hint is clamped into that paragraph's current text on a char
//!    boundary.
//! 3. Otherwise: reject, before any mutation (spec I-15).
//!
//! Cursor caveats baked in from the semantics audit: `get_cursor_pos` on a
//! deleted anchor is a commit barrier + `DiffCalculator` run — legal here (we
//! hold the gate; no pending foreign txn exists inside an intent before its
//! first mutation), but never legal inside a subscription callback (I-9b).
//! Degraded `id: None` cursors resolve to 0 or the whole-body end and must be
//! clamped back into a paragraph (audit F7).

use flowstate_document::DocumentProjection;
use gpui_flowtext::{DocumentOffset, ParagraphId};
use loro::cursor::Cursor;
use loro::{ContainerTrait as _, LoroDoc};

use super::intents::{TextAnchor, WriteRejected};
use crate::crdt_runtime::ProjectionRuntimeIndex;

/// A text anchor resolved against live Loro state.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ResolvedTextPosition {
  /// Paragraph index in the in-sync projection (render space).
  pub paragraph_ix: usize,
  /// UTF-8 byte offset within that paragraph's text, clamped to a char
  /// boundary.
  pub byte: usize,
  /// Unicode-codepoint index in the LIVE Loro body flow (mutation space).
  pub body_unicode: usize,
}

/// Resolve a [`TextAnchor`] per the resolution law.
pub(crate) fn resolve_text_anchor(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  index: &ProjectionRuntimeIndex,
  anchor: &TextAnchor,
) -> Result<ResolvedTextPosition, WriteRejected> {
  // 1. Cursor basis, when present and resolvable inside the body flow.
  if let Some(encoded) = &anchor.cursor
    && let Some(position) = resolve_body_cursor(doc, projection, index, encoded)
  {
    return Ok(position);
  }

  // 2. Identity basis: paragraph id through the maintained index, byte hint
  //    clamped into the paragraph.
  resolve_paragraph_position(doc, projection, index, anchor.paragraph, anchor.byte_hint)
    .ok_or(WriteRejected::UnresolvedParagraph(anchor.paragraph))
}

/// Resolve a paragraph identity + byte hint to a live body position.
pub(crate) fn resolve_paragraph_position(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  index: &ProjectionRuntimeIndex,
  paragraph: ParagraphId,
  byte_hint: usize,
) -> Option<ResolvedTextPosition> {
  let paragraph_ix = index.paragraph_index(paragraph)?;
  // Defense-in-depth (I-2): the index and projection are in-sync inside the
  // gate, but the identity check is cheap and a mismatch here would mean a
  // bookkeeping bug about to become a wrong-position op. Reject loudly.
  if projection.ids.paragraph_ids.get(paragraph_ix).copied() != Some(paragraph) {
    tracing::error!(
      paragraph = paragraph.0,
      paragraph_ix,
      "projection index/id desync detected during resolution; rejecting intent"
    );
    return None;
  }
  let byte = clamp_byte_to_char_boundary(projection, paragraph_ix, byte_hint);
  let body_unicode = index.body_unicode_for_offset_in_loro(
    doc,
    projection,
    DocumentOffset {
      paragraph: paragraph_ix,
      byte,
    },
  )?;
  Some(ResolvedTextPosition {
    paragraph_ix,
    byte,
    body_unicode,
  })
}

/// Resolve an encoded body cursor to a live position, mapping it back into
/// render space. Returns `None` (identity fallback) rather than erroring: a
/// cursor is an accelerator, and the paragraph identity is the required basis.
fn resolve_body_cursor(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  index: &ProjectionRuntimeIndex,
  encoded: &[u8],
) -> Option<ResolvedTextPosition> {
  let cursor = Cursor::decode(encoded).ok()?;
  // The cursor must address the body flow; cursors into cell flows or foreign
  // containers fall back to identity resolution.
  if cursor.container != flowstate_document::loro_schema::body_text(doc).id() {
    return None;
  }
  let position = doc.get_cursor_pos(&cursor).ok()?;
  let body_unicode = clamp_degraded_cursor_position(projection, index, &cursor, position.current.pos);
  // Map to render space through the maintained index; a position we cannot map
  // is not a safe mutation target.
  let offset = index.offset_for_body_unicode(projection, body_unicode)?;
  Some(ResolvedTextPosition {
    paragraph_ix: offset.paragraph,
    byte: offset.byte,
    body_unicode,
  })
}

/// Audit F7: a degraded (`id: None`) cursor resolves to 0 or the WHOLE-BODY
/// end, not a paragraph-relative position. Clamp such results into the last
/// valid body position so a degraded cursor can never target the wrong end of
/// a large document silently.
fn clamp_degraded_cursor_position(projection: &DocumentProjection, index: &ProjectionRuntimeIndex, cursor: &Cursor, pos: usize) -> usize {
  if cursor.id.is_some() {
    return pos;
  }
  tracing::warn!(pos, "degraded (id-less) cursor resolved; clamping into body range");
  let _ = projection;
  index.clamp_body_unicode(pos)
}

/// Clamp a byte hint into the paragraph's current text on a UTF-8 char
/// boundary.
pub(crate) fn clamp_byte_to_char_boundary(projection: &DocumentProjection, paragraph_ix: usize, byte: usize) -> usize {
  let Some(paragraph) = projection.paragraphs.get(paragraph_ix) else {
    return 0;
  };
  let text = flowstate_document::paragraph_text(projection, paragraph_ix);
  let mut byte = byte.min(flowstate_document::paragraph_text_len(paragraph));
  while byte > 0 && !text.is_char_boundary(byte) {
    byte -= 1;
  }
  byte
}

/// Resolve an ordered (start, end) anchored range. Start and end resolve
/// independently; a range that resolves inverted (concurrent edits moved the
/// endpoints past each other) is normalized rather than rejected — both
/// positions are individually valid, and a normalized range preserves the
/// user's intent of "delete what sits between my endpoints".
pub(crate) fn resolve_text_range(
  doc: &LoroDoc,
  projection: &DocumentProjection,
  index: &ProjectionRuntimeIndex,
  start: &TextAnchor,
  end: &TextAnchor,
) -> Result<(ResolvedTextPosition, ResolvedTextPosition), WriteRejected> {
  let a = resolve_text_anchor(doc, projection, index, start)?;
  let b = resolve_text_anchor(doc, projection, index, end)?;
  if a.body_unicode <= b.body_unicode { Ok((a, b)) } else { Ok((b, a)) }
}
