use std::{borrow::BorrowMut, collections::HashMap, sync::Arc, time::Duration};

use flowstate_collab::{
  SessionId,
  bluetooth::NativeBluetoothBackend,
  discovery::{DiscoveryAdvertisement, DiscoveryTransport, RendezvousBackend},
  dropbox::{DropboxClient, DropboxRendezvousBackend},
  identity::{PortableIdentitySecret, SignedProfile},
};
use futures_util::{FutureExt as _, future::Either};
use gpui::{App, Timer};
use iroh::EndpointAddr;

use crate::app_settings::{load_app_settings, load_dropbox_collaboration};

const DISCOVERY_REFRESH: Duration = Duration::from_secs(30);
const ADVERTISEMENT_LIFETIME_SECS: u64 = 90;
const BLUETOOTH_SCAN_DURATION: Duration = Duration::from_secs(4);

#[derive(Clone)]
pub(super) struct DiscoveryRuntime {
  commands: async_channel::Sender<DiscoveryCommand>,
}

#[derive(Clone)]
pub(super) struct DiscoveryPublication {
  pub secret: PortableIdentitySecret,
  pub device_id: u128,
  pub document_fingerprint: [u8; 32],
  pub session: SessionId,
  pub endpoint: EndpointAddr,
  pub profile: SignedProfile,
}

enum DiscoveryCommand {
  Upsert(Box<DiscoveryPublication>),
  Remove {
    session: SessionId,
  },
  Scan {
    document_fingerprint: [u8; 32],
    reply: async_channel::Sender<DiscoveryScanResult>,
  },
  Shutdown,
}

pub struct DiscoveryScanResult {
  pub advertisements: Vec<DiscoveryAdvertisement>,
  pub failures: Vec<(DiscoveryTransport, String)>,
  pub active_transports: Vec<DiscoveryTransport>,
  pub paused: bool,
}

impl DiscoveryRuntime {
  pub fn start<C>(cx: &mut C) -> Self
  where
    C: BorrowMut<App>,
  {
    let settings = load_app_settings();
    let dropbox = load_dropbox_collaboration();
    let bluetooth_enabled = settings.bluetooth_collaboration_discovery_enabled && !settings.collaboration_discovery_paused;
    let discovery_paused = settings.collaboration_discovery_paused;
    let (commands, receiver) = async_channel::unbounded();
    cx.borrow_mut()
      .spawn(async move |_| run_discovery_actor(receiver, dropbox, bluetooth_enabled, discovery_paused))
      .detach();
    Self { commands }
  }

  pub fn upsert(&self, publication: DiscoveryPublication) {
    if let Err(error) = self
      .commands
      .try_send(DiscoveryCommand::Upsert(Box::new(publication)))
    {
      tracing::warn!(%error, "queueing collaboration discovery publication failed");
    }
  }

  pub fn remove(&self, session: SessionId) {
    if let Err(error) = self.commands.try_send(DiscoveryCommand::Remove { session }) {
      tracing::warn!(%error, %session, "queueing collaboration discovery removal failed");
    }
  }

  pub(super) fn scan(&self, document_fingerprint: [u8; 32]) -> async_channel::Receiver<DiscoveryScanResult> {
    let (reply, receiver) = async_channel::bounded(1);
    if let Err(error) = self
      .commands
      .try_send(DiscoveryCommand::Scan { document_fingerprint, reply })
    {
      tracing::warn!(%error, "queueing collaboration discovery scan failed");
    }
    receiver
  }

  pub fn shutdown(&self) {
    let _ = self.commands.try_send(DiscoveryCommand::Shutdown);
  }
}

