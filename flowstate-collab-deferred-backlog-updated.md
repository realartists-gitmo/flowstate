# Flowstate collaboration: deferred, tabled, failed, and not-yet-landed work

Generated: 2026-06-11  
Baseline checked: `realartists-gitmo/flowstate`, branch `troubleshooting`  
Observed remote HEAD during this update: `81cd764aadbd1c2bb0cfdda3a7699a43a3a53243` (`Fixed paragraph joining for non-host`)

## Scope

This is a backlog of items discussed during the collaboration/CRDT repair sequence that are not currently treated as fully landed, final, or verified on the pushed `troubleshooting` branch. It includes:

- Items explicitly tabled or delayed.
- Patches that were attempted but failed to apply, failed to compile, or were superseded.
- Performance/architecture work that was discussed but only partially implemented.
- Hardening and test work that remains necessary even after the host/non-host join path became smooth locally.

This is not a general product roadmap. It is narrowly scoped to the collaboration, DB8/CRDT, rich text projection, structural edit, and latency work from this debugging sequence.

## Current implemented baseline, as understood

The current pushed branch appears to include:

- Atomic remote paragraph insertion/split handling through `apply_remote_insert_paragraph_authoritative`.
- Direct authoritative join rewrite inside `apply_remote_join_paragraphs_authoritative`.
- Direct remote join path avoiding `delete_cross_paragraph_range` for already-authoritative join text.
- Paragraph style compile fix from `ParagraphStyle::default()` to `ParagraphStyle::Normal`.
- `new_text.as_str()` compile fix for the join text path.
- DB8 canonical join fast path for pure `JoinParagraphs` update applications, bypassing the crashing DB8 paragraph-diff walker.
- Prior latency improvements sufficient to noticeably improve interactive behavior.
- Split/join parity repairs up to the point where host split and host join are smooth in manual testing.

The current pushed remote now includes the DB8 canonical join fast path. That item is no longer pending implementation; it remains in this backlog as landed-but-needs-test-coverage and cleanup.

## Priority legend

- **P0**: Required to keep host/non-host collaboration correct and non-crashing.
- **P1**: Strongly recommended before merging into a durable working branch.
- **P2**: Performance/hardening improvement; can be delayed if behavior is stable.
- **P3**: Cleanup, diagnostics, or architectural polish.

Improvement size estimates:

- **Critical**: prevents crash/data corruption.
- **Large**: visibly improves latency, reliability, or convergence in common workflows.
- **Medium**: improves maintainability or avoids edge-case failures.
- **Small**: cleanup, log noise reduction, or narrow optimization.

## Executive backlog

| # | Item | Priority | Improvement | Status |
|---|------|----------|-------------|--------|
| 1 | DB8 canonical join fast path: keep, test, and clean up | P0/P1 | Critical | Landed on origin; needs tests and cleanup |
| 2 | Fix DB8 paragraph diff walker crash for joins | P0/P1 | Critical | Root cause not fixed; bypassed by fast path |
| 3 | Add regression tests for host/non-host joins and splits | P0/P1 | Critical | Not implemented |
| 4 | Batch remote incremental projection into one editor projection transaction | P1 | Large | Not implemented |
| 5 | Skip text diffs consumed by structural join diffs | P1 | Medium/Large | Not implemented |
| 6 | Normalize canonical-operation fast paths beyond joins | P1/P2 | Large | Not implemented |
| 7 | Replace fragile PowerShell rewrite scripts with normal Rust changes/patches | P1 | Medium | Not implemented |
| 8 | Remove or gate temporary canaries | P2 | Small/Medium | Not cleaned up |
| 9 | Add formal invariants for document paragraphs/blocks/ids/offsets | P1 | Large | Not implemented |
| 10 | Complete inbound/outbound latency architecture | P2 | Large | Partially implemented |
| 11 | Add join/split DB8 fuzz/property tests | P1/P2 | Large | Not implemented |
| 12 | Replace DB8 full materialization dependence in structural diffs | P2 | Medium/Large | Not implemented |
| 13 | Authority/actor/session model refactor | P2/P3 | Medium | Attempted/tabled |
| 14 | Snapshot/recovery pipeline hardening | P2 | Medium/Large | Partially implemented |
| 15 | Undo/redo collaboration reconciliation | P2 | Medium | Attempted/tabled |
| 16 | CRDT delete/offset hardening | P2 | Medium | Earlier attempts; unclear final coverage |
| 17 | Structural metadata/projection cleanup | P2/P3 | Medium | Partially superseded |
| 18 | Long-document layout invalidation reduction | P2 | Large | Partially implemented/tabled |
| 19 | Presence/frontier traffic refinement | P2/P3 | Medium | Partially implemented |
| 20 | Collaboration diagnostics test harness | P2 | Medium | Not implemented |

