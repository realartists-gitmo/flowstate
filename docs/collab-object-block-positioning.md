# Object block-structure op positioning (Insert/Delete/Move/Replace)

**Status:** PARTIALLY FIXED. InsertBlock and MoveBlock positioning now resolve from the
coalescing-aware projection structure; ReplaceBlock was already correct. A shared root
(the sentinel-first-paragraph / coalesced-phantom-empty interaction) still breaks three
cases; they are scoped below and excluded from the fuzz until the root is fixed.

## What was fixed

The Loro-side object insert/move used a raw body walk
(`object_insert_unicode_pos_for_projection_block`) that counted one block per `\n` and per
`OBJECT_REPLACEMENT`, which does NOT match the **coalescing-aware** projection block index
(the projection drops record-less phantom empties after an object). So a projection
`block_ix` mapped to the wrong body position — an off-by-N around objects/coalesced
empties that diverged the canonical Loro rebuild's block ids from the incremental replay.

Replaced with `projection_block_lead_pos_in_loro` (crdt_runtime.rs): it resolves the body
insertion position from the projection's own (already-coalesced) block list plus each
block's durable cursor — an object's `anchor_cursor`, or a paragraph's boundary `\n`
(sentinel-clamped). Insert-before-a-paragraph lands on its boundary `\n`, which attaches
the object to the previous block's tail exactly as `push_flow_blocks` re-segments it.
`insert_projection_object_block` and `move_projection_object_block` now take the
(incremental-replay-evolved) working projection and use it; the old body-walk positioner
was deleted. MoveBlock maps its post-removal `new_block_ix` back to the pre-removal block
it lands before, resolving that lead position on the post-delete body via durable cursors.

**Validated:** `object_block_positioning_single_peer` (multi_peer_convergence_tests.rs)
drives InsertBlock (non-leading) + ReplaceBlock on the object-bearing structural fixture,
asserting incremental == fresh after every op across 8 seeds. Isolated runs also proved
MoveBlock (non-leading source AND target) convergent.

## What still diverges — one shared root

All three remaining failures come from the **sentinel-anchored first paragraph** and the
**coalesced phantom empty** an object implies. The reserved first paragraph is anchored to
the body sentinel `\n` (pos 0), and the first `\n` after an object is coalesced out of the
projection (Fork B). Any structural op that changes what sits at the front, or that
adds/removes an object whose phantom empty then materializes, desyncs the incremental
replay (which just reorders/removes projection blocks) from the canonical rebuild (which
re-segments and re-resolves ids).

1. **Leading insert (block_ix 0).** Inserting an object before the sentinel-anchored first
   paragraph steals that paragraph's boundary: `\n OBJECT P0text` leaves P0 with no
   boundary `\n`, so the canonical rebuild fabricates a new id for it (seen as
   `ParagraphId(1)`-ish) while the incremental replay keeps P0's id. Correct form is
   `\n OBJECT \n P0text` with P0 RE-ANCHORED to the new boundary — mirror SplitParagraph's
   `\n`-insert + style mark + `repair_paragraph_metadata_after_stable_split`, but for the
   reserved first paragraph (`ROOT_FIRST_PARAGRAPH_ID`).
2. **Move touching the leading block.** Moving the leading object away (or to index 1)
   changes front-of-doc coalescing; the incremental prediction gains an extra leading
   paragraph the canonical rebuild doesn't have. Same re-anchor need as (1).
3. **DeleteBlock of an object with a coalesced phantom empty.** Removing the object
   un-coalesces its trailing phantom `\n`, which materializes as a REAL empty paragraph in
   the canonical rebuild; the incremental replay only drops the object block, so a later
   SplitParagraph diverges block ids. Minimal repro: structural_fixture, seed 0x7,
   `DeleteBlock` then `SplitParagraph` (6 ops).

### Fix direction (shared)

Teach the incremental replay (`gpui-flowtext` block ops in projection_apply.rs /
lifecycle.rs) and the Loro-side ops to agree on ONE phantom/sentinel rule:
materialize/coalesce the object-adjacent empty and re-anchor the reserved first paragraph
identically on both sides. This is the same class as the Fork B coalescing-parity work,
extended to the object structural ops. Once done, re-enable the excluded arms in
`block_stress_command` (leading insert, MoveBlock, DeleteBlock) and grind to green, then
fold the object ops into the N-peer fuzz and add the 9 table ops.

## Coverage status

- InsertBlock (non-leading), ReplaceBlock: convergent (fuzzed).
- MoveBlock (non-leading source+target): convergent in isolation; excluded from the mixed
  fuzz because a leading-block source/target hits root case (2). Positioning fix retained.
- DeleteBlock, leading insert, leading move: excluded pending the shared-root fix.
- Table ops (9 variants): not yet generated.
