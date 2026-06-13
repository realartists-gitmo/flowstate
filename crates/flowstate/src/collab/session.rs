use std::{
  cmp::Ordering,
  collections::HashSet,
  rc::Rc,
  sync::{Arc, Mutex},
  time::Instant,
};

use anyhow::{Context as _, Result, anyhow, bail};
use flowstate_collab::{
  SessionId,
  binding::DocBinding,
  local_apply::LocalApplier,
  net::{NetCommand, PublishPayload, runtime::CommandSender, direct::{DirectServeRequest, DirectSessionHandler}},
  presence::{PresenceState, PresenceStore},
  projection,
  proto_direct::AssetBytes,
  proto_gossip::GossipMsg,
  remote_apply::RemoteApplier,
  schema,
};
use gpui::{Context, Entity, Subscription};
use loro::{ExportMode, LoroDoc, Subscription as LoroSubscription, VersionVector, event::Subscriber};
use uuid::Uuid;

use crate::app_settings::load_document_theme;
use crate::rich_text_element::{AssetId, CollabPatch, Document, EditorEvent, RichTextEditor, UndoRedirect};

#[derive(Clone, Debug)]
pub enum SessionPhase {
  Creating,
  Joining(JoinStage),
  Attached(Attachment),
  Detached(DetachReason),
}

#[derive(Clone, Debug)]
pub struct Attachment {
  pub connectivity: Connectivity,
  pub peers_present: usize,
}

#[derive(Clone, Debug)]
pub enum Connectivity {
  Online,
  Offline { since: Instant, retries: u32 },
}

#[derive(Clone, Debug)]
pub enum JoinStage {
  Resolving,
  Subscribing,
  FetchingSnapshot { got: u64, total: Option<u64> },
  Building,
}

#[derive(Clone, Debug)]
pub enum DetachReason {
  UserLeft,
  JoinFailed(String),
  Fatal(String),
}

pub struct JoinedDocument {
  pub session: SessionId,
  pub title: String,
  pub document: Document,
}

pub struct CollabSession {
  session: SessionId,
  title: String,
  phase: SessionPhase,
  doc: Option<LoroDoc>,
  binding: Option<DocBinding>,
  editor: Option<Entity<RichTextEditor>>,
  panel_id: Option<Uuid>,
  pending_remote_patches: Vec<CollabPatch>,
  pending_remote_updates: Vec<Vec<u8>>,
  presence: Option<PresenceStore>,
  net_tx: CommandSender,
  direct_tx: async_channel::Sender<DirectServeRequest>,
  direct_rx: async_channel::Receiver<DirectServeRequest>,
  editor_subscriptions: Vec<Subscription>,
  loro_subscriptions: Vec<LoroSubscription>,
  neighbors: HashSet<flowstate_collab::ids::PeerId>,
  update_pull_in_flight: bool,
  direct_pump_started: bool,
  local_update_publish_attached: bool,
}

impl CollabSession {
  pub fn from_local_document(
    session: SessionId,
    panel_id: Uuid,
    editor: Entity<RichTextEditor>,
    title: String,
    document: Document,
    net_tx: CommandSender,
  ) -> Result<Self> {
    let doc = schema::new_configured_doc();
    projection::populate_from_document(&doc, session, &title, &document)?;
    let binding = DocBinding::build(&doc, &document)?;
    let (direct_tx, direct_rx) = async_channel::unbounded();

    Ok(Self {
      session,
      title,
      phase: SessionPhase::Creating,
      doc: Some(doc),
      binding: Some(binding),
      editor: Some(editor),
      panel_id: Some(panel_id),
      pending_remote_patches: Vec::new(),
      pending_remote_updates: Vec::new(),
      presence: None,
      net_tx,
      direct_tx,
      direct_rx,
      editor_subscriptions: Vec::new(),
      loro_subscriptions: Vec::new(),
      neighbors: HashSet::new(),
      update_pull_in_flight: false,
      direct_pump_started: false,
      local_update_publish_attached: false,
    })
  }

