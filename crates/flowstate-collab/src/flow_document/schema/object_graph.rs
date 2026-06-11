use super::*;

pub(in crate::flow_document) fn materialize_object_graph(
  doc: &LoroDoc,
  node_id: FlowNodeId,
  limits: &FlowSourceLimits,
) -> CollabResult<MaterializedObjectGraph> {
  let root = read_node_record(doc, node_id)?;
  if root.kind != FlowNodeKind::Object {
    return Err(CollabError::InvalidSchema("vNext object graph root is not an object"));
  }
  let mut flows = BTreeMap::new();
  for child in &root.child_flows {
    materialize_reachable_flow(doc, *child, limits, &mut flows)?;
  }
  Ok(MaterializedObjectGraph {
    root,
    assets: asset_references(doc)?,
    flows,
  })
}

fn materialize_reachable_flow(
  doc: &LoroDoc,
  flow_id: FlowId,
  limits: &FlowSourceLimits,
  output: &mut BTreeMap<FlowId, MaterializedFlow>,
) -> CollabResult<()> {
  if output.contains_key(&flow_id) {
    return Ok(());
  }
  let flow = parse_flow(doc, flow_id, limits)?.materialized;
  let children = flow
    .nodes
    .iter()
    .flat_map(|node| node.record().child_flows.iter().copied())
    .collect::<Vec<_>>();
  output.insert(flow_id, flow);
  for child in children {
    materialize_reachable_flow(doc, child, limits, output)?;
  }
  Ok(())
}