---

## 1. DB8 canonical join fast path: keep, test, and clean up

**Priority:** P0/P1  
**Improvement size:** Critical  
**Current status:** Landed on pushed `troubleshooting` in `81cd764aadbd1c2bb0cfdda3a7699a43a3a53243` (`Fixed paragraph joining for non-host`). It is no longer pending implementation, but it still needs regression tests and diagnostic cleanup.

### Why this is necessary

The non-host crash happened after receiving a host update and entering `apply_source_enter`, but before editor-level join canaries fired. That means the crash occurred before `apply_remote_join_paragraphs_authoritative`, most likely inside DB8 paragraph diff computation.

The host update already carries a canonical `JoinParagraphs` payload. For pure join updates, the receiver should apply that canonical operation directly instead of reconstructing the join by diffing DB8 paragraph shape.

### Pertinent files

- `crates/flowstate/src/workspace/workspace/documents.rs`
- `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`
- `crates/gpui-flowtext/src/collaboration.rs`

### How it is implemented now

In `Workspace::apply_collaboration_source_to_panel`, before `source.compute_paragraph_changes(&self.collaboration_last_frontier)`, the code checks for `UpdateApplication::Db8CanonicalOperations(bytes)`, decodes canonical operations, verifies the operation list is non-empty and all operations are `CanonicalOperation::JoinParagraphs`, then applies them via `editor.apply_remote_operations(&operations, cx)`. If successful, it advances `collaboration_last_frontier`, refreshes DB8 remote carets, and returns without entering the DB8 paragraph-diff walker.

### Remaining work

1. Add regression tests for the fast path.
2. Confirm the fast path is only used for pure joins.
3. Decide whether to keep or demote `apply_db8_application_join_fast_path` canaries.
4. Add a negative test showing non-join DB8 updates still use the incremental DB8 diff path.
5. Add a stress test for consecutive joins.

### Expected impact

Critical reliability improvement. It prevents host-originated joins from entering the crashing DB8 paragraph-diff path.

Latency impact is positive: direct canonical operation application is cheaper than DB8 paragraph change computation.

### Notes

This is not a full-projection containment hack. It is a correct semantic fast path because the wire update already includes the canonical structural operation.

---

## 2. Fix DB8 paragraph diff walker crash for joins

**Priority:** P0/P1  
**Improvement size:** Critical  
**Current status:** Not fixed. The direct canonical join fast path now landed on origin and bypasses the crash for pure join applications.

### Why this is necessary

Even if pure joins are handled through canonical operations, `source.compute_paragraph_changes(...)` should not crash the process. It should either return a valid structural diff, return an error, or trigger fallback.

A process exit code such as `0xcfffffff` without a Rust panic suggests a lower-level abort, FFI issue, allocator issue, unchecked panic across boundary, or unsafe invariant failure in a dependency.

### Pertinent files

Likely files:

- DB8/CRDT paragraph diff implementation, wherever `compute_paragraph_changes` is defined.
- `crates/flowstate/src/workspace/workspace/documents.rs`
- DB8 source/document adapter files.
- Any flowstate-sync DB8 mutation application code.

Search targets:

- `compute_paragraph_changes`
- `ParagraphDiffEntry`
- `ParagraphRemoved`
- `ParagraphAdded`
- `Text { text_id, new_text, marks }`

### How to implement

Add canaries immediately before and after `compute_paragraph_changes` and inside the diff walker itself:

```rust
collab_canary("compute_paragraph_changes_enter", ...);
let result = source.compute_paragraph_changes(&frontier);
collab_canary("compute_paragraph_changes_exit", ...);
```

Inside the DB8 diff walker, log old/new frontier, old/new paragraph count, removed/added/moved text IDs, text IDs with content diffs, and whether the diff resembles a join.

Then isolate the failing operation:

1. Run host join only.
2. Confirm crash occurs before `compute_paragraph_changes_exit`.
3. Add progressively deeper logs until the last emitted stage identifies the exact failing internal phase.

The fix should make diff computation total and non-aborting: return `Err` if DB8 state cannot be compared safely, return `ParagraphRemoved + Text` if join shape is safe, or force caller fallback.

