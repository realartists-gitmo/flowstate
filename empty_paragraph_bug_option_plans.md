# Empty-paragraph boundary bug option plans

Collected plan outputs for Options 5, 3, 2, and 1.

## Option 5

## Summary

Introduce explicit CRDT-level **paragraph split/join operations** and push the system to use them instead of implicit newline-based paragraph inference when the caret is in “empty paragraph” states. The goal is to eliminate ambiguous paragraph boundaries by making paragraph transitions first-class commands in the editor pipeline, runtime reducer, and projection layer.

This fixes the failure mode where an empty paragraph is treated inconsistently depending on caret adjacency and causes incorrect insert/mutation targeting.

---

## Changes

### 1. Command model (core semantic layer)

**File (likely):**
- `src/editor/commands.rs`  
- `src/editor/command_model.rs`  
- `src/crdt/commands.rs`

#### Add explicit commands

Introduce two new semantic commands:

```rust
SplitParagraph {
    at: Cursor,
    source_paragraph_id: ParagraphId,
}

JoinParagraph {
    left_paragraph_id: ParagraphId,
    right_paragraph_id: ParagraphId,
}
```

#### Modify existing InsertText behavior

Existing:

- InsertText at caret implicitly triggers paragraph boundary creation when newline exists.

Change:

- If insert includes `\n` OR caret is in empty paragraph context → DO NOT infer split.
- Instead emit:

```rust
SplitParagraph { at, source_paragraph_id }
```

Then emit remaining text insert into new paragraph.

#### Add invariant

- Paragraph boundaries MUST be represented by CRDT node split, not text content.

---

### 2. Editor pipeline (input → command generation)

**File (likely):**
- `src/editor/input.rs`
- `src/editor/transaction_builder.rs`
- `src/editor/keymap.rs`

#### Change newline handling

Current behavior [INFERENCE]:
- newline → insert `\n` into text node

Replace:

- newline key ALWAYS produces semantic command:

```rust
Key::Enter → SplitParagraph
```

BUT with conditional refinement:

If caret is:
- at end of non-empty paragraph → split normally
- inside empty paragraph → still split (no-op-safe)
- between empty paragraph + next text → ensure stable split target selection

#### Add caret context classification

Introduce helper:

```rust
enum CaretContext {
    InText,
    EmptyParagraph,
    BoundaryBetweenParagraphs,
}
```

Used to choose split/join behavior deterministically.

---

### 3. Runtime execution layer (CRDT mutation)

**File (likely):**
- `src/runtime/apply.rs`
- `src/crdt/runtime.rs`
- `src/crdt/paragraph.rs`

#### Implement SplitParagraph

Operation:

1. Locate paragraph node in Loro doc graph
2. Split its text node at cursor offset
3. Create new paragraph CRDT node with:
   - new id
   - inserted sibling ordering link
4. Move trailing content into new node

Key rule:

- Empty paragraph split must still create a new node, even if both sides are empty

Edge invariant:

- Never allow "implicit empty paragraph identity collapse"

#### Implement JoinParagraph

Operation:

1. Validate adjacency in paragraph graph
2. Merge right into left:
   - append text
   - reparent inline nodes
3. Delete right node from CRDT graph

---

### 4. Projection layer (CRDT → editor view model)

**File (likely):**
- `src/editor/projection.rs`
- `src/editor/view_model.rs`

#### Update projection rules

Current [INFERENCE]:
- paragraphs inferred from newline runs

Replace:

- paragraph list comes directly from CRDT paragraph nodes only

Add rule:

```text
Paragraph boundaries = explicit CRDT paragraph nodes only
NOT text parsing
```

#### Empty paragraph representation fix

Fix:

- empty paragraph MUST render as node, not collapse into "no paragraph"

This is critical for bug reproduction case:
- caret on empty line before text

Now that empty paragraph exists structurally, projection is stable.

---

### 5. Edit command reconciliation (undo/redo / batching)

**File (likely):**
- `src/editor/history.rs`
- `src/editor/transaction.rs`

#### Treat split/join as atomic units

