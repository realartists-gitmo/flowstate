# Flowstate CRDT-native collaboration model and remaining latency roadmap

This document has two parts.

First: full vision for making the editor CRDT-native all the way down, not merely collaboration-aware through projection diffs.

Second: remaining latency/collaboration backlog, current status, suggested implementation points in the codebase, and whether the CRDT-native model invalidates, strengthens, or changes each item.

---

## Part 1: Full CRDT-native model, all the way down

### 1. Decision and scope

Keep Loro as Flowstate's durable CRDT engine. Do not introduce a parallel
`TextSegment { actor, lamport, ... }` CRDT or a second semantic operation log.
The vNext work is a new Loro document schema and a new edit pipeline, not an
extension of the current canonical-operation fast paths.

The durable source of truth is a committed `LoroDoc` frontier. The editor
`Document`, selections, identity maps, projection indexes, layout objects, and
native-file projection cache are derived state.

Target local flow:

```text
UI intent
  -> resolve current Loro cursors/IDs
  -> mutate the authoritative LoroDoc and commit once
  -> capture the exact local Loro update bytes
  -> derive one typed projection delta from that committed change
  -> hand off atomically to projection and the ordered local outbox
  -> paint without waiting for network/disk fsync
  -> publish the same update bytes after outbox acceptance
```

Target remote flow:

```text
network Loro update bytes
  -> validate on an isolated fork
  -> import into the authoritative LoroDoc
  -> derive one typed projection delta from that integrated change
  -> update editor projection, selection, layout, and caches
```

"Publish the same transaction" means publishing the exact Loro update or
causal delta produced by the committed local mutation. Optional application
hints may improve animation or diagnostics, but they are never authoritative.

Use the same CRDT-backed document controller in single-user and collaborative
mode. Single-user mode is one stable local actor with no remote replicas, not a
different document model.

### 2. Why this rewrite is necessary

The current implementation has several temporarily competing truths:

- `RichTextEditor` owns and mutates the local `Document`, selection, undo/redo
  stacks, identity map, and layout caches.
- Local edits mutate that projection and then record `CanonicalOperation`
  values addressed by `ParagraphId` plus UTF-8 byte offsets.
- Workspace publishing later reads the editor's last canonical operation and
  DB8 source mutations.
- Remote receive first attempts canonical-operation fast paths, then falls back
  to `compute_paragraph_changes(...)` and projection patching/materialization.
- The durable DB8 collaboration source already uses `LoroDoc`, `LoroText`,
  `LoroMap`, and `LoroMovableList`.

That split allows a local visual edit and its later source mutation to be based
on different paragraph/offset mappings. Latency and concurrent structural edits
then expose wrong-line inserts, duplicated text, or remote-only characters.

Current granular and canonical operations are still byte-offset based. They are
useful as migration diagnostics, but they cannot remain the normal concurrent
wire coordinate.

Primary transition points in the current codebase:

- `crates/flowstate-collab/src/source.rs`: replace the current DB8 granular
  per-paragraph source schema/mutations with versioned vNext Loro primitives,
  exact update capture, validation, and materialization.
- `crates/flowstate-document`: make this the natural DB8 document-controller
  boundary because it already sits above both `flowstate-collab` and
  `gpui-flowtext`.
- `crates/gpui-flowtext/src/rich_text/editor`: replace projection-first
  collaboration capture with typed-intent submission and atomic projection
  delta application while retaining editor/layout responsibilities.
- `crates/flowstate/src/workspace/workspace/documents.rs`: remove semantic
  translation, canonical fast-path authority, and paragraph-diff application
  from the normal live path.
- `crates/flowstate-sync`: transport/validate exact updates, snapshots,
  presence, and assets without interpreting application hints as source truth.

### 3. Ownership and crate boundaries

Do not make `gpui-flowtext` directly own Loro or collaboration transport.
`gpui-flowtext` is a reusable editor/projection and rendering component.

Preferred ownership:

- `flowstate-collab`: generic Loro source primitives, schema/version helpers,
  cursors, snapshots, exact update capture, import validation, and causal
  metadata.
- `flowstate-document` or a new document-model crate: the CRDT-backed DB8
  document controller, typed edit intents, vNext schema mapping, projection
  deltas, and migration.
- `gpui-flowtext`: input handling, a derived rich-text `Document`, cursor
  display, projection application, and layout/cache invalidation.
- `flowstate-sync`: transport, durable outbox/ack handling, presence, snapshot
  transfer, asset transfer, authentication, and host policy.
- workspace/panel code: lifecycle and orchestration, not semantic translation.

The editor sends an intent to the document controller. The controller commits
the source mutation and returns the authoritative projection delta and resolved
selection. No caller may mutate the rendered `Document` first and translate it
into a source mutation later.

### 4. Loro vNext document schema

The vNext schema must be versioned separately from the native-file envelope and
projection-cache formats. Its exact container names are an implementation
detail, but its authority boundaries are not.

Conceptually:

```text
LoroDoc
  -> protected schema/model metadata
  -> root flow ID
  -> text flows
      -> LoroText content with structural tokens and inline marks
  -> paragraph records keyed by stable ParagraphId
  -> block records keyed by stable BlockId
  -> table records with stable row/cell IDs and movable order
  -> child-flow references for editable nested content
  -> document theme/style manifest
  -> content-addressed asset manifest
```

Every durable paragraph, block, row, cell, and flow has a stable ID stored in
the source. Visual indexes, vector positions, table dimensions, and runtime
identity-map IDs are never durable identities. ID creation must be
collision-resistant across offline replicas, using an established UUID scheme
or registered replica ID plus monotonic counter.

Store enough ownership metadata to resolve a changed node or child flow back to
its projected root object in work proportional to graph depth, not document
size. Validate this reverse ownership/dependency index against the reachable
forward graph. It is derived authority metadata, not a second block-order
source.

