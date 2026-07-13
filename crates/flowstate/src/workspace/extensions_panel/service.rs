use std::{
  collections::HashMap,
  hash::{DefaultHasher, Hash, Hasher},
  fs,
  path::PathBuf,
  sync::{Arc, Mutex, mpsc},
  thread,
};

use flowstate_extension::{
  CancellationHandle, ComponentDigest, ExtensionHost, HostError, InstalledExtension, Invocation, Runtime, RuntimeConfig, RuntimeError, TrustDecision,
  TrustStore, discover_extensions,
};
use gpui::SharedString;
use serde::{Deserialize, Serialize};

use super::{ExtensionActionView, ExtensionPanelAdapter, ExtensionPanelEvent, ExtensionView};

pub struct ExtensionService {
  extensions_root: PathBuf,
  data_root: PathBuf,
  trust_path: PathBuf,
  grants_path: PathBuf,
  runtime: Arc<Runtime>,
  installed: Mutex<HashMap<String, InstalledExtension>>,
  trust: Mutex<TrustStore>,
  grants: Mutex<Vec<PersistedGrant>>,
  running: Arc<Mutex<HashMap<String, CancellationHandle>>>,
  events_tx: mpsc::SyncSender<ExtensionPanelEvent>,
  events_rx: Mutex<mpsc::Receiver<ExtensionPanelEvent>>,
}

impl ExtensionService {
  pub fn new(flowstate_data: PathBuf) -> Result<Arc<Self>, SharedString> {
    let extensions_root = flowstate_data.join("extensions");
    let data_root = flowstate_data.join("extension-data");
    let trust_path = flowstate_data.join("extension-trust.json");
    let grants_path = flowstate_data.join("extension-directory-grants.json");
    fs::create_dir_all(&extensions_root).map_err(shared_error)?;
    fs::create_dir_all(&data_root).map_err(shared_error)?;
    let trust = fs::read(&trust_path)
      .ok()
      .and_then(|bytes| serde_json::from_slice(&bytes).ok())
      .unwrap_or_default();
    let runtime = Runtime::new(RuntimeConfig::default()).map_err(shared_error)?;
    let grants = fs::read(&grants_path).ok().and_then(|bytes| serde_json::from_slice(&bytes).ok()).unwrap_or_default();
    let (events_tx, events_rx) = mpsc::sync_channel(256);
    Ok(Arc::new(Self {
      extensions_root,
      data_root,
      trust_path,
      grants_path,
      runtime: Arc::new(runtime),
      installed: Mutex::new(HashMap::new()),
      trust: Mutex::new(trust),
      grants: Mutex::new(grants),
      running: Arc::new(Mutex::new(HashMap::new())),
      events_tx,
      events_rx: Mutex::new(events_rx),
    }))
  }

  pub fn invoke_with_host(
    self: &Arc<Self>,
    extension_id: &str,
    action_id: &str,
    document_root: Option<PathBuf>,
    host: Box<dyn ExtensionHost>,
  ) -> Result<(), SharedString> {
    let extension = self
      .installed
      .lock()
      .map_err(shared_error)?
      .get(extension_id)
      .cloned()
      .ok_or_else(|| SharedString::from("Extension is no longer installed"))?;
    let directory_grants = self.directory_grants(extension_id, &extension.component_path)?;
    let invocation = Invocation {
      component: extension.component_path,
      extension_root: extension.root,
      data_root: self.data_root.join(extension_id),
      document_root,
      action_id: action_id.to_owned(),
      directory_grants,
    };
    let cancellation = self.runtime.cancellation_handle();
    self.running.lock().map_err(shared_error)?.insert(extension_id.to_owned(), cancellation.clone());
    let service = Arc::clone(self);
    let extension_id = extension_id.to_owned();
    let action_id = action_id.to_owned();
    thread::Builder::new()
      .name(format!("flowstate-extension-{extension_id}"))
      .spawn(move || {
        let _ = service.events_tx.send(ExtensionPanelEvent::Started {
          extension_id: extension_id.clone().into(),
          action_id: action_id.into(),
        });
        let result = service.runtime.invoke(&extension_id, &invocation, BoxedHost(host), &cancellation);
        service.running.lock().ok().map(|mut running| running.remove(&extension_id));
        let event = match result {
          Ok(output) => {
            let mut bytes = output.stdout;
            bytes.extend(output.stderr);
            if !bytes.is_empty() {
              let _ = service.events_tx.send(ExtensionPanelEvent::Output {
                extension_id: extension_id.clone().into(),
                output: String::from_utf8_lossy(&bytes).into_owned().into(),
              });
            }
            ExtensionPanelEvent::Finished { extension_id: extension_id.into() }
          },
          Err(RuntimeError::Cancelled) => ExtensionPanelEvent::Cancelled { extension_id: extension_id.into() },
          Err(error) => ExtensionPanelEvent::Failed { extension_id: extension_id.into(), message: error.to_string().into() },
        };
        let _ = service.events_tx.send(event);
      })
      .map_err(shared_error)?;
    Ok(())
  }