- SplitParagraph is atomic (not split into insert + layout change)
- JoinParagraph is atomic inverse

Undo stack:

```
SplitParagraph <-> JoinParagraph
```

---

### 6. Compatibility layer (existing semantic commands)

**File (likely):**
- `src/editor/legacy_commands.rs`
- `src/editor/compat.rs`

#### Backward compatibility mapping

Old behavior:

- InsertText("\n") → implicit paragraph break

New mapping:

```text
InsertText("\n") → SplitParagraph
```

But:

- Keep InsertText semantics unchanged for non-newline text

Add compatibility shim:

- If external API still emits InsertText with embedded newlines:
  - split into multiple SplitParagraph + InsertText segments

---

### 7. Caret affinity + empty paragraph bug fix integration

**File (likely):**
- `src/editor/caret.rs`
- `src/editor/selection.rs`

#### Fix root issue

Problem condition:

- caret sits in empty paragraph node that is not structurally stable

Fix:

- caret ALWAYS anchors to paragraph node id + offset
- never to "visual line"

Add rule:

```text
Caret target = (ParagraphId, Offset)
NOT (TextNodeId, inferred line)
```

This removes ambiguity that triggers bug.

---

### 8. Runtime paragraph normalization (migration safety)

**File (likely):**
- `src/crdt/migrate.rs`
- `src/crdt/normalize.rs`

Add migration pass:

- Convert legacy newline-in-text paragraphs into explicit paragraph nodes

Pseudo:

```rust
for node in doc.text_nodes {
    if node.contains('\n') {
        split_into_paragraph_nodes(node);
    }
}
```

---

## Sequence

1. Add CRDT operations:
   - SplitParagraph
   - JoinParagraph

2. Implement runtime CRDT mutation logic

3. Update editor command generation (Enter key, InsertText)

4. Update projection layer to rely only on paragraph nodes

5. Fix caret anchoring model

6. Update undo/redo semantics

7. Add compatibility layer for legacy InsertText("\n")

8. Add migration/normalization pass

---

## Edge Cases

- Empty document:
  - must still allow SplitParagraph creating first paragraph node

- Consecutive empty paragraphs:
  - must preserve distinct nodes (no collapsing)

- Caret at start of empty paragraph before text:
  - SplitParagraph must NOT duplicate or drop paragraph

- Undo of split at document start:
  - must restore single paragraph node cleanly

- Mixed legacy documents:
  - newline-in-text + paragraph nodes coexist during migration window

---

## Verification

### Unit tests

- split_empty_paragraph_creates_new_node()
- join_paragraph_removes_right_node()
- caret_in_empty_paragraph_is_stable_target()
- newline_key_produces_split_command()
- legacy_insert_text_newline_is_translated()

### Integration tests

- reproduce original bug:
  - caret on empty line before text
  - insert text → must target correct paragraph

- document round-trip:
  - split → join → split consistency

### CRDT invariants

- no orphan paragraph nodes
- no collapsed empty paragraph ambiguity
- projection always equals CRDT graph size

---

## Critical Files

Likely core touchpoints (must verify in repo):

- `src/editor/commands.rs`
- `src/editor/input.rs`
- `src/editor/projection.rs`
- `src/editor/caret.rs`
- `src/editor/transaction.rs`
- `src/runtime/apply.rs`
- `src/crdt/paragraph.rs`
- `src/crdt/runtime.rs`
- `src/crdt/normalize.rs`

---

## Cutover Strategy

1. Introduce new commands alongside existing behavior (no removal yet)
2. Route Enter key → SplitParagraph in feature flag
3. Enable projection change behind flag
4. Enable caret anchoring change last
5. Remove legacy newline-in-text paragraph inference after stabilization

---

If you want, next step is mapping this plan onto the actual Flowstate files once tool access is fixed, to replace all `[INFERENCE]` areas with exact symbol-level edits.

## Option 3

## Summary

Align “new document creation” with the same structural rules used when importing documents so that empty documents do not behave differently from parsed/imported ones. The fix is to remove ad-hoc initialization for brand-new docs and route everything through a single normalized “document seed” pipeline that guarantees identical paragraph/block invariants.

