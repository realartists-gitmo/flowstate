# DB8 and FL0 collaboration architecture handoff

Date updated: 2026-05-31

Collaboration is still not product-ready. The repository now has a substantially stronger collaboration substrate: versioned Loro-backed source documents, recoverable projection caches, typed `.db8`/`.fl0` materialization bridges, real invite links, an invite capability registry, and an Iroh host/join snapshot smoke path. The remaining product work is to wire this into the GPUI workspace and replace projection-only editor mutation paths with real collaborative operations.

## Locked product constraints

These constraints remain fixed:

1. No runtime back compatibility. Old development files and fixtures should be regenerated or converted outside product runtime.
2. No MVP or staged behavior. The product is ready only when the full dev-to-dev workflow works in the built app.
3. `.db8` and `.fl0` remain user-facing extensions.
4. Synchronous collaborators receive the entire source of truth.
5. Distrusted parties receive exports, not sync access.
6. Owner/editor/viewer is the complete role model.
7. Document access implies access to all embedded assets in that document.
8. Fold/collapse, selection, focused flow, panel layout, scroll, and similar state are local only, except optional ephemeral presence.
9. Styles are semantic durable document data. Display is client-side.
10. Do not invent new competitive-debate semantics while implementing collaboration.

## Current readiness verdict

Not ready for dev-to-dev synchronous editing in the app.

Ready infrastructure now exists for:

- Loro snapshot source validation.
- Projection cache recovery from source.
- In-memory update convergence at the whole-projection source boundary.
- Viewer update rejection at the collab core boundary.
- Real invite URL encode/decode.
- Host-side invite capability validation.
- Iroh host/join snapshot transfer smoke testing.
- Default and all-features clippy.

Still missing for product readiness:

- Workspace share/join UI.
- App-level async session ownership.
- Editor mutation paths that emit/import CRDT transactions.
- Granular `.db8` and `.fl0` CRDT schemas.
- Peer fanout, reconnect, update gap repair, and asset transfer UX.
- Viewer read-only enforcement in every UI command path.
- Full convergence and network simulation tests.

## Completed architecture

### `flowstate-collab`

New file:

- `crates/flowstate-collab/src/source.rs`

Implemented types:

- `CollabRolePolicy`
- `CollabProjectionPatch`
- `CollabImportOutcome`
- `CollabDocument`
- `Db8CollabDocument`
- `Fl0CollabDocument`

Implemented Loro root schema:

- Root map: `flowstate`
- `schema_version`
- `format_kind`
- `document_id`
- `created_by_actor`
- `role_policy`
- `source_payload`
- `source_payload_hash`
- `projection_hash`
- `asset_manifest_hash`

Implemented behavior:

- `CollabDocument::from_projection_source(...)` creates a Loro source document from a DB8/FL0 projection payload.
- `CollabDocument::from_snapshot(...)` validates schema, expected format, and expected document ID.
- `materialize_projection_cache()` returns the durable source payload.
- `projection_hash()` returns the source-derived projection hash.
- `frontier()` serializes Loro `VersionVector` with postcard.
- `export_snapshot()` exports a Loro snapshot.
- `export_update_since_frontier(...)` exports Loro updates from a serialized frontier.
- `replace_projection_source(...)` is role-checked and exports the update since the previous frontier.
- `import_update_checked(...)` rejects viewer updates before mutation, validates the update against a cloned candidate snapshot, then imports into the live doc and returns a projection patch.

Envelope changes:

- `encode_native_file(...)` now builds snapshots through `source_snapshot(...)`.
- `projection_snapshot(...)` remains as a compatibility-named wrapper around `source_snapshot(...)`.
- `decode_native_file(...)` validates the Loro source with `CollabDocument::from_snapshot(...)`.
- Projection chunks are now treated as recoverable cache: if the stored projection chunk hash fails, decode materializes from Loro source.
- Snapshot corruption remains fatal.

New errors:

- `InvalidSchema`
- `MissingRootValue`
- `Unauthorized`
- `UnsupportedCollabSchema`

Tests added:

- Native envelope round trip.
- Wrong magic rejection.
- Wire message round trip.
- Corrupt projection cache rebuild from Loro source.
- In-memory update convergence through `CollabDocument`.
- Viewer update rejection before mutation.

Important limitation:

- The current Loro source payload is still a whole-projection payload. It is now source-of-truth inside Loro and can converge as Loro updates, but it is not yet the final granular CRDT schema for text, styles, objects, tables, assets, and flows.

### `flowstate-sync`

Dependencies added:

- `base64`
- `postcard`
- `serde`
- `tokio` as dev dependency for async tests

Invite architecture implemented:

- `FLOWSTATE_INVITE_PREFIX = "flowstate://collab/"`
- `InviteTicket` now includes endpoint address, document ID, format kind, invited role, capability, expiry, label, and multi-use flag.
- `encode_invite_link(...)`
- `decode_invite_link(...)`
- `RedactedInviteTicket`
- `InviteRegistry`

Invite capability behavior:

- Host issues capability tokens.
- Registry binds tokens to document ID, format kind, role, expiry, label, and multi-use policy.
- Invalid, expired, revoked, wrong-document, wrong-format, and role-escalating invites are rejected.
- Single-use invites are revoked on successful authorization.
- Secret capability bytes can be redacted for diagnostics.

Session/runtime infrastructure implemented:

- `SessionDocumentState`
- `SessionState`
- `SessionEvent`
- `JoinedSnapshot`
- `router_with_invites(...)`
- `connect_and_receive_snapshot(...)`

Protocol handler improvements:

- `FlowstateProtocol` now carries:
  - `config`
  - `role_policy`
  - `invite_registry`
  - optional `document_state`
- Accept path validates `Hello`, authorizes by invite registry first, falls back to `RolePolicy`, sends `Authorize`, then optionally sends snapshot and `Have`.
- Live stream handles:
  - `Update`: verifies hash, enforces remote role through `CollabDocument::import_update_checked`, sends `Ack`.
  - `Need`: sends snapshot or update since frontier.
  - `AssetNeed`: serves verified chunk from `AssetStore`.
  - `Presence`: sends `Ack`.

Tests added:

- Role policy rejects escalation.
- Asset chunk hash verification.
- Invite link round trip.
- Invite registry rejects tampered role escalation.
- Iroh host/join smoke test transfers a snapshot and materializes it on the joiner.

Important limitations:

- No long-running multi-peer fanout manager yet.
- No workspace event stream integration yet.
- No reconnect loop, durable update queue, or missing-update repair policy beyond the low-level `Need` response.
- No asset transfer progress model.
- No UI-facing peer list/session status wiring.

### `.db8` persistence bridge

File touched:

- `crates/flowstate-document/src/persistence/io.rs`

Dependency added:

- `postcard`

New public APIs:

- `db8_projection_cache_bytes(document)`
- `read_db8_projection_cache_bytes(bytes)`
- `db8_collab_document(document, created_by_actor)`
- `document_from_db8_collab_source(source)`

Implemented behavior:

- Existing native `.db8` save path still writes the collaboration envelope.
- A DB8 document can now be converted into a typed `Db8CollabDocument`.
- A validated `CollabDocument` source can be materialized back into a `Document`.
- Asset manifest records are shared through a dedicated helper.

Test added:

- DB8 collab source materializes back into a projection with matching document ID and paragraph count.

Important limitation:

- The editor still mutates `Document` projection state first. The bridge allows collaboration source creation/materialization, but active edits are not yet CRDT-first.

### `.fl0` persistence bridge

Files touched:

- `crates/flowstate-flow/src/persistence.rs`
- `crates/flowstate-flow/src/lib.rs`

New public APIs:

- `fl0_collab_document(document, created_by_actor)`
- `flow_document_from_collab_source(source)`

Implemented behavior:

- A `FlowDocument` can now be converted into `Fl0CollabDocument`.
- A validated `CollabDocument` source can be materialized back into `FlowDocument`.

Test added:

- FL0 collab source materializes back into an equivalent flow document.

Important limitation:

- `Action`/`ActionBundle` still mutate the local projection. They are not yet mapped into granular Loro transactions.

### Build and lint cleanup

Files touched:

- Root `Cargo.toml`
- `crates/flowstate-document/Cargo.toml`
- `crates/flowstate-docx/Cargo.toml`
- `crates/flowstate-flow/Cargo.toml`
- measured helper functions in document/flow/docx crates

Implemented behavior:

- Removed duplicate renamed `hotpath-cpu-support` dependencies that made all-features clippy fail before code analysis.
- `hotpath-cpu` features are now no-op feature chains on Windows instead of enabling `hotpath/hotpath-cpu`, which hard-errors on Windows.
- Changed hotpath-measured `const fn`s to normal functions because hotpath instrumentation cannot run in const contexts.

Verified commands:

- `cargo clippy --workspace --all-targets`
- `cargo clippy --workspace --all-targets --all-features`
- `cargo test -p flowstate-collab -p flowstate-sync -p flowstate-document -p flowstate-flow -p flowstate-docx`

## Current architecture boundary

The current architecture is best described as:

1. Existing editors produce projections.
2. Persistence can wrap those projections into validated Loro source snapshots.
3. Sync can authorize a peer and transfer a Loro source snapshot over Iroh.
4. A receiver can materialize the source into a projection.
5. In-memory whole-source updates can converge through Loro update import/export.

It is not yet:

1. Keystroke-level collaborative editing.
2. Granular object-level CRDT editing.
3. App-integrated peer sessions.
4. End-user share/join workflow.

## Remaining architecture work

### Replace whole-projection source payloads with granular CRDT schemas

The final product should not use `source_payload` as a whole projection overwrite for ordinary editing. Replace it with stable Loro containers.

DB8 required containers:

- Document metadata map.
- Role policy map.
- Block order movable list/tree.
- Block metadata map keyed by stable `BlockId`.
- Paragraph order and paragraph metadata keyed by stable `ParagraphId`.
- Loro text per paragraph ID.
- Paragraph style register/map per paragraph ID.
- Loro rich-text marks for semantic run style, direct underline, strikethrough, and highlight.
- Image map keyed by image/block ID, including asset ID, alt text, caption, sizing, alignment.
- Equation map keyed by block ID, including source, syntax, display.
- Table structure keyed by stable table/row/cell IDs.
- Loro text per table-cell paragraph ID.
- Asset manifest keyed by BLAKE3 content hash.

FL0 required containers:

- Root flow order movable list.
- Node existence/tree entries keyed by stable node ID.
- Per-parent movable child lists or Loro tree.
- Flow title/content Loro text.
- Flow invert register.
- Flow columns list/register with stable column identity.
- Box content Loro text.
- Box flag registers: `empty`, `crossed`, `bold`, `is_extension`.
- Placeholder register.

Migration strategy:

- Keep current projection materializers as load/save accelerators.
- Add granular schema version `2` or extend schema version `1` before external release.
- Remove product runtime paths that treat projection bytes as durable truth.
- Keep projection cache rebuild path.

### Implement DB8 transaction builders

Every durable edit must be expressed as a Loro transaction before mutating visible projection during active collaboration.

Required DB8 builders:

- Insert text.
- Delete text.
- Replace selection.
- Split paragraph.
- Join paragraphs.
- Set paragraph style.
- Apply/clear semantic run style.
- Apply/clear direct underline.
- Apply/clear strikethrough.
- Apply/clear highlight.
- Insert/delete/move block.
- Insert/delete/update image.
- Resize image.
- Edit image alt text.
- Edit image caption.
- Insert/edit/delete equation.
- Change equation display.
- Insert table.
- Insert/delete table row.
- Insert/delete table column.
- Edit table cell text.
- Insert/delete nested table.
- Replace full document as an explicit CRDT transaction for import or command-level replace.

