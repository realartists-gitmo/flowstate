pub mod anti_entropy;
pub mod auth;
pub mod blobs;
pub mod direct;
pub mod runtime;
pub mod swarm;
pub(crate) mod wire_compression;

use anyhow::Result;
use async_channel::Sender;
use iroh::{EndpointAddr, EndpointId, Signature};

use crate::{
  capability::{CapabilityRole, SessionCapability},
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
  /// Owner-signed capability revocation control frame (FS-080).
  CapabilityEpoch { epoch: u64, signature: Signature },
}

impl PublishPayload {
  #[must_use]
  pub fn kind(&self) -> &'static str {
    match self {
      Self::Update(_) => "update",
      Self::Presence(_) => "presence",
      Self::Digest { .. } => "digest",
      Self::CapabilityEpoch { .. } => "capability_epoch",
    }
  }

  #[must_use]
  pub fn byte_len(&self) -> usize {
    match self {
      Self::Update(bytes) | Self::Presence(bytes) => bytes.len(),
      Self::Digest { vv } => vv.len(),
      Self::CapabilityEpoch { .. } => 8 + Signature::LENGTH,
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
  CreateSession {
    session: SessionId,
    reply: Reply<Result<TicketSeed>>,
  },
  JoinSession {
    session: SessionId,
    bootstrap: Vec<EndpointAddr>,
    /// The owner-signed grant from the invite ticket. Configures owner-key
    /// and epoch verification for the session and is presented to peers
    /// during the direct handshake.
    capability: SessionCapability,
  },
  LeaveSession {
    session: SessionId,
  },
  /// Mint a signed invite ticket seed: reachable inviter address plus an
  /// owner-signed capability at the session's current revocation epoch.
  MintTicket {
    session: SessionId,
    role: CapabilityRole,
    ttl_secs: u64,
    reply: Reply<Result<MintedTicket>>,
  },
  /// Bump the session capability epoch, revoking all previously minted
  /// tickets, and gossip the signed bump to peers. Replies with the new epoch.
  RevokeCapabilities {
    session: SessionId,
    reply: Reply<Result<u64>>,
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
      Self::CreateSession { .. } => "create_session",
      Self::JoinSession { .. } => "join_session",
      Self::LeaveSession { .. } => "leave_session",
      Self::MintTicket { .. } => "mint_ticket",
      Self::RevokeCapabilities { .. } => "revoke_capabilities",
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
}

/// Reply payload of [`NetCommand::MintTicket`].
#[derive(Clone, Debug)]
pub struct MintedTicket {
  pub inviter: EndpointAddr,
  pub capability: SessionCapability,
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