Security-sensitive host ACLs and authorization are not editor-writable document
state. If a role-policy projection remains inside the Loro source for display or
offline inspection, it is protected and host-authored; host enforcement does
not trust it.

### 5. Text flows solve split/join identity

The current schema stores each paragraph in a separate `LoroText`. Split copies
the suffix into a new container and deletes it from the old one; join copies the
second container into the first and deletes the second. That loses text-level
CRDT identity across the operation and cannot give stable semantics to
concurrent edits anchored in the copied/deleted container.

The vNext schema uses one `LoroText` per editable **text flow**, not one per
paragraph:

- the root block sequence is a text flow;
- each table cell is a child flow;
- editable image captions and equation source are child flows;
- nested tables recursively reference child flows;
- joins never cross flow boundaries.

Within a flow, reserved structural tokens define block structure:

- a paragraph-start token owns a stable `ParagraphId`;
- an object token owns a stable non-paragraph `BlockId`;
- user text exists only inside a paragraph's range;
- projection strips structural tokens before rendering.

The initial encoding should use reserved Loro text elements with non-expanding
structural marks. The encoding is accepted only after a focused Loro spike
proves insertion ordering, mark expansion, merge-closed grammar under concurrent
text/structure edits, cursor resolution, undo, export, snapshot, and
shallow-snapshot behavior. If marked tokens cannot satisfy those invariants,
use a cursor-indexed structural-record encoding in the same text flow. Do not
fall back to copy/delete between per-paragraph text containers.

With this model:

- split inserts a new paragraph-start token at a resolved Loro cursor;
- the original paragraph ID remains on the left;
- the new paragraph ID owns the right side;
- join deletes the second adjacent paragraph-start token;
- user text never moves containers, so its Loro identity and cursors survive;
- concurrent inserts at a split/join boundary use Loro's normal ordering.

A flow always begins with a structural token. Empty paragraphs are represented
by adjacent paragraph-start tokens. Text after an object token is invalid unless
another paragraph-start token begins it. User input that contains a reserved
token value must be escaped or rejected before source mutation.

This text-flow decision solves insertion, deletion, split, and join identity; it
does **not** automatically solve an arbitrary move of a text-bearing paragraph
span. `LoroText` has no identity-preserving span move. Before enabling
collaborative paragraph moves, prove one of:

- a Loro tree/movable-order layer plus reparentable references to stable text
  fragments;
- a rigorously specified reparentable segment composition built on Loro
  containers;
- another Loro-native representation that preserves text cursors through move.

Do not implement paragraph move as delete/reinsert and claim cursor continuity.
If no design passes the same convergence/cursor/undo/snapshot tests, keep
collaborative paragraph moves disabled. Object blocks with independent child
flows may move without moving their child text, but their token occurrence and
single-owner conflict semantics still require a focused proof.

### 6. Styling and metadata

Use Loro text marks for inline styling and source maps/registers for paragraph
and block metadata. Structural marks must not expand into user text.

Editor policies such as "Enter at the end of a heading creates a plain
paragraph" are local intent rules, but their result is authoritative source
state written in the same Loro commit:

- insert the new paragraph token;
- write the resolved paragraph style/metadata;
- set any required typing marks;
- commit once.

Remote replicas do not rerun the sender's UI heuristic and do not need a custom
`SplitStylePolicy` wire operation. They integrate the resulting Loro update.

Metadata merge semantics must be explicit per field. Do not use a blanket
last-writer-wins rule where concurrent additions, ordered values, or protected
fields need different behavior.

Document sections currently derived from paragraph styles remain derived unless
a future feature gives sections independent editable semantics.

### 7. Rich blocks and assets

The current adapter does not map block insert/delete/move/replace operations to
granular CRDT mutations, and current rich blocks are largely opaque binary
records. vNext must make rich-block structure first-class.

- Object tokens establish block order in the parent flow for the initial vNext
  edit set; move semantics remain subject to the text-flow move gate above.
- Block records store typed metadata and child-flow references.
- Tables use durable `RowId` and `CellId` values with CRDT order. IDs are not
  derived from row/cell indexes or table shape.
- Every editable table cell owns a child flow. Nested tables use the same model.
- Editable equation source and image captions use child flows rather than opaque
  replacement when practical.
- Asset bytes remain on a separate content-addressed transfer/storage lane.
  CRDT state contains immutable hashes and typed references, not large blobs.
- Missing assets render placeholders and retry retrieval; they do not invalidate
  otherwise valid document updates.

Opaque block replacement may remain as a compatibility bridge for block types
without collaborative semantics, but it must be explicitly versioned and cannot
silently overwrite concurrently edited child content.

Live document state is the reachable graph from root structural tokens. Deleting
a paragraph/object token makes its record and child flows unreachable; it does
not immediately destroy data still needed by undo, concurrent updates, offline
replicas, or recovery. Concurrent edits to an unreachable child flow do not
implicitly resurrect the block. Orphan records/flows/assets are garbage
collected only when the history epoch and retention policy prove they are no
longer reachable by supported undo/replay.

### 8. Local transaction path

Every user-visible mutation is an atomic document-controller transaction:

1. Capture input intent and current anchored selection.
2. Resolve the selection against the controller's current Loro frontier.
3. Apply all text, structure, marks, metadata, and selection-affecting changes.
4. Commit once with an origin, undo-group metadata, and diagnostics.
5. Capture the exact local update bytes, resulting frontier, and root diff.
6. Hand the update atomically to one projection delta and the ordered outbox.
7. Paint without waiting for network or disk fsync; publish only after the
   outbox accepts the update.

Use Loro's local-update/root subscriptions or an equivalent explicit
before/after export boundary to associate exact update bytes and diffs with the
commit. There must be no gap in which a second mutation can be accidentally
included. Define the accepted crash-loss window and outbox/WAL durability
policy explicitly. An outbox write failure must surface and stop further
untracked publication, but normal input-to-paint latency must not wait on fsync.