---

## Changes

### 1. Unify document initialization entrypoints
Target (likely [INFERENCE]):
- `src/document/mod.rs`
- `src/document/document.rs`
- `src/editor/session.rs`
- `src/editor/bootstrap.rs`

Changes:
- Route all of these through one constructor:
  - `Document::new_empty(...)`
  - `Document::from_import(...)`
  - `Document::open(...)`

to a shared internal function:

- `Document::init_from_seed(seed: DocumentSeed)`

Add:

```rust
struct DocumentSeed {
    blocks: Vec<Block>,
    origin: DocumentOrigin, // New | Import | Open
}
```

Rule: no constructor directly mutates CRDT state outside this path.

---

### 2. Define canonical empty-document seed

Target:
- `src/document/seed.rs` [INFERENCE new or existing module]

Replace current “new doc” logic with:

- Empty document MUST be:

```text
blocks = [Paragraph { text: "" }]
```

NOT:
- empty tree
- missing root node
- implicit placeholder cursor node

Invariant:

- A document always has ≥1 block
- The first block is always a paragraph
- Paragraph text may be empty string

---

### 3. Align imported-doc normalization to same seed pipeline

Target:
- `src/document/import.rs` [INFERENCE]

Change:
- Imported content is normalized into `Vec<Block>` then passed into same `DocumentSeed`.

Rules:
- If import yields 0 blocks → convert to single empty paragraph
- If import yields trailing empty structure → preserve explicitly, not implicit editor state

---

### 4. CRDT initialization consistency (Loro layer)

Target:
- `src/crdt/loro_doc.rs` [INFERENCE]
- `src/crdt/schema.rs` [INFERENCE]

Change:
- Ensure initial Loro tree is created from `DocumentSeed.blocks` only.
- Remove special-case branch like:

```rust
if is_new_doc { insert_placeholder_paragraph() }
```

Replace with:

- always apply same insert logic used by imports

Invariant:

- CRDT root children count == DocumentSeed.blocks.len()
- No implicit hidden node insertion for new docs

---

### 5. Editor bootstrap alignment (caret + selection)

Target:
- `src/editor/caret.rs`
- `src/editor/selection.rs`
- `src/editor/view.rs` [INFERENCE]

Change:

New rule for empty paragraph behavior:

- caret position is ALWAYS:
  - block_index = 0
  - offset = 0

No special-case “empty doc caret float” logic.

Remove divergence:

- imported docs and new docs must produce identical caret state after load

Invariant:

- `caret.doc_state(new_doc) == caret.doc_state(imported_doc(empty))`

---

### 6. Empty-paragraph boundary semantics

Target:
- `src/document/paragraph.rs` [INFERENCE]
- `src/runtime/paragraph_ops.rs` [INFERENCE]

Fix alignment rule:

Current bug class:
- empty paragraph treated as “non-boundary”
- or treated as synthetic separator

New rule:

- empty paragraph is a real block with valid boundaries
- cursor before/after empty paragraph MUST behave identically to imported empty paragraph

Invariant:

- boundary detection depends only on block structure, not origin

---

### 7. Remove “new doc special casing”

Search likely areas:
- `is_new_document`
- `if doc.is_empty()`
- `create_blank_document`

Replace with:

- `doc.origin == DocumentOrigin::New` should not affect structure
- origin is metadata only

---

### 8. Migration strategy for existing docs

Target:
- `src/storage/migrations.rs` [INFERENCE]

Rules:

- Existing persisted docs are NOT rewritten eagerly
- On load:
  - normalize through `DocumentSeed::from_persisted`
  - apply empty-doc rule only if document has zero blocks

Migration logic:

- version bump: `doc_schema_version += 1`
- lazy normalization at load time

No bulk rewrite required.

---

## Sequence