Implementation detail:

- `CanonicalOperation` should stop being the network representation.
- Existing edit records may remain local undo/history adapters.
- `last_collaboration_edit` should be replaced by exported Loro update batches or a collaboration controller event.
- Remote updates should import into `CollabDocument`, materialize patches, then apply patches to `RichTextEditor`.

### Implement DB8 projection patching

Required patch types:

- Text dirty range by paragraph ID.
- Paragraph split/join.
- Paragraph style.
- Run style mark range.
- Block insert/delete/move.
- Image/equation/table payload.
- Asset availability.
- Section rebuild.
- Layout invalidation range.
- Selection/caret remapping for local user.

Initial acceptable strategy:

- Whole-document materialization on file load and snapshot join.
- Dirty-range materialization for ordinary text/style updates.
- Block-level rematerialization for image/equation/table updates.

Do not rematerialize the whole document on every remote keystroke.

### Implement FL0 transaction builders

Required FL0 builders:

- Create flow.
- Delete flow.
- Reorder flow.
- Create box.
- Delete box subtree.
- Move box under parent.
- Create extension box.
- Create empty spacer boxes.
- Edit flow title/content.
- Edit box content.
- Toggle bold.
- Toggle crossed.
- Set empty flag.
- Set extension flag.
- Set placeholder.
- Change columns.
- Change invert.
- Replace document as an explicit CRDT transaction.

Implementation detail:

- `Action` should remain a local command/history adapter.
- Every document-changing `Action` must map to one or more CRDT transactions during active collaboration.
- Index-based moves must resolve through stable IDs and Loro move semantics.

### Implement FL0 projection patching

Required patch types:

- Node text patch.
- Node insert/delete.
- Node move.
- Flow metadata.
- Box flags.
- Columns.
- Local focus preservation.

Folded/selected/focused state remains local unless emitted as presence.

### Build app-level collaboration controller

Required new runtime layer:

- `CollabSession`
- `PeerConnection`
- peer actor/session mapping
- session state machine
- outbound durable update queue
- inbound update batcher
- presence queue
- asset request queue
- event stream to GPUI workspace
- graceful shutdown

Required states:

- idle
- hosting
- joining
- syncing snapshot
- live
- reconnecting
- closed
- failed

Required events:

- session state changed
- peer joined
- peer left
- peer role changed
- snapshot applied
- update applied
- update rejected
- asset transfer progress
- asset transfer failed
- presence updated
- reconnecting
- fatal error

Implementation detail:

- Network I/O must stay off GPUI hot paths.
- GPUI entities should receive compact events and apply state updates on the UI thread.
- Session shutdown must run on document close and app exit.

### Workspace integration

Required workspace state:

- Active sessions by document/flow panel ID.
- Mapping from panel to `DocumentId`.
- Local role.
- Host/join mode.
- Peer list.
- Connection state.
- Last error.
- Pending asset status.
- Save state while collaborating.

Required commands:

- Start collaboration.
- Stop collaboration.
- Copy owner invite.
- Copy editor invite.
- Copy viewer invite.
- Join from invite.
- Disconnect.
- Reconnect.
- Change peer role as owner.
- Kick peer as owner.
- Copy diagnostic session info.

Required UI:

- Share menu.
- Join dialog/paste field.
- Invite role picker.
- Peer list.
- Session status indicator.
- Syncing/reconnecting/error states.
- Viewer/editor/owner badge.
- Asset transfer progress/error.
- Presence indicators in DB8.
- Presence indicators in FL0.
- Permission-denied message for viewer edits.
- Error detail panel for rejected imports.

Implementation detail:

- Use existing GPUI/gpui-component menu, dialog, button, badge, and status patterns.
- Avoid terminal-only collaboration controls.

### CLI and OS link integration

Required binary behavior:

- Accept a `flowstate://collab/...` invite link as a launch argument.
- Route invite links into workspace join flow.
- Register OS URL handler where platform support is available.
- Initialize async Iroh runtime safely with GPUI.
- Surface firewall/network failures without panic.

