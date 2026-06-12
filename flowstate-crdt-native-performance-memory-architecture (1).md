# Flowstate CRDT-Native Architecture: Authority, Responsiveness, and Memory Recovery

## Purpose

This document specifies the architectural changes required to keep Flowstate fully CRDT-native while restoring the editor qualities that mattered before the CRDT migration: immediate editing, fast scrolling, low latency, low memory usage, and high runtime performance.

The target is not to fall back to the old projection-diff collaboration model. The target is to make CRDT the native source of truth without forcing the editor to carry duplicate full-document representations or rebuild the whole document on startup, scrolling, editing, save, or sync.

The central rule is:

**CRDT-native means every semantic mutation commits to the CRDT source first. It does not mean every layer stores a full copy of the whole document.**

The editor may keep its mature projection, layout, virtualization, selection, and rendering structures as caches over the CRDT source. Those caches should be fast, local, and disposable. The CRDT source is authoritative; the projection is the UI cache.

---

## Current Failure Mode

The current vNext push moves in the right direction conceptually, but it introduces a practical anti-pattern:

```text
Loaded DB8 projection
+ CRDT/Loro source (built from seed of loaded projection)
+ full projection clone on every local edit (finish_source_commit)
+ full projection clone on every incremental materialization attempt
+ preparing authority installed while background init runs
+ real authority installed without generation guard (stale race possible)
+ full Document carried inside every Db8ProjectionDelta
+ snapshot/update byte buffers
+ scroll/layout caches
```

This creates three classes of failure.

First, startup and open become expensive because building a `FlowDocumentSeed` from the loaded projection serializes it, then constructs a new `LoroDoc` and runs schema validation. Although `from_existing_projection` correctly reuses the loaded projection instead of rematerializing, the seed-build phase itself is heavy for large documents and runs on the background executor with the full cloned projection. If the file lacks a vNext snapshot, there is no way to avoid this cost.

Second, edits become non-functional if the editor installs a preparing authority, a stale authority build wins a race (no generation counter guards `install_db8_authority_result`), or a replica lease failure calls `block_local_edits` which blocks all local source edits. Caret movement and selection still work because those are projection-local. Actual edits do nothing because `apply_source_request` checks `commit_outbox_error` first and returns a recovery response.

Third, memory usage grows because `finish_source_commit` clones the entire projection before every edit, `materialize_incremental_root_projection` clones it again for windowed patching, and `Db8ProjectionDelta` stores yet another full `Document` clone. For a 200 MB document, a single keystroke temporarily holds three full projection copies (before, patched, and delta).

---

## Non-Negotiable Goals

The following goals must all hold at the same time.

1. CRDT source is the only authoritative mutation target.
2. Local edits never mutate the visible document first and infer CRDT operations later.
3. Remote edits never patch the projection through a separate non-CRDT semantic path.
4. Opening a document must paint quickly from a projection cache or loaded DB8 projection.
5. Typing must be accepted only when the CRDT authority is ready, but readiness must occur quickly and must not require full rematerialization.
6. Scroll performance must remain primarily governed by the existing editor projection/layout virtualization, not by CRDT traversal.
7. Memory must be close to the old editor memory plus bounded CRDT overhead, not old editor times several full copies.
8. Full materialization is recovery-only, not the normal edit/open/render path.
9. Authority initialization must be single-writer, latest-generation-wins, and must not leave stale preparing controllers installed.
10. Autosave, snapshots, and retained updates must have explicit retention and compaction rules.

---

## Correct Layering

The stable architecture should have these layers.

```text
CRDT Source Layer
  Durable FlowDocument / Loro source
  Stable FlowId / FlowNodeId / text anchors / causal frontier
  Owns semantic mutation and import/export

Projection Index Layer
  Lightweight mapping between CRDT nodes/anchors and editor coordinates
  Paragraph ID -> projection paragraph index
  Block ID -> projection block index
  CRDT changed range -> projection window
  Does not own full duplicate document content

Editor Projection Layer
  Existing high-performance Document / paragraphs / blocks / rope / run structures
  Existing layout and scroll virtualization
  Disposable cache derived from CRDT source plus file projection cache

UI/Layout Layer
  Existing GPUI editor rendering, selection, scroll, outline, etc.
  Reads projection cache, sends source edit requests
```

The CRDT source is authoritative. The projection is allowed to exist because an editor needs a fast rendering model. But the projection must be treated as a cache, not a second authority and not a permanent duplicate created from the CRDT after every open.

---

## Opening Documents

### Required behavior

Opening a DB8 file should do this:

```text
read file
load projection cache / DB8 projection
paint editor immediately
start CRDT source initialization in background (from_existing_projection or from_snapshot_with_projection)
install CRDT authority when ready, guarded by generation counter
allow edits only after authority is ready
```

Opening must not do this:

```text
read DB8 projection
build CRDT source
materialize whole CRDT source back into another DB8 projection
replace editor document
invalidate layout caches
then paint
```

### DB8 without saved vNext snapshot

For an old DB8 file without a saved vNext snapshot:

1. Load the DB8 projection normally.
2. Create a `PreparingDb8EditorAuthority` that is explicitly read-only.
3. Spawn background initialization.
4. Build a `FlowDocumentSeed` from the loaded projection.
5. Construct `FlowDocument::from_seed`.
6. Create `Db8DocumentController` using the already-loaded projection, not by rematerializing from source.
7. Install `Db8EditorAuthority` only if the panel generation still matches.

The required constructor is conceptually:

```rust
Db8DocumentController::from_existing_projection(
    projection: Document,
    actor_id: ActorId,
    replica_id: ReplicaId,
) -> io::Result<Self>
```

It should normalize the document only once, build the seed, build the CRDT source, create undo manager, and then keep the existing projection as the projection cache.

It must not call `materialize_db8_flow_document`.

### DB8 with saved vNext snapshot

For a DB8 file with an embedded vNext snapshot:

1. Load the DB8 projection cache immediately.
2. Load/import the CRDT snapshot in background.
3. Create `Db8DocumentController::from_snapshot_with_projection(snapshot, projection, ...)`.
4. Do not also build a seed-derived authority.
5. Do not rematerialize projection unless the snapshot/projection hash says the projection cache is stale or corrupt.

This avoids duplicate authority construction and avoids replacing the visible projection when the loaded projection is already valid.

### Projection verification

The file envelope should carry a projection hash, source hash, or both.

On open:

- If projection hash matches source/projection metadata, trust the projection cache.
- If it does not match, build a temporary recovery projection off the UI thread.
- If recovery succeeds, swap projection once and invalidate only necessary caches.
- If recovery fails, show recovery error and keep document read-only.

---

## Authority Lifecycle

### Single authority per panel

Each document panel must have exactly one active authority lifecycle. Use a generation counter.

```rust
struct DocumentPanelState {
    authority_generation: u64,
    authority: Rc<RefCell<dyn AuthoritativeEditController>>,
    authority_status: AuthorityStatus,
}
```

Starting an authority build increments generation. The background task captures that generation. Completion installs the result only if the generation still matches.

```rust
let generation = panel.bump_authority_generation();
spawn(async move {
    let result = build_authority(...).await;
    install_if_generation_matches(panel_id, generation, result);
});
```

Stale completions must be ignored and logged.

Required canaries:

- `db8_authority_init_started`
- `db8_authority_init_finished`
- `db8_authority_init_ignored_stale`
- `db8_authority_installed`
- `db8_authority_init_failed`

### Preparing authority

`PreparingDb8EditorAuthority` is acceptable only as a short-lived read-only placeholder. It must:

- Allow selection anchoring only if possible.
- Reject source edits with a visible reason: `CRDT source still preparing`.
- Never silently no-op edits without surfacing the state.
- Be replaced once the real authority is ready.

The editor UI should expose this state so the user understands why edits are temporarily disabled.

### Lease failure must not block local editing

Replica/outbox leases should protect durable update lineage, not ordinary local CRDT editing.

Wrong behavior:

```text
lease acquisition failed -> block all local edits
```

Correct behavior:

```text
lease acquisition failed -> local CRDT edits remain possible
lease acquisition failed -> durable outbox / collaboration publishing is disabled
lease acquisition failed -> diagnostics show outbox persistence unavailable
```

A local user should be able to edit a local document even if durable collaboration outbox locking fails. The failure should block only the thing the lease protects.

---

## Editing Path

### Required local edit path

Every local edit should follow this path:

```text
Editor input event
  -> AuthoritativeSourceEditRequest
  -> Db8EditorAuthority.apply_source
  -> Db8DocumentController converts request to FlowEdit
  -> FlowDocument.apply_edits commits to CRDT
  -> FlowChangeSummary returned
  -> projection window patched from CRDT change summary
  -> editor applies projection delta
  -> commit queued for sync/outbox/save
```

No projection-diff guessing. No byte-offset-only late capture. No editor-first mutation that is later translated into source ops.

### Responsiveness rule

This path must stay sub-frame for normal edits. For local typing:

- One character insert should not validate or materialize the whole document.
- One style toggle should not scan all marks in the document.
- One paragraph split should not rebuild every paragraph.
- One table cell edit should patch only the containing object/root block.

If an edit cannot be committed to CRDT quickly, the system must queue/retry or show a clear degraded-state diagnostic. It must not silently drop the edit.

### Transaction requirements

Each edit transaction should carry:

```rust
struct SourceEditTransaction {
    base_frontier: Vec<u8>,
    edits: Vec<FlowEdit>,
    touched_flows: SmallVec<[FlowId; N]>,
    touched_nodes: SmallVec<[FlowNodeId; N]>,
    projection_impact_hint: Option<ProjectionImpactHint>,
    undo_group: UndoGroupKind,
}
```

The transaction should produce:

```rust
struct SourceCommit {
    update_bytes: Vec<u8>,
    base_frontier: Vec<u8>,
    resulting_frontier: Vec<u8>,
    changes: FlowChangeSummary,
    projection_impact: ProjectionImpact,
}
```

The projection impact should be known or cheaply derived from touched nodes/ranges. It should not require full projection comparison in normal cases.

---

## Projection and Materialization

### Full materialization is recovery-only

`materialize_db8_flow_document` should be used only for:

- initial recovery when projection cache is missing or invalid,
- test assertions,
- explicit diagnostics,
- snapshot repair,
- catastrophic projection mismatch.

It should not be used for:

- normal DB8 open when a projection is already loaded,
- every `from_document`,
- normal local edits,
- normal remote edits,
- normal authority install.

### Windowed materialization

Normal updates should use windowed materialization:

```text
FlowChangeSummary
  -> changed flow ranges / touched nodes
  -> MaterializedFlowWindow
  -> patch projection window
  -> update paragraph/block ID indexes
  -> invalidate layout only for affected paragraphs/blocks
```

This keeps CRDT native while preserving editor performance.

### Projection index

Add a persistent lightweight projection index owned by the controller or editor:

```rust
struct Db8ProjectionIndex {
    paragraph_to_projection_ix: HashMap<ParagraphId, usize>,
    block_to_projection_ix: HashMap<BlockId, usize>,
    flow_node_to_root_block: HashMap<FlowNodeId, BlockId>,
    paragraph_byte_ranges: Vec<Range<usize>>,
    block_unicode_starts: Vec<usize>,
}
```

This index avoids scanning blocks/paragraphs repeatedly during incremental patching.

It should be updated incrementally when projection windows are patched.

---

## Memory Architecture

### Desired memory shape

The target memory profile is:

```text
editor projection and layout cache: roughly old baseline
CRDT source: bounded overhead
projection index: small
transient buffers: short-lived and bounded
```

Not:

```text
old projection
+ CRDT source
+ rematerialized projection
+ duplicate authority projection
+ stale authority projection
+ full snapshot bytes retained
+ full before/after document clones
+ layout cache rebuilds
```

### Avoid duplicate projection ownership

Short-term acceptable:

- `Db8EditorAuthority` owns one projection cache while the editor displays the same projection through shared/Arc-backed document structures.
- No duplicate authority instances survive.
- No rematerialized projection replaces the loaded projection on open.

Long-term better:

- Editor owns the heavy projection cache.
- Authority owns CRDT source plus projection index.
- Authority returns projection patches, not a full `Document` clone.
- Editor applies patches to its projection.

This longer-term design lowers resident memory because only the editor owns the heavy render model.

### Remove full projection clones from normal edits

`finish_source_commit` should not clone the whole projection on every edit. Replace full before/after comparison with explicit impact data.

Current pattern to avoid (confirmed in `controller.rs` line 794):

```rust
let before_projection = self.projection.clone();
let (projection, impact) = materialize_db8_projection_for_changes(...)?;
self.projection = projection.clone();
build_projection_delta(..., &before_projection, projection)?;
```

Second clone to avoid (confirmed in `controller.rs` line 1056):

```rust
let mut projection = current.clone(); // full clone even in incremental path
patch_root_projection_window(source, &mut projection, ...);
```

Preferred pattern for `finish_source_commit`:

```rust
let impact = projection_impact_from_changes(&self.projection_index, &source.changes)?;
let patch = materialize_projection_patch(&self.source, impact)?;
self.apply_projection_patch_in_place(patch)?;
```

Preferred pattern for incremental materialization (in-place mutation):

```rust
fn patch_projection_in_place(
    source: &FlowDocument,
    projection: &mut Document,
    changes: &FlowChangeSummary,
) -> io::Result<ProjectionImpact> {
    // Mutate projection blocks/paragraphs/ids directly in the affected range.
    // No full clone. Return only the impact metadata.
}
```

