use std::path::PathBuf;

use flowstate_collab::{
  SessionId,
  presence::{PresenceSelection, PresenceStore, SelectionAffinity, SelectionDirection, SelectionEndpoint, VisualGravity},
};
use flowstate_document::loro_schema::body_text;
use loro::{
  ContainerTrait as _, LoroDoc,
  cursor::{Cursor, PosType, Side},
};

use crate::rich_text_element::{Document, DocumentOffset, ExternalCaret, RichTextEditor, global_byte, global_to_document_offset};

pub fn selection_for_editor(doc: &LoroDoc, editor: &RichTextEditor) -> Option<PresenceSelection> {
  let selection = editor.selection().clone();
  let direction = selection_direction(selection.anchor, selection.head);
  let (anchor_affinity, head_affinity, anchor_gravity, head_gravity) = endpoint_intent(direction);
  Some(PresenceSelection {
    anchor: endpoint_for_offset(doc, editor.document(), selection.anchor, anchor_affinity, anchor_gravity)?,
    head: endpoint_for_offset(doc, editor.document(), selection.head, head_affinity, head_gravity)?,
    direction,
  })
}

pub fn external_carets(doc: &LoroDoc, document: &Document, presence: &PresenceStore) -> Vec<ExternalCaret> {
  let self_key = presence.self_key();
  presence
    .roster()
    .into_iter()
    .filter(|entry| entry.key != self_key)
    .filter_map(|entry| {
      entry
        .selection
        .as_ref()
        .and_then(|selection| external_caret_for_presence(doc, document, selection, entry.color_rgb))
    })
    .collect()
}

pub fn collaboration_recovery_path(session: SessionId, title: &str) -> PathBuf {
  let dir = std::env::temp_dir().join("flowstate-collab-recovery");
  if let Err(error) = std::fs::create_dir_all(&dir) {
    tracing::warn!("creating collaboration recovery directory failed ({}): {error}", dir.display());
  }
  let session_hex = session.to_string();
  let prefix = session_hex.get(..16).unwrap_or(&session_hex);
  dir.join(format!("{prefix}-{}.db8", sanitized_recovery_title(title)))
}

fn endpoint_for_offset(
  doc: &LoroDoc,
  document: &Document,
  offset: DocumentOffset,
  affinity: SelectionAffinity,
  visual_gravity: VisualGravity,
) -> Option<SelectionEndpoint> {
  let text = body_text(doc);
  let byte = global_byte(document, offset).min(text.len_utf8());
  let pos = text.convert_pos(byte, PosType::Bytes, PosType::Unicode)?;
  text.get_cursor(pos, side_for_affinity(affinity)).map(|cursor| SelectionEndpoint {
    cursor: cursor.encode(),
    affinity,
    visual_gravity,
  })
}

fn external_caret_for_presence(doc: &LoroDoc, document: &Document, selection: &PresenceSelection, color_rgb: u32) -> Option<ExternalCaret> {
  let cursor = Cursor::decode(&selection.head.cursor).ok()?;
  let text = body_text(doc);
  if cursor.container != text.id() {
    return None;
  }
  let resolved = doc.get_cursor_pos(&cursor).ok()?;
  let byte = text.convert_pos(resolved.current.pos, PosType::Unicode, PosType::Bytes)?;
  Some(ExternalCaret {
    offset: global_to_document_offset(document, byte),
    color_rgb,
  })
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

fn sanitized_recovery_title(title: &str) -> String {
  let mut out = String::new();
  for ch in title.chars() {
    if out.len() >= 48 {
      break;
    }
    if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
      out.push(ch);
    } else if ch.is_whitespace() && !out.ends_with('_') {
      out.push('_');
    }
  }
  let trimmed = out.trim_matches(['_', '.']).to_string();
  if trimmed.is_empty() { "shared-document".to_string() } else { trimmed }
}