Hostile remote updates require full isolated-fork validation. Normal local typed
intents do not: `LoroDoc::fork()` and a complete schema walk are document-sized
operations and cannot run for every keystroke on the UI thread. Local mutation
APIs must make invalid states unrepresentable or preflight their bounded
affected scope before the one authoritative commit. Periodic/background full
validation and recovery snapshots remain defense in depth.

IME composition needs an explicit policy. Composition may use a temporary local
overlay, but commit/replace/cancel must resolve back to Loro cursors and enter
the normal atomic path. Never publish intermediate projection-only byte edits.

### 9. Projection and layout

The rendered editor document remains a high-performance projection. It
maintains:

- visual block/paragraph index to stable source ID and the reverse mapping;
- projected UTF-8 byte ranges to Loro cursors and the reverse mapping;
- resolved inline styles and block metadata;
- anchored selections to displayed carets/ranges;
- source frontier and exact projection provenance.

Derive a typed transaction-scoped projection delta from Loro root diffs. Local
and remote commits use the same delta builder. Split, join, table mutation, and
multi-field commits must not expose intermediate projection states.

Normal editing must not use DB8 paragraph diffing or full source materialization.
A full rebuild remains a validated recovery path. Projection caches are
disposable and may be reused only when their schema, source frontier, and source
hash/provenance match exactly.

Child-flow changes must resolve through the stable ownership/dependency index
and rematerialize only the affected root rich object or smaller supported
subtree. Transaction-scoped projection deltas carry their exact affected block
and paragraph ranges; constructing a delta must not compare or copy every
projected paragraph after each edit.

Subparagraph virtualization remains a layout-only cache:

```text
Loro text flow and structural tokens
  -> rich-text projection with cursor map
  -> subparagraph layout chunks with projected byte ranges
  -> glyph/layout cache
```

Layout chunk boundaries are viewport/font/wrapping artifacts, not CRDT
boundaries. Projection-delta application should invalidate only affected
paragraphs/chunks where possible and must not perform heavy snapshot import or
full materialization on the UI thread.

### 10. Cursors, selections, presence, and text coordinates

Use serialized Loro cursors plus flow identity and affinity for durable edit
anchors, selections, undo cursor metadata, and presence:

```text
AnchoredPosition = FlowId + Loro Cursor + affinity + optional fallback ID
AnchoredSelection = start + end + direction
```

Current presence serializes paragraph/table indexes and UTF-8 byte offsets.
Replace it with anchored positions. Presence remains ephemeral, independently
coalesced, and unable to block durable updates.

When history removal makes a cursor unresolvable, use a defined fallback:
nearest surviving structural ID when possible, otherwise hide the remote
presence/request a refresh. Never guess a byte offset into unrelated text.

Coordinate conversions must be centralized and tested. Flowstate projection
uses UTF-8 byte offsets, Loro cursor APIs use their own text indexing semantics,
and platform/IME APIs may use UTF-16 or grapheme positions. No wire operation
may treat a projected byte offset as authority.

### 11. Actor, session, origin, and undo

Define and persist these separately:

- `ActorId`: durable authenticated user/device identity for attribution and
  authorization.
- `ReplicaId`/Loro peer ID: unique to one live local replica/outbox/undo
  lineage, stable across reconnect/restart while that lineage is retained, and
  never shared by simultaneously active replicas of the same document.
- `SessionId`: ephemeral connection/editor session identity.
- `UpdateOrigin`: local input, undo, redo, migration, recovery, remote import,
  or protected host mutation.
- `SourceFrontier`: authoritative causal state.
- `ProjectionGeneration`: disposable local cache generation.

The current creation of a new actor for each sync configuration and one-to-one
hashing of actor ID into Loro peer ID are insufficient. Define actor
persistence, actor-to-replica registration, peer-ID collision handling, and
replica rotation before cutover. Never change a `LoroDoc` peer ID while an undo
manager or unacknowledged outbox for that replica is active.

Replica registration needs an exclusive live lease per document/replica
lineage. Opening the same document in another process or recovered workspace
must either take over an expired lease, rotate to a new replica while replaying
the old outbox as retained history, or open read-only. A global device replica
ID shared by simultaneous editors is not sufficient.

Use Loro's `UndoManager` as the default design, configured to track the stable
local peer and exclude remote, migration, recovery, projection, and protected
host origins. Explicitly control typing-burst grouping and structural-edit
boundaries. Store/restore anchored selections with undo items where supported.

Undo and redo are new local Loro commits, follow the same projection/outbox
path, and never rewrite only the editor projection. Define behavior when target
history was compacted or concurrently removed; partial/no-op outcomes must be
deterministic and visible to diagnostics.

### 12. Transport, offline queue, and authority

The durable update lane carries exact Loro update bytes plus envelope metadata:

- document ID, authenticated actor, and session;
- registered replica/Loro peer ID;
- update ID/hash for dedupe;
- base/resulting frontier or equivalent causal metadata;
- schema/model version and update byte length;
- optional non-authoritative application hint.

Do not transport a second custom `Vec<CrdtOperation>` as authority. Canonical
operations and `UpdateApplication` hints may temporarily support diagnostics,
animation, or migration, but receivers must verify/ignore them against the
integrated Loro diff.

Persist local updates before relying on them for offline recovery. A bounded
in-memory queue may provide backpressure, but it must not drop or semantically
coalesce unacknowledged durable edits using paragraph/byte coordinates. Retain
updates by causal frontier/update hash until ack or snapshot policy makes them
redundant.

The durable outbox record and exact committed update are one handoff contract:
the editor may paint optimistically after commit, but publication is forbidden
until outbox acceptance succeeds. Outbox recovery must replay every retained
exact update with its original replica/peer identity before new lineage updates
overtake it. A failed acceptance quarantines the lineage rather than silently
continuing with projection-only edits.

