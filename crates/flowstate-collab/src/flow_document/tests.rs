use super::*;

fn first_paragraph(document: &FlowDocument) -> FlowNodeId {
  let materialized = document.materialize().unwrap();
  materialized.flows[&document.root_flow_id()].nodes[0].record().id
}

fn paragraph_text(document: &FlowDocument, paragraph_id: FlowNodeId) -> String {
  let materialized = document.materialize().unwrap();
  materialized
    .flows
    .values()
    .flat_map(|flow| &flow.nodes)
    .find_map(|node| match node {
      FlowNode::Paragraph { record, text, .. } if record.id == paragraph_id => Some(text.clone()),
      FlowNode::Paragraph { .. } | FlowNode::Object { .. } => None,
    })
    .unwrap()
}

fn root_paragraphs(document: &FlowDocument) -> Vec<(FlowNodeId, String)> {
  document
    .materialize_flow(document.root_flow_id())
    .unwrap()
    .nodes
    .into_iter()
    .filter_map(|node| match node {
      FlowNode::Paragraph { record, text, .. } => Some((record.id, text)),
      FlowNode::Object { .. } => None,
    })
    .collect()
}

fn char_boundaries(text: &str) -> Vec<usize> {
  text
    .char_indices()
    .map(|(byte, _)| byte)
    .chain(std::iter::once(text.len()))
    .collect()
}

#[derive(Clone, Copy)]
struct TestRng(u64);

impl TestRng {
  fn next(&mut self) -> u64 {
    self.0 ^= self.0 << 13;
    self.0 ^= self.0 >> 7;
    self.0 ^= self.0 << 17;
    self.0
  }

  fn index(&mut self, len: usize) -> usize {
    (self.next() as usize) % len
  }
}

#[test]
fn text_flow_split_and_join_preserve_text_identity_and_converge() {
  let document_id = DocumentId::new();
  let actor_id = ActorId::new();
  let mut left = FlowDocument::new(document_id, actor_id, ReplicaId::new(), b"normal").unwrap();
  let first = first_paragraph(&left);
  let start = left.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  left.insert_text(Role::Owner, &start, "abcd").unwrap();

  let snapshot = left.export_snapshot().unwrap();
  let mut right = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  let split_anchor = left.anchor_in_paragraph_utf8(first, 2, Side::Right).unwrap();
  let second = FlowNodeId::new();
  let split = left
    .split_paragraph(Role::Owner, &split_anchor, second, b"normal")
    .unwrap();
  right
    .import_update_checked(&split.update, &FlowImportPolicy::editor_from_peer(left.peer_id()))
    .unwrap();

  assert_eq!(paragraph_text(&left, first), "ab");
  assert_eq!(paragraph_text(&left, second), "cd");
  assert_eq!(left.materialize().unwrap(), right.materialize().unwrap());

  let join = right.join_paragraph(Role::Editor, second).unwrap();
  left
    .import_update_checked(&join.update, &FlowImportPolicy::editor_from_peer(right.peer_id()))
    .unwrap();
  assert_eq!(paragraph_text(&left, first), "abcd");
  assert_eq!(left.materialize().unwrap(), right.materialize().unwrap());
}

#[test]
fn concurrent_typing_around_split_converges_without_copying_text_containers() {
  let document_id = DocumentId::new();
  let actor_id = ActorId::new();
  let mut left = FlowDocument::new(document_id, actor_id, ReplicaId::new(), b"normal").unwrap();
  let first = first_paragraph(&left);
  let start = left.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  left.insert_text(Role::Owner, &start, "ab").unwrap();
  let snapshot = left.export_snapshot().unwrap();
  let mut right = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();

  let split_anchor = left.anchor_in_paragraph_utf8(first, 1, Side::Right).unwrap();
  let second = FlowNodeId::new();
  let split = left
    .split_paragraph(Role::Owner, &split_anchor, second, b"normal")
    .unwrap();
  let insert_anchor = right.anchor_in_paragraph_utf8(first, 1, Side::Right).unwrap();
  let insert = right.insert_text(Role::Editor, &insert_anchor, "R").unwrap();

  left
    .import_update_checked(&insert.update, &FlowImportPolicy::editor_from_peer(right.peer_id()))
    .unwrap();
  right
    .import_update_checked(&split.update, &FlowImportPolicy::editor_from_peer(left.peer_id()))
    .unwrap();
  assert_eq!(left.materialize().unwrap(), right.materialize().unwrap());
  assert!(paragraph_text(&left, first).contains('R') || paragraph_text(&left, second).contains('R'));
}