The `Db8ProjectionDelta` struct should not carry a full `Document`. It should carry only the replacement blocks and affected ranges:

```rust
struct Db8ProjectionDelta {
    before_frontier: Vec<u8>,
    after_frontier: Vec<u8>,
    source_hash: Option<[u8; 32]>,
    changes: FlowChangeSummary,
    replaced_blocks_before: Range<usize>,
    replacement_blocks: Vec<Block>,
    replacement_paragraphs: Vec<Paragraph>,
    affected_paragraphs_before: Range<usize>,
    affected_paragraphs_after: Range<usize>,
}
```

Only recovery should allocate a full before/after pair.

### Drop snapshot/update buffers aggressively

After a `FlowDocument` is created from snapshot bytes, the editor authority should not retain the snapshot bytes unless they are needed for an active network handshake or pending save.

Rules:

- Editor authority stores `FlowDocument`, not `Vec<u8>` snapshot.
- Live sync session may keep snapshot bytes while sending, then drops them.
- Save path exports snapshot on demand, writes it, then drops bytes.
- Retained update log is bounded and compacted after checkpoints.

### Retained updates and compaction

The outbox must have explicit policy:

```text
retain recent updates until saved checkpoint or acknowledged sync checkpoint
write snapshot checkpoint
drop update bytes covered by snapshot frontier
bound retained update count and total bytes
```

Suggested limits:

- max retained update bytes per document: configurable, default 16–64 MB,
- max retained update count: configurable, default 1,000–10,000,
- checkpoint after N edits or M bytes or T seconds,
- compact on save.

The CRDT itself may retain internal history, but external update buffers should not grow unbounded.

---

## CRDT Storage Strategy

### Avoid per-character architecture where possible

CRDT text should avoid excessive per-character metadata at the application layer. Loro may internally manage text ops efficiently, but Flowstate should not add per-character records, per-character marks, or repeated full mark vectors.

Preferred shape:

```text
Flow text: CRDT sequence/text container
Structural tokens: paragraph/object boundaries with stable node IDs
Inline marks: sparse ranges
Projection runs: compact DB8 TextRun arrays
```

### Mark handling

The current mark path can become memory- and CPU-heavy if style-heavy DB8 runs are converted into many CRDT marks and then repeatedly materialized through full mark vectors.

Required changes:

1. Structural token mark lookup must be O(tokens + marks), not O(chars × marks).
2. Inline marks should be swept/projected only for the changed window.
3. Mark vectors should not be cached globally unless bounded.
4. Projection run arrays remain the efficient render format.
5. CRDT marks are authoritative, but projection marks/runs are cached compactly.

### Parsing and scanning

`parse_flow` should be reserved for full validation/recovery. Normal edit paths should use targeted functions:

- `materialize_flow_window`,
- `materialize_node_window`,
- `resolve_node_token_cursor`,
- `node_token_cursors_in_range`,
- changed-range mark sweep.

Full `raw_text = text.to_string()` over the entire flow should not happen in normal typing, scrolling, or opening if a valid projection cache exists.

---

## Scrolling and Layout

The existing subparagraph chunk virtualization should remain the rendering solution. CRDT chunks and render chunks must not be conflated.

Correct relationship:

```text
CRDT source segments / Loro text
  -> projection paragraph/runs/cache
  -> existing layout/render chunks
  -> viewport
```

CRDT source should not be queried while scrolling except for rare cache misses or diagnostics. Scrolling should mostly hit existing projection/layout caches.

Opening a source or installing an authority must not replace the projection if the projection is already valid, because replacing projection invalidates layout caches and creates a large memory spike during fast scroll.

---

## Save and Autosave

Autosave must not force full source/projection rebuilds on the UI thread.

Correct autosave path:

```text
editor asks authority for native snapshot bytes
authority exports CRDT snapshot in background
persistence writes projection cache + CRDT snapshot + compacted recent updates
UI remains responsive
```

Rules:

- Snapshot export must be background.
- Large snapshot bytes must be dropped immediately after write.
- Autosave should coalesce rapid edits.
- Autosave should not call full materialization if projection cache is already current.
- Autosave should not clone entire document more than necessary.

---

## Collaboration and Sync

The CRDT-native model simplifies collaboration if authority lifecycle is correct.

### Local edits

Local edits should be committed to local `FlowDocument` immediately. The resulting update is then queued for host/sync/outbox.

### Remote edits

Remote updates import into `FlowDocument`, produce `FlowChangeSummary`, then patch projection incrementally.

### Host authority

