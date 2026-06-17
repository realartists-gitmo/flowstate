//! Peer-to-peer collaboration support for Flowstate rich-text documents.
//!
//! This crate contains the GPUI-free collaboration core: CRDT projection,
//! transport protocol types, and networking state. Application/UI integration
//! lives in `crates/flowstate/src/collab`.

pub mod binding;
pub mod body_index;
pub mod ids;
pub mod local_apply;
pub mod net;
pub mod patch_apply;
pub mod presence;
pub mod projection;
pub mod proto_direct;
pub mod proto_gossip;
pub mod remote_apply;
pub mod schema;
pub mod self_check;
pub mod ticket;

pub use ids::{BlobId, SessionId};
pub use proto_direct::DIRECT_ALPN;
pub use proto_gossip::PROTOCOL_VERSION;
