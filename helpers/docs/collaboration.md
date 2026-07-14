# Flowstate Collaboration

Flowstate collaboration is a transport and presence layer around the same Loro-native document used for local editing and persistence. It does not introduce a second document model, operation log, or save format.

## Canonical data flow

Local editing:

1. `RichTextEditor` resolves input against its current `DocumentProjection` and queues semantic command batches tagged with that projection frontier.
2. The document or session owner sends those commands to the document's single `CrdtRuntimeHandle`.
3. The runtime rejects stale-frontier commands, mutates its owned `LoroDoc`, commits the semantic undo group, and emits a frontier-scoped projection patch or snapshot.
4. The UI applies only that derived projection result. Loro update bytes are persisted and, for a live session, published to peers.

Remote editing:

1. The session imports received update bytes directly into the runtime's `LoroDoc`.
2. Pending dependency status triggers immediate version-vector anti-entropy.
3. The runtime's permanent subscription produces projection invalidations and update events.
4. The UI applies derived projection patches or a defensive full projection rebuild.

Image bytes are synchronized separately through the content-addressed asset store. Loro stores BLAKE3 identities and image metadata; missing bytes render as recoverable placeholders.

## Ownership

Each open document has one CRDT runtime actor. The actor owns:

- the canonical `LoroDoc`;
- permanent Loro subscriptions;
- the per-replica Loro `UndoManager`;
- package persistence and revision checkpoints;
- projection construction and invalidation;
- update import/export and asset metadata coordination.

A joined collaboration tab installs the session's existing runtime handle directly. It does not create a temporary mirror runtime.

## Presence

Presence is ephemeral. Carets and selections are encoded as Loro cursors with affinity, visual gravity, and direction. User identity is distinct from replica identity; every active replica uses a unique Loro peer ID.

## Persistence

Saved `.db8` files are the chunked Loro-native Flowstate package. Collaboration does not change the file format and does not maintain session-only canonical history. Explicit saves create package checkpoints/revision boundaries. PDF source recovery embeds only these package bytes.

## Verification

- `crates/flowstate-collab/src/crdt_runtime.rs` contains runtime, projection, undo, table, revision, and anti-entropy coverage.
- `crates/flowstate-collab/src/crdt_runtime_actor.rs` covers actor serialization and stale-frontier error preservation.
- `crates/gpui-flowtext/src/rich_text/tests/collab_capture.rs` covers semantic command capture and projection patch application.
- `helpers/docs/collab_qa.md` contains the manual multi-peer QA matrix.