async fn run_discovery_actor(
  receiver: async_channel::Receiver<DiscoveryCommand>,
  dropbox: Option<(flowstate_collab::dropbox::DropboxCredentials, String)>,
  bluetooth_enabled: bool,
  discovery_paused: bool,
) {
  let mut backends: Vec<Arc<dyn RendezvousBackend>> = Vec::new();
  if !discovery_paused {
    if let Some((credentials, root)) = dropbox {
      backends.push(Arc::new(DropboxRendezvousBackend::new(DropboxClient::new(credentials), root)));
    }
    if bluetooth_enabled {
      match NativeBluetoothBackend::new(BLUETOOTH_SCAN_DURATION).await {
        Ok(backend) => backends.push(Arc::new(backend)),
        Err(error) => tracing::warn!(%error, "native Bluetooth collaboration discovery is unavailable"),
      }
    }
  }

  let mut publications: HashMap<SessionId, DiscoveryPublication> = HashMap::new();
  let mut bluetooth_cursor = 0_usize;
  loop {
    let command = receiver.recv().boxed();
    let refresh = Timer::after(DISCOVERY_REFRESH).boxed();
    match futures_util::future::select(command, refresh).await {
      Either::Left((Ok(DiscoveryCommand::Upsert(publication)), _)) => {
        let session = publication.session;
        publications.insert(session, *publication);
        publish_session(&backends, publications.get(&session).expect("inserted publication")).await;
      },
      Either::Left((Ok(DiscoveryCommand::Remove { session }), _)) => {
        if let Some(publication) = publications.remove(&session) {
          clear_publication(&backends, &publication).await;
        }
      },
      Either::Left((Ok(DiscoveryCommand::Scan { document_fingerprint, reply }), _)) => {
        let mut advertisements = Vec::new();
        let mut failures = Vec::new();
        for backend in &backends {
          match backend.scan(document_fingerprint).await {
            Ok(mut found) => advertisements.append(&mut found),
            Err(error) => failures.push((backend.transport(), error.to_string())),
          }
        }
        let _ = reply
          .send(DiscoveryScanResult {
            advertisements,
            failures,
            active_transports: backends.iter().map(|backend| backend.transport()).collect(),
            paused: discovery_paused,
          })
          .await;
      },
      Either::Left((Ok(DiscoveryCommand::Shutdown) | Err(_), _)) => break,
      Either::Right((_instant, _)) => {
        for backend in &backends {
          match backend.transport() {
            DiscoveryTransport::Dropbox => {
              for publication in publications.values() {
                publish_backend(backend, publication).await;
              }
            },
            DiscoveryTransport::Bluetooth if !publications.is_empty() => {
              let index = bluetooth_cursor % publications.len();
              if let Some(publication) = publications.values().nth(index) {
                publish_backend(backend, publication).await;
              }
              bluetooth_cursor = bluetooth_cursor.wrapping_add(1);
            },
            DiscoveryTransport::Bluetooth => {},
          }
        }
      },
    }
  }

  for publication in publications.values() {
    clear_publication(&backends, publication).await;
  }
}

async fn publish_session(backends: &[Arc<dyn RendezvousBackend>], publication: &DiscoveryPublication) {
  for backend in backends {
    publish_backend(backend, publication).await;
  }
}

async fn publish_backend(backend: &Arc<dyn RendezvousBackend>, publication: &DiscoveryPublication) {
  let advertisement = DiscoveryAdvertisement::issue(
    &publication.secret,
    publication.device_id,
    publication.document_fingerprint,
    publication.session,
    publication.endpoint.clone(),
    unix_now().saturating_add(ADVERTISEMENT_LIFETIME_SECS),
    publication.profile.clone(),
  );
  if let Err(error) = backend.publish(advertisement).await {
    tracing::warn!(%error, session = %publication.session, transport = ?backend.transport(), "publishing collaboration discovery advertisement failed");
  }
}

async fn clear_publication(backends: &[Arc<dyn RendezvousBackend>], publication: &DiscoveryPublication) {
  let identity = publication.secret.public();
  for backend in backends {
    if let Err(error) = backend
      .clear(identity, publication.device_id, publication.document_fingerprint)
      .await
    {
      tracing::warn!(%error, session = %publication.session, transport = ?backend.transport(), "clearing collaboration discovery advertisement failed");
    }
  }
}

fn unix_now() -> u64 {
  std::time::SystemTime::now()
    .duration_since(std::time::UNIX_EPOCH)
    .unwrap_or(Duration::ZERO)
    .as_secs()
}