  pub fn joining(session: SessionId, title: String, net_tx: CommandSender) -> Self {
    let (direct_tx, direct_rx) = async_channel::unbounded();
    Self {
      session,
      title,
      phase: SessionPhase::Joining(JoinStage::Resolving),
      doc: None,
      binding: None,
      editor: None,
      panel_id: None,
      pending_remote_patches: Vec::new(),
      pending_remote_updates: Vec::new(),
      presence: None,
      net_tx,
      direct_tx,
      direct_rx,
      editor_subscriptions: Vec::new(),
      loro_subscriptions: Vec::new(),
      neighbors: HashSet::new(),
      update_pull_in_flight: false,
      direct_pump_started: false,
      local_update_publish_attached: false,
    }
  }

  pub fn session_id(&self) -> SessionId {
    self.session
  }

  pub fn panel_id(&self) -> Option<Uuid> {
    self.panel_id
  }

  pub fn title(&self) -> &str {
    &self.title
  }

  pub fn phase(&self) -> &SessionPhase {
    &self.phase
  }

  pub fn direct_handler(&self) -> DirectSessionHandler {
    DirectSessionHandler::new(self.direct_tx.clone())
  }

  pub fn set_join_stage(&mut self, stage: JoinStage, cx: &mut Context<Self>) {
    self.phase = SessionPhase::Joining(stage);
    cx.notify();
  }

  pub fn begin_join_bootstrap(
    &mut self,
    inviter: flowstate_collab::ids::PeerId,
    cx: &mut Context<Self>,
  ) -> async_channel::Receiver<Result<JoinedDocument>> {
    let (result_tx, result_rx) = async_channel::bounded(1);
    self.phase = SessionPhase::Joining(JoinStage::FetchingSnapshot { got: 0, total: None });
    cx.notify();

    let (reply_tx, reply_rx) = async_channel::bounded(1);
    if let Err(error) = self.net_tx.try_send(NetCommand::PullSnapshot {
      session: self.session,
      from: inviter,
      reply: reply_tx,
    }) {
      let detail = format!("requesting collaboration snapshot failed: {error}");
      self.detach(DetachReason::JoinFailed(detail.clone()), cx);
      let _ = result_tx.try_send(Err(anyhow!(detail)));
      return result_rx;
    }

    cx.spawn(async move |session, cx| {
      let joined = match reply_rx.recv().await {
        Ok(Ok(bytes)) => session
          .update(cx, |session, cx| match session.finish_join_snapshot(&bytes, cx) {
            Ok(joined) => Ok(joined),
            Err(error) => {
              let detail = format!("building collaboration document failed: {error:#}");
              session.detach(DetachReason::JoinFailed(detail), cx);
              Err(error.context("building collaboration document failed"))
            },
          })
          .unwrap_or_else(|error| Err(anyhow!("collaboration join session disappeared: {error}"))),
        Ok(Err(error)) => {
          let detail = format!("pulling collaboration snapshot failed: {error:#}");
          let _ = session.update(cx, |session, cx| {
            session.detach(DetachReason::JoinFailed(detail.clone()), cx);
          });
          Err(anyhow!(detail))
        },
        Err(error) => {
          let detail = format!("collaboration snapshot reply channel closed: {error}");
          let _ = session.update(cx, |session, cx| {
            session.detach(DetachReason::JoinFailed(detail.clone()), cx);
          });
          Err(anyhow!(detail))
        },
      };
      let _ = result_tx.send(joined).await;
    })
    .detach();

    result_rx
  }

  pub fn attach_joined_editor(
    &mut self,
    panel_id: Uuid,
    editor: Entity<RichTextEditor>,
    cx: &mut Context<Self>,
  ) -> Result<()> {
    if self.doc.is_none() || self.binding.is_none() {
      bail!("collaboration snapshot has not finished loading");
    }

    self.panel_id = Some(panel_id);
    self.editor = Some(editor);
    self.attach(cx);

    let pending_updates = std::mem::take(&mut self.pending_remote_updates);
    for update in pending_updates {
      self.import_update_bytes(&update, cx)?;
    }
    self.flush_pending_remote_patches(cx);
    self.publish_digest();
    cx.notify();
    Ok(())
  }

  pub fn attach(&mut self, cx: &mut Context<Self>) {
    self.attach_editor_hooks(cx);
    self.attach_loro_publish_hook();
    self.attach_direct_request_pump(cx);
  }

