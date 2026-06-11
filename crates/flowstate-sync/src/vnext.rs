use std::fmt;
use std::sync::mpsc;

use flowstate_collab::{
  ActorId, CollabError, CollabResult, FlowAssetId, FlowAssetReference, FlowDocument, FlowImportOutcome, FlowImportPolicy, ReplicaId, Role,
};
use loro::PeerID;
use std::collections::BTreeMap;

type FlowAuthorityJob = Box<dyn FnOnce(&mut FlowDocument) + Send + 'static>;

/// Serializes access to the host's authoritative vNext Loro source.
///
/// The workspace and transport intentionally keep separate `FlowDocument`
/// instances. The workspace owns projection and UI state; this authority owns
/// validation, repair snapshots, and causal transport state.
#[derive(Clone)]
pub struct FlowDocumentAuthority {
  sender: mpsc::Sender<FlowAuthorityJob>,
}

impl fmt::Debug for FlowDocumentAuthority {
  fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
    f.debug_struct("FlowDocumentAuthority").finish_non_exhaustive()
  }
}

impl FlowDocumentAuthority {
  pub fn from_snapshot(snapshot: &[u8], expected_document_id: flowstate_collab::DocumentId) -> CollabResult<Self> {
    let document = FlowDocument::from_snapshot(snapshot, Some(expected_document_id), ReplicaId::new())?;
    Ok(Self::new(document))
  }

  pub fn new(mut document: FlowDocument) -> Self {
    let (sender, receiver) = mpsc::channel::<FlowAuthorityJob>();
    std::thread::Builder::new()
      .name("flowstate-vnext-authority".to_string())
      .spawn(move || {
        while let Ok(job) = receiver.recv() {
          job(&mut document);
        }
      })
      .expect("failed to start Flowstate vNext authority thread");
    Self { sender }
  }

  fn call<R, F>(&self, operation: F) -> CollabResult<R>
  where
    R: Send + 'static,
    F: FnOnce(&mut FlowDocument) -> CollabResult<R> + Send + 'static,
  {
    let (reply_tx, reply_rx) = mpsc::sync_channel(1);
    self
      .sender
      .send(Box::new(move |document| {
        let _ = reply_tx.send(operation(document));
      }))
      .map_err(|_| CollabError::Loro("Flowstate vNext authority thread is closed".to_string()))?;
    reply_rx
      .recv()
      .map_err(|_| CollabError::Loro("Flowstate vNext authority reply channel is closed".to_string()))?
  }

  pub fn frontier(&self) -> CollabResult<Vec<u8>> {
    self.call(|document| document.frontier())
  }

  pub fn export_snapshot_and_frontier(&self) -> CollabResult<(Vec<u8>, Vec<u8>)> {
    self.call(|document| Ok((document.export_snapshot()?, document.frontier()?)))
  }

  pub fn export_update_since(&self, frontier: Vec<u8>) -> CollabResult<Vec<u8>> {
    self.call(move |document| document.export_update_since(&frontier))
  }

  pub fn asset_references(&self) -> CollabResult<BTreeMap<FlowAssetId, FlowAssetReference>> {
    self.call(|document| document.asset_references())
  }

  pub fn created_by_actor(&self) -> CollabResult<ActorId> {
    self.call(|document| document.created_by_actor())
  }

  pub fn import_update_checked(&self, role: Role, peer_id: PeerID, bytes: Vec<u8>) -> CollabResult<FlowImportOutcome> {
    self.call(move |document| document.import_update_checked(&bytes, &FlowImportPolicy::from_peer(role, peer_id)))
  }

  pub fn import_update_checked_at_frontiers(
    &self,
    role: Role,
    peer_id: PeerID,
    bytes: Vec<u8>,
    base_frontier: Vec<u8>,
    resulting_frontier: Vec<u8>,
  ) -> CollabResult<FlowImportOutcome> {
    self.call(move |document| {
      // An ordered replica/outbox lineage may only build on source history the
      // authority already knows. After import, the sender's declared result
      // must be a causal prefix of the authoritative merge result.
      document.export_update_since(&base_frontier)?;
      let outcome = document.import_update_checked(&bytes, &FlowImportPolicy::from_peer(role, peer_id))?;
      document.export_update_since(&resulting_frontier)?;
      Ok(outcome)
    })
  }
}

#[cfg(test)]
mod tests {
  use flowstate_collab::{ActorId, DocumentId, FlowNode, ReplicaId};
  use loro::cursor::Side;

  use super::*;

  #[test]
  fn authority_accepts_exact_update_from_registered_peer() {
    let document_id = DocumentId::new();
    let mut source = FlowDocument::new(document_id, ActorId::new(), ReplicaId::new(), b"paragraph").unwrap();
    let snapshot = source.export_snapshot().unwrap();
    let peer_id = source.peer_id();
    let materialized = source.materialize().unwrap();
    let paragraph_id = match &materialized.flows[&materialized.root_flow_id].nodes[0] {
      FlowNode::Paragraph { record, .. } => record.id,
      FlowNode::Object { .. } => panic!("root flow did not begin with a paragraph"),
    };
    let at = source.anchor_in_paragraph_utf8(paragraph_id, 0, Side::Right).unwrap();
    let commit = source.insert_text(Role::Owner, &at, "hello").unwrap();

    let authority = FlowDocumentAuthority::from_snapshot(&snapshot, document_id).unwrap();
    let outcome = authority
      .import_update_checked(Role::Editor, peer_id, commit.update)
      .unwrap();
    assert_eq!(outcome.frontier, source.frontier().unwrap());
    let (host_snapshot, _) = authority.export_snapshot_and_frontier().unwrap();
    let host = FlowDocument::from_snapshot(&host_snapshot, Some(document_id), ReplicaId::new()).unwrap();
    let root = host.materialize_flow(host.root_flow_id()).unwrap();
    assert!(matches!(&root.nodes[0], FlowNode::Paragraph { text, .. } if text == "hello"));
  }

  #[test]
  fn authority_rejects_update_labeled_as_another_peer() {
    let document_id = DocumentId::new();
    let mut source = FlowDocument::new(document_id, ActorId::new(), ReplicaId::new(), b"paragraph").unwrap();
    let snapshot = source.export_snapshot().unwrap();
    let materialized = source.materialize().unwrap();
    let paragraph_id = materialized.flows[&materialized.root_flow_id].nodes[0].record().id;
    let at = source.anchor_in_paragraph_utf8(paragraph_id, 0, Side::Right).unwrap();
    let commit = source.insert_text(Role::Owner, &at, "no").unwrap();
    let wrong_peer = source.peer_id().wrapping_add(1).max(1);

    let authority = FlowDocumentAuthority::from_snapshot(&snapshot, document_id).unwrap();
    assert!(
      authority
        .import_update_checked(Role::Editor, wrong_peer, commit.update)
        .is_err()
    );
  }
}