### Expected impact

Critical. It removes an entire crash class even when future code paths accidentally call DB8 diff for structural joins.

---

## 3. Add regression tests for host/non-host joins and splits

**Priority:** P0/P1  
**Improvement size:** Critical  
**Current status:** Not implemented.

### Why this is necessary

The same area regressed multiple times:

- Host join crashed non-host.
- Host split later crashed non-host.
- Non-host-originated joins/splits behaved differently from host-originated ones.
- Compile fixes were needed after receiver changes.

These must be covered by tests before further latency work.

### Pertinent files

Likely locations:

- `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`
- `crates/gpui-flowtext/src/collaboration.rs`
- `crates/flowstate/src/workspace/workspace/documents.rs`
- `crates/flowstate/src/workspace/workspace/tests.rs`
- any existing collaboration test module.

### How to implement

Create tests that simulate two editor/document replicas:

1. Start with identical document.
2. Host performs split paragraph, join paragraphs, join after styled text, join at empty paragraph, and join with following paragraphs present.
3. Non-host receives update.
4. Assert no panic, paragraph IDs converge, text converges, runs/marks converge, block/paragraph IDs lengths match, offset index validates, and sections rebuild validly.

Add mirrored non-host-originated cases.

### Expected impact

Critical. It prevents future regressions in the most fragile area.

---

## 4. Batch remote incremental projection into one editor projection transaction

**Priority:** P1  
**Improvement size:** Large  
**Current status:** Not implemented.

### Why this is necessary

Remote DB8 updates currently process multiple `ParagraphDiffEntry` changes inside a loop. Each editor method may call `finish_remote_projection_change(...)`, which can trigger reconcile/layout/update work before the whole remote diff is applied.

Structural diffs should be applied as a single receiver transaction whenever possible.

### Pertinent files

- `crates/flowstate/src/workspace/workspace/documents.rs`
- `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`

### How to implement

Wrap the incremental loop:

```rust
let incremental_ok = editor.update(cx, |editor, cx| {
    editor.apply_remote_projection_batch(cx, |editor, cx| {
        editor.clear_collaboration_edit();
        for entry in &changes {
            ...
        }
        editor.clear_collaboration_edit();
        true
    })
});
```

Ensure `apply_remote_projection_batch` defers identity reconciliation, section rebuilds, layout notification, and restores backup document/selection on failure.

### Expected impact

Large. Reduces invalid intermediate states and repeated layout/reconcile work. It also helps with latency under multi-entry remote updates.

---

## 5. Skip text diffs consumed by structural join diffs

**Priority:** P1  
**Improvement size:** Medium/Large  
**Current status:** Not implemented.

### Why this is necessary

A join-shaped DB8 diff often appears as:

- `Text(first_paragraph = merged_text)`
- `ParagraphRemoved(second_paragraph)`

If the text diff is applied first, then the structural join applies merged text again. This can be safe if perfectly idempotent, but it is unnecessary and can trigger intermediate invalid states.

### Pertinent files

- `crates/flowstate/src/workspace/workspace/documents.rs`

### How to implement

Before processing entries, precompute join pairs and consumed text IDs. In the `ParagraphDiffEntry::Text` arm, skip text entries whose paragraph ID is the predecessor consumed by a `ParagraphRemoved` entry.

### Expected impact

Medium to large. It reduces redundant work and makes remote structural application more deterministic.

---

## 6. Normalize canonical-operation fast paths beyond joins

**Priority:** P1/P2  
**Improvement size:** Large  
**Current status:** Not implemented.

### Why this is necessary

Once canonical join fast path is proven, other canonical structural edits may benefit from the same approach. The canonical operation stream is already the semantic edit boundary; DB8 diff reconstruction should not be required for every receiver case.

### Pertinent files

- `crates/flowstate/src/workspace/workspace/documents.rs`
- `crates/gpui-flowtext/src/collaboration.rs`
- `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`

### How to implement

Add a general canonical application path for safe operation classes:

- `JoinParagraphs`
- `SplitParagraph`
- maybe `InsertText`
- maybe `DeleteRange`

Start conservatively with pure joins and pure splits. Add mixed operation support only after regression tests exist.

### Expected impact

Large for latency and reliability. It reduces dependence on DB8 paragraph diff reconstruction.

---

## 7. Replace fragile PowerShell rewrite scripts with normal source changes

**Priority:** P1  
**Improvement size:** Medium  
**Current status:** Not implemented.

### Why this is necessary

