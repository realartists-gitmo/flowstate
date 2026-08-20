//! Selection wire types needed by the CRDT runtime without browser networking.

use serde::{Deserialize, Serialize};

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct PresenceSelection {
  pub anchor: SelectionEndpoint,
  pub head: SelectionEndpoint,
  pub direction: SelectionDirection,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SelectionEndpoint {
  pub cursor: Vec<u8>,
  pub affinity: SelectionAffinity,
  pub visual_gravity: VisualGravity,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SelectionAffinity {
  Before,
  After,
  Neutral,
}

impl From<gpui_flowtext::SelectionAffinity> for SelectionAffinity {
  fn from(value: gpui_flowtext::SelectionAffinity) -> Self {
    match value {
      gpui_flowtext::SelectionAffinity::Before => Self::Before,
      gpui_flowtext::SelectionAffinity::After => Self::After,
      gpui_flowtext::SelectionAffinity::Neutral => Self::Neutral,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum VisualGravity {
  Upstream,
  Downstream,
  Neutral,
}

impl From<gpui_flowtext::VisualGravity> for VisualGravity {
  fn from(value: gpui_flowtext::VisualGravity) -> Self {
    match value {
      gpui_flowtext::VisualGravity::Upstream => Self::Upstream,
      gpui_flowtext::VisualGravity::Downstream => Self::Downstream,
      gpui_flowtext::VisualGravity::Neutral => Self::Neutral,
    }
  }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub enum SelectionDirection {
  Forward,
  Backward,
  None,
}