#[test]
fn multi_replica_structural_style_undo_and_snapshot_schedule_converges() {
  let document_id = DocumentId::new();
  let actor_id = ActorId::new();
  let mut seed = FlowDocument::new(document_id, actor_id, ReplicaId::new(), b"normal").unwrap();
  let first = first_paragraph(&seed);
  let start = seed.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  seed.insert_text(Role::Owner, &start, "abcdef").unwrap();
  let snapshot = seed.export_snapshot().unwrap();

  let mut left = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  let mut middle = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  let mut right = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  let left_peer = left.peer_id();
  let middle_peer = middle.peer_id();
  let right_peer = right.peer_id();
  let mut left_undo = left.new_undo_manager();

  let left_second = FlowNodeId::new();
  let split = left
    .split_paragraph(
      Role::Editor,
      &left.anchor_in_paragraph_utf8(first, 2, Side::Right).unwrap(),
      left_second,
      b"left",
    )
    .unwrap();
  let typed = left
    .insert_text(
      Role::Editor,
      &left.anchor_in_paragraph_utf8(left_second, 0, Side::Right).unwrap(),
      "L",
    )
    .unwrap();

  let middle_second = FlowNodeId::new();
  let middle_split = middle
    .split_paragraph(
      Role::Editor,
      &middle.anchor_in_paragraph_utf8(first, 4, Side::Right).unwrap(),
      middle_second,
      b"middle",
    )
    .unwrap();
  let marked = middle
    .mark_text(
      Role::Editor,
      &middle.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap(),
      &middle.anchor_in_paragraph_utf8(first, 2, Side::Left).unwrap(),
      "bold",
      FlowMarkValue::Bool(true),
    )
    .unwrap();

  let deleted = right
    .delete_text(
      Role::Editor,
      &right.anchor_in_paragraph_utf8(first, 2, Side::Right).unwrap(),
      &right.anchor_in_paragraph_utf8(first, 4, Side::Left).unwrap(),
    )
    .unwrap();
  let metadata = right.set_node_metadata(Role::Editor, first, b"right").unwrap();

  let left_updates = [split, typed];
  let middle_updates = [middle_split, marked];
  let right_updates = [deleted, metadata];
  for update in &middle_updates {
    left
      .import_update_checked(&update.update, &FlowImportPolicy::editor_from_peer(middle_peer))
      .unwrap();
  }
  for update in &right_updates {
    left
      .import_update_checked(&update.update, &FlowImportPolicy::editor_from_peer(right_peer))
      .unwrap();
  }
  for update in &right_updates {
    middle
      .import_update_checked(&update.update, &FlowImportPolicy::editor_from_peer(right_peer))
      .unwrap();
  }
  for update in &left_updates {
    middle
      .import_update_checked(&update.update, &FlowImportPolicy::editor_from_peer(left_peer))
      .unwrap();
  }
  for update in &left_updates {
    right
      .import_update_checked(&update.update, &FlowImportPolicy::editor_from_peer(left_peer))
      .unwrap();
  }
  for update in &middle_updates {
    right
      .import_update_checked(&update.update, &FlowImportPolicy::editor_from_peer(middle_peer))
      .unwrap();
  }
  // Duplicate delivery is idempotent and must not perturb materialization.
  right
    .import_update_checked(&left_updates[0].update, &FlowImportPolicy::editor_from_peer(left_peer))
    .unwrap();

  assert_eq!(left.materialize().unwrap(), middle.materialize().unwrap());
  assert_eq!(middle.materialize().unwrap(), right.materialize().unwrap());

  let undo = left.undo(Role::Editor, &mut left_undo).unwrap().unwrap();
  middle
    .import_update_checked(&undo.update, &FlowImportPolicy::editor_from_peer(left_peer))
    .unwrap();
  right
    .import_update_checked(&undo.update, &FlowImportPolicy::editor_from_peer(left_peer))
    .unwrap();
  assert_eq!(left.materialize().unwrap(), middle.materialize().unwrap());
  assert_eq!(middle.materialize().unwrap(), right.materialize().unwrap());

  let restarted = FlowDocument::from_snapshot(&right.export_snapshot().unwrap(), Some(document_id), ReplicaId::new()).unwrap();
  assert_eq!(right.materialize().unwrap(), restarted.materialize().unwrap());
}

