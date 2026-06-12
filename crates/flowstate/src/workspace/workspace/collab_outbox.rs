use flowstate_collab::{FLOW_SOURCE_SCHEMA_VERSION, FlowCommit};

use super::*;

#[derive(Clone, Debug)]
pub(super) struct Db8OutboxReceipt {
  pub outbox: Arc<Mutex<DurableFlowOutbox>>,
  pub key: FlowOutboxKey,
}

#[derive(Debug)]
struct WorkspaceDb8CommitOutbox {
  outbox: Arc<Mutex<DurableFlowOutbox>>,
  document_id: CollabDocumentId,
  actor_id: ActorId,
  replica_id: ReplicaId,
  peer_id: u64,
}

impl Db8CommitOutbox for WorkspaceDb8CommitOutbox {
  fn accept(&mut self, commit: &FlowCommit) -> std::io::Result<()> {
    let entry = FlowOutboxEntry {
      document_id: self.document_id,
      actor_id: self.actor_id,
      replica_id: self.replica_id,
      peer_id: self.peer_id,
      schema_version: FLOW_SOURCE_SCHEMA_VERSION,
      update: commit.update.clone(),
      base_frontier: commit.base_frontier.clone(),
      resulting_frontier: commit.resulting_frontier.clone(),
      hash: flowstate_collab::blake3_hash(&commit.update),
    };
    self
      .outbox
      .lock()
      .map_err(|_| std::io::Error::other("DB8 durable outbox lock is poisoned"))?
      .accept(entry)?;
    Ok(())
  }

  fn compact(&mut self) -> std::io::Result<()> {
    let mut outbox = self
      .outbox
      .lock()
      .map_err(|_| std::io::Error::other("DB8 durable outbox lock is poisoned"))?;
    outbox.compact()
  }
}

impl Workspace {
  pub(super) fn attach_db8_durable_outbox(&mut self, panel_id: Uuid, cx: &mut Context<Self>) -> anyhow::Result<()> {
    anyhow::ensure!(
      self.db8_replica_leases.contains_key(&panel_id),
      "DB8 replica lineage lease is unavailable; this document is source-read-only"
    );
    let authority = self
      .db8_authorities
      .get(&panel_id)
      .cloned()
      .ok_or_else(|| anyhow::anyhow!("DB8 authority is unavailable for durable outbox attachment"))?;
    let (document_id, peer_id) = {
      let authority = authority.borrow();
      (authority.controller().source().document_id(), authority.peer_id())
    };
    let path = flowstate_data_dir()
      .join("collaboration-outbox")
      .join(document_id.0.to_string())
      .join(format!("{}.flow-outbox", self.local_replica_id.0));
    let outbox = if let Some(outbox) = self.db8_outboxes.get(&panel_id) {
      outbox.clone()
    } else {
      Arc::new(Mutex::new(DurableFlowOutbox::open(path)?))
    };
    let recovered = outbox
      .lock()
      .map_err(|_| anyhow::anyhow!("DB8 durable outbox lock is poisoned"))?
      .pending()
      .iter()
      .cloned()
      .collect::<Vec<_>>();
    for entry in &recovered {
      anyhow::ensure!(entry.document_id == document_id, "durable outbox document identity mismatch");
      anyhow::ensure!(entry.actor_id == self.local_actor_id, "durable outbox actor identity mismatch");
      anyhow::ensure!(entry.replica_id == self.local_replica_id, "durable outbox replica identity mismatch");
      anyhow::ensure!(entry.peer_id == peer_id, "durable outbox Loro peer identity mismatch");
    }
    let recovered_updates = recovered.iter().map(|entry| entry.update.clone()).collect::<Vec<_>>();
    if let Err(error) = authority.borrow_mut().replay_retained_updates(peer_id, &recovered_updates) {
      authority
        .borrow_mut()
        .block_local_edits(format!("durable outbox replay failed: {error}"));
      return Err(error.into());
    }
    authority.borrow_mut().set_commit_outbox(Box::new(WorkspaceDb8CommitOutbox {
        outbox: outbox.clone(),
        document_id,
        actor_id: self.local_actor_id,
        replica_id: self.local_replica_id,
        peer_id,
      }));
    self.db8_outboxes.insert(panel_id, outbox.clone());
    if !recovered.is_empty() {
      let projection = authority.borrow().controller().projection().clone();
      let editor = self
        .document_editor_for_panel(panel_id, cx)
        .ok_or_else(|| anyhow::anyhow!("collaboration DB8 panel is no longer open"))?;
      editor.update(cx, |editor, cx| editor.replace_document_from_collaboration(projection, cx));
      self.collaboration_last_frontier = authority.borrow().frontier()?;
    }
    let recovered_count = recovered.len();
    for entry in recovered {
      let already_queued = self.collaboration_pending_updates.iter().any(|pending| {
        matches!(
          pending,
          PendingCollaborationUpdate::FlowUpdate {
            hash,
            base_frontier,
            resulting_frontier,
            ..
          } if *hash == entry.hash && *base_frontier == entry.base_frontier && *resulting_frontier == entry.resulting_frontier
        )
      });
      if already_queued {
        continue;
      }
      let receipt = Db8OutboxReceipt {
        key: entry.key(),
        outbox: outbox.clone(),
      };
      self
        .collaboration_pending_updates
        .push_back(PendingCollaborationUpdate::FlowUpdate {
          peer_id: entry.peer_id,
          update: entry.update,
          base_frontier: entry.base_frontier,
          resulting_frontier: entry.resulting_frontier,
          hash: entry.hash,
          outbox_receipt: Some(Box::new(receipt)),
        });
    }
    if recovered_count > Self::MAX_PENDING_COLLABORATION_UPDATES {
      self.collaboration.last_error = Some(format!(
        "Replaying {recovered_count} exact DB8 updates recovered from the durable outbox; live edits remain journaled until replay catches up."
      ));
    }
    Ok(())
  }

  pub(super) fn detach_db8_durable_outbox(&mut self, panel_id: Uuid) {
    if let Some(authority) = self.db8_authorities.get(&panel_id) {
      authority.borrow_mut().clear_commit_outbox();
    }
    self.db8_outboxes.remove(&panel_id);
  }
}

pub(super) fn acknowledge_db8_outbox(update: &PendingCollaborationUpdate) -> anyhow::Result<()> {
  let PendingCollaborationUpdate::FlowUpdate {
    outbox_receipt: Some(receipt),
    ..
  } = update
  else {
    return Ok(());
  };
  receipt
    .outbox
    .lock()
    .map_err(|_| anyhow::anyhow!("DB8 durable outbox lock is poisoned"))?
    .acknowledge(&receipt.key)?;
  Ok(())
}