Host owns authoritative `FlowDocumentAuthority`. It should not also keep unnecessary full projections unless it needs one for snapshot compatibility. Transport should operate on CRDT updates/snapshots.

### Ack behavior

Ack should affect transport reliability, not local edit responsiveness. Local source commit can happen before host ack. Host rejection triggers recovery from authoritative snapshot.

### Presence

Presence should use anchored selections/cursors and frontiers. It should not force projection rebuilds.

---

## Cursor and Selection Stability

### Local cursor preservation during remote edits

When a remote edit inserts or deletes text before the local cursor, the local cursor position must remain logically stable. Loro cursors resolve to updated positions after imports, but the editor selection is expressed in projection byte offsets.

Required behavior:

1. Before applying a remote projection delta, resolve the current editor selection to Loro anchored positions.
2. Apply the projection delta.
3. Re-resolve anchored positions to new projection byte offsets.
4. Update the editor selection.

This must not scan the entire document. The projection index should provide O(1) paragraph lookup from the anchored node ID, then local byte-offset resolution within the paragraph.

### Range selection stability

Range selections (highlight/bold toggle in progress) must survive remote structural edits (split/join) that affect paragraphs within the selection range. If a remote peer splits a paragraph inside the selection, the selection must expand to include the new paragraph boundary, not collapse or jump.

### Multi-cursor stability

If the editor supports multiple cursors, each cursor is independently anchored. Remote edits that delete the text at a cursor position should collapse that cursor to the nearest valid position, not delete the cursor or crash.

---

## Child Flows and Nested Objects

### Incremental patching of non-root flows

The current windowed materialization targets the root flow. Child flows (tables, text boxes, nested structures) must also be patchable incrementally.

When `FlowChangeSummary` reports changes to a non-root flow:

1. Identify the owning object node in the root flow.
2. Materialize only the changed child flow window.
3. Patch the corresponding rich object block in the projection.
4. Invalidate layout only for that block and containing paragraph.

This must not rematerialize the entire object graph or rebuild all child flows of an object.

### Deep object graph memory bound

For documents with deeply nested structures (tables within tables, text boxes within table cells), the materialization depth must be bounded. Set a maximum materialization depth (suggested: 8 levels). Beyond that, show placeholder blocks.

---

## Batch and Bulk Operations

### Paste of large content

Pasting N paragraphs must be a single CRDT transaction producing a single `FlowCommit`. The projection must be patched once for the entire paste, not N times.

Required flow:

```text
paste event
  -> build FlowDocumentSeed fragment from clipboard
  -> single transaction: insert all nodes and text
  -> one FlowCommit with touched_nodes covering all new nodes
  -> one windowed projection patch covering the insertion point
  -> one layout invalidation for the affected paragraph range
```

### Find-and-replace

Document-wide find-and-replace must batch all replacements into a single CRDT transaction. The projection must be patched once using the combined change summary, not once per replacement.

If the replacement count exceeds a threshold (suggested: 500), fall back to full materialization for that single transaction rather than attempting windowed patching for each site.

### Style application across large selections

Applying a style (bold, italic, font change) across a multi-paragraph selection should be a single CRDT transaction. The `set_text_marks` call already handles this. The projection patch should be bounded to the affected paragraph range, not full materialization.

---

## Validation Cost in the Edit Path

### Current problem

`materialize_incremental_root_projection` calls `validate_document_invariants(&projection)` after every incremental patch. For large documents, this validation scans all paragraphs and blocks.

### Required change

Validation in the hot edit path should be windowed:

```rust
fn validate_projection_window(
    projection: &Document,
    affected_paragraphs: Range<usize>,
    affected_blocks: Range<usize>,
) -> Result<(), String>
```

Full `validate_document_invariants` should run only:

- after full materialization/recovery,
- on file save (optional, can be background),
- in debug/test builds on every edit (behind `#[cfg(debug_assertions)]`),
- periodically as a background health check.

Production edits should validate only the patched window.

---

## Node Token Cache Invalidation

### Current cache

`FlowDocument` maintains `node_tokens: RwLock<HashMap<FlowNodeId, (FlowId, Cursor)>>`. This maps node IDs to their Loro cursor positions for fast lookup.

### Invalidation rules

1. After a local commit that inserts or deletes structural tokens, the affected flow's token cursors must be refreshed.
2. After a remote import, rebuild token cursors for all flows mentioned in `FlowChangeSummary.flow_text_changes`.
3. Do not rebuild the entire cache on every edit. Use the change summary to scope invalidation.
4. Bound the cache size: for documents with >10,000 nodes, consider LRU eviction of cold node cursors.