#[test]
fn randomized_multi_replica_text_structure_style_undo_and_restart_converges() {
  let document_id = DocumentId::new();
  let actor_id = ActorId::new();
  let mut seed = FlowDocument::new(document_id, actor_id, ReplicaId::new(), b"normal").unwrap();
  let first = first_paragraph(&seed);
  let at = seed.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  seed.insert_text(Role::Owner, &at, "seed").unwrap();
  let snapshot = seed.export_snapshot().unwrap();
  let mut replicas = (0..3)
    .map(|_| FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap())
    .collect::<Vec<_>>();
  let mut undo = replicas.iter().map(FlowDocument::new_undo_manager).collect::<Vec<_>>();
  let mut rng = TestRng(0x6a09_e667_f3bc_c909);
  let inserts = ["a", "β", "文", "🙂", "e\u{301}"];

  for round in 0..32 {
    let mut updates = Vec::new();
    for replica_ix in 0..replicas.len() {
      let paragraphs = root_paragraphs(&replicas[replica_ix]);
      let paragraph_ix = rng.index(paragraphs.len());
      let (paragraph_id, text) = &paragraphs[paragraph_ix];
      let boundaries = char_boundaries(text);
      let boundary_ix = rng.index(boundaries.len());
      let byte = boundaries[boundary_ix];
      let operation = rng.index(6);
      let commit = match operation {
        0 => {
          let at = replicas[replica_ix]
            .anchor_in_paragraph_utf8(*paragraph_id, byte, Side::Right)
            .unwrap();
          replicas[replica_ix]
            .insert_text(Role::Editor, &at, inserts[rng.index(inserts.len())])
            .ok()
        },
        1 if boundaries.len() > 1 => {
          let start_ix = rng.index(boundaries.len() - 1);
          let start = replicas[replica_ix]
            .anchor_in_paragraph_utf8(*paragraph_id, boundaries[start_ix], Side::Right)
            .unwrap();
          let end = replicas[replica_ix]
            .anchor_in_paragraph_utf8(*paragraph_id, boundaries[start_ix + 1], Side::Left)
            .unwrap();
          replicas[replica_ix].delete_text(Role::Editor, &start, &end).ok()
        },
        2 => {
          let at = replicas[replica_ix]
            .anchor_in_paragraph_utf8(*paragraph_id, byte, Side::Right)
            .unwrap();
          replicas[replica_ix]
            .split_paragraph(Role::Editor, &at, FlowNodeId::new(), format!("split-{round}-{replica_ix}").as_bytes())
            .ok()
        },
        3 if paragraph_ix > 0 => replicas[replica_ix].join_paragraph(Role::Editor, *paragraph_id).ok(),
        4 if boundaries.len() > 1 => {
          let start = replicas[replica_ix]
            .anchor_in_paragraph_utf8(*paragraph_id, 0, Side::Right)
            .unwrap();
          let end = replicas[replica_ix]
            .anchor_in_paragraph_utf8(*paragraph_id, *boundaries.last().unwrap(), Side::Left)
            .unwrap();
          replicas[replica_ix]
            .mark_text(
              Role::Editor,
              &start,
              &end,
              "bold",
              FlowMarkValue::Bool((round + replica_ix) % 2 == 0),
            )
            .ok()
        },
        5 => replicas[replica_ix].undo(Role::Editor, &mut undo[replica_ix]).ok().flatten(),
        _ => replicas[replica_ix]
          .set_node_metadata(Role::Editor, *paragraph_id, format!("meta-{round}-{replica_ix}").as_bytes())
          .ok(),
      };
      if let Some(commit) = commit {
        updates.push((replica_ix, replicas[replica_ix].peer_id(), commit.update));
      }
    }

    while !updates.is_empty() {
      let update_ix = rng.index(updates.len());
      let (origin, peer_id, update) = updates.swap_remove(update_ix);
      for (replica_ix, replica) in replicas.iter_mut().enumerate() {
        if replica_ix != origin {
          replica
            .import_update_checked(&update, &FlowImportPolicy::editor_from_peer(peer_id))
            .unwrap();
        }
      }
      if rng.next().is_multiple_of(3) {
        let duplicate_ix = rng.index(replicas.len());
        if duplicate_ix != origin {
          replicas[duplicate_ix]
            .import_update_checked(&update, &FlowImportPolicy::editor_from_peer(peer_id))
            .unwrap();
        }
      }
    }

    let expected = replicas[0].materialize().unwrap();
    for replica in &replicas[1..] {
      assert_eq!(replica.materialize().unwrap(), expected);
    }
    if round.is_multiple_of(7) {
      let restart_ix = rng.index(replicas.len());
      let restarted =
        FlowDocument::from_snapshot(&replicas[restart_ix].export_snapshot().unwrap(), Some(document_id), ReplicaId::new()).unwrap();
      assert_eq!(restarted.materialize().unwrap(), expected);
    }
  }
}

#[test]
fn ordinary_text_changes_materialize_a_bounded_flow_window() {
  let root_flow = FlowId::new();
  let paragraph_ids = (0..128).map(|_| FlowNodeId::new()).collect::<Vec<_>>();
  let seed = FlowDocumentSeed {
    root_flow_id: root_flow,
    document_metadata: Vec::new(),
    assets: Vec::new(),
    flows: vec![FlowSeedFlow {
      id: root_flow,
      nodes: paragraph_ids
        .iter()
        .enumerate()
        .map(|(index, id)| FlowSeedNode {
          record: FlowNodeRecord {
            id: *id,
            kind: FlowNodeKind::Paragraph,
            metadata: b"normal".to_vec(),
            child_flows: Vec::new(),
          },
          text: format!("paragraph-{index}"),
          marks: Vec::new(),
        })
        .collect(),
    }],
  };
  let mut document = FlowDocument::from_seed(DocumentId::new(), ActorId::new(), ReplicaId::new(), &seed).unwrap();
  let target = paragraph_ids[64];
  let at = document.anchor_in_paragraph_utf8(target, 3, Side::Right).unwrap();
  let commit = document.insert_text(Role::Owner, &at, "🙂").unwrap();
  let change = &commit.changes.flow_text_changes[&root_flow];
  let window = document
    .materialize_flow_window(root_flow, change.after_unicode.clone())
    .unwrap();

  assert!(window.nodes.len() <= 2);
  assert!(window.nodes.iter().any(|node| node.record().id == target));
  assert_eq!(document.materialize_flow(root_flow).unwrap().nodes.len(), paragraph_ids.len());
}

