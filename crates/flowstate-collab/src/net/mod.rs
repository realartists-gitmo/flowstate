pub mod anti_entropy;
pub mod auth;
pub mod blobs;
pub mod direct;
pub mod runtime;
pub mod swarm;
pub(crate) mod wire_compression;

use anyhow::Result;
use async_channel::Sender;
use iroh::{EndpointAddr, EndpointId, PublicKey};

use crate::{
  admission::SessionAdmission,
  ids::{BlobId, SessionId},
  proto_direct::AssetBytes,
  proto_gossip::GossipMsg,
};

use self::direct::DirectSessionHandler;

pub type Reply<T> = Sender<T>;
pub type PeerAddr = EndpointAddr;

#[derive(Clone, Debug)]
pub enum PublishPayload {
  Update(Vec<u8>),
  Presence(Vec<u8>),
  Digest { vv: Vec<u8> },
}

impl PublishPayload {
  #[must_use]
  pub fn kind(&self) -> &'static str {
    match self {
      Self::Update(_) => "update",
      Self::Presence(_) => "presence",
      Self::Digest { .. } => "digest",
    }
  }

  #[must_use]
  pub fn byte_len(&self) -> usize {
    match self {
      Self::Update(bytes) | Self::Presence(bytes) => bytes.len(),
      Self::Digest { vv } => vv.len(),
    }
  }
}

#[derive(Debug)]
pub enum NetCommand {
  EnsureUp,
  RegisterDirectHandler {
    session: SessionId,
    handler: DirectSessionHandler,
  },
  ConfigureStandingAccess {
    session: SessionId,
    document_fingerprint: [u8; 32],
    title: String,
    identities: Vec<PublicKey>,
  },
  RequestDiscoveredTicket {
    advertisement: crate::discovery::DiscoveryAdvertisement,
    request: crate::discovery::DiscoveryAdmissionRequest,
    reply: Reply<Result<crate::ticket::SessionTicket>>,
  },
  CreateSession {
    session: SessionId,
    reply: Reply<Result<TicketSeed>>,
  },
  JoinSession {
    session: SessionId,
    bootstrap: Vec<EndpointAddr>,
    admission: SessionAdmission,
  },
  LeaveSession {
    session: SessionId,
  },
  MintTicket {
    session: SessionId,
    reply: Reply<Result<MintedTicket>>,
  },
  Publish {
    session: SessionId,
    payload: PublishPayload,
  },
  PullUpdates {
    session: SessionId,
    candidates: Vec<EndpointId>,
    our_vv: Vec<u8>,
    reply: Reply<Result<Vec<u8>>>,
  },
  PullSnapshot {
    session: SessionId,
    candidates: Vec<EndpointId>,
    progress: Option<Reply<PullProgress>>,
    reply: Reply<Result<Vec<u8>>>,
  },
  PullBlob {
    session: SessionId,
    candidates: Vec<EndpointId>,
    blob: BlobId,
    reply: Reply<Result<Vec<u8>>>,
  },
  PullAsset {
    session: SessionId,
    candidates: Vec<EndpointId>,
    asset: u128,
    reply: Reply<Result<AssetBytes>>,
  },
  MintTicketAddr {
    reply: Reply<EndpointAddr>,
  },
  Shutdown,
}

impl NetCommand {
  #[must_use]
  pub fn kind(&self) -> &'static str {
    match self {
      Self::EnsureUp => "ensure_up",
      Self::RegisterDirectHandler { .. } => "register_direct_handler",
      Self::ConfigureStandingAccess { .. } => "configure_standing_access",
      Self::RequestDiscoveredTicket { .. } => "request_discovered_ticket",
      Self::CreateSession { .. } => "create_session",
      Self::JoinSession { .. } => "join_session",
      Self::LeaveSession { .. } => "leave_session",
      Self::MintTicket { .. } => "mint_ticket",
      Self::Publish { .. } => "publish",
      Self::PullUpdates { .. } => "pull_updates",
      Self::PullSnapshot { .. } => "pull_snapshot",
      Self::PullBlob { .. } => "pull_blob",
      Self::PullAsset { .. } => "pull_asset",
      Self::MintTicketAddr { .. } => "mint_ticket_addr",
      Self::Shutdown => "shutdown",
    }
  }
}

#[derive(Clone, Copy, Debug)]
pub struct PullProgress {
  pub got: u64,
  pub total: u64,
}

#[derive(Clone, Debug)]
pub struct TicketSeed {
  pub inviter: EndpointAddr,
  pub admission: SessionAdmission,
}

/// Reply payload of [`NetCommand::MintTicket`].
#[derive(Clone, Debug)]
pub struct MintedTicket {
  pub inviter: EndpointAddr,
  pub admission: SessionAdmission,
}

#[derive(Clone, Debug)]
pub enum NetEvent {
  Gossip { session: SessionId, from: EndpointId, msg: GossipMsg },
  IncompatibleVersion { session: SessionId, peer: EndpointId },
  NeighborUp { session: SessionId, peer: EndpointId },
  NeighborDown { session: SessionId, peer: EndpointId },
  GossipLagged { session: SessionId },
  SubscribeFailed { session: SessionId, error: String },
  EndpointOnline(bool),
}
