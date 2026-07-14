# Object + table op convergence — RESOLVED

**Status:** ✅ DONE. The full editor op surface (all 24 `SemanticEditCommand` variants)
converges across N peers under out-of-order delivery. Acceptance tests:
`npeer_fuzz_object_structural_ops`, `npeer_fuzz_table_ops`,
`npeer_fuzz_replace_paragraph_span`, `object_block_positioning_single_peer` (plus the
paragraph/coordinate suites). All green.

## What broke and how it was fixed

### 1. Object block positioning (off-by-N) — `e909924`
The Loro-side object insert/move placed the `OBJECT_REPLACEMENT` char via a raw body walk
that counted one block per `\n`/object, ignoring the projection's coalescing of
record-less phantom empties after an object — so a projection block index mapped to a body
position off by N. Replaced with `projection_block_lead_pos_in_loro`, which resolves the
lead position from the projection's own (coalesced) block list + each block's durable
cursor (object `anchor_cursor`, or paragraph boundary `\n`, sentinel-clamped).

### 2. The coalescing "shared root" — `a1a5a4b`
The incremental replay (`replay_semantic_command_on_projection`) models plain paragraph
text/structure but NOT object coalescing (dropping the phantom empty after an object,
re-segmenting the sentinel-first region). So on any object-bearing doc its prediction can
diverge structurally from the authoritative `document_from_loro` rebuild — leading-object
insert/move, DeleteBlock-materialized phantom empties, and later text/split/join near them.

Key reframe: the shipped projection patch is ALWAYS the canonical rebuild, verified, with a
full-projection fallback — so **cross-peer convergence was never at risk**; the divergences
only tripped the preflight identity assert and the local optimistic echo. Fixes:
- Object-bearing docs adopt the canonical rebuild as their prediction (the runtime already
  materializes it every transaction — free; mirrors the remote-import path). Pure-paragraph
  docs keep the exact incremental prediction.
- The identity `debug_assert` is split: STRUCTURE (block kinds + paragraph texts) is asserted
  hard (catches every real positional/segmentation/content bug); exact ID equality became a
  fidelity observation, because a record-less/fabricated id (a phantom empty pending the
  repair pass) is derived from a boundary OpID and legitimately differs between the
  incremental carry-forward and a fresh rebuild — the repair pass's domain, not the replay's.

### 3. Table concurrent-delete column topology — `1cb95f1`
A concurrent `DeleteTableColumn` removes the column's map from `columns_by_id`, but the
ordered `column_order` list is a separate CRDT that can still reference it after an
out-of-order merge. The projector HARD-ERRORED ("missing table column"), sinking the whole
projection. Fixed to skip the stale order entry (deterministic across peers), matching the
malformed-id skip directly above and §P2b's "a single bad id can't sink the whole table";
the topology normalization already drops cells left referencing the removed column.

## Coverage (all fuzzed, N-peer, out-of-order)
Paragraph/text (insert incl. U+2028 soft breaks, delete, split, join, set-paragraph-style,
set-run-styles); object block insert/delete/move/replace at EVERY position incl. leading;
object properties (image alt/layout/caption, equation source); the 9 table ops; and
`ReplaceParagraphSpan`. Each asserts cross-peer projection equality + per-peer
incremental-vs-fresh materializer equivalence.