#[test]
fn structural_split_window_contains_only_the_affected_neighbors() {
  let mut document = FlowDocument::new(DocumentId::new(), ActorId::new(), ReplicaId::new(), b"normal").unwrap();
  let first = first_paragraph(&document);
  let at = document.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  document.insert_text(Role::Owner, &at, "abcdef").unwrap();
  let second = FlowNodeId::new();
  let split = document
    .split_paragraph(
      Role::Owner,
      &document.anchor_in_paragraph_utf8(first, 3, Side::Right).unwrap(),
      second,
      b"normal",
    )
    .unwrap();
  let change = &split.changes.flow_text_changes[&document.root_flow_id()];
  let window = document
    .materialize_flow_window(document.root_flow_id(), change.after_unicode.clone())
    .unwrap();

  assert_eq!(window.nodes.len(), 2);
  assert_eq!(window.nodes[0].record().id, first);
  assert_eq!(window.nodes[1].record().id, second);
}

#[test]
fn paragraph_local_cursor_resolution_survives_split_and_join() {
  let mut document = FlowDocument::new(DocumentId::new(), ActorId::new(), ReplicaId::new(), b"normal").unwrap();
  let first = first_paragraph(&document);
  let start = document.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  document.insert_text(Role::Owner, &start, "abcd").unwrap();
  let durable = document.anchor_in_paragraph_utf8(first, 3, Side::Right).unwrap();
  let split_at = document.anchor_in_paragraph_utf8(first, 2, Side::Right).unwrap();
  let second = FlowNodeId::new();
  document
    .split_paragraph(Role::Owner, &split_at, second, b"normal")
    .unwrap();
  assert_eq!(
    document.resolve_anchor_in_paragraph_utf8(&durable).unwrap(),
    ResolvedFlowPosition {
      flow_id: document.root_flow_id(),
      node_id: second,
      byte_offset: 1,
    }
  );
  document.join_paragraph(Role::Owner, second).unwrap();
  assert_eq!(
    document.resolve_anchor_in_paragraph_utf8(&durable).unwrap(),
    ResolvedFlowPosition {
      flow_id: document.root_flow_id(),
      node_id: first,
      byte_offset: 3,
    }
  );
}

#[test]
fn cross_paragraph_delete_is_one_valid_replicable_commit() {
  let document_id = DocumentId::new();
  let mut left = FlowDocument::new(document_id, ActorId::new(), ReplicaId::new(), b"normal").unwrap();
  let first = first_paragraph(&left);
  let start = left.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  left.insert_text(Role::Owner, &start, "abcd").unwrap();
  let split_at = left.anchor_in_paragraph_utf8(first, 2, Side::Right).unwrap();
  let second = FlowNodeId::new();
  left
    .split_paragraph(Role::Owner, &split_at, second, b"normal")
    .unwrap();
  let snapshot = left.export_snapshot().unwrap();
  let mut right = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();

  let delete_start = left.anchor_in_paragraph_utf8(first, 1, Side::Right).unwrap();
  let delete_end = left.anchor_in_paragraph_utf8(second, 1, Side::Right).unwrap();
  let commit = left
    .delete_document_range(Role::Owner, &delete_start, &delete_end)
    .unwrap();
  assert!(commit.changes.touched_flows.contains(&left.root_flow_id()));
  right
    .import_update_checked(&commit.update, &FlowImportPolicy::editor_from_peer(left.peer_id()))
    .unwrap();
  assert_eq!(paragraph_text(&left, first), "ad");
  assert_eq!(left.materialize().unwrap(), right.materialize().unwrap());
}

#[test]
fn insert_with_marks_is_one_source_commit() {
  let mut document = FlowDocument::new(DocumentId::new(), ActorId::new(), ReplicaId::new(), b"normal").unwrap();
  let first = first_paragraph(&document);
  let at = document.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  let commit = document
    .insert_text_with_marks(
      Role::Owner,
      &at,
      "styled",
      &[("bold".to_string(), FlowMarkValue::Bool(true))],
    )
    .unwrap();
  assert!(!commit.update.is_empty());
  let materialized = document.materialize().unwrap();
  let FlowNode::Paragraph { marks, .. } = &materialized.flows[&document.root_flow_id()].nodes[0] else {
    panic!("root node is not a paragraph");
  };
  assert_eq!(marks.len(), 1);
  assert_eq!(marks[0].range_utf8, 0.."styled".len());
}

