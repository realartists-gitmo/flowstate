//! The FLOW runtime (flow architecture spec Part 2.2): the collaborative
//! entrance to the flow's one intent executor. Mirrors the .db8 split —
//! [`FlowRuntime`] owns the Loro doc, projection caches, ordered streams, the
//! publish queue and undo, all behind the generic [`WriteGate`]; the
//! [`FlowDocHandle`] is the app-facing authority. Transport carries opaque
//! Loro bytes, so everything from `net/` down is shared with .db8 unchanged.

mod handle;
mod runtime;
#[cfg(test)]
mod tests;

pub use handle::{FlowDocHandle, FlowWriteRejected};
pub use runtime::{FlowLocalOutcome, FlowPublishEvent, FlowRuntime, FlowStreamItem};

pub use crate::local_write::WriteGate;