Implementation detail:

- Extend `Cli.path` handling so invite links do not go through file path open logic.
- Add a typed startup input enum: file path, invite link, empty workspace.

### Editor integration

DB8 editor required work:

- Add collaboration controller handle to `RichTextEditor`.
- On active collaboration, convert every local mutation to CRDT transaction.
- Export generated update batch to session.
- Apply remote projection patches.
- Preserve local selection through remote edits.
- Enforce viewer read-only in keyboard, paste, drag/drop, object edit, undo, redo, and programmatic command paths.
- Ensure autosave/recovery writes Loro source state.

DB8 paths to audit:

- fast text typing
- paste rich fragment
- delete/backspace
- paragraph split/join
- paragraph style changes
- run style changes
- highlight changes
- invisibility mode transformations
- block insertion
- object selection delete
- image insert/resize/caption/alt
- equation insert/edit/delete
- table insert
- table row/column/cell edits
- nested table edits
- send/export paths
- recovery writes

FL0 editor required work:

- Add collaboration controller handle to `FlowEditor`.
- Map `ActionBundle` to CRDT transactions.
- Replace whole-node text updates with Loro text edits during active collaboration.
- Apply remote patches to GPUI inputs.
- Preserve local focus/selection where possible.
- Enforce viewer read-only for every command path.
- Ensure undo/redo emits CRDT operations.

FL0 paths to audit:

- add flow
- delete flow
- move flow
- add box
- delete box subtree
- move box
- extension box creation
- empty box creation
- box text input
- flow title input
- bold/crossed toggles
- column changes
- debate style template application

### Asset exchange completion

Current state:

- `AssetStore` can hash, store, chunk, and verify complete chunks.
- Protocol can answer `AssetNeed`.

Remaining work:

- Build session-level asset index from DB8 manifest.
- Advertise assets after handshake.
- Request missing assets by hash/range.
- Stream multi-range assets.
- Retry missing ranges.
- Verify final BLAKE3 hash.
- Reject unknown assets unless CRDT manifest references them.
- Enforce max chunk size and max in-flight requests.
- Surface missing-asset status in UI.

### Permissions and security

Current state:

- Core role model exists.
- Invite registry rejects role escalation.
- `CollabDocument` rejects viewer durable update import/export.

Remaining work:

- Enforce viewer read-only in every UI command path.
- Enforce role before serving assets.
- Owner role changes.
- Peer role downgrade/upgrade.
- Kick peer.
- Revoke invite from UI.
- Persist role policy hash coverage in envelope/source.
- Validate max frame, max snapshot, max update batch, max peer count, max queue sizes, max presence rate, max asset request rate.

Malformed input that must be tested:

- invalid postcard message
- invalid Loro update
- wrong document ID
- wrong format kind
- wrong schema
- wrong role
- unknown asset hash
- out-of-bounds asset range
- corrupt asset hash
- corrupt projection cache
- corrupt snapshot
- oversized message

### Persistence and recovery

Current state:

- Native envelopes store Loro source snapshots.
- Projection cache can be rebuilt from Loro source.

Remaining work:

- Store real granular Loro snapshots.
- Store recent updates only when useful and hash-covered.
- Store snapshot frontier.
- Store role policy hash.
- Store complete asset manifest hash.
- Compact updates into snapshot on save.
- Recovery writes must include Loro source state.
- Recovery must not write partial imported updates.
- Recovery must not persist live invite secrets unless explicitly intended.
- Loading recovery must validate snapshot before projection.

### Search, export, and tub

Required checks:

- `flowstate-tub` indexes materialized projections only.
- Global search reads current collaboration envelope.
- Send/export output is generated from projection, not hidden sync state.
- Distrusted-party exports do not include invite capabilities.
- PDF embedded DB8 uses collaboration-native bytes.
- DOCX/PDF conversion rejects corrupt collaborative DB8 input cleanly.

