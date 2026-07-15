use std::path::PathBuf;

use flowstate_collab::SessionId;

pub fn collaboration_recovery_path(session: SessionId, title: &str) -> PathBuf {
  let dir = std::env::temp_dir().join("flowstate-collab-recovery");
  if let Err(error) = std::fs::create_dir_all(&dir) {
    tracing::warn!("creating collaboration recovery directory failed ({}): {error}", dir.display());
  }
  let session_hex = session.to_string();
  let prefix = session_hex.get(..16).unwrap_or(&session_hex);
  dir.join(format!("{prefix}-{}.db8", sanitized_recovery_title(title)))
}

/// Flow twin of [`collaboration_recovery_path`] — a framed `.fl0`.
pub fn collaboration_flow_recovery_path(session: SessionId, title: &str) -> PathBuf {
  collaboration_recovery_path(session, title).with_extension("fl0")
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
