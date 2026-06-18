//! Peer-to-peer collaboration support for Flowstate rich-text documents.
//!
//! This crate contains the GPUI-free collaboration core: the Loro-native CRDT
//! runtime, transport protocol types, presence, and networking state.
//! Application/UI integration lives in `crates/flowstate/src/collab`.

pub mod crdt_runtime;
pub mod ids;
pub mod net;
pub mod presence;
pub mod proto_direct;
pub mod proto_gossip;
pub mod self_check;
pub mod ticket;

pub use ids::{BlobId, SessionId};
pub use proto_direct::DIRECT_ALPN;
pub use proto_gossip::PROTOCOL_VERSION;