  pub fn establish_local_peer(&mut self, peer: &flowstate_collab::ids::PeerId, cx: &mut Context<Self>) {
    if self.presence.is_none() {
      let presence = PresenceStore::new(peer);
      let session = self.session;
      let net_tx = self.net_tx.clone();
      self.loro_subscriptions.push(presence.subscribe_local_updates(move |bytes| {
        let _ = net_tx.try_send(NetCommand::Publish {
          session,
          payload: PublishPayload::Presence(bytes.clone()),
        });
        true
      }));
      self.presence = Some(presence);
    }
    self.refresh_own_presence(cx);
    self.phase = SessionPhase::Attached(Attachment {
      connectivity: Connectivity::Online,
      peers_present: self.peers_present(),
    });
    self.publish_presence_snapshot();
    self.publish_digest();
    cx.notify();
  }

  fn finish_join_snapshot(&mut self, snapshot: &[u8], cx: &mut Context<Self>) -> Result<JoinedDocument> {
    self.phase = SessionPhase::Joining(JoinStage::Building);
    cx.notify();

    let doc = schema::new_configured_doc();
    doc
      .import_with(snapshot, "remote")
      .context("importing collaboration snapshot failed")?;
    projection::verify_lineage(&doc, self.session)?;
    let document = projection::document_from_loro(&doc, load_document_theme())?;
    let binding = DocBinding::build(&doc, &document)?;

    self.doc = Some(doc);
    self.binding = Some(binding);
    Ok(JoinedDocument {
      session: self.session,
      title: self.title.clone(),
      document,
    })
  }

  pub fn detach(&mut self, reason: DetachReason, cx: &mut Context<Self>) -> bool {
    if matches!(self.phase, SessionPhase::Detached(_)) {
      return false;
    }

    if let Some(presence) = &self.presence {
      presence.delete_self();
      self.publish_presence_bytes(presence.encode_self());
    }
    let _ = self.net_tx.try_send(NetCommand::LeaveSession { session: self.session });
    self.flush_pending_remote_patches(cx);

    if let Some(editor) = self.editor.clone() {
      editor.update(cx, |editor, cx| {
        editor.set_collab_undo_redirect(None);
        editor.set_collab_capture(false);
        let _ = editor.take_pending_collab_edits();
        editor.set_external_carets(Vec::new(), cx);
      });
    }

    self.editor_subscriptions.clear();
    self.loro_subscriptions.clear();
    self.presence = None;
    self.binding = None;
    self.doc = None;
    self.pending_remote_patches.clear();
    self.pending_remote_updates.clear();
    self.neighbors.clear();
    self.local_update_publish_attached = false;
    self.phase = SessionPhase::Detached(reason);
    cx.notify();
    true
  }

  pub fn flush_local_edits(&mut self, editor: Entity<RichTextEditor>, cx: &mut Context<Self>) -> Result<()> {
    if matches!(self.phase, SessionPhase::Detached(_) | SessionPhase::Joining(_)) {
      return Ok(());
    }

    let document = editor.read(cx).document().clone();
    let edits = editor.update(cx, |editor, _| editor.take_pending_collab_edits());
    let Some(doc) = &self.doc else {
      return Ok(());
    };
    let Some(binding) = &mut self.binding else {
      return Ok(());
    };

    for edit in edits {
      if edit.operations.is_empty() {
        continue;
      }
      LocalApplier { doc, binding }.apply(&document, &edit.operations)?;
    }
    Ok(())
  }

  pub fn handle_gossip(&mut self, from: flowstate_collab::ids::PeerId, msg: GossipMsg, cx: &mut Context<Self>) {
    let result = match msg {
      GossipMsg::Update(bytes) => self.import_update_bytes(&bytes, cx),
      GossipMsg::UpdateAvailable { blob, .. } => {
        self.pull_blob(from, blob, cx);
        Ok(())
      },
      GossipMsg::Presence(bytes) => {
        self.apply_presence(&bytes, cx);
        Ok(())
      },
      GossipMsg::Digest { session, vv } => self.handle_digest(from, session, &vv, cx),
    };
    if let Err(error) = result {
      self.detach(DetachReason::Fatal(format!("collaboration update failed: {error:#}")), cx);
    }
  }

  pub fn neighbor_up(&mut self, peer: flowstate_collab::ids::PeerId, cx: &mut Context<Self>) {
    self.neighbors.insert(peer);
    if let SessionPhase::Attached(attachment) = &mut self.phase {
      attachment.connectivity = Connectivity::Online;
    }
    self.publish_digest();
    cx.notify();
  }