#[test]
fn paragraph_anchors_use_unicode_positions_inside_marked_text() {
  let mut document = FlowDocument::new(DocumentId::new(), ActorId::new(), ReplicaId::new(), b"normal").unwrap();
  let first = first_paragraph(&document);
  let start = document.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  document
    .insert_text_with_marks(
      Role::Owner,
      &start,
      "abcd",
      &[("bold".to_string(), FlowMarkValue::Bool(true))],
    )
    .unwrap();

  for byte_offset in 0..=4 {
    let anchor = document.anchor_in_paragraph_utf8(first, byte_offset, Side::Right).unwrap();
    assert_eq!(document.resolve_anchor_in_paragraph_utf8(&anchor).unwrap().byte_offset, byte_offset);
  }

  let middle = document.anchor_in_paragraph_utf8(first, 2, Side::Right).unwrap();
  document.insert_text(Role::Owner, &middle, "X").unwrap();
  assert_eq!(paragraph_text(&document, first), "abXcd");
}

#[test]
fn exact_plain_insert_does_not_inherit_adjacent_marks() {
  let mut document = FlowDocument::new(DocumentId::new(), ActorId::new(), ReplicaId::new(), b"normal").unwrap();
  let first = first_paragraph(&document);
  let start = document.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  document
    .insert_text_with_marks(
      Role::Owner,
      &start,
      "a",
      &[("bold".to_string(), FlowMarkValue::Bool(true))],
    )
    .unwrap();
  let end = document.anchor_in_paragraph_utf8(first, 1, Side::Right).unwrap();
  document.insert_text(Role::Owner, &end, "b").unwrap();

  let materialized = document.materialize().unwrap();
  let FlowNode::Paragraph { text, marks, .. } = &materialized.flows[&document.root_flow_id()].nodes[0] else {
    panic!("root node is not a paragraph");
  };
  assert_eq!(text, "ab");
  assert_eq!(marks.len(), 1);
  assert_eq!(marks[0].range_utf8, 0..1);
}

#[test]
fn styled_paragraph_fragment_is_one_replayable_source_commit() {
  let document_id = DocumentId::new();
  let actor_id = ActorId::new();
  let mut left = FlowDocument::new(document_id, actor_id, ReplicaId::new(), b"normal").unwrap();
  let first = first_paragraph(&left);
  let start = left.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  left.insert_text(Role::Owner, &start, "tail").unwrap();
  let snapshot = left.export_snapshot().unwrap();
  let mut right = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  let second = FlowNodeId::new();

  let commit = left
    .apply_edits(
      Role::Owner,
      &[FlowEdit::InsertParagraphFragment {
        at: left.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap(),
        first_runs: vec![FlowTextInsert {
          text: "bold".to_string(),
          marks: vec![("bold".to_string(), FlowMarkValue::Bool(true))],
        }],
        additional_paragraphs: vec![FlowParagraphInsert {
          paragraph_id: second,
          metadata: b"heading".to_vec(),
          runs: vec![FlowTextInsert {
            text: "plain".to_string(),
            marks: Vec::new(),
          }],
        }],
      }],
    )
    .unwrap();
  right
    .import_update_checked(&commit.update, &FlowImportPolicy::editor_from_peer(left.peer_id()))
    .unwrap();

  let materialized = left.materialize().unwrap();
  let nodes = &materialized.flows[&left.root_flow_id()].nodes;
  let FlowNode::Paragraph { text, marks, .. } = &nodes[0] else {
    panic!("first node is not a paragraph");
  };
  assert_eq!(text, "bold");
  assert_eq!(marks.len(), 1);
  let FlowNode::Paragraph { text, marks, record } = &nodes[1] else {
    panic!("second node is not a paragraph");
  };
  assert_eq!(record.id, second);
  assert_eq!(record.metadata, b"heading");
  assert_eq!(text, "plaintail");
  assert!(marks.is_empty());
  assert_eq!(materialized, right.materialize().unwrap());
}

#[test]
fn rejects_reserved_tokens_and_unregistered_update_authors() {
  let document_id = DocumentId::new();
  let actor_id = ActorId::new();
  let mut left = FlowDocument::new(document_id, actor_id, ReplicaId::new(), b"normal").unwrap();
  let first = first_paragraph(&left);
  let start = left.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  assert!(left.insert_text(Role::Owner, &start, "\u{FDD0}").is_err());

  let snapshot = left.export_snapshot().unwrap();
  let mut right = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  let update = left.insert_text(Role::Owner, &start, "x").unwrap();
  let wrong_peer = right.peer_id();
  assert!(right
    .import_update_checked(&update.update, &FlowImportPolicy::editor_from_peer(wrong_peer))
    .is_err());
}

#[test]
fn rejects_updates_that_exceed_the_validation_time_budget() {
  let document_id = DocumentId::new();
  let actor_id = ActorId::new();
  let mut left = FlowDocument::new(document_id, actor_id, ReplicaId::new(), b"normal").unwrap();
  let snapshot = left.export_snapshot().unwrap();
  let first = first_paragraph(&left);
  let start = left.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  let update = left.insert_text(Role::Owner, &start, "x").unwrap();
  let mut right = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  let mut policy = FlowImportPolicy::editor_from_peer(left.peer_id());
  policy.limits.max_validation_millis = 0;
  assert!(right.import_update_checked(&update.update, &policy).is_err());
  assert!(matches!(
    &right.materialize_flow(right.root_flow_id()).unwrap().nodes[0],
    FlowNode::Paragraph { text, .. } if text.is_empty()
  ));
}

