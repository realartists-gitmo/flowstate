//! Peer-to-peer collaboration support for Flowstate rich-text documents.
//!
//! This crate contains the GPUI-free collaboration core: the Loro-native CRDT
//! runtime, transport protocol types, presence, and networking state.
//! Application/UI integration lives in `crates/flowstate/src/collab`.

pub mod admission;
pub mod bluetooth;
pub mod crdt_runtime;
pub mod discovery;
pub mod doc_io;
pub mod dropbox;
pub mod flow;
pub mod identity;
pub mod ids;
pub(crate) mod io_util;
pub mod local_write;
pub mod net;
pub mod presence;
pub mod proto_direct;
pub mod proto_gossip;
pub mod self_check;
pub mod ticket;

pub use admission::SessionAdmission;
pub use ids::{BlobId, SessionId};
pub use proto_direct::DIRECT_ALPN;
pub use proto_gossip::PROTOCOL_VERSION;