1. Introduce `DocumentSeed` + canonical empty seed
2. Route all document constructors through `init_from_seed`
3. Align import pipeline to same seed system
4. Fix CRDT initialization to remove special-case new-doc logic
5. Align caret/selection bootstrap
6. Enforce empty-paragraph boundary invariants in runtime layer
7. Remove legacy “new doc” branching
8. Add migration normalization on load

---

## Edge Cases

- Completely empty persisted document (0 blocks)
  - must become single empty paragraph
- Import producing malformed trailing empty nodes
  - must normalize, not preserve broken structure
- Undo stack initialization for new docs
  - must start from seed state, not post-hoc mutation
- Multi-paragraph imports vs new doc single paragraph equivalence
- Caret placed at end-of-document in empty doc
- Rapid open → edit before hydration completes

---

## Verification

Add behavioral tests (not snapshots of implementation internals):

1. New doc parity test
- create empty doc
- create imported doc from `[]`
- assert identical block tree

2. Caret invariance test
- both docs:
  - caret at creation == (0,0)

3. Empty paragraph boundary test
- insert empty paragraph between text blocks
- verify cursor movement across boundary identical for both doc types

4. CRDT structure test
- assert root children count = 1 for empty docs

5. Regression reproduction test
- scenario:
  - new doc
  - move caret to empty line
  - ensure no boundary misclassification occurs

---

## Critical Files

[INFERENCE — must confirm in repo]
- `src/document/*`
- `src/editor/*`
- `src/crdt/loro_doc.rs`
- `src/runtime/paragraph_ops.rs`
- `src/document/import.rs`
- `src/storage/*`

---

If you want, next step is mapping this plan onto exact Rust symbols by scanning the actual initialization paths (`Document::new`, `Editor::open_document`, CRDT root builder).

## Option 2

## Summary

Option 2 fixes caret/selection behavior when the caret is on an empty paragraph by making remapping boundary-aware. The core change is to treat empty paragraphs as valid positional anchors with explicit affinity rules, instead of collapsing/clamping them away during remap/projection. This prevents the caret from snapping into adjacent text paragraphs incorrectly when replaying edits or applying CRDT transforms.

The fix spans caret representation, remap logic, paragraph projection, and empty-paragraph invariants in the CRDT layer.

---

## Changes

### 1. Caret / Selection model (core data)

Likely impacted module (name inferred):
- `src/editor/selection.rs` [INFERENCE]
- `src/editor/caret.rs` [INFERENCE]

#### Add explicit empty-paragraph anchor support

Introduce or extend caret position enum:

```rust
enum CaretPosition {
    InText {
        paragraph_id: ParagraphId,
        offset: usize,
    },

    // NEW: explicit anchor for empty paragraphs
    InEmptyParagraph {
        paragraph_id: ParagraphId,
        affinity: CaretAffinity,
    },
}
```

Selection stays the same but must allow zero-width range anchored in empty paragraphs.

#### Affinity definition

```rust
enum CaretAffinity {
    Left,
    Right,
    Upstream,
    Downstream,
}
```

Use this instead of implicit gravity.

---

### 2. Remap / transform logic

Likely module:
- `src/editor/remap.rs` [INFERENCE]
- `src/editor/transform.rs` [INFERENCE]

#### Problem being fixed

Current behavior likely:
- empty paragraph → no valid text index
- remap collapses it to nearest paragraph
- caret jumps incorrectly when replaying edits

#### Fix: boundary-aware remap

Introduce rule:

```text
If paragraph is empty:
    DO NOT clamp into neighbor text nodes
    Preserve paragraph identity if it still exists in CRDT
    Otherwise resolve via affinity
```

#### New remap function behavior

Pseudo:

```rust
fn remap_caret(pos: CaretPosition, tx: &Transform) -> CaretPosition {
    match pos {
        InText { .. } => remap_text_position(...),

        InEmptyParagraph { paragraph_id, affinity } => {
            if tx.contains(paragraph_id) {
                InEmptyParagraph { paragraph_id, affinity }
            } else {
                resolve_via_affinity(paragraph_id, affinity, tx)
            }
        }
    }
}
```

---

### 3. Projection layer (CRDT → view)