#[test]
fn rejects_unknown_attached_schema_keys_before_authoritative_import() {
  let document_id = DocumentId::new();
  let attacker = FlowDocument::new(document_id, ActorId::new(), ReplicaId::new(), b"normal").unwrap();
  let snapshot = attacker.export_snapshot().unwrap();
  let mut authority = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  attacker.doc.commit();
  let before = attacker.doc.oplog_vv();
  attacker
    .doc
    .get_map("flowstate_vnext")
    .insert("attacker_state", "hidden")
    .unwrap();
  attacker.doc.commit();
  let update = attacker.doc.export(ExportMode::updates(&before)).unwrap();
  let authority_hash = authority.source_hash().unwrap();
  assert!(authority
    .import_update_checked(&update, &FlowImportPolicy::editor_from_peer(attacker.peer_id()))
    .is_err());
  assert_eq!(authority.source_hash().unwrap(), authority_hash);
}

#[test]
fn full_history_epoch_is_protected_and_survives_snapshot_restart() {
  let document_id = DocumentId::new();
  let attacker = FlowDocument::new(document_id, ActorId::new(), ReplicaId::new(), b"normal").unwrap();
  assert_eq!(attacker.history_policy().unwrap(), FlowHistoryPolicy::full_history());
  assert!(!attacker.history_policy().unwrap().permits_compaction());
  let restarted = FlowDocument::from_snapshot(&attacker.export_snapshot().unwrap(), Some(document_id), ReplicaId::new()).unwrap();
  assert_eq!(restarted.history_policy().unwrap(), FlowHistoryPolicy::full_history());

  let snapshot = attacker.export_snapshot().unwrap();
  let mut authority = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  attacker.doc.commit();
  let before = attacker.doc.oplog_vv();
  attacker
    .doc
    .get_map("flowstate_vnext")
    .insert("history_epoch", 1_i64)
    .unwrap();
  attacker.doc.commit();
  let update = attacker.doc.export(ExportMode::updates(&before)).unwrap();
  assert!(authority
    .import_update_checked(&update, &FlowImportPolicy::editor_from_peer(attacker.peer_id()))
    .is_err());
  assert_eq!(authority.history_policy().unwrap(), FlowHistoryPolicy::full_history());
}

#[test]
fn rejects_updates_that_exceed_operation_or_history_quotas() {
  let document_id = DocumentId::new();
  let actor_id = ActorId::new();
  let mut attacker = FlowDocument::new(document_id, actor_id, ReplicaId::new(), b"normal").unwrap();
  let snapshot = attacker.export_snapshot().unwrap();
  let first = first_paragraph(&attacker);
  let at = attacker.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  let update = attacker.insert_text(Role::Owner, &at, "quota").unwrap();

  let mut operation_limited = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  let mut policy = FlowImportPolicy::editor_from_peer(attacker.peer_id());
  policy.limits.max_update_ops = 0;
  assert!(operation_limited.import_update_checked(&update.update, &policy).is_err());

  let mut total_limited = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  let mut policy = FlowImportPolicy::editor_from_peer(attacker.peer_id());
  policy.limits.max_total_ops = total_limited.doc.len_ops();
  assert!(total_limited.import_update_checked(&update.update, &policy).is_err());

  let limits = FlowSourceLimits {
    max_total_changes: 0,
    ..FlowSourceLimits::default()
  };
  assert!(attacker.validate(&limits).is_err());
}

#[test]
fn child_flow_is_materialized_only_when_reachable() {
  let mut document = FlowDocument::new(DocumentId::new(), ActorId::new(), ReplicaId::new(), b"normal").unwrap();
  let first = first_paragraph(&document);
  let at = document.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  let object_id = FlowNodeId::new();
  let child_flow = FlowId::new();
  let child_paragraph = FlowNodeId::new();
  document
    .create_child_flow_object(
      Role::Owner,
      &at,
      object_id,
      b"table",
      child_flow,
      child_paragraph,
      b"normal",
    )
    .unwrap();
  let materialized = document.materialize().unwrap();
  assert!(materialized.flows.contains_key(&document.root_flow_id()));
  assert!(materialized.flows.contains_key(&child_flow));
}