  pub fn neighbor_down(&mut self, peer: flowstate_collab::ids::PeerId, cx: &mut Context<Self>) {
    self.neighbors.remove(&peer);
    cx.notify();
  }

  pub fn handle_gossip_lagged(&mut self, cx: &mut Context<Self>) {
    self.publish_digest();
    if let Some(peer) = self.neighbors.iter().next().copied() {
      let vv = self.doc.as_ref().map(|doc| doc.oplog_vv().encode()).unwrap_or_default();
      self.pull_updates(peer, vv, cx);
    }
  }

  pub fn set_endpoint_online(&mut self, online: bool, cx: &mut Context<Self>) {
    if let SessionPhase::Attached(attachment) = &mut self.phase {
      attachment.connectivity = if online {
        Connectivity::Online
      } else {
        Connectivity::Offline { since: Instant::now(), retries: 0 }
      };
      cx.notify();
    }
  }

  pub fn import_update_bytes(&mut self, bytes: &[u8], cx: &mut Context<Self>) -> Result<()> {
    if self.doc.is_none() || self.binding.is_none() || self.editor.is_none() {
      self.pending_remote_updates.push(bytes.to_vec());
      return Ok(());
    }

    let editor = self.editor.clone().context("collaboration session has no editor")?;
    let document = Arc::new(editor.read(cx).document().clone());
    let doc = self.doc.clone().context("collaboration session has no Loro document")?;
    let binding = self.binding.take().context("collaboration session has no document binding")?;
    let binding = Arc::new(Mutex::new(binding));
    let patches = Arc::new(Mutex::new(Vec::<CollabPatch>::new()));
    let sub = self.diff_subscription(doc.clone(), document, binding.clone(), patches.clone());

    let import_result = doc.import_with(bytes, "remote");
    drop(sub);
    self.binding = Some(take_mutex_value(binding, "document binding")?);
    let patches = take_mutex_value(patches, "remote patches")?;
    import_result.context("importing collaboration update failed")?;
    self.apply_or_queue_patches(patches, cx);
    Ok(())
  }

  fn attach_editor_hooks(&mut self, cx: &mut Context<Self>) {
    if !self.editor_subscriptions.is_empty() {
      return;
    }
    let Some(editor) = self.editor.clone() else {
      return;
    };

    editor.update(cx, |editor, _| {
      editor.set_collab_capture(true);
      editor.set_collab_undo_redirect(Some(Rc::new(|_redirect: UndoRedirect| {
        // TODO(Flowstate P2P §8/M3): replace this guard with Loro UndoManager undo/redo routing.
      })));
    });

    self.editor_subscriptions.push(cx.observe(&editor, |session, editor, cx| {
      if let Err(error) = session.flush_local_edits(editor.clone(), cx) {
        session.detach(DetachReason::Fatal(format!("capturing local collaboration edit failed: {error:#}")), cx);
        return;
      }
      session.flush_pending_remote_patches(cx);
    }));

    self.editor_subscriptions.push(cx.subscribe(&editor, |session, _, event: &EditorEvent, cx| {
      if matches!(event, EditorEvent::SelectionChanged { .. }) {
        session.refresh_own_presence(cx);
      }
    }));
  }

  fn attach_loro_publish_hook(&mut self) {
    if self.doc.is_none() || self.local_update_publish_attached {
      return;
    }
    let Some(doc) = &self.doc else {
      return;
    };
    let session = self.session;
    let net_tx = self.net_tx.clone();
    self.loro_subscriptions.push(doc.subscribe_local_update(Box::new(move |bytes| {
      let _ = net_tx.try_send(NetCommand::Publish {
        session,
        payload: PublishPayload::Update(bytes.clone()),
      });
      true
    })));
    self.local_update_publish_attached = true;
  }

  fn attach_direct_request_pump(&mut self, cx: &mut Context<Self>) {
    if self.direct_pump_started {
      return;
    }
    self.direct_pump_started = true;
    let requests = self.direct_rx.clone();
    cx.spawn(async move |session, cx| {
      while let Ok(request) = requests.recv().await {
        if session.update(cx, |session, cx| session.handle_direct_request(request, cx)).is_err() {
          break;
        }
      }
    })
    .detach();
  }

