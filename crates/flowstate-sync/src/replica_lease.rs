use std::fs;
use std::io;
use std::path::{Path, PathBuf};

use fslock::LockFile;

/// An exclusive live lease for one document/replica lineage.
///
/// The OS lock is released on process exit, so a crashed process does not
/// permanently strand the durable outbox lineage. The lock file is retained to
/// make the lineage visible to diagnostics and to let `fslock` record the
/// current process ID while the lease is active.
pub struct ReplicaLease {
  path: PathBuf,
  _lock: LockFile,
}

impl std::fmt::Debug for ReplicaLease {
  fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
    f.debug_struct("ReplicaLease").field("path", &self.path).finish_non_exhaustive()
  }
}

impl ReplicaLease {
  pub fn try_acquire(path: impl AsRef<Path>) -> io::Result<Self> {
    let path = path.as_ref().to_path_buf();
    if let Some(parent) = path.parent().filter(|parent| !parent.as_os_str().is_empty()) {
      fs::create_dir_all(parent)?;
    }
    let mut lock = LockFile::open(&path)?;
    if !lock.try_lock_with_pid()? {
      return Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("replica lineage is already active: {}", path.display()),
      ));
    }
    Ok(Self { path, _lock: lock })
  }

  #[must_use]
  pub fn path(&self) -> &Path {
    &self.path
  }
}

#[cfg(test)]
mod tests {
  use flowstate_collab::SessionId;

  use super::*;

  #[test]
  fn lease_is_exclusive_and_recoverable_after_release() {
    let path = std::env::temp_dir()
      .join("flowstate-replica-lease-tests")
      .join(format!("{}.lock", SessionId::new().0));
    let first = ReplicaLease::try_acquire(&path).unwrap();
    assert_eq!(first.path(), path);
    assert_eq!(ReplicaLease::try_acquire(&path).unwrap_err().kind(), io::ErrorKind::AlreadyExists);
    drop(first);
    let second = ReplicaLease::try_acquire(&path).unwrap();
    drop(second);
    let _ = fs::remove_file(path);
  }
}