#[test]
fn seed_round_trips_document_metadata_nodes_and_inline_marks() {
  let root_flow = FlowId::new();
  let paragraph = FlowNodeId::new();
  let seed = FlowDocumentSeed {
    root_flow_id: root_flow,
    document_metadata: b"theme".to_vec(),
    assets: Vec::new(),
    flows: vec![FlowSeedFlow {
      id: root_flow,
      nodes: vec![FlowSeedNode {
        record: FlowNodeRecord {
          id: paragraph,
          kind: FlowNodeKind::Paragraph,
          metadata: b"heading".to_vec(),
          child_flows: Vec::new(),
        },
        text: "hello".to_string(),
        marks: vec![FlowInlineMark {
          range_utf8: 0..5,
          key: "bold".to_string(),
          value: FlowMarkValue::Bool(true),
        }],
      }],
    }],
  };
  let document = FlowDocument::from_seed(DocumentId::new(), ActorId::new(), ReplicaId::new(), &seed).unwrap();
  let materialized = document.materialize().unwrap();
  assert_eq!(materialized.document_metadata, b"theme");
  let FlowNode::Paragraph { text, marks, .. } = &materialized.flows[&root_flow].nodes[0] else {
    panic!("seed paragraph materialized as object");
  };
  assert_eq!(text, "hello");
  assert_eq!(marks.len(), 1);
  assert_eq!(document.source_hash().unwrap(), document.source_hash().unwrap());
}

#[test]
fn asset_references_round_trip_and_hostile_asset_updates_are_rejected_before_import() {
  use loro::{Container, ValueOrContainer};

  let document_id = DocumentId::new();
  let mut left = FlowDocument::new(document_id, ActorId::new(), ReplicaId::new(), b"normal").unwrap();
  let snapshot = left.export_snapshot().unwrap();
  let mut right = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  let asset = FlowAssetReference {
    id: FlowAssetId(Uuid::new_v4()),
    blake3_hash: [7; 32],
    byte_len: 1234,
    mime_type: "image/png".to_string(),
    original_name: Some("figure.png".to_string()),
  };
  let commit = left
    .apply_edits(Role::Owner, &[FlowEdit::PutAssetReference { asset: asset.clone() }])
    .unwrap();
  right
    .import_update_checked(&commit.update, &FlowImportPolicy::editor_from_peer(left.peer_id()))
    .unwrap();
  assert_eq!(left.asset_references().unwrap().get(&asset.id), Some(&asset));
  assert_eq!(left.asset_references().unwrap(), right.asset_references().unwrap());
  let restarted = FlowDocument::from_snapshot(&right.export_snapshot().unwrap(), Some(document_id), ReplicaId::new()).unwrap();
  assert_eq!(restarted.asset_references().unwrap().get(&asset.id), Some(&asset));

  let attacker = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  let mut authority = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  attacker.doc.commit();
  let before = attacker.doc.oplog_vv();
  let root = attacker.doc.get_map("flowstate_vnext");
  let assets = match root.get("assets") {
    Some(ValueOrContainer::Container(Container::Map(assets))) => assets,
    _ => panic!("vNext asset map missing"),
  };
  let oversized = FlowAssetReference {
    id: FlowAssetId(Uuid::new_v4()),
    blake3_hash: [9; 32],
    byte_len: 1,
    mime_type: "x".repeat(FlowSourceLimits::default().max_asset_reference_bytes + 1),
    original_name: None,
  };
  assets
    .insert(
      oversized.id.0.simple().to_string().as_str(),
      postcard::to_stdvec(&oversized).unwrap().as_slice(),
    )
    .unwrap();
  attacker.doc.commit();
  let hostile_update = attacker.doc.export(ExportMode::updates(&before)).unwrap();
  let authority_hash = authority.source_hash().unwrap();
  assert!(authority
    .import_update_checked(&hostile_update, &FlowImportPolicy::editor_from_peer(attacker.peer_id()))
    .is_err());
  assert_eq!(authority.source_hash().unwrap(), authority_hash);
}

#[test]
fn structural_tokens_do_not_inherit_marks_from_preceding_seed_text() {
  let root_flow = FlowId::new();
  let first = FlowNodeId::new();
  let second = FlowNodeId::new();
  let paragraph = |id, text: &str, marks| FlowSeedNode {
    record: FlowNodeRecord {
      id,
      kind: FlowNodeKind::Paragraph,
      metadata: b"normal".to_vec(),
      child_flows: Vec::new(),
    },
    text: text.to_string(),
    marks,
  };
  let seed = FlowDocumentSeed {
    root_flow_id: root_flow,
    document_metadata: Vec::new(),
    assets: Vec::new(),
    flows: vec![FlowSeedFlow {
      id: root_flow,
      nodes: vec![
        paragraph(
          first,
          "styled",
          vec![FlowInlineMark {
            range_utf8: 0..6,
            key: "bold".to_string(),
            value: FlowMarkValue::Bool(true),
          }],
        ),
        paragraph(second, "plain", Vec::new()),
      ],
    }],
  };

  let document = FlowDocument::from_seed(DocumentId::new(), ActorId::new(), ReplicaId::new(), &seed).unwrap();
  let materialized = document.materialize().unwrap();
  let nodes = &materialized.flows[&root_flow].nodes;
  let FlowNode::Paragraph { marks, .. } = &nodes[0] else {
    panic!("first seed node materialized as object");
  };
  assert_eq!(marks.len(), 1);
  let FlowNode::Paragraph { marks, .. } = &nodes[1] else {
    panic!("second seed node materialized as object");
  };
  assert!(marks.is_empty());
}

