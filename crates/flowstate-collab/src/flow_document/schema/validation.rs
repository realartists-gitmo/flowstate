use super::*;

const ROOT_KEYS: &[&str] = &[
  KEY_SCHEMA_VERSION,
  KEY_HISTORY_EPOCH,
  KEY_HISTORY_MODE,
  KEY_DOCUMENT_ID,
  KEY_CREATED_BY_ACTOR,
  KEY_ROOT_FLOW_ID,
  KEY_DOCUMENT_METADATA,
  KEY_FLOWS,
  KEY_NODES,
  KEY_ASSETS,
];
const FLOW_KEYS: &[&str] = &[KEY_FLOW_CONTENT];
const NODE_KEYS: &[&str] = &[KEY_NODE_KIND, KEY_NODE_OWNER_FLOW, KEY_NODE_METADATA, KEY_NODE_CHILD_FLOWS];

pub(in crate::flow_document) fn validate(
  doc: &LoroDoc,
  expected_document_id: Option<DocumentId>,
  limits: &FlowSourceLimits,
) -> CollabResult<ValidatedSchema> {
  validate_history_limits(doc, limits)?;
  validate_map_keys(
    &doc
      .try_get_map(ROOT_MAP)
      .ok_or(CollabError::MissingRootValue(ROOT_MAP))?,
    ROOT_KEYS,
    "vNext root keys",
  )?;
  let protected = protected_state(doc)?;
  if protected.schema_version != i64::from(FLOW_SOURCE_SCHEMA_VERSION) {
    return Err(CollabError::UnsupportedCollabSchema(u32::try_from(protected.schema_version).unwrap_or(u32::MAX)));
  }
  if history_policy(doc)? != FlowHistoryPolicy::full_history() {
    return Err(CollabError::InvalidSchema("vNext history policy"));
  }
  let document_id = DocumentId(protected.document_id);
  if expected_document_id.is_some_and(|expected| expected != document_id) {
    return Err(CollabError::InvalidSchema("vNext document ID"));
  }
  let root_flow_id = FlowId(protected.root_flow_id);
  let all_flow_ids = flow_ids(doc)?;
  if all_flow_ids.len() > limits.max_flows {
    return Err(CollabError::InvalidSchema("vNext flow count limit"));
  }
  if !all_flow_ids.contains(&root_flow_id) {
    return Err(CollabError::InvalidSchema("vNext root flow"));
  }
  let all_node_ids = node_ids(doc)?;
  if all_node_ids.len() > limits.max_nodes {
    return Err(CollabError::InvalidSchema("vNext node count limit"));
  }
  let assets = asset_references(doc)?;
  if assets.len() > limits.max_assets {
    return Err(CollabError::InvalidSchema("vNext asset count limit"));
  }

  let mut token_owners = HashMap::<FlowNodeId, FlowId>::new();
  let mut total_text_bytes = 0;
  let mut total_metadata_bytes = document_metadata(doc)?.len();
  for asset in assets.values() {
    let bytes = postcard::to_stdvec(asset)?;
    if bytes.len() > limits.max_asset_reference_bytes {
      return Err(CollabError::InvalidSchema("vNext asset reference size limit"));
    }
    total_metadata_bytes += bytes.len();
  }
  let mut total_marks = 0;
  let mut parsed_flows = BTreeMap::new();
  for flow_id in &all_flow_ids {
    validate_map_keys(&flow_record(doc, *flow_id)?, FLOW_KEYS, "vNext flow keys")?;
    let parsed = parse_flow(doc, *flow_id, limits)?;
    total_text_bytes += parsed.text_bytes;
    total_marks += parsed.mark_count;
    for parsed_node in &parsed.parsed_nodes {
      if node_owner_flow(doc, parsed_node.node.record().id)? != *flow_id {
        return Err(CollabError::InvalidSchema("vNext node owner flow mismatch"));
      }
      if token_owners.insert(parsed_node.node.record().id, *flow_id).is_some() {
        return Err(CollabError::InvalidSchema("duplicate live vNext node token"));
      }
    }
    parsed_flows.insert(*flow_id, parsed);
  }
  for node_id in &all_node_ids {
    validate_map_keys(&node_record(doc, *node_id)?, NODE_KEYS, "vNext node keys")?;
    let record = read_node_record(doc, *node_id)?;
    total_metadata_bytes += record.metadata.len();
    if record.metadata.len() > limits.max_node_metadata_bytes {
      return Err(CollabError::InvalidSchema("vNext node metadata limit"));
    }
    if record.child_flows.len() > limits.max_child_flows_per_node {
      return Err(CollabError::InvalidSchema("vNext child flow count limit"));
    }
    for child in &record.child_flows {
      if !all_flow_ids.contains(child) {
        return Err(CollabError::InvalidSchema("vNext missing child flow"));
      }
    }
  }
  if total_text_bytes > limits.max_total_text_bytes {
    return Err(CollabError::InvalidSchema("vNext total text limit"));
  }
  if total_metadata_bytes > limits.max_total_metadata_bytes {
    return Err(CollabError::InvalidSchema("vNext total metadata limit"));
  }
  if total_marks > limits.max_marks {
    return Err(CollabError::InvalidSchema("vNext mark count limit"));
  }
  validate_reachable_graph(root_flow_id, &parsed_flows, limits.max_flow_depth)?;
  Ok(ValidatedSchema {
    document_id,
    root_flow_id,
  })
}

