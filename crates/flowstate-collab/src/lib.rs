//! Peer-to-peer collaboration support for Flowstate rich-text documents.
//!
//! This crate contains the GPUI-free collaboration core: the Loro-native CRDT
//! runtime, transport protocol types, presence, and networking state.
//! Application/UI integration lives in `crates/flowstate/src/collab`.

#[cfg(not(target_family = "wasm"))]
pub mod admission;
#[cfg(not(target_family = "wasm"))]
pub mod bluetooth;
pub mod crdt_runtime;
#[cfg(not(target_family = "wasm"))]
pub mod discovery;
#[cfg(not(target_family = "wasm"))]
pub mod doc_io;
#[cfg(not(target_family = "wasm"))]
pub mod dropbox;
#[cfg(not(target_family = "wasm"))]
pub mod identity;
#[cfg(not(target_family = "wasm"))]
pub mod ids;
pub mod local_write;
#[cfg(not(target_family = "wasm"))]
pub mod net;
#[cfg(not(target_family = "wasm"))]
pub mod presence;
#[cfg(target_family = "wasm")]
#[path = "presence_core.rs"]
pub mod presence;
#[cfg(not(target_family = "wasm"))]
pub mod proto_direct;
#[cfg(not(target_family = "wasm"))]
pub mod proto_gossip;
#[cfg(not(target_family = "wasm"))]
pub mod self_check;
#[cfg(not(target_family = "wasm"))]
pub mod ticket;

#[cfg(not(target_family = "wasm"))]
pub use admission::SessionAdmission;
#[cfg(not(target_family = "wasm"))]
pub use ids::{BlobId, SessionId};
#[cfg(not(target_family = "wasm"))]
pub use proto_direct::DIRECT_ALPN;
#[cfg(not(target_family = "wasm"))]
pub use proto_gossip::PROTOCOL_VERSION;