  fn handle_direct_request(&mut self, request: DirectServeRequest, cx: &mut Context<Self>) {
    match request {
      DirectServeRequest::Snapshot { reply } => {
        let _ = reply.try_send(self.snapshot_bytes());
      },
      DirectServeRequest::Updates { have_vv, reply } => {
        let _ = reply.try_send(self.update_bytes(&have_vv));
      },
      DirectServeRequest::Asset { asset, reply } => {
        let _ = reply.try_send(self.asset_bytes(asset, cx));
      },
    }
  }

  fn snapshot_bytes(&self) -> Result<Vec<u8>> {
    self
      .doc
      .as_ref()
      .context("collaboration session is not attached")?
      .export(ExportMode::Snapshot)
      .context("exporting collaboration snapshot failed")
  }

  fn update_bytes(&self, have_vv: &[u8]) -> Result<Vec<u8>> {
    let vv = VersionVector::decode(have_vv).context("decoding collaboration version vector failed")?;
    self
      .doc
      .as_ref()
      .context("collaboration session is not attached")?
      .export(ExportMode::updates(&vv))
      .context("exporting collaboration updates failed")
  }

  fn asset_bytes(&self, asset: u128, cx: &mut Context<Self>) -> Result<AssetBytes> {
    let editor = self.editor.as_ref().context("collaboration session has no editor")?;
    let bytes = editor
      .read(cx)
      .document()
      .assets
      .assets
      .get(&AssetId(asset))
      .map(|record| record.bytes.as_ref().clone())
      .ok_or_else(|| anyhow!("collaboration asset {asset} is not available"))?;
    Ok(AssetBytes { bytes })
  }

  fn diff_subscription(
    &self,
    doc: LoroDoc,
    document: Arc<Document>,
    binding: Arc<Mutex<DocBinding>>,
    patches: Arc<Mutex<Vec<CollabPatch>>>,
  ) -> LoroSubscription {
    let subscribed_doc = doc.clone();
    let callback: Subscriber = Arc::new(move |event| {
      let produced = {
        let mut binding = match binding.lock() {
          Ok(binding) => binding,
          Err(error) => {
            eprintln!("flowstate collab binding lock failed: {error}");
            return;
          },
        };
        let result = {
          let mut applier = RemoteApplier {
            doc: &doc,
            binding: &mut binding,
          };
          applier.apply_event(&document, &event)
        };
        drop(binding);
        result
      };
      match produced {
        Ok(mut produced) => {
          if let Ok(mut patches) = patches.lock() {
            patches.append(&mut produced);
          }
        },
        Err(error) => eprintln!("flowstate collab remote apply failed: {error:#}"),
      }
    });
    subscribed_doc.subscribe_root(callback)
  }

  fn apply_or_queue_patches(&mut self, mut patches: Vec<CollabPatch>, cx: &mut Context<Self>) {
    if patches.is_empty() {
      return;
    }
    self.pending_remote_patches.append(&mut patches);
    self.flush_pending_remote_patches(cx);
  }

  fn flush_pending_remote_patches(&mut self, cx: &mut Context<Self>) -> bool {
    let Some(editor) = self.editor.clone() else {
      return false;
    };
    if self.pending_remote_patches.is_empty() || editor.read(cx).collab_apply_deferred() {
      return false;
    }
    let patches = std::mem::take(&mut self.pending_remote_patches);
    editor.update(cx, |editor, cx| editor.apply_collab_patches(&patches, cx));
    true
  }

  fn handle_digest(
    &mut self,
    from: flowstate_collab::ids::PeerId,
    digest_session: SessionId,
    vv: &[u8],
    cx: &mut Context<Self>,
  ) -> Result<()> {
    if digest_session != self.session {
      bail!("received a collaboration digest for a different session");
    }
    let Some(doc) = &self.doc else {
      return Ok(());
    };
    let sender_vv = VersionVector::decode(vv).context("decoding collaboration digest failed")?;
    let our_vv = doc.oplog_vv();
    match sender_vv.partial_cmp(&our_vv) {
      Some(Ordering::Greater) | None => self.pull_updates(from, our_vv.encode(), cx),
      Some(Ordering::Equal | Ordering::Less) => {},
    }
    Ok(())
  }

