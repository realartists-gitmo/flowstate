//! Tiny helpers shared by the per-kind I/O services (`doc_io` keeps its own
//! battle-tested copies untouched; `flow/flow_io` uses these).

use std::sync::Arc;

use anyhow::Result;
use async_channel::Sender;

use crate::local_write::{GateHolder, WriteGate};

pub(crate) fn gate_call<C, T>(core: &Arc<WriteGate<C>>, holder: GateHolder, call: impl FnOnce(&mut C) -> Result<T>) -> Result<T> {
  let mut guard = core
    .lock(holder)
    .map_err(|poisoned| anyhow::anyhow!(poisoned))?;
  call(&mut guard)
}

pub(crate) fn send_reply<T>(reply: &Sender<Result<T>>, value: Result<T>) {
  if let Err(error) = reply.try_send(value) {
    tracing::warn!(%error, "I/O reply channel dropped before the reply was sent");
  }
}