Several changes landed through script-based replacement because patches failed against drifting local state. That is acceptable during debugging, but not ideal for review or long-term maintenance.

### Pertinent files

- `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`
- `crates/flowstate/src/workspace/workspace/documents.rs`

### How to implement

Once the code is stable:

1. Ensure local branch is clean.
2. Commit the script-produced source changes.
3. Generate a normal Git diff from the final file state.
4. Remove references to one-off scripts from the working process.

### Expected impact

Medium. Improves reviewability and makes future bisecting possible.

---

## 8. Remove or gate temporary canaries

**Priority:** P2  
**Improvement size:** Small/Medium  
**Current status:** Not cleaned up.

### Why this is necessary

Canaries were useful during debugging but may create log noise or slight overhead. Some should remain behind `FLOWSTATE_COLLAB_CANARY`; others should be removed or converted to structured diagnostics.

### Pertinent files

- `crates/flowstate/src/workspace/workspace/documents.rs`
- `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`
- sync/client/host update handling files.

### How to implement

Keep high-value gated logs such as update receive/send phase logs, fallback diagnostics, and canonical fast-path hit/miss. Remove or demote high-frequency logs such as `publish_db8_skip_no_local_edit`, temporary `remote_join phase=...` lines, and redundant structural insert logs once tests cover the path.

### Expected impact

Small/medium. Cleaner logs and lower overhead during active collaboration.

---

## 9. Add formal document invariants

**Priority:** P1  
**Improvement size:** Large  
**Current status:** Not implemented.

### Why this is necessary

Most crashes came from structural mismatch possibilities:

- paragraph vector length vs paragraph IDs length,
- blocks vs paragraph blocks,
- block IDs vs blocks,
- paragraph byte ranges vs rope text,
- offset index vs paragraph ranges,
- sections vs paragraph IDs.

The editor needs a cheap invariant checker that can run after remote structural mutations in debug/canary mode.

### Pertinent files

- `crates/gpui-flowtext/src/document/core.rs`
- `crates/gpui-flowtext/src/edit_ops/offsets.rs`
- `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`

### How to implement

Add:

```rust
pub fn validate_document_invariants(document: &Document) -> Result<(), DocumentInvariantError>
```

Check ID vector lengths, paragraph block correspondence, paragraph byte ranges, run lengths, UTF-8 boundaries, and section references.

### Expected impact

Large. It turns silent corruption/crashes into actionable errors.

---

## 10. Complete inbound/outbound latency architecture

**Priority:** P2  
**Improvement size:** Large  
**Current status:** Partially implemented.

### Why this is necessary

Latency improved after prior patches, but the full architecture discussed earlier was not fully landed. Collaboration still has several work queues that can block each other or do repeated work.

### Pertinent files

- `crates/flowstate/src/workspace/workspace/documents.rs`
- collaboration publish/receive code
- sync client/host code
- workspace pending update queue code

### How to implement

Break into independent pieces:

1. Separate durable document updates from presence.
2. Keep only latest presence per peer.
3. Process document updates with priority over background snapshot/recovery.
4. Batch small text updates adaptively.
5. Coalesce adjacent text mutations more aggressively.
6. Avoid doing layout/reconcile more than once per remote update.
7. Use per-update size/time budgets.

### Expected impact

Large for large documents and active collaboration.

---

## 11. Add join/split DB8 fuzz or property tests

**Priority:** P1/P2  
**Improvement size:** Large  
**Current status:** Not implemented.

### Why this is necessary

Join/split correctness depends on byte offsets, paragraph IDs, runs/marks, and CRDT order. Simple unit tests will miss many edge cases.

### Pertinent files

- DB8 source adapter/diff code
- `crates/gpui-flowtext/src/collaboration.rs`
- editor edit-op modules
- workspace collaboration tests

### How to implement

Generate random documents with multiple paragraphs, random UTF-8 text, random run styling, empty paragraphs, and adjacent structural edits. Apply random split/join operations to two replicas, then assert convergence and invariants.

### Expected impact

Large. It catches the exact class of bugs that caused this debugging session.

---

## 12. Replace DB8 full materialization dependence in structural diffs

**Priority:** P2  
**Improvement size:** Medium/Large  
**Current status:** Not implemented.

### Why this is necessary

Earlier full-projection fallback was unsafe for some structural cases because full DB8 materialization compared the new CRDT paragraph order against old projection shape and could fail before dedicated handlers ran.

### Pertinent files