  fn pull_updates(&mut self, from: flowstate_collab::ids::PeerId, our_vv: Vec<u8>, cx: &mut Context<Self>) {
    if self.update_pull_in_flight {
      return;
    }
    self.update_pull_in_flight = true;
    let (reply_tx, reply_rx) = async_channel::bounded(1);
    let send_result = self.net_tx.try_send(NetCommand::PullUpdates {
      session: self.session,
      from,
      our_vv,
      reply: reply_tx,
    });
    if send_result.is_err() {
      self.update_pull_in_flight = false;
      return;
    }
    cx.spawn(async move |session, cx| {
      let result = reply_rx.recv().await;
      let _ = session.update(cx, |session, cx| {
        session.update_pull_in_flight = false;
        if let Ok(Ok(bytes)) = result
          && let Err(error) = session.import_update_bytes(&bytes, cx)
        {
          session.detach(DetachReason::Fatal(format!("pulling collaboration updates failed: {error:#}")), cx);
        }
      });
    })
    .detach();
  }

  fn pull_blob(&mut self, from: flowstate_collab::ids::PeerId, blob: flowstate_collab::BlobId, cx: &mut Context<Self>) {
    let (reply_tx, reply_rx) = async_channel::bounded(1);
    if self
      .net_tx
      .try_send(NetCommand::PullBlob {
        session: self.session,
        from,
        blob,
        reply: reply_tx,
      })
      .is_err()
    {
      return;
    }
    cx.spawn(async move |session, cx| {
      let result = reply_rx.recv().await;
      let _ = session.update(cx, |session, cx| {
        if let Ok(Ok(bytes)) = result
          && let Err(error) = session.import_update_bytes(&bytes, cx)
        {
          session.detach(DetachReason::Fatal(format!("pulling collaboration blob failed: {error:#}")), cx);
        }
      });
    })
    .detach();
  }

  fn publish_digest(&self) {
    if let Some(doc) = &self.doc {
      let _ = self.net_tx.try_send(NetCommand::Publish {
        session: self.session,
        payload: PublishPayload::Digest { vv: doc.oplog_vv().encode() },
      });
    }
  }

  fn apply_presence(&mut self, bytes: &[u8], cx: &mut Context<Self>) {
    if let Some(presence) = &self.presence {
      if let Err(error) = presence.apply(bytes) {
        eprintln!("flowstate collab presence update failed: {error:#}");
      }
      self.refresh_peer_count();
      cx.notify();
    }
  }

  fn refresh_own_presence(&mut self, _cx: &mut Context<Self>) {
    let Some(presence) = &self.presence else {
      return;
    };
    let state = PresenceState {
      name: default_presence_name(),
      selection: None,
    };
    if let Err(error) = presence.set_self(&state) {
      eprintln!("flowstate collab presence encode failed: {error:#}");
    }
    self.refresh_peer_count();
  }

  fn publish_presence_snapshot(&self) {
    if let Some(presence) = &self.presence {
      self.publish_presence_bytes(presence.encode_all());
    }
  }

  fn publish_presence_bytes(&self, bytes: Vec<u8>) {
    if bytes.is_empty() {
      return;
    }
    let _ = self.net_tx.try_send(NetCommand::Publish {
      session: self.session,
      payload: PublishPayload::Presence(bytes),
    });
  }

  fn refresh_peer_count(&mut self) {
    let peers_present = self.peers_present();
    if let SessionPhase::Attached(attachment) = &mut self.phase {
      attachment.peers_present = peers_present;
    }
  }

  fn peers_present(&self) -> usize {
    self
      .presence
      .as_ref()
      .map_or(0, |presence| presence.roster().len().saturating_sub(1))
  }
}

fn take_mutex_value<T>(value: Arc<Mutex<T>>, label: &str) -> Result<T> {
  match Arc::try_unwrap(value) {
    Ok(mutex) => mutex
      .into_inner()
      .map_err(|error| anyhow!("collaboration {label} lock was poisoned: {error}")),
    Err(_) => Err(anyhow!("collaboration {label} is still referenced")),
  }
}

fn default_presence_name() -> String {
  std::env::var("FLOWSTATE_COLLAB_NAME")
    .or_else(|_| std::env::var("USER"))
    .or_else(|_| std::env::var("USERNAME"))
    .unwrap_or_else(|_| "Flowstate user".to_string())
}