Likely module:
- `src/crdt/projection.rs` [INFERENCE]

#### Issue

Projection probably skips empty paragraphs or collapses them.

#### Fix

Empty paragraphs must emit a stable visible node:

- Must produce a paragraph node with:
  - valid id
  - zero-length text
  - caret anchor slot

Add invariant:

```rust
// MUST: empty paragraph still has projection entry
assert!(paragraph.is_empty() => paragraph.has_visible_anchor == true);
```

---

### 4. Edit replay / lifecycle

Likely:
- `src/editor/lifecycle.rs` [INFERENCE]
- `src/editor/history.rs` [INFERENCE]

#### Problem

During replay:
- caret is applied after document mutations
- empty paragraph may disappear/reappear
- caret loses stable anchor

#### Fix

Introduce two-phase restore:

1. Restore structural position (paragraph id based)
2. If missing:
   - fallback to nearest structural sibling
   - use affinity direction

Pseudo:

```rust
fn restore_caret(snapshot: CaretSnapshot, doc: &Doc) -> CaretPosition {
    if let Some(p) = doc.find_paragraph(snapshot.paragraph_id) {
        return match p.is_empty() {
            true => CaretPosition::InEmptyParagraph { ... },
            false => CaretPosition::InText { ... }
        }
    }

    resolve_missing_paragraph(snapshot)
}
```

---

### 5. Empty paragraph CRDT invariants

Likely:
- `src/crdt/paragraph.rs` [INFERENCE]
- `src/crdt/doc.rs` [INFERENCE]

#### Required invariant change

Empty paragraphs must be:
- stable nodes in Loro tree
- not removed or merged implicitly during normalization

Add rule:

```rust
// MUST NOT auto-prune empty paragraphs if they have caret focus or selection anchors
```

Add flag:

```rust
struct Paragraph {
    id: ParagraphId,
    text: Rope,
    keep_alive: bool, // NEW
}
```

Set `keep_alive = true` when caret is inside.

---

### 6. Caret affinity rules (core logic change)

Introduce explicit rules instead of implicit gravity:

| Situation | Behavior |
|----------|----------|
| empty paragraph + insert text before | stay in same paragraph |
| empty paragraph + delete paragraph above | move via Upstream |
| empty paragraph + insert below | Downstream |
| empty paragraph disappears | resolve via affinity fallback |

---

### 7. Tests

Likely:
- `tests/editor_caret.rs` [INFERENCE]
- `tests/crdt_projection.rs` [INFERENCE]

Add tests:

#### A. Empty paragraph stability

- create doc:
  - paragraph A (text)
  - paragraph B (empty)
  - paragraph C (text)
- place caret in B
- insert/delete in A and C
- assert caret remains in B

#### B. Replay test

- record edit sequence:
  - insert text
  - create empty paragraph
  - move caret into empty paragraph
- replay
- assert identical caret final position

#### C. Deletion fallback

- delete empty paragraph
- caret resolves to:
  - next paragraph if affinity = Downstream
  - previous if Upstream

#### D. Projection invariant

- empty paragraph must always produce visible node if caret exists

---

## Sequence

1. Extend caret model (add empty paragraph anchor + affinity)
2. Update remap logic (boundary-aware handling)
3. Update CRDT projection (preserve empty nodes)
4. Fix lifecycle restore (two-phase resolution)
5. Add keep-alive invariant in paragraph CRDT
6. Implement tests
7. Validate replay consistency

---

## Edge Cases

- Empty paragraph deleted while caret is inside → must not produce dangling paragraph id
- Concurrent edits remove and reinsert same paragraph id
- Rapid insert/delete around empty paragraph boundary
- Undo/redo restoring stale caret snapshot
- Multi-selection spanning empty paragraph + text paragraph
- Reflow where empty paragraph becomes merged or normalized away

---

## Verification

- Run caret placement tests across:
  - empty paragraph focus
  - boundary transitions
  - replay consistency
- Validate CRDT projection does not drop empty paragraphs with active caret
- Ensure remap is idempotent:
  - remap(remap(x)) == remap(x)
