# BINDING COMPLETION DIRECTIVE — COLLABORATION ARCHITECTURE

## Objective

Finish the full contract in `COLLABOPT.md`.

DB8 collaboration must use Loro CRDT state as sole durable authority. Native editor state is only a projection. Canonical operations may remain as local UI/history hints, but must never be required to repair, replace, or reconstruct durable DB8 collaboration state.

Do not stop after partial compile fixes. Stop only when every requirement and verification item below passes.

---

## 1. Restore build integrity first

Fix all malformed or incomplete edits in:

- `crates/flowstate-collab/src/source.rs`
- `crates/gpui-flowtext/src/collaboration.rs`
- `crates/flowstate/src/workspace/workspace/documents.rs`
- `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`
- `crates/gpui-flowtext/src/rich_text/editor/edit_pipeline.rs`

Required result:

```text
cargo check -p flowstate-collab -p gpui-flowtext -p flowstate-sync -p flowstate
```

must pass before architectural work is considered complete.

No dead fields, stale helpers, duplicate branches, malformed ranges, or unused hash helpers may remain.

---

## 2. Implement true frontier dedup

File:

- `crates/flowstate/src/workspace/workspace/documents.rs`

Required behavior:

1. Read current CRDT frontier once.
2. Compare it directly with `collaboration_last_frontier`.
3. If equal, return immediately.
4. Do not compute `projection_hash()`.
5. Do not read or materialize full granular source for dedup.
6. Update `collaboration_last_frontier` only after successful incremental projection or successful explicit recovery.

Required removal:

- all DB8 use of `collaboration_last_published_hash`
- all DB8 hash-based early-return logic
- all DB8 projection-hash dedup

Frontier equality is the no-op test. Content hashing is not.

---

## 3. Make DB8 CRDT diff coverage complete

File:

- `crates/flowstate-collab/src/source.rs`

`compute_paragraph_changes` must handle every DB8-relevant CRDT change without panic and without returning an unrepresentable partial result.

Required support:

- text insert
- text delete
- text replacement
- text marks
- text unmarks
- paragraph metadata change
- paragraph insertion
- paragraph removal
- paragraph move/reorder
- multiple list inserts/deletes in one diff
- mixed text + metadata + order changes in one batch

Requirements:

- no `catch_unwind`
- no panic-based control flow
- validate old frontier against current VersionVector before diffing
- return typed error only when frontier is genuinely incompatible or schema is invalid
- never silently ignore an unknown `Diff` variant
- never treat unsupported structural diff as empty
- preserve exact final order from Loro list state

Add tests for every case above, including mixed batches and stale/incompatible frontier.

---

## 4. Complete paragraph mutation semantics

Files:

- `crates/flowstate-collab/src/source.rs`
- `crates/gpui-flowtext/src/collaboration.rs`

`InsertParagraph` and `RemoveParagraph` must preserve document content and order.

Required behavior:

### InsertParagraph

- create text container
- initialize metadata when required
- insert ID at exact CRDT order position
- reject duplicate IDs
- reject unknown anchor unless operation explicitly means insertion at start
- avoid orphan container if order insertion fails

### RemoveParagraph

- remove ID from order list
- remove text container and metadata
- produce deterministic behavior if ID is absent
- avoid leaving orphan records

### Structural canonical operations

Every operation must map losslessly to granular mutations:

- `SplitParagraph`
- `JoinParagraphs`
- cross-paragraph `DeleteRange`
- `ReplaceParagraphSpan`
- block insert/delete/move/replace where DB8 collaboration supports those records
- `ReplaceDocument`

A mapping is not complete if it only inserts or removes paragraph IDs while losing text, styles, metadata, blocks, or ordering.

`ReplaceDocument` must not map to an empty mutation list.

If a canonical operation cannot be represented by current mutation types, add required mutation types. Do not silently drop it. Do not restore `repair_required`.

---

## 5. Eliminate dual authority for DB8

Files:

- `crates/gpui-flowtext/src/collaboration.rs`
- `crates/gpui-flowtext/src/rich_text/editor/edit_pipeline.rs`
- `crates/flowstate/src/workspace/workspace/documents.rs`

Required architecture:

```text
Local edit
  -> granular CRDT mutations
  -> Loro durable state
  -> editor projection

Remote edit
  -> Loro import
  -> CRDT diff
  -> editor projection
```

Forbidden architecture:

```text
Local edit
  -> native document as durable truth
  -> canonical operation adapter
  -> repair/full-source replacement when adapter fails
```

Canonical operations may remain for undo/history/local echo, but DB8 durability must not depend on them.

Required removal:

- `repair_required`
- DB8 full-source repair fallback
- silent no-op adaptation
- canonical-operation application as authoritative remote truth

Local DB8 edits must always produce complete granular mutations.

---

## 6. Implement frontier-anchored identity mapping

Files:

- `crates/gpui-flowtext/src/collaboration.rs`
- `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`

Current `ParagraphId`-only lookup is insufficient.

Add stable mapping between CRDT paragraph/text identity and editor projection identity.

Required mapping:

