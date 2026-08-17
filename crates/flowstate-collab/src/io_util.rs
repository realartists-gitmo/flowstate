//! Shared plumbing for the background I/O services (`doc_io`, `flow/flow_io`):
//! the gate-held call helper and the reply-channel send. Only these two are
//! shared — the services themselves stay independent (the .db8 coalescer is
//! battle-tested and is not generalized; the flow service mirrors its shape).

use std::sync::Arc;

use anyhow::Result;
use async_channel::Sender;

use crate::local_write::{GateHolder, WriteGate};

/// Send a reply, tolerating a caller that gave up waiting.
pub(crate) fn send_reply<T>(reply: &Sender<Result<T>>, value: Result<T>) {
  if let Err(error) = reply.try_send(value) {
    tracing::warn!(%error, "I/O reply channel dropped before the reply was sent");
  }
}

/// One gate-held call: acquire as `holder`, run, release. Every Loro touch is
/// a potential commit barrier (I-9a), so services never cache doc references
/// across calls.
pub(crate) fn gate_call<Runtime, T>(
  core: &Arc<WriteGate<Runtime>>,
  holder: GateHolder,
  call: impl FnOnce(&mut Runtime) -> Result<T>,
) -> Result<T> {
  let mut guard = core
    .lock(holder)
    .map_err(|poisoned| anyhow::anyhow!(poisoned))?;
  call(&mut guard)
}