### Tests still required

Unit tests:

- Real DB8 granular source validation.
- Real FL0 granular source validation.
- Projection cache rebuild from granular Loro.
- Corrupt snapshot fatal.
- Corrupt manifest rejected.
- Corrupt asset bytes rejected.
- Invite tampering for document/format/role/capability.
- Viewer UI command rejection.
- Owner role changes.
- Presence not persisted.
- Asset range bounds.
- Oversized frame rejected.

DB8 convergence tests:

- Concurrent text insert same paragraph.
- Concurrent delete/insert same paragraph.
- Concurrent paragraph split/join.
- Concurrent paragraph style changes.
- Concurrent overlapping run styles.
- Concurrent highlight changes.
- Concurrent block insert/delete/move.
- Concurrent image insert/delete/resize/caption.
- Concurrent equation edits.
- Concurrent table row/column/cell edits.
- Concurrent nested table edits.
- Missing asset repair.
- Deterministic section rebuild.
- Projection hash equality after randomized schedules.

FL0 convergence tests:

- Concurrent box text edits.
- Concurrent flow title edits.
- Concurrent add under same parent.
- Concurrent delete/move under same parent.
- Concurrent flow creation.
- Concurrent flow reorder.
- Concurrent flag toggles.
- Concurrent column changes.
- Delete parent while another peer edits child.
- Projection hash equality after randomized schedules.

Network tests:

- Host/join success.
- Wrong document rejection.
- Wrong role rejection.
- Late join from empty state.
- Reconnect from stale state.
- Duplicate update delivery.
- Out-of-order legal delivery.
- Dropped update repaired by `Need`.
- Snapshot fallback.
- Presence exchange.
- Asset exchange.
- Host disconnect.
- Joiner disconnect.
- Multi-peer fanout.

UI/integration tests:

- Share menu creates invite.
- Join link opens panel.
- Peer list updates.
- Viewer cannot edit.
- Editor can edit.
- Presence visible.
- Reconnect status visible.
- Asset missing/error status visible.
- Save while collaborating.
- Close while collaborating.

### Performance work

Required measurements:

- Local keystroke latency with collaboration enabled but no peers.
- Local keystroke latency with one peer.
- Remote keystroke materialization latency.
- Large DB8 snapshot load from Loro.
- Large DB8 projection cache load.
- Large FL0 load.
- Update import batch cost.
- Projection patch cost.
- Memory growth during long typing session.
- Snapshot compaction cost.
- Asset transfer throughput.
- UI frame stability while importing updates.

Targets:

- Local ordinary keystroke has no meaningful regression outside collaboration overhead.
- Remote ordinary keystroke materializes under one frame for normal paragraph edits.
- Import batching stays off GPUI render hot path.
- Large load has bounded overhead over projection-cache load.
- Memory is bounded by save-time compaction.
- Network does not send packet-per-character under normal typing.
- Layout invalidates only affected paragraphs/blocks for ordinary edits.

## Final acceptance checklist

The product is collaboration-ready only when all of this works in built binaries:

1. Dev A creates or opens `.db8`.
2. Dev A starts owner session.
3. Dev A copies editor invite.
4. Dev B opens invite.
5. Dev B receives complete source of truth.
6. Both edit text and semantic styles concurrently.
7. Both edit blocks, images, equations, and tables concurrently.
8. Both converge after every operation.
9. Save/reload preserves source and projection.
10. Disconnect/reconnect catches up from stale state.
11. Viewer invite allows observation but no durable edits.
12. Presence shows partner location/focus and is not saved.
13. Embedded assets transfer and verify.
14. Corrupt/tampered invite fails.
15. Invalid update fails without mutation.
16. Export/send output works and grants no sync access.
17. Repeat the workflow for `.fl0`: flows, boxes, moves, text, flags, columns, save, reload, reconnect, viewer mode.

Until this checklist passes, Flowstate has collaboration infrastructure, not collaboration-ready synchronous editing.