### Correctness invariant

A stale cursor in `node_tokens` must not cause silent data corruption. If a cursor resolves to an unexpected position (wrong token kind or missing structural mark), the code must fall back to scanning or return an error that triggers recovery. Never silently use a stale cursor for editing.

---

## Thread Safety and Scheduling

### Current model

`FlowDocument` wraps `LoroDoc` which is not `Send + Sync`. The document controller lives in the authority which is `Rc<RefCell<...>>` on the main thread. Background work uses cloned projections.

### Required invariants

1. All CRDT mutations (local edits, remote imports) happen on the main thread through the authority.
2. Background snapshot export must use `LoroDoc::export(Snapshot)` which takes `&self` and produces owned bytes. This is safe because Loro export does not mutate.
3. Background seed building (`from_existing_projection`) operates on a cloned `Document` snapshot, then constructs a new `LoroDoc`. The result is moved to the main thread for installation.
4. No two threads may hold mutable access to the same `FlowDocument` simultaneously.

### Scheduling priorities

- Local edit commit: highest priority, must be sub-frame.
- Remote update import: high priority, must not block local edits.
- Projection patching: synchronous with the triggering edit/import.
- Snapshot export for autosave: background, interruptible.
- Authority initialization: background, cancellable by generation counter.
- Full recovery materialization: background, interruptible.

---

## Undo/Redo Across Remote Edits

### Loro undo model

Loro's undo manager undoes only local operations, automatically transforming against concurrent remote changes. This is the correct behavior for collaborative editing.

### Required guarantees

1. Undo must undo only local changes, never remote changes.
2. After undo, the local projection must be re-patched from the resulting CRDT state, not from a stored projection snapshot.
3. Undo group boundaries must survive authority re-initialization. If the authority is rebuilt (e.g., from snapshot recovery), undo history is lost. This is acceptable but must be surfaced to the user.
4. `reset_undo_lineage` after `replay_retained_updates` is correct: retained updates from a previous session should not be undoable in the new session.

### Selection restoration

Each undo group stores an `AnchoredSelection`. After undo/redo, this selection must be resolved against the resulting frontier to get correct projection coordinates. If the anchor resolves to a deleted position, collapse to the nearest valid position.

---

## Network Partition and Offline Editing

### Divergence during offline

When a peer is offline, local CRDT edits accumulate in the durable outbox. On reconnection, all accumulated updates are published.

### Divergence limits

If accumulated offline updates exceed a threshold (suggested: 128 MB or 50,000 transactions), the reconnecting peer should offer the host a full snapshot replacement instead of incremental update replay. The host decides whether to accept the catch-up or request a full resync.

### Conflict resolution at reconnection

CRDT convergence handles semantic merging. However, the projection must be fully rebuilt after a large divergence import because windowed patching cannot handle thousands of interleaved remote changes efficiently.

Threshold for fallback to full materialization on reconnection:

- More than 100 changed flows in a single import.
- More than 10,000 changed unicode positions.
- Import duration exceeds 50ms.

### Split-brain prevention

Two hosts must never run simultaneously for the same document. The replica lease prevents this locally. For network-level prevention, the signaling server must enforce single-host-per-document-ID.

---

## Progressive Projection Hydration

### Motivation

For very large documents (1,000+ paragraphs, 10,000+ blocks), loading the full projection at once delays initial paint. The existing DB8 projection cache loads the full document structure, but layout/render only needs the viewport.

### Staged hydration (future optimization)

```text
Phase 1: Load projection skeleton (paragraph count, block IDs, byte ranges)
Phase 2: Hydrate viewport paragraphs fully (text + runs + layout)
Phase 3: Hydrate above/below viewport lazily on scroll
Phase 4: CRDT authority ready, all paragraphs can be re-hydrated on demand
```

This is a future optimization, not a Phase 1 requirement. But the architecture must not prevent it:

- The projection index must work with partially-hydrated projections.
- Scroll virtualization must request paragraph hydration from the controller when a cold paragraph enters the viewport.
- CRDT windowed materialization already supports per-node access, which enables lazy hydration.

### Current minimum viable behavior

For Phase 1, full projection load from DB8 cache is acceptable. The key requirement is that the CRDT authority initialization does not also rebuild this projection.

---

## Snapshot Isolation for Saves

### Problem

Autosave calls `document_for_serialization` which normalizes and clones the projection. During this clone, edits should not be blocked. But the save must capture a consistent state.

### Required behavior