- `crates/flowstate/src/workspace/workspace/documents.rs`
- DB8 materialization code
- DB8 paragraph diff code

### How to implement

Make DB8 materialization independent of old editor projection shape. It should build a complete document from DB8 records without relying on old paragraph order for validation.

### Expected impact

Medium/large. Makes fallback safer and reduces the need for special-case structural handlers.

---

## 13. Authority/actor/session model refactor

**Priority:** P2/P3  
**Improvement size:** Medium  
**Current status:** Attempted/tabled.

### Why this is necessary

There were earlier attempts to refactor authority/actor/session behavior. The goal was to make host/client source mutations clearer and avoid ambiguity around who authored which CRDT operation.

### Pertinent files

Likely:

- sync actor/session types,
- collaboration source mutation types,
- workspace collaboration routing,
- DB8 mutation application code.

### How to implement

Document and then refactor these concepts:

- host authority,
- client actor ID,
- session ID,
- CRDT actor,
- update source,
- local echo behavior,
- remote application behavior.

### Expected impact

Medium. It reduces future collaboration routing bugs.

---

## 14. Snapshot/recovery pipeline hardening

**Priority:** P2  
**Improvement size:** Medium/Large  
**Current status:** Partially implemented.

### Why this is necessary

Logs showed snapshot export/chunking for a large snapshot. Large snapshot transfer must not block live collaboration or overload message limits.

### Pertinent files

- sync host snapshot export/chunk code
- sync client receive/recovery code
- workspace collaboration connect/join handling

### How to implement

Improve chunk scheduling, resume/retry behavior, progress canaries, cancellation when a newer snapshot supersedes older chunks, live update queue priority over snapshot chunks, and max-message-size enforcement.

### Expected impact

Medium/large for large documents and reconnects.

---

## 15. Undo/redo collaboration reconciliation

**Priority:** P2  
**Improvement size:** Medium  
**Current status:** Earlier patch attempted/tabled.

### Why this is necessary

Undo/redo must produce canonical operations that remote replicas can apply safely, and local undo state must remain coherent after remote operations transform the document.

### Pertinent files

- editor undo/redo stack code
- canonical operation capture
- collaboration publish path
- selection transform code

### How to implement

Define undo/redo collaboration semantics: local-only undo of own operations, no undo of remote operations, remote operations transform local undo entries, and undo emits canonical operations like normal edits. Add tests for undo after remote split/join/text edit.

### Expected impact

Medium. Important for usability but less urgent than crash prevention.

---

## 16. CRDT delete/offset hardening

**Priority:** P2  
**Improvement size:** Medium  
**Current status:** Earlier patches attempted; final coverage unclear.

### Why this is necessary

Delete operations are byte-offset sensitive. Earlier delete offset patches suggest known risk around deleted spans, remote offsets, and paragraph boundary edits.

### Pertinent files

- `crates/gpui-flowtext/src/edit_ops/split_delete.rs`
- `crates/gpui-flowtext/src/edit_ops/offsets.rs`
- `crates/gpui-flowtext/src/edit_ops/text.rs`
- DB8 mutation adapter
- workspace collaboration diff application

### How to implement

Add tests and invariant checks for delete at paragraph start/end, delete across paragraphs, delete adjacent to split, delete adjacent to join, styled text deletion, and remote delete after local concurrent edit.

### Expected impact

Medium. Prevents edge-case convergence failures.

---

## 17. Structural metadata/projection cleanup

**Priority:** P2/P3  
**Improvement size:** Medium  
**Current status:** Partially superseded by atomic insert/split work.

### Why this is necessary

Earlier structural metadata/projection patches were aimed at ensuring paragraph style, runs, and metadata are applied consistently when remote structural edits arrive.

Atomic insert now handles paragraph text/runs/style in one operation, but the broader projection model still needs cleanup.

### Pertinent files

- `crates/flowstate/src/workspace/workspace/documents.rs`
- `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`
- metadata serialization/deserialization code in `flowstate_document`

### How to implement

Audit all remote structural paths: `ParagraphAdded`, `ParagraphRemoved`, `ParagraphMoved`, `Metadata`, and `Text`. Ensure each path has exactly one owner for paragraph insertion/removal, text initialization, runs initialization, style initialization, block replacement, and section rebuild.

### Expected impact

Medium. Reduces future regressions.

---

## 18. Long-document layout invalidation reduction

**Priority:** P2  
**Improvement size:** Large  
**Current status:** Partially implemented/tabled.

### Why this is necessary