  pub fn drain_events(&self) -> Vec<ExtensionPanelEvent> {
    let Ok(receiver) = self.events_rx.lock() else { return Vec::new() };
    std::iter::from_fn(|| receiver.try_recv().ok()).collect()
  }

  pub fn grant_directory(
    &self,
    extension_id: &str,
    access: flowstate_extension::DirectoryAccess,
    host_path: PathBuf,
  ) -> Result<flowstate_extension::DirectoryGrantResponse, HostError> {
    let extension = self.installed.lock().map_err(host_service)?.get(extension_id).cloned().ok_or_else(|| host_service("extension missing"))?;
    let digest = ComponentDigest::from_file(&extension.component_path).map_err(host_service)?;
    let mut path_hash = DefaultHasher::new();
    host_path.hash(&mut path_hash);
    let grant_id = format!("grant-{:016x}", path_hash.finish());
    let persisted = PersistedGrant {
      extension_id: extension_id.to_owned(),
      component_hash: digest.as_str().to_owned(),
      grant_id: grant_id.clone(),
      host_path,
      read_write: access == flowstate_extension::DirectoryAccess::ReadWrite,
    };
    let mut grants = self.grants.lock().map_err(host_service)?;
    grants.retain(|grant| !(grant.extension_id == extension_id && grant.grant_id == grant_id));
    grants.push(persisted);
    let bytes = serde_json::to_vec_pretty(&*grants).map_err(host_service)?;
    drop(grants);
    fs::write(&self.grants_path, bytes).map_err(host_service)?;
    Ok(flowstate_extension::DirectoryGrantResponse {
      grant_id: grant_id.clone(),
      mount_path: format!("/grants/{grant_id}"),
      available_next_invocation: true,
    })
  }

