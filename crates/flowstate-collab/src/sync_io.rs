//! The kind-dispatched sync I/O handle (flow architecture spec S7): transport
//! serves both document families through ONE surface. `net/*` needs exactly
//! three calls — the anti-entropy digest's version vector, incremental update
//! export, and snapshot pulls — and both I/O services answer them under their
//! write gates, so everything below this enum stays bytes-are-bytes shared.

use anyhow::Result;

use crate::doc_io::DocIoHandle;
use crate::flow::FlowIoHandle;

/// One live session's document I/O service, dispatched by document kind.
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
