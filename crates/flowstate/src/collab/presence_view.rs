use std::path::PathBuf;

use flowstate_collab::{SessionId, binding::DocBinding, presence::{PresenceSelection, PresenceStore}};
use loro::{LoroDoc, cursor::{Cursor, PosType, Side}};

use crate::rich_text_element::{DocumentOffset, ExternalCaret, ParagraphId, RichTextEditor};

pub fn selection_for_editor(editor: &RichTextEditor, binding: &DocBinding) -> Option<PresenceSelection> {
  let selection = editor.selection().clone();
  let anchor_paragraph = editor.paragraph_id(selection.anchor.paragraph)?;
  let head_paragraph = editor.paragraph_id(selection.head.paragraph)?;
  Some(PresenceSelection {
    anchor: cursor_bytes_for_offset(binding, anchor_paragraph, selection.anchor.byte)?,
    head: cursor_bytes_for_offset(binding, head_paragraph, selection.head.byte)?,
  })
}

pub fn external_carets(doc: &LoroDoc, binding: &DocBinding, presence: &PresenceStore) -> Vec<ExternalCaret> {
  let self_key = presence.self_key();
  presence
    .roster()
    .into_iter()
    .filter(|entry| entry.key != self_key)
    .filter_map(|entry| {
      entry
        .selection
        .as_ref()
        .and_then(|selection| external_caret_for_presence(doc, binding, selection, entry.color_rgb))
    })
    .collect()
}

pub fn collaboration_recovery_path(session: SessionId, title: &str) -> PathBuf {
  let dir = std::env::temp_dir().join("flowstate-collab-recovery");
  let _ = std::fs::create_dir_all(&dir);
  let session_hex = session.to_string();
  let prefix = session_hex.get(..16).unwrap_or(&session_hex);
  dir.join(format!("{prefix}-{}.db8", sanitized_recovery_title(title)))
}

fn cursor_bytes_for_offset(binding: &DocBinding, paragraph: ParagraphId, byte: usize) -> Option<Vec<u8>> {
  let row_ix = binding.by_paragraph.get(&paragraph).copied()?;
  let row = binding.rows.get(row_ix)?;
  let text = row.text.as_ref()?;
  let byte = byte.min(text.len_utf8());
  let pos = text.convert_pos(byte, PosType::Bytes, PosType::Unicode)?;
  text.get_cursor(pos, Side::Middle).map(|cursor| cursor.encode())
}

fn external_caret_for_presence(
  doc: &LoroDoc,
  binding: &DocBinding,
  selection: &PresenceSelection,
  color_rgb: u32,
) -> Option<ExternalCaret> {
  let cursor = Cursor::decode(&selection.head).ok()?;
  let row_ix = binding.by_container.get(&cursor.container).copied()?;
  let row = binding.rows.get(row_ix)?;
  let text = row.text.as_ref()?;
  let resolved = doc.get_cursor_pos(&cursor).ok()?;
  let byte = text.convert_pos(resolved.current.pos, PosType::Unicode, PosType::Bytes)?;
  let paragraph = paragraph_ordinal_for_row(binding, row_ix)?;
  Some(ExternalCaret {
    offset: DocumentOffset { paragraph, byte },
    color_rgb,
  })
}

fn paragraph_ordinal_for_row(binding: &DocBinding, target_row: usize) -> Option<usize> {
  let mut paragraph = 0;
  for (row_ix, row) in binding.rows.iter().enumerate() {
    if row_ix == target_row {
      return row.paragraph_id.map(|_| paragraph);
    }
    if row.paragraph_id.is_some() {
      paragraph += 1;
    }
  }
  None
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
  if trimmed.is_empty() {
    "shared-document".to_string()
  } else {
    trimmed
  }
}
