use std::fmt;

use loro::VersionVector;

use crate::FlowProjection;

pub type FlowFrontier = Vec<u8>;
pub type FlowUpdateBytes = Vec<u8>;
pub type FlowTransactionId = u128;

#[derive(Clone, Debug)]
pub struct FlowProjectionSnapshot {
  pub projection: FlowProjection,
  pub frontier: FlowFrontier,
  pub version_vector: VersionVector,
}

#[derive(Clone, Debug)]
pub enum FlowRuntimeEvent {
  LocalUpdate {
    bytes: FlowUpdateBytes,
    frontier: FlowFrontier,
    version_vector: VersionVector,
  },
  RemoteUpdateApplied {
    bytes_len: usize,
    frontier: FlowFrontier,
    version_vector: VersionVector,
  },
  ProjectionUpdated {
    snapshot: Box<FlowProjectionSnapshot>,
  },
}

#[derive(Clone, Debug)]
pub struct FlowCommitResult {
  pub transaction_id: FlowTransactionId,
  pub base_frontier: FlowFrontier,
  pub new_frontier: FlowFrontier,
  pub events: Vec<FlowRuntimeEvent>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StaleFlowProjectionError {
  pub expected: FlowFrontier,
  pub actual: FlowFrontier,
}

impl fmt::Display for StaleFlowProjectionError {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.write_str("flow collaboration transaction was based on a stale projection frontier")
  }
}

impl std::error::Error for StaleFlowProjectionError {}