- Confirm no fallback into adjacent paragraph unless explicit affinity resolution triggers

---

## Critical Files (inferred)

- `src/editor/selection.rs`
- `src/editor/caret.rs`
- `src/editor/remap.rs`
- `src/editor/lifecycle.rs`
- `src/editor/history.rs`
- `src/crdt/projection.rs`
- `src/crdt/paragraph.rs`
- `src/crdt/doc.rs`

---

If you want, I can next break this into a minimal patch order (smallest safe diffs first) or map it onto the other options so you can compare tradeoffs.

## Option 1

## Option 1 — Preserve empty-boundary metadata instead of pruning/replacing it

This plan assumes Flowstate uses a CRDT-backed document model (Loro-like, based on your scouts) where “empty paragraph boundary metadata” is currently being normalized away during edits, import, or caret-driven repair.

The bug context: empty paragraph positions (especially caret parked on a blank line before later content) lose structural identity because a repair/prune step collapses or replaces them. Option 1 explicitly keeps them as first-class paragraph nodes with stable metadata.

---

# Summary

Change the runtime so empty paragraphs are never treated as “invalid / transient”. Instead, they remain stable CRDT paragraph nodes with preserved boundary metadata (id, position anchor, caret affinity hooks). Modify pruning, edit-command reconciliation, and initialization repair so they *skip removal* of empty-but-valid paragraph nodes and instead normalize only truly orphaned or duplicate structures.

---

# Changes

## 1. Core CRDT paragraph representation

### File (likely)
`crates/document/src/paragraph.rs` or `src/crdt/paragraph.rs`

### Changes
- Ensure paragraph node struct distinguishes:
  - `content: Vec<Inline>` (may be empty)
  - `boundary: ParagraphBoundaryMeta` (must persist even if content empty)

Add invariant:

```rust
/// Empty content is valid. Never implies deletion.
content.is_empty() == true => paragraph still exists
```

Add/strengthen metadata:

```rust
struct ParagraphBoundaryMeta {
    id: ParagraphId,
    created_at_op: OpId,
    caret_affinity_anchor: Option<AnchorId>,
    is_explicit_empty_line: bool,
}
```

Key change:
- Remove any implicit rule like “empty paragraph == removable tombstone”

---

## 2. Runtime repair / normalization layer

### File
`crates/runtime/src/paragraph_repair.rs` or similar (EmptyParagraphScout likely pointed here)

### Current behavior (assumed [INFERENCE])
- merges consecutive empty paragraphs
- prunes empty paragraphs during normalization
- replaces empty paragraph with newline/text node

### Option 1 modification

Replace pruning rule:

```rust
if paragraph.is_empty() {
    drop_or_merge(paragraph)
}
```

with:

```rust
if paragraph.is_empty() {
    preserve_boundary(paragraph);
    keep_as_stable_node();
}
```

### Add rule set:

#### Rule A — Empty paragraph preservation
- Never delete empty paragraph nodes if:
  - they were explicitly created by user action (Enter, caret split)
  - or they have caret affinity anchor
  - or they exist in committed CRDT history

#### Rule B — Duplicate empty collapse (safe only)
Only collapse if ALL are true:
- consecutive empty paragraphs
- no caret anchors
- no op-id divergence requirement (same insertion source)

Result:
- preserve one node, not zero nodes

---

## 3. Edit command reconciliation

### File
`crates/editor/src/edit_command.rs`

### Problem
Edit commands likely treat empty paragraphs as “no-op targets” and collapse them.

### Change
In insertion / deletion reconciliation:

- DO NOT interpret empty paragraph as deletion candidate
- If cursor is inside empty paragraph:
  - treat as valid insertion target
  - preserve paragraph id

Pseudo-change:

```rust
match paragraph {
    Empty => insert_into_existing_paragraph,
    NonEmpty => normal_flow,
}
```

becomes:

```rust
match paragraph {
    Empty { stable: true } => insert_into_existing_paragraph,
    Empty { stable: false } => only then allow merge/prune,
    NonEmpty => normal_flow,
}
```