Outbox journal compaction and rotation must also be crash-recoverable. Never
delete the last valid journal before a fully written and synced replacement can
be promoted; startup must recognize and recover interrupted rotations without
silently dropping retained lineage updates.

Host ack confirms relay/durability, not local semantic validity. Host rejection
must trigger a defined quarantine/recovery path; do not continue on a silently
forked source. Reconnect sends missing causal updates or accepts an authoritative
snapshot according to the epoch policy.

Snapshots, live updates, presence, and assets remain separate priority lanes.
Live durable updates should not wait behind presence or large snapshot/asset
transfers.

### 13. Host validation and protected state

Loro update bytes are authoritative but not trusted. Validate an incoming update
on an isolated fork before importing or relaying it.

Validation must include:

- every client-authored Loro peer ID in the update is registered to the
  authenticated actor/session and is allowed for that document replica;
- the role is allowed to mutate the document;
- protected schema, model-version, ACL, owner, and host-only records are
  unchanged by ordinary editors;
- schema invariants hold after integration;
- structural-token grammar is valid in every touched flow;
- references point to valid typed records/flows or an explicitly allowed
  pending asset;
- stable IDs are unique and type-correct;
- table nesting depth and child-flow graph are valid and acyclic where required;
- decoded operation count, container count, text/state growth, mark count,
  metadata size, nesting depth, and asset-reference size stay within quotas;
- decode/import/validation time stays within bounded resource budgets.

Existing message-size limits remain necessary but are insufficient because a
small encoded update can cause large state expansion or pathological structure.
Reject invalid updates before they affect authoritative state or other peers.

Validation is not a substitute for a merge-closed schema. Two independently
valid, authorized concurrent edits must merge into valid source state or an
explicit deterministic conflict representation. Do not rely on host arrival
order/rejection to resolve normal structural conflicts, because clients may
already have integrated those edits optimistically.

### 14. Snapshots, compaction, and native saves

Express persistence policy in Loro terms:

- periodic full Loro snapshots for recovery;
- exact incremental Loro updates retained by causal frontier;
- a versioned projection cache that is always disposable;
- an initial epoch-0 full-history mode that explicitly prohibits shallow
  snapshots and `compact_change_store`;
- any future compacted mode only behind a new explicit history epoch, retention
  policy, cursor/undo behavior, and incompatible-client recovery protocol.

Compaction can invalidate old cursors, undo history, and offline updates. It may
advance only after policy determines which actors/frontiers must remain
replayable. Clients behind the retained epoch must receive a new authoritative
snapshot and must not blindly replay stale updates. Presence older than retained
history expires. Undo history older than retained history is cleared or
explicitly truncated.

Native save/autosave/export/search must read a consistent source frontier and
matching projection. The native envelope records its envelope version,
collaboration schema/model version, Loro snapshot/frontier, retained updates,
projection-cache version/provenance, and asset manifest. Source state wins when
any cache disagrees.

### 15. Migration and compatibility

The current collaboration schema, DB8 projection format, and native envelope
already have independent version fields. Use them.

Adopt dual-read/single-write migration:

1. Open existing native/DB8/Loro-v2 state with the legacy reader.
2. Materialize and validate the legacy DB8 document.
3. Deterministically create the vNext Loro flows, structural tokens, stable
   records, marks, and asset references while preserving existing durable IDs.
4. Allocate and persist stable IDs for legacy table rows/cells and other
   structures that currently rely on indexes/runtime identities.
5. Round-trip materialize vNext and compare all supported document semantics.
6. Save a new schema/model version and rebuild the projection cache.

Migration should be atomic and recoverable; keep the prior snapshot/file until
the new source validates and saves. Do not permit a mixed live collaboration
session where some peers write the legacy schema and others write vNext.
Handshake rejects incompatible writers and offers migration/read-only guidance.

During rollout, legacy canonical operations, granular DB8 mutations, and
paragraph diffing may serve bridge, telemetry, and recovery roles. They are
never co-authoritative with vNext Loro state and should be deleted from the live
path after convergence and migration gates pass.

### 16. Invariants and recovery

Core source invariants:

- every flow begins with one structural token and obeys the token grammar;
- every live structural ID has exactly one compatible record;
- every referenced child flow exists and has one valid owner unless explicitly
  shareable;
- block, row, and cell IDs are stable and unique;
- protected records are modified only by allowed origins;
- inline marks do not corrupt structural tokens;
- materialization is deterministic for a given Loro frontier;
- the schema is closed under the merge of independently valid authorized edits.

Core projection invariants:

- projected order and content exactly match source materialization;
- visual-index/ID and byte/cursor mappings round-trip at valid boundaries;
- selections resolve to valid visible positions or the defined fallback;
- layout ranges cover projected content without gaps or overlaps;
- a transaction delta is applied atomically;
- no normal local publish is derived from projection state.

If update import, projection-delta application, or invariant checking fails:

1. quarantine the offending update and record diagnostics;
2. keep/restore the last valid source frontier;
3. rebuild the projection from that source;
4. request an authoritative snapshot when needed;
5. never hide the failure by inserting at a best-effort offset.

### 17. Tests and acceptance gates

Build a property/model-test harness with multiple Loro replicas and a simple
reference materializer. Randomize operation generation and delivery order,
including duplicate, delayed, dropped-before-reconnect, and offline updates.

Required operation coverage:

- Unicode text insert/delete/replace and IME commit/cancel;
- concurrent inserts and deletes at structural boundaries;
- split/join concurrent with typing, styling, undo, and another split/join;
- inline marks and paragraph metadata conflicts;
- table row/cell edits, nested tables, captions, equations, and block moves;
- delete/undo concurrent with edits inside a removed paragraph or child flow;
- the selected paragraph-move representation, if collaborative moves are
  enabled;
