//! `SyncIoHandle` — the kind-agnostic face of a session's document I/O
//! service (flow spec Part C). Transport code (direct pulls, anti-entropy)
//! needs exactly three calls — oplog version vector, incremental update
//! export, snapshot export — and bytes are bytes: the two runtimes differ
//! only in which gate the I/O thread answers under.

use anyhow::Result;

use crate::doc_io::DocIoHandle;
use crate::flow::FlowIoHandle;

#[derive(Clone)]
pub enum SyncIoHandle {
  RichText(DocIoHandle),
  Flow(FlowIoHandle),
}

impl SyncIoHandle {
  pub async fn oplog_version_vector(&self) -> Result<Vec<u8>> {
    match self {
      Self::RichText(io) => io.oplog_version_vector().await,
      Self::Flow(io) => io.oplog_version_vector().await,
    }
  }

  pub async fn export_updates_for(&self, remote_vv: Vec<u8>) -> Result<Vec<u8>> {
    match self {
      Self::RichText(io) => io.export_updates_for(remote_vv).await,
      Self::Flow(io) => io.export_updates_for(remote_vv).await,
    }
  }

  pub async fn snapshot_bytes(&self) -> Result<Vec<u8>> {
    match self {
      Self::RichText(io) => io.snapshot_bytes().await,
      Self::Flow(io) => io.snapshot_bytes().await,
    }
  }

  #[must_use]
  pub fn as_rich_text(&self) -> Option<&DocIoHandle> {
    match self {
      Self::RichText(io) => Some(io),
      Self::Flow(_) => None,
    }
  }

  #[must_use]
  pub fn as_flow(&self) -> Option<&FlowIoHandle> {
    match self {
      Self::Flow(io) => Some(io),
      Self::RichText(_) => None,
    }
  }
}

impl From<DocIoHandle> for SyncIoHandle {
  fn from(io: DocIoHandle) -> Self {
    Self::RichText(io)
  }
}

impl From<FlowIoHandle> for SyncIoHandle {
  fn from(io: FlowIoHandle) -> Self {
    Self::Flow(io)
  }
}
