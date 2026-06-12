# Flowstate collaboration data-loss / divergence bug report

This report summarizes the observed collaboration bug, the code paths that appear responsible, and architectural solution options.

## Executive summary

The canary run shows two interacting problems.

Update: Option D is now the locked-in target architecture. The tested reproduction used the character `s`, but the bug class applies to character insert operations generally: any repeated/plain character insert that travels through the DB8 canonical-operation plus granular-source-mutation path can trigger the same divergence, lag, or repair behavior.

First, ordinary held-key character insertion input is correctly converted into durable granular Loro updates. The non-host log repeatedly shows `publish_db8_edit application=db8_ops:28b source_mutations=1 repair_required=false`, `publish_db8_granular_enter mutations=1 kinds=insert_text`, and `client_publish_granular_source_mutations bytes=94/95 mutations=1`. So the text path is not merely sending app-only hints.

Second, the collaboration stream can still collapse because presence updates are treated as connection-critical traffic. The workspace queues presence updates on ordinary editor observation, and the sync server rate-limits presence with a fatal `ensure!`. The non-host log shows repeated `client_local_update_error failed to write Flowstate frame length: connection lost: closed by peer: 0` after `presence hint` queue entries. That means lossy cursor/presence traffic can close the same stream used for durable edits.

Third, the visible editor is often advanced using `Db8CanonicalOperations` rather than by materializing the durable Loro source. In `apply_collaboration_source_to_panel`, DB8 remote source updates with an application payload attempt `editor.apply_remote_operations(...)`; if that succeeds, the function returns without applying the materialized source. If the application payload was already remembered, it skips source application entirely. That makes the application-op path more authoritative than it should be. A duplicated/missed/out-of-phase application op can cause the visible document and source document to diverge.

The clean architectural fix is: presence must be lossy and isolated; durable updates must be prioritized and never dropped behind presence; and application operations must be treated only as visual acceleration, never as a substitute for durable source reconciliation.

## Observed symptoms

1. Non-host holds test character. Both screens initially show repeated character inserts.
2. Eventually one peer’s UI stops tracking the other peer’s character insert edits.
3. The non-host may continue to show local text that later disappears when it reconnects or reconciles to the host/source state.
4. In the canary run, a more severe variant occurred: the non-host and host had different caret positions and insert locations. Example: the non-host thought it inserted before `B` in `Brigham Uni`, while the host inserted between `U` and `n`.
5. The canary run showed the non-host terminal freezing while the host continued updating. This is consistent with the non-host’s local UI/update loop being overloaded by logging, queued outbound work, presence churn, or reconnect/replacement work while the host still receives some already-sent durable updates.

## Files and code paths involved

### Text input and edit capture

`crates/flowstate/src/rich_text_element/action_handlers.rs`

- Lines 220-254: printable key input is read from `KeyDownEvent`; Windows applies Caps Lock, then calls `insert_text_command(&key_char, cx)` when the input is not routed to a table cell or equation.

`crates/flowstate/src/rich_text_element/edit_pipeline.rs`

- Lines 4-20: `insert_single_grapheme_fast_path` accepts plain-text caret inserts.
- Lines 39-46: the editor mutates the local document and advances the selection.
- Lines 48-64: the edit record contains both an `EditOperation::InsertText` and a `CanonicalOperation::InsertText`.
- Line 71: the editor records the collaboration edit via `mark_document_changed_with_reconcile(...)`.

`crates/flowstate/src/rich_text_element/collaboration.rs`

- Lines 300-352: `CanonicalOperation` includes `InsertText`, `DeleteRange`, style ops, and structural fallback ops.
- Lines 506-523: canonical operations are encoded/decoded for the `Db8CanonicalOperations` application payload.
- Lines 531-560: durable DB8 source mutations include `InsertText`, `DeleteText`, marks, unmarks, and metadata changes.
- Lines 572-580: `Db8CollabAdapter::adapt(...)` adapts canonical operations into durable source mutations.
- Lines 582-598: `CanonicalOperation::InsertText` becomes `Db8CollabSourceMutation::InsertText`; non-default styles mark `repair_required = true`, but plain character insert with default styles should not require repair.

`crates/flowstate-collab/src/source.rs`

- Lines 546-561: `apply_granular_source_mutations` applies the granular mutations, commits the Loro doc, validates schema, and exports Loro update bytes.
- Lines 761-780: `import_update_checked` imports Loro update bytes and reports a patch if the frontier changed.

Conclusion: plain repeated character insertion input should normally produce real Loro update bytes.

### Outbound workspace queue and presence interleaving

`crates/flowstate/src/workspace/workspace/documents.rs`