```text
Loro text/container ID <-> editor ParagraphId <-> current paragraph index
```

Required behavior:

- paragraph insert updates mapping
- paragraph remove updates mapping
- paragraph move updates index mapping
- full initial projection builds mapping
- incremental text/style apply resolves through CRDT identity first
- no incremental apply may fail only because native index map drifted
- mapping must remain valid across remote structural edits

`apply_remote_text_change` and style equivalents must not return `false` because paragraph identity was not reconciled after a prior structural change.

Add tests for insert, remove, move, then text edit on affected paragraph.

---

## 7. Remove normal DB8 full materialization fallback

File:

- `crates/flowstate/src/workspace/workspace/documents.rs`

Normal DB8 live updates must never call full document replacement.

Remove DB8 runtime path to:

- `collab_document_to_workspace_document(...)` for ordinary live updates
- `replace_document_from_collaboration(...)`
- `apply_db8_replace`
- any equivalent 6170-paragraph materialization fallback

Allowed full materialization cases only:

- initial join/open
- explicit snapshot recovery after confirmed incompatible frontier or corrupted schema
- user-triggered/document-recovery flow

Recovery must be explicit, logged, rare, and separate from normal update processing.

An incremental projection failure must expose an error and request recovery. It must not silently materialize full DB8 source on every update.

---

## 8. Simplify DB8 outbound path

Files:

- `crates/flowstate/src/workspace/workspace/documents.rs`
- `crates/flowstate-sync/src/lib.rs`

DB8 outbound durable updates must use only granular mutations.

Required:

- `publish_db8_collaboration_edit` sends granular mutations only
- no DB8 call to `publish_collaboration_source`
- no DB8 call to `replace_source_from`
- no DB8 `PendingCollaborationUpdate::Source`
- no DB8 full `CollabDocument` queued as fallback
- no DB8 projection hash calculation during publish

FL0 may retain source replacement if its architecture requires it, but DB8 path must be statically separate. Do not keep shared enum behavior that permits DB8 source replacement.

Preferred queue shape:

```rust
enum PendingCollaborationUpdate {
    Db8GranularMutations { ... },
    Fl0Source { ... },
    Presence { ... },
}
```

Names may differ. Type structure must make DB8 source replacement impossible.

---

## 9. Make recovery explicit

Files:

- `crates/flowstate/src/workspace/workspace/documents.rs`
- `crates/flowstate-sync/src/lib.rs`

When frontier is incompatible or CRDT schema is invalid:

1. stop incremental apply for that update
2. emit clear diagnostic
3. request fresh snapshot using existing recovery protocol, such as `WireMessage::Need`
4. rebuild projection once
5. reset frontier and identity mapping
6. resume granular incremental updates

Do not disguise recovery as normal source publication or normal remote apply.

---

## 10. Remove obsolete paths and symbols

Search entire workspace and remove or isolate DB8 references to:

```text
repair_required
collaboration_last_published_hash
apply_db8_replace
publish_db8_source_fallback
replace_source_from
PendingCollaborationUpdate::Source
ReplaceDocument => empty mutations
catch_unwind
projection_hash() in DB8 hot path
```

Any remaining occurrence must be justified as FL0-only, initial load, explicit recovery, test fixture, or unrelated code.

---

## 11. Required tests

Add or update tests proving:

1. equal frontier causes zero projection work
2. one-character remote edit touches one paragraph only
3. mark/unmark remains incremental
4. paragraph insert remains incremental
5. paragraph remove remains incremental
6. paragraph move remains incremental
7. split preserves both halves, styles, IDs, and order
8. join preserves both texts and styles
9. cross-paragraph delete produces correct final records
10. mixed structural + text diff applies in one batch
11. identity mapping survives insert/remove/move
12. unsupported canonical operation cannot silently produce empty mutations
13. DB8 outbound queue cannot contain full source replacement
14. incompatible frontier triggers explicit snapshot recovery once
15. 6170-paragraph document does not fully materialize on ordinary remote keystroke

Tests must assert behavior, not only enum shape.

---

## 12. Final verification

Run:

```text
cargo fmt --check
cargo check -p flowstate-collab -p gpui-flowtext -p flowstate-sync -p flowstate
cargo clippy -p flowstate-collab -p gpui-flowtext -p flowstate-sync -p flowstate -- -D warnings
cargo test -p flowstate-collab
cargo test -p gpui-flowtext
cargo test -p flowstate-sync
cargo test -p flowstate
```

Then run workspace-wide searches for obsolete symbols listed above.

Provide final report containing:

- files changed
- architectural path after changes
- removed fallback paths
- recovery behavior
- test names added
- command results
- any remaining FL0-only source replacement paths

---

## Completion rule

Do not declare completion because code compiles.

Contract is complete only when:

- DB8 Loro state is sole durable authority
- every local DB8 edit produces lossless granular mutations
- every normal remote DB8 update projects incrementally
- paragraph identity is anchored to CRDT identity
- DB8 cannot queue or publish full source replacement
- full DB8 materialization occurs only during initial load or explicit recovery
- all required checks and tests pass