---

## 4. Initialization invariant repair

### File
`crates/doc/src/init.rs` or `document_init.rs`

### Problem
New documents likely run a cleanup pass that removes empty trailing/leading paragraphs.

### Change

Remove or restrict:

- “trim empty leading/trailing paragraphs”
- “normalize blank structure on load”

Replace with:

- ensure at least one paragraph exists
- preserve all explicit empty paragraphs

Add invariant:

```rust
fn validate_document(doc: &Document) {
    assert!(doc.paragraphs.len() >= 1);
    assert!(doc.all_explicit_empty_paragraphs_have_ids());
}
```

Empty paragraphs are valid structural nodes, not formatting artifacts.

---

## 5. Caret affinity system (critical coupling)

### File
`crates/editor/src/caret_affinity.rs`

### Change

Empty paragraph must retain caret binding:

- caret position = valid even if no inline content
- do not “snap forward” into next paragraph unless explicit user action

Add rule:

```rust
if paragraph.is_empty() {
    caret.must_bind_to_paragraph_id();
}
```

Remove any logic like:
- “if empty → move caret to next non-empty paragraph”

---

## 6. Duplicate / stale pruning rules

### File
`crates/runtime/src/prune.rs`

### Replace global rule:

OLD:
- prune empty paragraphs aggressively
- treat empty nodes as structural noise

NEW:

### Tiered pruning model

#### Tier 1 — Safe prune
Only prune if:
- paragraph is empty
- AND has no op-id in history
- AND no caret anchor
- AND not referenced by selection snapshot

#### Tier 2 — Merge-only
- consecutive empty paragraphs:
  - merge metadata only
  - preserve first node

#### Tier 3 — Never prune
- user-created empty lines
- caret-anchored paragraphs
- imported structure nodes

---

## 7. New invariants

Add to document invariants module:

### Invariants

1. Empty paragraph ≠ invalid paragraph
2. Every user-created newline produces a stable paragraph node
3. Empty paragraph must preserve:
   - id
   - op history linkage
   - caret affinity (if any)
4. Pruning may reduce duplication, never eliminate structural empties entirely

---

## Sequence

1. Update paragraph data model (boundary metadata preservation)
2. Adjust runtime repair to stop deletion of empty nodes
3. Fix prune rules (tiered model)
4. Fix edit-command handling for empty targets
5. Fix caret affinity behavior
6. Adjust initialization cleanup pass
7. Add invariants + regression tests

---

## Edge Cases

### 1. Multiple empty lines in a row
- Must collapse duplicates ONLY if no caret anchors
- Must preserve at least one stable node

### 2. Empty paragraph at document end
- must not be trimmed on load

### 3. Caret on empty line + typing
- insertion must not recreate paragraph (avoid duplication)
- must reuse existing node

### 4. Undo/redo
- empty paragraph creation/deletion must be replayable
- no synthetic reconstruction

### 5. Import (DOCX or plain text)
- blank lines become real nodes, not filtered artifacts

---

## Verification

### Unit tests

Add tests in:
`crates/runtime/tests/empty_paragraph.rs`

#### Test 1 — empty paragraph persistence
- create doc with empty line
- run repair
- assert paragraph count unchanged

#### Test 2 — caret binding
- place caret in empty paragraph
- insert character
- assert same paragraph id used

#### Test 3 — prune safety
- consecutive empty paragraphs
- run prune
- assert at least one remains

#### Test 4 — initialization
- new document
- assert empty paragraph exists and is stable

---

## Critical Files

Likely high-signal files to inspect before implementation:

- `crates/runtime/src/paragraph_repair.rs`
- `crates/runtime/src/prune.rs`
- `crates/editor/src/edit_command.rs`
- `crates/editor/src/caret_affinity.rs`
- `crates/doc/src/init.rs`
- `crates/document/src/paragraph.rs`
- any CRDT layer: `loro`, `oplog`, or `doc_state.rs`

---

If you want, Option 2 can be contrasted directly (treat empties as structural placeholders that are *logically* preserved but physically merged).
