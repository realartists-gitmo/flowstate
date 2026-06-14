pub mod anti_entropy;
pub mod blobs;
pub mod direct;
pub mod runtime;
pub mod swarm;

use anyhow::Result;
use async_channel::Sender;
use iroh::{EndpointAddr, EndpointId};

use crate::{
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
  },
  LeaveSession {
    session: SessionId,
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

#[derive(Clone, Copy, Debug)]
pub struct PullProgress {
  pub got: u64,
  pub total: u64,
}

#[derive(Clone, Debug)]
pub struct TicketSeed {
  pub inviter: EndpointAddr,
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
