Yes. Preferred answers:

Snapshot recovery: request fresh snapshot in-session via WireMessage::Need { snapshot: true, ... }. Do not enter Failed for ordinary incompatible-frontier recovery. Existing host path already handles Need: snapshot: true sends full snapshot; snapshot: false sends incremental update since frontier when possible. Client already handles Snapshot/SnapshotChunk and emits SessionEvent::SnapshotApplied.

Recommended state flow:

compute_paragraph_changes -> incompatible frontier
-> mark projection as Recovering
-> send WireMessage::Need { snapshot: true }
-> suppress further projection applies or queue updates
-> receive Snapshot / SnapshotChunk
-> replace local CRDT state
-> rebuild editor projection + CRDT identity map once
-> set collaboration_last_frontier
-> emit/use SnapshotApplied
-> return to Live

Use Failed only when recovery transport fails, snapshot is invalid, protocol mismatches, or retry budget is exhausted. Reconnect is last resort, not normal frontier repair. This matches original plan, which explicitly says diverged clients should request fresh snapshot using WireMessage::Need.

One implementation gap: live client currently treats incoming WireMessage::Need as Continue; it needs an outbound method or command path to send Need. Existing host support means protocol redesign is unnecessary.

Structural operations: text/paragraph mappings alone are not sufficient for full contract.

Option D does make canonical operations non-authoritative remotely, but local edits still must generate lossless granular CRDT mutations. Any user-visible DB8 operation that changes durable document content needs schema/mutation coverage. Current adapter silently ignores block operations and ReplaceDocument; that loses local edits before they reach CRDT.

Required now:

Complete text operations.
Complete paragraph split/join/delete/span replacement, including text, marks, metadata, IDs, and order.
Add granular schema mappings for block insert/delete/move/replace if those operations are available in DB8 editor during collaboration.
Replace ReplaceDocument fallback with granular reconciliation: diff old/new durable records, then emit create/update/delete/reorder mutations.
Remove silent empty mutation results.

Current split/join mappings are insufficient: split only inserts an empty paragraph; join only removes second paragraph. They do not transfer text/styles. Cross-paragraph delete also only trims endpoint text and does not remove/interconnect intermediate paragraphs correctly.

Practical scope rule:

If operation can occur while DB8 collaboration is enabled and changes saved DB8 state, it must have granular CRDT representation in this phase.

Only operations impossible/disabled during collaboration may remain unmapped, and that must be enforced at UI/type boundary—not silently ignored in adapter. Current code and tests explicitly encode unsupported block operations/ReplaceDocument as absent or no-op, which violates contract.