fn validate_history_limits(doc: &LoroDoc, limits: &FlowSourceLimits) -> CollabResult<()> {
  if doc.len_ops() > limits.max_total_ops {
    return Err(CollabError::InvalidSchema("vNext total operation limit"));
  }
  if doc.len_changes() > limits.max_total_changes {
    return Err(CollabError::InvalidSchema("vNext total change limit"));
  }
  Ok(())
}

fn validate_map_keys(map: &LoroMap, expected: &[&str], label: &'static str) -> CollabResult<()> {
  let actual = map.keys().map(|key| key.to_string()).collect::<BTreeSet<_>>();
  let expected = expected.iter().map(|key| (*key).to_string()).collect::<BTreeSet<_>>();
  if actual == expected {
    Ok(())
  } else {
    Err(CollabError::InvalidSchema(label))
  }
}

fn flow_record(doc: &LoroDoc, flow_id: FlowId) -> CollabResult<LoroMap> {
  match flows_map(doc)?.get(&flow_key(flow_id)) {
    Some(ValueOrContainer::Container(Container::Map(flow))) => Ok(flow),
    _ => Err(CollabError::MissingRootValue("vNext flow")),
  }
}

fn validate_reachable_graph(root: FlowId, parsed: &BTreeMap<FlowId, ParsedFlow>, max_depth: usize) -> CollabResult<()> {
  fn visit(
    flow_id: FlowId,
    parsed: &BTreeMap<FlowId, ParsedFlow>,
    max_depth: usize,
    depth: usize,
    visiting: &mut HashSet<FlowId>,
    owners: &mut HashMap<FlowId, FlowNodeId>,
  ) -> CollabResult<()> {
    if depth > max_depth {
      return Err(CollabError::InvalidSchema("vNext flow depth limit"));
    }
    if !visiting.insert(flow_id) {
      return Err(CollabError::InvalidSchema("vNext child flow cycle"));
    }
    let flow = parsed.get(&flow_id).ok_or(CollabError::InvalidSchema("vNext missing reachable flow"))?;
    for node in &flow.parsed_nodes {
      for child in &node.node.record().child_flows {
        if owners.insert(*child, node.node.record().id).is_some() {
          return Err(CollabError::InvalidSchema("vNext child flow has multiple live owners"));
        }
        visit(*child, parsed, max_depth, depth + 1, visiting, owners)?;
      }
    }
    visiting.remove(&flow_id);
    Ok(())
  }
  visit(root, parsed, max_depth, 0, &mut HashSet::new(), &mut HashMap::new())
}
