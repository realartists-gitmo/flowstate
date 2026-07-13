use std::sync::mpsc::{self, Receiver, SyncSender};

use flowstate_extension::{DirectoryAccess, DirectoryGrantResponse, ExtensionHost, HostError};

pub enum EditorHostRequest {
  Snapshot(SyncSender<Result<String, HostError>>),
  Selection(SyncSender<Result<String, HostError>>),
  ApplyEdits(String, SyncSender<Result<String, HostError>>),
  Refresh(SyncSender<Result<String, HostError>>),
  ActionLabel(String, String),
  Status(String),
  DirectoryAccess(DirectoryAccess, Option<String>, SyncSender<Result<DirectoryGrantResponse, HostError>>),
}

pub struct EditorHostBridge {
  requests: SyncSender<EditorHostRequest>,
}

impl EditorHostBridge {
  pub fn bounded(capacity: usize) -> (Self, Receiver<EditorHostRequest>) {
    let (requests, receiver) = mpsc::sync_channel(capacity);
    (Self { requests }, receiver)
  }

  fn request(
    &self,
    build: impl FnOnce(SyncSender<Result<String, HostError>>) -> EditorHostRequest,
  ) -> Result<String, HostError> {
    let (reply, response) = mpsc::sync_channel(1);
    self.requests.send(build(reply)).map_err(|_| unavailable())?;
    response.recv().map_err(|_| unavailable())?
  }
}

impl ExtensionHost for EditorHostBridge {
  fn snapshot(&mut self) -> Result<String, HostError> {
    self.request(EditorHostRequest::Snapshot)
  }

  fn selection(&mut self) -> Result<String, HostError> {
    self.request(EditorHostRequest::Selection)
  }

  fn apply_edits(&mut self, edits_json: &str) -> Result<String, HostError> {
    self.request(|reply| EditorHostRequest::ApplyEdits(edits_json.to_owned(), reply))
  }

  fn refresh_from_disk(&mut self) -> Result<String, HostError> {
    self.request(EditorHostRequest::Refresh)
  }

  fn set_action_label(&mut self, action_id: &str, label: &str) -> Result<(), HostError> {
    self
      .requests
      .send(EditorHostRequest::ActionLabel(action_id.to_owned(), label.to_owned()))
      .map_err(|_| unavailable())
  }

  fn set_status(&mut self, message: &str) {
    let _ = self.requests.send(EditorHostRequest::Status(message.to_owned()));
  }

  fn request_directory_access(&mut self, mode: DirectoryAccess, suggested_path: Option<&str>) -> Result<DirectoryGrantResponse, HostError> {
    let (reply, response) = mpsc::sync_channel(1);
    self
      .requests
      .send(EditorHostRequest::DirectoryAccess(mode, suggested_path.map(ToOwned::to_owned), reply))
      .map_err(|_| unavailable())?;
    response.recv().map_err(|_| unavailable())?
  }
}

fn unavailable() -> HostError {
  HostError { code: "host-unavailable".to_owned(), message: "Flowstate closed the extension host".to_owned() }
}

#[cfg(test)]
mod tests {
  use super::*;

  #[test]
  fn snapshot_request_round_trips_over_bounded_bridge() {
    let (mut host, requests) = EditorHostBridge::bounded(1);
    let worker = std::thread::spawn(move || host.snapshot().unwrap());
    let EditorHostRequest::Snapshot(reply) = requests.recv().unwrap() else { panic!("expected snapshot request") };
    reply.send(Ok("{\"generation\":7}".to_owned())).unwrap();
    assert_eq!(worker.join().unwrap(), "{\"generation\":7}");
  }

  #[test]
  fn closed_bridge_returns_typed_host_error() {
    let (mut host, requests) = EditorHostBridge::bounded(1);
    drop(requests);
    let error = host.selection().unwrap_err();
    assert_eq!(error.code, "host-unavailable");
  }
}