1. `export_snapshot()` on FlowDocument takes `&self` and produces owned bytes without mutation. This is inherently consistent because Loro snapshot export reads committed state.
2. The projection cache for save should be the last committed projection state. Since the projection is updated synchronously after each edit commit, any snapshot of the projection is self-consistent.
3. Save must not hold a lock that blocks the edit path.
4. If an edit arrives during save serialization, the save captures either pre-edit or post-edit state, never a torn state.

### Implementation

The current approach of cloning `document_for_serialization` is acceptable short-term. Long-term, the projection cache written to disk should be produced by the authority (which owns the committed state) rather than by cloning the editor's display projection.

---

## Error and Degraded-State Behavior

Silent no-op editing is unacceptable.

If edits are temporarily unavailable, the editor should expose why:

- CRDT source still preparing,
- source initialization failed,
- projection/source mismatch recovery required,
- document opened read-only due to unsupported format,
- durable outbox unavailable but local edits allowed,
- collaboration publishing disabled due to lease conflict.

For each edit rejection, emit a canary and a user-visible status if it lasts more than a short threshold.

Required canaries:

- `authoritative_edit_rejected_preparing`
- `authoritative_edit_rejected_source_error`
- `authoritative_edit_rejected_read_only`
- `durable_outbox_unavailable`
- `local_crdt_edit_committed`
- `projection_patch_applied`
- `projection_full_recovery_started`
- `projection_full_recovery_finished`
- `snapshot_export_started`
- `snapshot_export_finished`

---

## Migration Plan

### Phase 1: Stop the bleeding

Implement immediately.

1. One authority init per panel, generation-guarded.
2. If vNext snapshot exists, do not also seed-build authority.
3. `from_document` keeps existing projection and does not rematerialize.
4. `from_snapshot_with_projection` imports snapshot and keeps existing projection if hash/invariants permit.
5. Lease failure no longer blocks local editing; it disables durable outbox/sync publishing only.
6. Preparing authority surfaces edit rejection visibly.
7. Drop snapshot bytes after authority install.
8. Add timing and memory canaries.
9. Remove `validate_document_invariants` from hot edit path; replace with windowed validation.

Expected result: documents open, scroll, and edit again. Memory drops because duplicate authorities and rematerialized projections disappear.

### Phase 2: Make normal edits bounded

1. Remove full projection clone from normal `finish_source_commit` by mutating in place.
2. Remove full projection clone from `materialize_incremental_root_projection` by patching in place.
3. Change `Db8ProjectionDelta` to carry only replacement blocks/paragraphs, not full `Document`.
4. Add `Db8ProjectionIndex`.
5. Build projection delta from explicit CRDT change summary and projection index.
6. Keep full materialization only as recovery fallback.
7. Bound retained updates and compact on save.

Expected result: typing/editing latency returns to near old editor behavior. Per-edit memory allocation drops by ~3× for large documents.

### Phase 3: Make mark/style path scalable

1. Replace full-flow mark scans with sweep-based changed-window mark projection.
2. Keep projection `TextRun` arrays compact.
3. Avoid repeated `to_delta` / `to_string` over full flow except recovery.
4. Add style-heavy benchmarks.
5. Batch multi-paragraph style operations into single transactions.

Expected result: styled long docs avoid CPU and memory spikes.

### Phase 4: Move authority to source + index only

1. Editor owns heavy projection.
2. Authority owns CRDT source and lightweight projection index.
3. Authority returns patches, not full `Document` values.
4. Editor applies patches directly to its projection.
5. Layout invalidation is based on patch ranges.
6. Cursor/selection stability uses anchored resolution through projection index.

Expected result: memory approaches old baseline plus CRDT overhead.

### Phase 5: Snapshot/compaction hardening

1. Write projection cache + CRDT snapshot + compacted recent updates.
2. Drop update bytes covered by checkpoint frontier.
3. Add corruption/recovery tests.
4. Add large-document memory tests.
5. Implement snapshot isolation: save produces bytes without blocking edits.

Expected result: long sessions do not accumulate unbounded memory or disk update logs.

### Phase 6: Child flows, batch ops, and progressive hydration

1. Incremental patching for non-root flows (tables, text boxes).
2. Single-transaction paste for multi-paragraph clipboard content.
3. Batched find-and-replace with combined change summary.
4. Progressive projection hydration for documents exceeding 1,000 paragraphs.
5. Offline divergence detection and fallback-to-snapshot on large catch-up.

Expected result: complex documents with nested structures, large paste, and offline editing perform at parity with simple documents.

---

## Test Plan

### Startup/open tests

