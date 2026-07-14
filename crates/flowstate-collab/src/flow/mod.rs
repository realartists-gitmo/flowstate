//! The .fl0 CRDT-first write path (flow spec Part B) — the flow mirror of the
//! .db8 `local_write`/`crdt_runtime` split, at board scale.
//!
//! One law, reused: every local mutation is a typed [`flowstate_flow::FlowIntent`]
//! applied through [`FlowDocHandle`] → `WriteGate<FlowRuntime>` (the SAME
//! generic gate the .db8 core uses) → resolve → mutate → ONE commit (origin
//! `"local"`, message = intent class) → in-place projection derivation → the
//! ordered streams → the publish queue. Remote traffic reaches the same doc
//! only through the flow I/O service, one import chunk per gate hold.
//!
//! Streams (single-ordered-stream law per consumer):
//! * ONE board stream (`FlowStreamItem::Board`, Replace-per-change — board
//!   metadata is ~100 bytes/cell with shared `Arc<str>` summaries), drained by
//!   the `FlowEditor`.
//! * ONE stream per OPEN cell (`ProjectionStreamItem`, whole-cell `Replace` —
//!   cells are tiny, the .db8 body patch synthesis is deliberately not
//!   ported), drained by that cell's `RichTextEditor` via its
//!   [`cell_authority::FlowCellAuthority`].

pub mod cell_authority;
pub mod cell_text;
pub mod commit;
pub mod flow_io;
pub mod handle;
pub mod runtime;

pub use cell_authority::FlowCellAuthority;
pub use flow_io::{FlowIoHandle, FlowIoService};
pub use handle::FlowDocHandle;
pub use runtime::{FlowPublishEvent, FlowRuntime, FlowStreamItem, FlowUndoMeta, FlowUndoOutcome, FlowWriteOutcome, FlowWriteRejected};