- local undo/redo interleaved with remote edits;
- snapshot, restart, incremental replay, and epoch-policy gates for shallow
  snapshots and compaction;
- legacy-to-vNext migration and native-file round trips;
- malformed/hostile updates, protected-state mutation, and author spoofing.

For every schedule, assert Loro convergence, deterministic materialization,
source/projection invariants, cursor-resolution behavior, and save/reload
equivalence.

For the initial full-history epoch, replace shallow-snapshot/compaction
acceptance with assertions that compaction is prohibited and epoch mismatches
are rejected. Enable compaction convergence/recovery tests only when a future
history epoch actually defines those semantics.

Performance gates must measure input-to-projection-delta, input-to-paint, update
size, update/import validation time, projection-delta application, and layout
invalidation at p50/p95/p99 on large documents. Normal typing, split, join,
style, undo, and remote update paths must avoid full materialization and avoid
unbounded work on the UI thread.

### 18. Implementation sequence

1. **Prove the schema.** Spike Loro structural-token/mark/cursor behavior under
   concurrent split/join, undo, snapshot, and compaction. Write the schema and
   invariants before broad editor changes.
2. **Create the CRDT-backed document controller.** Add the vNext source model,
   actor/replica identity, typed intents, exact update capture, and
   deterministic materializer in the document-model layer.
3. **Build the projection delta engine.** Convert Loro root diffs into atomic
   editor deltas with cursor/byte maps and full-rebuild verification.
4. **Move local edits source-first.** Start with text/selection, then
   split/join/style/undo. Keep legacy capture only as comparison telemetry.
5. **Move remote edits to exact updates.** Import validated bytes and apply the
   same projection delta path; remove canonical fast-path authority.
6. **Complete rich blocks, presence, assets, save, and migration.**
7. **Cut over and delete the dual path.** Make vNext the only writer after
   convergence, performance, migration, and hostile-update gates pass.

This is a substantial model rewrite. Treating it as a sequence of more
canonical-operation fast paths would preserve the split-truth failure class.

### 19. Explicit non-goals and prohibited shortcuts

- Do not replace Loro without a separate, evidence-backed engine decision.
- Do not keep one `LoroText` per paragraph and copy/delete text for split/join.
- Do not make `gpui-flowtext` depend on collaboration transport or source
  persistence.
- Do not use paragraph/table visual indexes or UTF-8 byte offsets as wire
  authority.
- Do not send custom semantic operations as a second source of truth.
- Do not apply remote structural edits by simulating editor-local commands.
- Do not use DB8 paragraph diffs or full source materialization in the normal
  edit path.
- Do not turn layout/subparagraph chunks into durable CRDT structure.
- Do not let host ack block valid local editing.
- Do not accept syntactically decodable updates without semantic/resource
  validation.
- Do not compact history without an explicit cursor/undo/offline-client epoch
  policy.
- Do not silently recover malformed operations at guessed positions.

---

## Part 2: Remaining latency/collaboration patches and how CRDT-native model affects them

### Status legend

- **Done enough for now**: existing patches substantially covered it.
- **Partial**: some source-level work landed, but not complete.
- **Not started**: no meaningful implementation yet.
- **Deferred**: user explicitly deferred earlier.
- **Changed by CRDT-native model**: implementation should be different under full CRDT architecture.
- **Still crucial**: CRDT-native model does not eliminate need.
- **Mostly invalidated**: CRDT-native model makes current planned patch unnecessary or replaces it with a different mechanism.

---

### #1 Land DB8 canonical join fast path on origin

Current status: **Done enough for now**.

Patches added/expanded canonical operation fast paths, starting with joins and later split/text/delete/style-like paths.

Implementation in current codebase:

- `crates/flowstate/src/workspace/workspace/documents.rs`
- `Workspace::apply_collaboration_source_to_panel`
- Decode canonical ops from `UpdateApplication::Db8CanonicalOperations`.
- Apply safe ops directly with `editor.apply_remote_operations(...)` before DB8 paragraph diff.

Effect of full CRDT-native model: **Mostly invalidated as a long-term path**.

In a CRDT-native model, canonical join fast path should not be a special bypass
around DB8 diff. The fast path disappears because all live remote edits
integrate authoritative Loro updates and project their typed root diffs.

In vNext, a join is represented by the integrated Loro change that deletes an
adjacent paragraph-start token. The resulting Loro root diff produces the typed
projection delta; a canonical join operation is not wire authority.

Still useful during migration because it prevents structural joins from entering dangerous DB8 diff code.

---

### #2 Fix DB8 paragraph diff walker crash for joins

Current status: **Deferred by user**.

Implementation in current codebase:

- Locate `compute_paragraph_changes` internals.
- Add internal canaries around paragraph diff phases.
- Make diff walker total: return `Err` or fallback instead of aborting.

Effect of full CRDT-native model: **Mostly invalidated for normal editing, still useful for recovery/import**.

CRDT-native live editing should not use DB8 paragraph diff for normal join projection. But DB8 materialization/diff code may still be used for snapshot/recovery/import diagnostics, so it should not crash.

Priority under full model: lower for live latency, still important for robustness.

---

### #3 Add regression tests for host/non-host joins and splits

Current status: **Deferred by user**.

Implementation in current codebase:

- Build two-replica tests around editor/source integration.
- Host and non-host split/join styled, empty, long, adjacent paragraphs.
- Assert no panic, convergence, invariant validity.

Effect of full CRDT-native model: **Still crucial, but test shape changes**.

Tests should assert Loro-source convergence, deterministic materialization, and
projection convergence, not DB8 diff behavior. They become more important
because rewrite is larger.

---

### #4 Batch remote incremental projection into one editor projection transaction

Current status: **Done enough for current architecture; changed by full model**.

Implementation already landed:

- Use `editor.apply_remote_projection_batch(...)` around remote diff/operation application.
- Defer reconcile/layout/selection restore until the batch completes.

Implementation points:

- `crates/flowstate/src/workspace/workspace/documents.rs`
- `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`

Effect of full CRDT-native model: **Still crucial, but mechanism changes**.

Instead of batching DB8 paragraph diffs, batch Loro update integration and
projection-delta application:

```text
integrate one Loro update -> collect one typed root-diff delta -> update projection once -> invalidate layout once
```

Batching remains central to latency.

---

### #5 Skip text diffs consumed by structural join diffs

Current status: **Done enough for current architecture; mostly invalidated by full model**.

Implementation already landed:

- Precompute join-shaped structural removals.
- Skip redundant text diff entries consumed by join application.

Implementation point:

- `crates/flowstate/src/workspace/workspace/documents.rs`

Effect of full CRDT-native model: **Mostly invalidated**.

A Loro-native join deletes one structural boundary in its source commit. The
typed root-diff projection delta must not reconstruct and apply a redundant text
diff plus paragraph removal.

Still useful until DB8 diff path is removed from live editing.

---

### #6 Normalize canonical-operation fast paths beyond joins

Current status: **Partial**.

What landed:

- Join and split fast paths.
- Some expansion to text/delete/style-like ops in later patches.
- Experimental anchored plain text operations.

Still missing:

- Mixed operation streams with reliable rollback.
- Style spans anchored to Loro cursors.
- Loro-native structural split/join and typed projection deltas.
- Normal path that never falls back to DB8 paragraph diffs for live edits.

Implementation in current codebase:

- `crates/flowstate/src/workspace/workspace/documents.rs`
- `crates/gpui-flowtext/src/collaboration.rs`
- `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`

Effect of full CRDT-native model: **Mostly invalidated as architecture**.

The normal path is typed local intent -> one Loro commit -> exact Loro update
bytes -> one typed projection delta. Canonical operations remain temporary
comparison telemetry/compatibility hints and do not become a second CRDT
operation log. DB8 diff fallback becomes recovery-only.

Priority: high only as a migration bridge; very high for replacing it with the
vNext path.

---

### #7 Replace fragile PowerShell rewrite scripts with normal source changes/patches

Current status: **Done enough**.

Patch sequence is normal git-applyable source diff.

Effect of full CRDT-native model: **Still useful process rule**.

Large rewrite should be staged as reviewable Rust patches, not script mutations.

---

### #8 Remove or gate temporary canaries

Current status: **Deferred by user**.

Implementation:

- Audit `collab_canary` usage.
- Keep high-value phase timings behind `FLOWSTATE_COLLAB_CANARY`.
- Remove noisy per-keystroke or duplicated logs.

Effect of full CRDT-native model: **Still crucial after rewrite, not before**.

During rewrite, canaries are valuable. After stable transaction pipeline exists, convert them to structured diagnostics and keep only useful timing/invariant logs.

---

### #9 Add formal document invariants

Current status: **Partial-to-done practical version, not complete formal model**.

What landed:

- Public-ish `DocumentInvariantError`/validation-style checks were added in later patch.
- Remote projection can call invariant validation under diagnostics.

Still missing:

- Full vNext Loro source invariant model.
- Style interval anchor validation.
- Selection anchor validation.
- Layout chunk coverage validation.
- Source/projection equivalence checks.

Implementation in current codebase:

- `crates/gpui-flowtext/src/document/core.rs`
- `crates/gpui-flowtext/src/edit_ops/offsets.rs`
- `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`

Effect of full CRDT-native model: **More crucial**.

Invariants should move down to the Loro source and projection boundary:

```text
validate_loro_document
validate_projection_matches_loro
validate_layout_chunks_match_projection
validate_selection_anchors
```

This becomes the safety net for the rewrite.

---

### #10 Complete inbound/outbound latency architecture

Current status: **Partial**.

What landed:

- Reduced outbound fixed delays.
- Presence coalescing/dedupe improvements.
- Durable update prioritization tweaks.
- Optimistic client import before ack, with opt-out env var.
- Source replacement supersession in queue.

Still missing:

- Explicit scheduler with separate durable, presence, snapshot/recovery queues.
- Per-update time/size budgets.
- Causally safe batching/export across exact Loro updates.
- Backpressure and fairness policies.
- Local transaction queue independent of host ack.
- Clear metrics from input event to remote projection.

Implementation points:

- collaboration publish path
- sync client/host event loop
- workspace pending update queue
- `crates/flowstate/src/workspace/workspace/documents.rs`

Effect of full CRDT-native model: **Still crucial, implementation changes**.

CRDT-native model removes many projection-diff costs, but scheduling still
matters. The scheduler should operate on exact Loro updates/frontiers:

```text
high priority: local durable Loro updates
high priority: remote durable Loro updates
medium: acks/frontier compression
low/coalesced: presence
low/background: snapshot chunks
```

Host ack should not block local editing. It should mark durability/relay success.

Expected speedup after full implementation:

- large reduction in keypress-to-send latency
- smoother remote application during large updates
- less UI starvation during snapshot/recovery

---

### #11 Add join/split DB8 fuzz or property tests

Current status: **Not started**.

Implementation in current codebase:

- Generate random docs with paragraphs, UTF-8, styles, empty paragraphs.
- Apply split/join/delete/insert concurrently to two replicas.
- Assert convergence and invariants.

Effect of full CRDT-native model: **Still crucial, but becomes CRDT property testing**.

Test operations should invoke typed document intents, exchange exact Loro update
bytes, and assert:

- same final Loro state independent of delivery order
- same projection text/style/paragraph order
- valid selections after transformations
- no layout invariant failures after projection

Priority: high after the vNext source model lands.

---

### #12 Replace DB8 full materialization dependence in structural diffs

Current status: **Partial only by avoidance**.

What landed:

- Fast paths avoid some DB8 materialization/diff paths.
- No real DB8 materializer rewrite.

Implementation in current codebase:

- DB8 materialization code
- `source.compute_paragraph_changes(...)`
- `crates/flowstate/src/workspace/workspace/documents.rs`

Effect of full CRDT-native model: **Mostly invalidated for live editing; still useful for snapshots**.

Normal live projection should not depend on DB8 full materialization. Materialization should become:

- snapshot build
- recovery rebuild
- import/export
- diagnostic full validation

It still must be safe and total, but no longer sits on hot path.

---

### #13 Authority/actor/session model refactor

Current status: **Not implemented**.

Implementation in current codebase:

- sync actor/session types
- collaboration source mutation types
- workspace collaboration routing
- DB8 mutation application

Required work:

- Define `ActorId`, `ReplicaId`/Loro peer ID, `SessionId`, `Authority`,
  `UpdateOrigin`, `Frontier`, and `UpdateId`.
- Ensure host/client logic does not conflate source authority with CRDT actor.
- Echo handling should fast-forward durability/frontier, not reapply local semantic mutations.
- Reconnect should preserve replica identity while its outbox/undo lineage is
  retained and always create a new session ID.

Effect of full CRDT-native model: **More crucial**.

The full model depends on durable attribution, unique live Loro replica IDs, and
causal frontiers. This refactor is foundational, not optional.

---

### #14 Snapshot/recovery pipeline hardening

Current status: **Partial**.

What landed:

- Snapshot chunk progress/yield canaries.
- Supersede stale partial snapshot state when offset 0 with new hash/length arrives.

Still missing:

- Resume/retry protocol.
- Cancellation when newer snapshot supersedes old stream.
- Live update priority over snapshot chunks.
- Max-message-size enforcement everywhere.
- Snapshot epoch/frontier binding.

Implementation points:

- sync host snapshot export/chunk code
- sync client receive/recovery code
- workspace collaboration join/reconnect path

Effect of full CRDT-native model: **Still crucial**.

CRDT-native model makes snapshots cleaner: snapshot is Loro state plus
frontier/epoch. Live updates after snapshot frontier can be buffered and
replayed.

Recommended model:

```text
Snapshot(epoch, frontier, chunks)
Live updates tagged with base/result frontier
Client buffers live updates newer than snapshot frontier while snapshot installs
After install, replay compatible live updates
Cancel older snapshot if newer epoch starts
```

---

### #15 Undo/redo collaboration reconciliation

Current status: **Partial**.

What landed:

- Some remote mutation handling around undo/redo selections.
- Redo clearing/selection transform guardrails in prior patches.

Still missing:

- Stable actor/Loro peer identity.
- Loro `UndoManager` origin filtering and explicit group boundaries.
- Anchored selection metadata for undo items.
- Compaction and concurrent-removal no-op/partial policies.

Implementation points:

- editor undo/redo stack code
- canonical operation capture
- collaboration publish path
- selection transform code

Effect of full CRDT-native model: **Implementation changes completely; still crucial**.

Undo should use Loro's local-peer-aware `UndoManager`, not projection byte
ranges or a parallel inverse-operation log. Undo/redo commits then follow the
same exact-update and projection-delta path.

---

### #16 CRDT delete/offset hardening

Current status: **Partial**.

What landed:

- Some extra range/delete guardrails.
- Experimental anchored plain text delete mutation.

Still missing:

- Cursor-resolved delete for all text/style cases.
- Cross-paragraph deletes as atomic Loro source mutations.
- Concurrent delete/split/join tests.
- Loro history-retention and compaction policies.

Implementation points:

- `crates/gpui-flowtext/src/edit_ops/split_delete.rs`
- `crates/gpui-flowtext/src/edit_ops/offsets.rs`
- `crates/gpui-flowtext/src/edit_ops/text.rs`
- DB8 mutation adapter
- workspace collaboration diff application

Effect of full CRDT-native model: **Becomes core storage design**.

Offset hardening becomes Loro-cursor semantics. Byte offsets should only exist
in projection. Deletes resolve cursor ranges and mutate the relevant text flow
and structural tokens in one source commit.

Priority: very high.

---

### #17 Structural metadata/projection cleanup

Current status: **Partial**.

What landed:

- Atomic insert/split work.
- Batching.
- Style cleansing patch for end-of-paragraph split.
- Some canonical fast paths.

Still missing:

- One owner for paragraph insertion/removal.
- One owner for text/runs/style/block/section projection.
- Metadata CRDT map/register model.
- Elimination of receiver-side local heuristic inference.

Implementation points:

- `crates/flowstate/src/workspace/workspace/documents.rs`
- `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`
- metadata serialization/deserialization in document model

Effect of full CRDT-native model: **Still crucial, but done through source/projection boundary**.

All structural metadata should live in CRDT records. Projection should render it. Remote receive should not infer paragraph style or block metadata from neighboring projection state.

---

### #18 Long-document layout invalidation reduction

Current status: **Partial**.

What landed:

- Some remote layout invalidation narrowing.

Still missing:

- Full affected-range tracking from Loro root diff through projection to layout chunks.
- Cache retention across remote edits.
- Paragraph/block/subparagraph invalidation policy.

Implementation points:

- rich text layout/prep code
- virtual list sizing/cache code
- editor lifecycle remote mutation paths
- document offset/block replacement code

Effect of full CRDT-native model: **More effective and more crucial**.

Typed projection deltas derived from Loro root diffs provide touched stable IDs
and cursor ranges. Use those to invalidate projection and layout minimally:

```text
Loro root diff -> typed projection delta touched cursors/IDs
  -> projection byte ranges
  -> subparagraph layout chunk IDs
  -> cache invalidation only for touched chunks
```

This is one of the main ways CRDT-native model pays for its bookkeeping overhead.

---

### #19 Presence/frontier traffic refinement

Current status: **Partial-to-mostly improved**.

What landed:

- Presence dedupe/coalescing.
- Remote caret refresh early exit.
- Presence priority below durable work.
- Frontier-aware dedupe improvements.