  fn directory_grants(&self, extension_id: &str, component: &std::path::Path) -> Result<Vec<flowstate_extension::DirectoryGrant>, SharedString> {
    let digest = ComponentDigest::from_file(component).map_err(shared_error)?;
    let grants = self.grants.lock().map_err(shared_error)?;
    let result = grants.iter().filter(|grant| grant.extension_id == extension_id && grant.component_hash == digest.as_str()).map(|grant| flowstate_extension::DirectoryGrant {
      grant_id: grant.grant_id.clone(),
      host_path: grant.host_path.clone(),
      access: if grant.read_write { flowstate_extension::DirectoryAccess::ReadWrite } else { flowstate_extension::DirectoryAccess::Read },
    }).collect();
    drop(grants);
    Ok(result)
  }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct PersistedGrant {
  extension_id: String,
  component_hash: String,
  grant_id: String,
  host_path: PathBuf,
  read_write: bool,
}

fn host_service(error: impl std::fmt::Display) -> HostError {
  HostError { code: "directory-grant".to_owned(), message: error.to_string() }
}

struct BoxedHost(Box<dyn ExtensionHost>);

impl ExtensionHost for BoxedHost {
  fn snapshot(&mut self) -> Result<String, flowstate_extension::HostError> { self.0.snapshot() }
  fn selection(&mut self) -> Result<String, flowstate_extension::HostError> { self.0.selection() }
  fn apply_edits(&mut self, edits_json: &str) -> Result<String, flowstate_extension::HostError> { self.0.apply_edits(edits_json) }
  fn refresh_from_disk(&mut self) -> Result<String, flowstate_extension::HostError> { self.0.refresh_from_disk() }
  fn set_action_label(&mut self, action_id: &str, label: &str) -> Result<(), flowstate_extension::HostError> {
    self.0.set_action_label(action_id, label)
  }
  fn set_status(&mut self, message: &str) { self.0.set_status(message); }
  fn request_directory_access(
    &mut self,
    mode: flowstate_extension::DirectoryAccess,
    suggested_path: Option<&str>,
  ) -> Result<flowstate_extension::DirectoryGrantResponse, flowstate_extension::HostError> {
    self.0.request_directory_access(mode, suggested_path)
  }
}

fn shared_error(error: impl std::fmt::Display) -> SharedString {
  error.to_string().into()
}

impl ExtensionPanelAdapter for ExtensionService {
  fn installed(&self) -> Result<Vec<ExtensionView>, SharedString> {
    let discovery = discover_extensions(&self.extensions_root);
    let mut discovered = HashMap::new();
    let mut views = Vec::with_capacity(discovery.extensions.len());
    for extension in discovery.extensions {
      let digest = ComponentDigest::from_file(&extension.component_path).map_err(shared_error)?;
      views.push(ExtensionView {
        id: extension.manifest.id.clone().into(),
        name: extension.manifest.name.clone().into(),
        version: extension.manifest.version.clone().into(),
        component_hash: digest.as_str().to_owned().into(),
        actions: extension
          .manifest
          .actions
          .iter()
          .map(|action| ExtensionActionView {
            id: action.id.clone().into(),
            label: action.label.clone().into(),
            requires_document: action.requires_document,
          })
          .collect(),
      });
      discovered.insert(extension.manifest.id.clone(), extension);
    }
    if views.is_empty() && !discovery.issues.is_empty() {
      return Err(discovery
        .issues
        .into_iter()
        .map(|issue| format!("{}: {}", issue.path.display(), issue.message))
        .collect::<Vec<_>>()
        .join("\n")
        .into());
    }
    *self.installed.lock().map_err(shared_error)? = discovered;
    Ok(views)
  }

  fn is_trusted(&self, extension_id: &str, component_hash: &str) -> bool {
    let Ok(installed) = self.installed.lock() else { return false };
    let Some(extension) = installed.get(extension_id) else { return false };
    let Ok(digest) = ComponentDigest::from_file(&extension.component_path) else { return false };
    if digest.as_str() != component_hash { return false }
    self
      .trust
      .lock()
      .is_ok_and(|trust| trust.decision(extension_id, &digest) == TrustDecision::Trusted)
  }

  fn trust(&self, extension_id: &str, component_hash: &str) -> Result<(), SharedString> {
    let installed = self.installed.lock().map_err(shared_error)?;
    let component = installed.get(extension_id).ok_or_else(|| SharedString::from("Extension is no longer installed"))?.component_path.clone();
    drop(installed);
    let digest = ComponentDigest::from_file(&component).map_err(shared_error)?;
    if digest.as_str() != component_hash { return Err("Extension changed before approval".into()) }
    let mut trust = self.trust.lock().map_err(shared_error)?;
    trust.approve(extension_id, digest);
    let bytes = serde_json::to_vec_pretty(&*trust).map_err(shared_error)?;
    drop(trust);
    if let Some(parent) = self.trust_path.parent() { fs::create_dir_all(parent).map_err(shared_error)?; }
    fs::write(&self.trust_path, bytes).map_err(shared_error)
  }

  fn cancel(&self, extension_id: &str) -> Result<(), SharedString> {
    let running = self.running.lock().map_err(shared_error)?;
    let handle = running.get(extension_id).cloned().ok_or_else(|| SharedString::from("Extension is not running"))?;
    drop(running);
    handle.cancel();
    Ok(())
  }
}
