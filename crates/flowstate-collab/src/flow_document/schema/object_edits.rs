use loro::cursor::Side;
use loro::LoroDoc;

use super::{
  AnchoredPosition, FlowId, FlowNodeId, FlowNodeKind, FlowSeedFlow, FlowSeedNode, FlowSourceLimits, create_seed_flow,
  flow_text, insert_structural_node, parse_flow,
};
use crate::{CollabError, CollabResult};

pub(crate) fn anchor_at_node_index(doc: &LoroDoc, flow_id: FlowId, node_index: usize, side: Side) -> CollabResult<AnchoredPosition> {
  let parsed = parse_flow(doc, flow_id, &FlowSourceLimits::default())?;
  let utf8 = parsed
    .parsed_nodes
    .get(node_index)
    .map_or_else(
      || parsed.parsed_nodes.last().map_or(0, |node| node.content_utf8.end),
      |node| node.content_utf8.start - node.node.record().kind.token().len_utf8(),
    );
  super::anchor_at_utf8(doc, flow_id, utf8, side)
}

pub(crate) fn insert_seed_object(
  doc: &LoroDoc,
  at: &AnchoredPosition,
  object: &FlowSeedNode,
  child_flows: &[FlowSeedFlow],
) -> CollabResult<()> {
  if object.record.kind != FlowNodeKind::Object || !object.text.is_empty() || !object.marks.is_empty() {
    return Err(CollabError::InvalidSchema("vNext inserted object seed is not an object"));
  }
  let unicode_pos = super::resolve_anchor_unicode(doc, at)?;
  super::validate_block_insert(doc, at.flow_id, unicode_pos)?;
  for flow in child_flows {
    create_seed_flow(doc, flow)?;
  }
  insert_structural_node(
    doc,
    at.flow_id,
    unicode_pos,
    FlowNodeKind::Object,
    object.record.id,
    &object.record.metadata,
    &object.record.child_flows,
  )
}

pub(crate) fn delete_object_at(doc: &LoroDoc, object_id: FlowNodeId, flow_id: FlowId, token: usize) -> CollabResult<()> {
  if super::read_node_record(doc, object_id)?.kind != FlowNodeKind::Object {
    return Err(CollabError::InvalidSchema("vNext object delete targets a paragraph"));
  }
  if super::node_owner_flow(doc, object_id)? != flow_id || super::token_node_id_at(&flow_text(doc, flow_id)?, token)? != object_id {
    return Err(CollabError::InvalidSchema("vNext object delete target position mismatch"));
  }
  flow_text(doc, flow_id)?.delete(token, 1).map_err(super::loro_error)
}