#[test]
fn loro_undo_emits_a_replicable_source_update() {
  let document_id = DocumentId::new();
  let mut left = FlowDocument::new(document_id, ActorId::new(), ReplicaId::new(), b"normal").unwrap();
  let first = first_paragraph(&left);
  let snapshot = left.export_snapshot().unwrap();
  let mut right = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  let mut undo = left.new_undo_manager();
  let at = left.anchor_in_paragraph_utf8(first, 0, Side::Right).unwrap();
  let insert = left.insert_text(Role::Owner, &at, "undo me").unwrap();
  right
    .import_update_checked(&insert.update, &FlowImportPolicy::editor_from_peer(left.peer_id()))
    .unwrap();
  let undo_commit = left.undo(Role::Owner, &mut undo).unwrap().unwrap();
  right
    .import_update_checked(&undo_commit.update, &FlowImportPolicy::editor_from_peer(left.peer_id()))
    .unwrap();
  assert_eq!(paragraph_text(&left, first), "");
  assert_eq!(left.materialize().unwrap(), right.materialize().unwrap());
}

#[test]
fn protected_root_mutation_is_rejected_before_authoritative_import() {
  let document_id = DocumentId::new();
  let attacker = FlowDocument::new(document_id, ActorId::new(), ReplicaId::new(), b"normal").unwrap();
  let snapshot = attacker.export_snapshot().unwrap();
  let mut authority = FlowDocument::from_snapshot(&snapshot, Some(document_id), ReplicaId::new()).unwrap();
  attacker.doc.commit();
  let before = attacker.doc.oplog_vv();
  attacker
    .doc
    .get_map("flowstate_vnext")
    .insert("document_id", DocumentId::new().0.as_bytes())
    .unwrap();
  attacker.doc.commit();
  let update = attacker.doc.export(ExportMode::updates(&before)).unwrap();
  let authority_hash = authority.source_hash().unwrap();
  assert!(authority
    .import_update_checked(&update, &FlowImportPolicy::editor_from_peer(attacker.peer_id()))
    .is_err());
  assert_eq!(authority.source_hash().unwrap(), authority_hash);
}

#[test]
fn native_envelope_dual_reads_vnext_flow_snapshot() {
  let document_id = DocumentId::new();
  let source = FlowDocument::new(document_id, ActorId::new(), ReplicaId::new(), b"normal").unwrap();
  let snapshot = source.export_snapshot().unwrap();
  let mut input = crate::NativeFileInput::new(crate::FormatKind::Db8, b"projection".to_vec());
  input.collab_schema = FLOW_SOURCE_SCHEMA_VERSION;
  input.document_id = document_id;
  input.source_snapshot = Some(snapshot.clone());

  let bytes = crate::encode_native_file(input).unwrap();
  let decoded = crate::decode_native_file(&bytes, crate::FormatKind::Db8).unwrap();
  assert_eq!(decoded.manifest.collab_schema, FLOW_SOURCE_SCHEMA_VERSION);
  assert_eq!(decoded.snapshot, snapshot);
  assert_eq!(decoded.projection_cache, b"projection");
}

#[test]
fn rich_object_insert_delete_is_one_exact_identity_preserving_transaction() {
  let document_id = DocumentId::new();
  let mut source = FlowDocument::new(document_id, ActorId::new(), ReplicaId::new(), b"normal").unwrap();
  let object_id = FlowNodeId::new();
  let child_flow_id = FlowId::new();
  let child_paragraph_id = FlowNodeId::new();
  let at = source
    .anchor_at_node_index(source.root_flow_id(), 1, Side::Left)
    .unwrap();
  let insert = source
    .apply_edits(
      Role::Owner,
      &[FlowEdit::InsertObject {
        at,
        object: FlowSeedNode {
          record: FlowNodeRecord {
            id: object_id,
            kind: FlowNodeKind::Object,
            metadata: b"object".to_vec(),
            child_flows: vec![child_flow_id],
          },
          text: String::new(),
          marks: Vec::new(),
        },
        child_flows: vec![FlowSeedFlow {
          id: child_flow_id,
          nodes: vec![FlowSeedNode {
            record: FlowNodeRecord {
              id: child_paragraph_id,
              kind: FlowNodeKind::Paragraph,
              metadata: b"normal".to_vec(),
              child_flows: Vec::new(),
            },
            text: "caption".to_string(),
            marks: Vec::new(),
          }],
        }],
      }],
    )
    .unwrap();
  assert!(!insert.update.is_empty());
  let root = source.materialize_flow(source.root_flow_id()).unwrap();
  assert!(matches!(&root.nodes[1], FlowNode::Object { record } if record.id == object_id));
  assert_eq!(paragraph_text(&source, child_paragraph_id), "caption");

  let delete = source
    .apply_edits(Role::Owner, &[FlowEdit::DeleteObject { object_id }])
    .unwrap();
  assert!(!delete.update.is_empty());
  assert_eq!(source.materialize_flow(source.root_flow_id()).unwrap().nodes.len(), 1);
}
