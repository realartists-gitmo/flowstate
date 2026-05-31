//! Native Flowstate model for competitive-debate flow documents.
//!
//! This crate ports the core data and mutation model from `debate-flow` into
//! Rust. It intentionally contains no GPUI code so `.fl0` documents can be
//! loaded, saved, tested, and transformed independently from the desktop UI.

mod actions;
mod document;
mod history;
mod persistence;
mod styles;

pub use actions::{
  Action, ActionBundle, CommandResult, FormatKind, add_new_box_actions, add_new_empty_actions, add_new_extension_actions, add_new_flow_actions,
  delete_node_actions, move_node_actions, new_update_action, toggle_box_format_actions,
};
pub use document::{
  BoxNode, Flow, FlowDocument, Node, NodeId, NodeValue, Nodes, ROOT_ID, constrain_index, new_box_id, new_document_id, new_flow_id,
};
pub use history::{History, HistoryAction, HistoryHolder};
pub use persistence::{
  CURRENT_SAVE_VERSION, SaveableFlowDocument, SaveableFlowNode, SaveableNode, SaveableNodeValue, fl0_bytes, flow_projection_bytes, get_json,
  load_flow_document, load_flow_document_or_new, load_nodes_from_projection, load_projection, save_flow_document,
};
pub use styles::{
  DebateStyle, DebateStyleFlow, DebateStyleKey, DebateStyleTemplate, TimerSpeech, all_debate_style_templates, debate_style, debate_style_label,
  debate_style_templates,
};