- Lines 1031-1042: every editor observation calls `publish_db8_presence(...)`, `maybe_autosave_document(...)`, and `publish_db8_collaboration_edit(...)`.
- Lines 2112-2123: `publish_db8_presence` immediately queues or sends a `PendingCollaborationUpdate::Presence`.
- Lines 2327-2376: `publish_db8_collaboration_edit` reads `last_collaboration_edit`, logs `publish_db8_edit`, clears the captured edit, and uses the granular path when `repair_required == false` and an application exists.
- Lines 2475-2524: `publish_db8_granular_collaboration_update` queues or sends `PendingCollaborationUpdate::GranularMutations`.
- Lines 2837-2857: pending update variants include durable source updates, granular mutations, application hints, and presence.
- Lines 2947-2964: `collect_outbound_update_batch` pulls up to 128 queued updates and only merges adjacent granular-mutation updates. Presence is not coalesced here.
- Lines 520-550: the outbound loop processes queued updates in batch order. Presence and granular mutations share the same send loop. A failure in any item breaks the batch and stores `update_error`.

### Sync-layer rate limiting and fatal presence handling

`crates/flowstate-sync/src/lib.rs`

- Line 38: default presence limit is `600` messages/minute.
- Lines 618-645: `RateWindow::check` returns false when the configured event count is exceeded inside the window.
- Lines 2848-2853: `serve_live_stream` creates `presence_rate` and `asset_request_rate` windows.
- Lines 3047-3049: presence messages are rate-limited with `ensure!(presence_rate.check(...), "presence rate limit exceeded")`.
- Lines 3049-3052: presence identity is also validated as protocol-critical.
- Lines 3064-3065: accepted presence is published to the live-update hub.

Problem: `ensure!` inside the main stream handler returns an error from `serve_live_stream`. This can close the peer stream. Presence is cursor/viewport metadata and should be lossy; exceeding the presence limit should not close durable collaboration traffic.

### Durable update import and visible-editor application path

`crates/flowstate-sync/src/lib.rs`

- Lines 2127-2165: the client’s `publish_granular_source_mutations` applies granular mutations locally, exports non-empty Loro bytes, remembers the local update hash, and sends `WireMessage::Update { bytes, application: Some(...) }`.
- Lines 2167-2192: `publish_application_update` sends an application-only update with `bytes = Vec::new()` and `hash = blake3_hash(&bytes)`.
- Lines 2905-2926: the host receives `WireMessage::Update`, validates document ID, actor, update size, and update hash, and logs `host_handle_update`.
- Lines 2928-2963: empty bytes with an application payload are treated as application-only hints; the host publishes `LiveUpdate::wire` and `SessionEvent::UpdateHint`, ACKing with the current frontier.
- Lines 2965-2974: non-empty Loro bytes are imported into the host `CollabDocument`, producing `host_imported_update patch=true` when the frontier changes.
- Lines 2975-2989: if a patch exists, the imported update is published to live subscribers with its application payload.

`crates/flowstate/src/workspace/workspace/documents.rs`

- Lines 1874-1883: the host UI subscribes to `LiveUpdateHub`; `RecvError::Lagged` is only logged as `host_subscriber_lagged` and ignored.
- Lines 1892-1919: remote wire updates with empty bytes apply only the application payload to the visible panel.
- Lines 1921-1949: remote wire updates with non-empty bytes clone the host `document_state.document` and call `apply_collaboration_source_to_panel`.
- Lines 2074-2110: applied application payloads are remembered by a hash of the application bytes.
- Lines 2193-2200: if the application has already been applied, `apply_collaboration_source_to_panel` sets `collaboration_last_published_hash` and returns without materializing/applying the source.
- Lines 2207-2223: for DB8 source updates with `Db8CanonicalOperations`, the visible editor applies the decoded operations; if successful, it remembers the application and returns without applying the source materialization.
- Lines 2237-2251: source materialization/replacement only happens after the DB8 application-op fast path is not available or fails.
- Lines 2287-2324: application-only updates call `apply_remote_operations` and remember the application hash; if the DB8 operations fail, the application-only path errors because there is no durable source update attached.

Problem: the UI path treats application operations as sufficient for visible state. The Loro source is imported, but the visible editor may not be forced to reconcile with that source. A duplicate application payload can also suppress source application entirely.

## Canary evidence summary

The logs confirm the durable path exists and is active.

Host log counts:

- `sync::host_handle_update`: 553
- `sync::host_imported_update`: 553
- `workspace::host_remote_wire_update`: 553
- `workspace::host_apply_to_panel`: 553
- `workspace::apply_source_enter`: 553
- `workspace::apply_db8_ops`: 550
- `editor::apply_remote_operations_result`: 550
- `workspace::apply_source_skip_duplicate_application`: 3

Non-host log counts:

- `workspace::publish_db8_edit`: 521
- `workspace::publish_db8_granular_enter`: 521
- `workspace::publish_db8_granular_taken`: 521
- `sync::client_publish_granular_source_mutations`: 458
- `workspace::client_local_update_error`: 92
- `workspace::apply_db8_replace`: 1
- `editor::replace_document_from_collaboration`: 1

Important non-host pattern:

```text
workspace::publish_db8_edit application=db8_ops:28b source_mutations=1 repair_required=false
workspace::publish_db8_granular_enter mutations=1 kinds=insert_text application=db8_ops:28b
workspace::publish_db8_granular_client_queue mutations=1 application=db8_ops:28b
workspace::publish_db8_granular_taken source_mutations=1
workspace::client_local_update_dequeue presence hint
workspace::client_local_update_error failed to write Flowstate frame length: connection lost: closed by peer: 0
```

Important host pattern:

```text
sync::host_handle_update bytes=94 application=db8_ops:28b
sync::host_imported_update patch=true bytes=94
workspace::host_remote_wire_update ... bytes=94 application=db8_ops:28b
workspace::host_apply_to_panel ... application=db8_ops:28b
workspace::apply_source_enter ... application=db8_ops:28b
workspace::apply_db8_ops ops=1 bytes=28
editor::apply_remote_operations_result applied_any=true outcome=Applied
```

Important host divergence signal:

```text
workspace::apply_source_skip_duplicate_application db8_ops:28b
```

Interpretation:

1. The sender is producing real durable granular insert mutations.
2. The host imports real Loro update bytes.
3. The host visible editor commonly applies `Db8CanonicalOperations` and returns early, not source replacement.
4. Presence and durable edits share the same outbound path.
5. A presence error can close the connection, causing later durable writes to fail.

## Root-cause model

There are two concrete bugs and one broader architecture problem.

### Bug 1: presence can terminate durable collaboration

Presence is lossy metadata. It should not be capable of closing the session stream used for text edits. The current host stream handler rate-limits presence with a fatal `ensure!`. Under held-key typing, every editor observation publishes presence, so presence volume can exceed the rate limit. Once that happens, the host can close the stream, and the non-host sees `connection lost: closed by peer`.

### Bug 2: application payloads can suppress source reconciliation

For a durable DB8 update, the code imports the Loro bytes into the collaboration source, but the visible editor often applies the application payload and returns early. If the same application payload is already remembered, it returns even earlier and skips source materialization entirely. This makes visual operations a substitute for source reconciliation. That violates the intended architecture where Loro is source of truth.

### Broader architecture problem: two write paths without a strict authority boundary

The system currently has:

- Durable Loro source updates.
- `Db8CanonicalOperations` application payloads for fast UI.
- Application-only hints with empty durable bytes.
- Presence updates mixed into the same outbound flow.

Those paths are useful, but their authority boundaries are not strict enough. Application payloads should optimize display latency, not define document truth. Presence should not be lossless or fatal. Durable Loro updates should be the only truth-bearing document state.

## Architectural requirements for the fix

1. Durable source mutations must never be dropped behind presence.
2. Presence must be lossy, coalesced, and non-fatal.
3. Application operations must never suppress durable source reconciliation.
4. If the UI fast path is used, it needs a confirmation/reconciliation mechanism against the source frontier/hash.
5. Stale presence/caret positions must be ignored or downgraded if their frontier is behind the visible/materialized document.
6. Queue overflow policy must distinguish lossy metadata from durable edits.
7. Repairs must materialize from source truth, not from stale application operation order.

## Solution options

### Option D — LOCKED-IN TARGET: full structural CRDT UI integration

Make the visible editor read/write directly against granular CRDT-backed source structures, or maintain an incremental source projection that is driven by Loro operations rather than by parallel app ops. This is the selected long-term architecture, not just an option. It addresses the general character-insert bug class, not only the tested `s` reproduction.

Mechanics:

- Editor character insert operations become structural CRDT mutations directly.
- Other editor operations become structural CRDT mutations where the schema supports them; unsupported operations force explicit source repair rather than implicit app-op authority.
- Remote Loro patches update the editor projection incrementally.
- Application operations are removed as a truth path and may remain only as local animation/latency hints.
- Presence is anchored to CRDT object IDs/frontiers and is ignored or downgraded when stale.
- Presence delivery remains lossy and non-fatal; durable source operations remain ordered, retained, and repairable.

Pros:

- Best long-term model.
- Removes dual-authority drift.
- Merges become explicit and deterministic at the source/model layer.
- Better foundation for complex collaborative editing.

Implementation notes for locked-in Option D:

1. Treat `Db8CollabSourceMutation` / granular source updates as the model-level operation stream for text, including all plain character inserts.
2. Move the visible editor toward an incremental projection of the granular source rather than a separate document model advanced by `Db8CanonicalOperations`.
3. Keep `Db8CanonicalOperations` only as an optional local UI acceleration payload. It must never be required for correctness, must never suppress source reconciliation, and must never be the only record of a character insert.
4. Add frontier/source-hash checks around every remote projection update. If a projected editor state is behind or inconsistent, repair from Loro source immediately.
5. Make presence lossy, coalesced, and non-fatal before or alongside the projection rewrite, because presence stream closure can still interrupt durable edits during the transition.
6. Add regression tests that hold a repeated character key and also test other character insert streams, not only `s`. The invariant is that every accepted local character insert either becomes durable source state or is visibly rejected; it must not persist optimistically and later disappear.