Still missing:

- Frontier acknowledgement compression.
- Per-peer presence latest-only queue.
- Presence anchors instead of byte offsets.
- No durable delay behind presence under all host/client states.

Implementation points:

- workspace pending update queue
- sync presence update handling
- collaboration client/host event loop

Effect of full CRDT-native model: **Still crucial, representation changes**.

Presence should carry Loro cursor selections and frontier, not only projected
offsets. This makes remote cursors stable under concurrent edits and lets
duplicate presence drop safely.

---

### #20 Collaboration diagnostics test harness

Current status: **Partial only through canaries; no harness**.

What landed:

- Phase canaries and timing logs in several patches.
- No real host/non-host replay harness.

Implementation points:

- workspace collaboration tests
- sync test utilities
- DB8/CRDT document construction helpers
- editor test harness

Required harness:

```text
create host + client replicas
apply typed local intent on one side
capture/persist/publish exact Loro update bytes
deliver in arbitrary order/delay
apply on other side
capture canary phases/timings
assert Loro convergence
assert projection convergence
assert layout/selection invariants
```

Effect of full CRDT-native model: **More crucial**.

A full CRDT rewrite must be validated with randomized delivery order, concurrent edits, reconnects, snapshots, undo, split/join, and style operations.

---

## Recommended order for coding agent

### Milestone 0: Prove Loro vNext structure

1. Define the text-flow, structural-token, typed-record, and protected-state
   schema.
2. Prove cursor/mark behavior for concurrent split/join, empty paragraphs,
   undo, snapshot, and compaction.
3. Define actor/replica/session identity, peer registration, and history epochs.
4. Build the multi-replica property-test/reference-materializer harness.

Expected result: the hardest storage decision is validated before broad editor
changes.

### Milestone 1: Build the authoritative document controller

1. Add the vNext source model in the document-model layer.
2. Expose typed edit intents that mutate Loro and commit once.
3. Capture exact local update bytes, root diffs, frontiers, and origins.
4. Add deterministic full materialization and source invariant validation.

Expected result: one authoritative local mutation path exists without coupling
`gpui-flowtext` to transport.

### Milestone 2: Build atomic projection deltas

1. Convert Loro root diffs into typed rich-text projection deltas.
2. Maintain stable-ID/cursor to visual-index/UTF-8-byte mappings.
3. Apply each source commit atomically to projection and selection.
4. Invalidate only affected subparagraph/layout chunks.

Expected result: local and remote updates share one incremental projection path.

### Milestone 3: Move editing source-first

1. Move plain text/selection/IME commits to typed intents and Loro cursors.
2. Move split/join/cross-paragraph delete to structural-token mutations.
3. Move styles/metadata and Loro `UndoManager` integration.
4. Keep canonical/granular capture only as comparison telemetry.

Expected result: eliminates the "host emitted an operation the host UI did not
show" failure class.

### Milestone 4: Replace the live wire/apply path

1. Persist and publish exact Loro update bytes.
2. Validate updates on a fork, including authorship/protected-state/resource
   limits.
3. Import updates and apply typed projection deltas.
4. Remove canonical fast-path and DB8 paragraph-diff authority from live edits.

Expected result: structural collaboration no longer depends on guessed offsets
or special-case fast paths.

### Milestone 5: Complete document semantics and persistence

1. Add first-class rich blocks, stable table row/cell IDs, and child flows.
2. Move presence/selections to Loro cursors.
3. Add content-addressed asset references and separate transfer.
4. Implement native save, snapshots, history epochs, compaction, and v2-to-vNext
   migration.

Expected result: the entire supported document model has one durable source.

### Milestone 6: Cutover, performance, and cleanup

1. Run randomized convergence, hostile-update, migration, and restart suites.
2. Meet input-to-paint, update-size, validation, and large-document layout
   budgets.
3. Enforce durable/presence/snapshot/asset queue priorities and backpressure.
4. Delete the dual live path after compatibility gates pass.

Expected result: a maintainable CRDT-native pipeline with measured latency and
no co-authoritative legacy path.

---

## Expected latency wins by area

### Loro-native source-first capture

Expected speedup: small direct latency gain, large correctness gain.

Noticeable behavior:

- fewer remote-only stray characters
- fewer recovery/reconnect pauses caused by bad granular ops

### Removing DB8 diff from normal live editing

Expected speedup: medium to large.

Noticeable behavior:

- remote split/join/text appears faster
- less freeze during remote burst
- fewer full projection fallbacks

### Loro-cursor projection deltas

Expected speedup: medium.

Noticeable behavior:

- remote edits invalidate less of document
- large docs feel smoother

### Scheduler split: durable vs presence vs snapshot

Expected speedup: medium to large during collaboration load.

Noticeable behavior:

- typing stays responsive while cursor/presence/snapshot traffic exists
- less several-second freeze during remote bursts

### Layout chunk invalidation from typed Loro root diffs

Expected speedup: large for long documents.

Noticeable behavior:

- edits in one paragraph do not cause whole-doc layout churn
- remote bursts apply incrementally

---

## Final recommendation

Do not keep stacking offset fast paths indefinitely. Retain Loro, but replace
the current per-paragraph/copy-delete schema and projection-first edit pipeline.
The important long-term change is to make a committed Loro frontier the native
document truth.

The highest-leverage rewrite boundary is not replacing the entire renderer first. It is replacing local edit capture first:

```text
current: editor edit -> later infer/source diff -> publish guessed mutation
wanted: typed intent -> one Loro commit -> project root diff -> persist/publish exact Loro update
```

Do not start source-first editing on the current per-paragraph Loro schema
without first proving the text-flow structural-token model; doing so would make
the split/join identity defect authoritative. Once that schema and exact-update
capture are proven, the current UI/projection/layout systems can migrate
incrementally behind the new document-controller boundary.