- Open old DB8 without vNext snapshot: paints immediately; authority becomes ready asynchronously.
- Open DB8 with vNext snapshot: no duplicate authority build; projection not replaced if valid.
- Disable session restore: blank app opens with no authority work.
- Enable session restore: restored docs open without UI freeze.

### Editing tests

- Type immediately after authority ready.
- Try typing while preparing: visible read-only/preparing status, no silent no-op.
- Insert/delete/split/join/style/table/image/equation all route through CRDT source.
- Undo/redo restore anchored selections.

### Memory tests

- Open large doc and measure resident set after idle.
- Scroll full document and measure peak/resident memory.
- Type 1,000 chars and measure memory delta.
- Autosave large doc and ensure snapshot bytes are dropped after write.
- Reopen saved vNext file and ensure no duplicate projection is retained.

### Performance tests

- Time `db8_flow_seed`.
- Time `FlowDocument::from_seed`.
- Time authority install.
- Time one-character insert.
- Time style toggle over small range.
- Time split/join.
- Time remote update import.
- Time projection patch.
- Time full recovery materialization separately.

### Convergence tests

- Multi-replica typing, split/join, style, table edits converge.
- Duplicate update delivery idempotent.
- Out-of-order updates converge or request recovery.
- Host rejection recovers without corrupting projection.

### Cursor and selection stability tests

- Local cursor position preserved when remote edit inserts before cursor.
- Local cursor position preserved when remote edit deletes before cursor.
- Range selection survives remote paragraph split within selection.
- Range selection survives remote paragraph join within selection.
- Multi-cursor positions remain stable under concurrent edits.
- Selection anchors resolve correctly after undo/redo.

### Batch operation tests

- Paste 100 paragraphs: single CRDT transaction, single projection patch.
- Find-and-replace 500 occurrences: single transaction, bounded memory.
- Style toggle across 50-paragraph selection: single transaction.
- Large paste does not cause full materialization if windowed patch covers the range.

### Child flow / nested object tests

- Edit inside table cell: only cell's child flow patched, not entire table.
- Remote edit in table cell: windowed patch for child flow.
- Deep nesting (table in table): materialization bounded by depth limit.
- Object graph changes invalidate only containing root block.

### Offline / reconnection tests

- Accumulate 1,000 offline edits, reconnect: incremental catch-up succeeds.
- Accumulate 100,000 offline edits, reconnect: falls back to snapshot replacement.
- Two peers diverge for 60 seconds, reconnect: converge within 200ms.
- Diverged reconnection does not cause unbounded memory spike.

---

## Acceptance Criteria

A change set is acceptable only if the following are true.

1. Blank app startup does not freeze.
2. Opening large DB8 paints quickly.
3. Scrolling large DB8 stays near old performance.
4. Typing works after CRDT authority readiness.
5. Editing never silently no-ops.
6. Memory after full scroll is near old baseline plus bounded CRDT overhead, not multi-gigabyte for ordinary docs.
7. Full materialization does not run in normal open/edit paths when projection cache is valid.
8. Only one authority is live per panel.
9. Lease failure does not block local CRDT edits.
10. Autosave/snapshot export does not retain large buffers after completion.
11. Collaboration still sends CRDT updates, not legacy projection diffs.
12. Remote updates patch projection incrementally from CRDT changes.
13. No full projection clone occurs during normal local edits.
14. Cursor position is stable across remote edits without visible jump.
15. Paste of multi-paragraph content is a single CRDT transaction.
16. Child flow edits do not rematerialize unrelated root flow content.
17. Authority initialization is cancellable by generation counter; stale installs are ignored.
18. Windowed validation replaces full document validation in the edit path.
19. Node token cache is invalidated correctly after structural edits.
20. Offline edits accumulate in outbox and reconcile on reconnection without data loss.

---

## Summary

The path forward is not abandoning CRDT-native architecture. The path is making CRDT native in the correct layer.

CRDT should own semantic truth and mutation. The existing editor projection should remain the high-performance UI cache. Open should trust a valid projection cache and build CRDT authority asynchronously. Normal edits should commit to CRDT and patch only affected projection windows in place, without cloning the full projection. Full materialization should be recovery-only. Memory should be controlled by ensuring there is one source, one projection cache, one authority, bounded update buffers, and no duplicate full-document rebuilds in normal operation.

Beyond the core edit loop, the architecture must also guarantee cursor stability across remote edits, efficient batch operations, incremental child flow patching, correct node token cache invalidation, bounded validation cost in the hot path, safe snapshot isolation for saves, and graceful offline divergence handling.

That preserves the CRDT-native model while restoring the editor quality that existed before the migration and extending it to handle collaborative, concurrent, and offline use cases at scale.