Large documents make every unnecessary layout/rebuild expensive. Remote collaboration updates should not invalidate or recompute more than required.

### Pertinent files

- rich text layout/prep code
- virtual list sizing/cache code
- editor lifecycle remote mutation paths
- document offset/block replacement code

### How to implement

Track affected paragraph/block ranges for remote updates and invalidate only those. Avoid full layout invalidation when a single paragraph text changes, a split affects two paragraphs, a join affects two paragraphs, or style changes affect one paragraph.

### Expected impact

Large for large documents.

---

## 19. Presence/frontier traffic refinement

**Priority:** P2/P3  
**Improvement size:** Medium  
**Current status:** Partially implemented.

### Why this is necessary

Presence updates are high-frequency and should never block durable document updates. Some coalescing exists, but frontier and presence behavior can be improved.

### Pertinent files

- workspace pending update queue
- sync presence update handling
- collaboration client/host event loop

### How to implement

Refine latest-presence-only queueing, per-peer presence dedupe, throttled cursor updates, frontier acknowledgement compression, and no durable update delay behind presence traffic.

### Expected impact

Medium. Improves perceived collaboration smoothness.

---

## 20. Collaboration diagnostics test harness

**Priority:** P2  
**Improvement size:** Medium  
**Current status:** Not implemented.

### Why this is necessary

The current debugging depended on manual canary logs. A reproducible harness would drastically reduce turnaround time.

### Pertinent files

- workspace collaboration tests
- sync test utilities
- DB8 document construction helpers
- editor test harness

### How to implement

Build a test helper that can create host and non-host documents, apply a local edit on one side, serialize/publish update, apply update on the other side, capture canary phases, and assert convergence/invariants.

### Expected impact

Medium. Improves debugging speed and prevents future manual log-chasing.

---

## Superseded or no-longer-recommended items

### A. Full authoritative projection for every paragraph removal

**Status:** Superseded / not recommended as final.

It would have avoided the host-join crash by bypassing incremental structural receiver logic, but it increases latency for structural edits and hides the real issue. The canonical join fast path is a better fix.

### B. Forcing all paragraph-added/removed diffs into fallback

**Status:** Superseded / not recommended.

Remote split is now stable through the atomic insert path. Forcing splits through full projection would discard that improvement.

### C. Continuing to patch `delete_cross_paragraph_range` for authoritative joins

**Status:** Superseded.

The authoritative join receiver should not use a local editing delete-range helper after the first paragraph has already been replaced with merged text.

### D. More one-off line-hunk patches for drifting files

**Status:** Avoid.

Repeated patch application failures were due to local/remote drift and brittle hunk anchors. Use exact current branch diffs or small source-level scripts only during active debugging.

## Recommended implementation order

1. Add host/non-host join/split regression tests, including the DB8 canonical join fast path.
2. Add document invariant checker behind debug/canary mode.
3. Clean up temporary canaries and keep only high-value gated diagnostics.
4. Batch remote DB8 incremental projection through `apply_remote_projection_batch`.
5. Skip text diffs consumed by structural joins for non-fast-path join-shaped diffs.
6. Fix the DB8 paragraph diff walker crash even though the fast path avoids it.
7. Resume latency architecture in smaller verified steps.

## Practical next patch candidates

### Candidate 1: Regression tests for join/split collaboration

Medium patch. Highest long-term value. Should verify host and non-host join/split behavior and assert the pure-join canonical fast path is used.

### Candidate 2: Canary cleanup

Medium patch. Highest long-term value.

Remove or demote temporary high-volume canaries while retaining fallback and fast-path diagnostics behind `FLOWSTATE_COLLAB_CANARY`.

### Candidate 3: Invariant checker

Medium patch. Strong debugging value.

### Candidate 4: Projection batch wrapper

Medium/high-risk patch. Large performance and correctness upside.

### Candidate 5: DB8 diff walker canary/fix

Investigative patch first, then targeted fix.

## Open uncertainties

The following should be verified before finalizing this backlog:

1. Whether `cargo check` passes on the latest pushed branch after all local scripts.
2. Which earlier patch attempts were actually committed versus applied only locally during debugging.
3. Whether the DB8 diff walker crash can still be reproduced if the join fast path is disabled.
4. Whether host/non-host join remains smooth with styled paragraphs, empty paragraphs, long paragraphs, adjacent rapid joins, joins after splits, and joins during remote cursor activity.
5. Whether the temporary `remote_join phase=...` canaries should be removed now that the pure-join fast path avoids that method for host joins.
