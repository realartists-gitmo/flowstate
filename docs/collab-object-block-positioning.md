# Next target: object block-structure op positioning (Insert/Delete/Move/Replace)

**Status:** open. Found by extending the N-peer fuzz generator to block ops.
**Reproduces:** enable the `TEMP-DISABLED` arms in `generate_command`
(`multi_peer_convergence_tests.rs`) and run `npeer_fuzz_structural_fixture_paragraph_ops`.
First failure: `MoveBlock` — canonical vs incremental disagree on the moved block's
final index by one.

## What's covered now (green)

The fuzz generator now drives, and the harness proves convergent for:
paragraph/text (InsertText incl. soft breaks, DeleteRange, SplitParagraph,
JoinParagraphs, SetParagraphStyle), **SetRunStyles**, and **object-property ops**
(ReplaceImageAltText, SetImageLayout, ReplaceImageCaption) — at N=2–5 with
out-of-order delivery. Fork B (object-adjacent empty parity) is done.

## The bug

The block-structure ops place/find objects in the Loro body via body-walk helpers
`object_insert_unicode_pos_for_projection_block` / `object_unicode_pos_for_projection_block`
(`crdt_runtime.rs`). These walk the raw body counting a block per `\n` and per
`OBJECT_REPLACEMENT`, which does **not** match the **coalescing-aware** projection
block index (the projection drops the record-less phantom empty after an object).
So a projection `block_ix` (e.g. `MoveBlock { new_block_ix }`, `InsertBlock { block_ix }`)
maps to the wrong body position, and the incremental replay (which moves by projection
index) and the Loro mutation (which moves by body-walk index) disagree — an off-by-N
around objects/coalesced empties.

`MoveBlock` additionally deletes the object then computes the insert position on the
post-delete body while `new_block_ix` is a pre-delete index — a second off-by-one on
forward moves.

## Fix direction

Mirror the paragraph coordinate fix (5d50099 / d4b11ae): resolve object block
positions from the **projection block structure + durable anchor cursors**, not a raw
body walk. To place an object at projection block index `N`:
- resolve the live body position of the block currently at `N` (its durable
  `anchor_cursor` for an object, or `paragraph_body_start_in_loro`-style boundary for
  a paragraph), and insert there;
- for `MoveBlock`, compute the target position **before** deleting the source, or
  adjust the index for the post-delete shift.

Touches: `object_insert_unicode_pos_for_projection_block`,
`object_unicode_pos_for_projection_block`, `move_projection_object_block`,
`insert_projection_object_block`, `delete_projection_object_block`
(`crdt_runtime.rs`), and the matching incremental replay (`lifecycle.rs`).

Then re-enable the `TEMP-DISABLED` generator arms and grind to green, then add the
9 table ops (cookbook ready).

## Coverage-extension results (what the expansion found)

I extended `generate_command` to the full op surface and ran the fuzz. Results, so
the next session starts from known ground:

- **SAFE (validated convergent on the structural fixture, N=2–5):** `SetRunStyles`,
  `ReplaceImageAltText`, `SetImageLayout`, `ReplaceImageCaption`. These can be added
  back with confidence.
- **BUG #1 — object block-structure positioning** (above): `InsertBlock`,
  `DeleteBlock`, `MoveBlock`, `ReplaceBlock`. First failure `MoveBlock` off-by-one.
- **BUG #2 — soft-break under broader op sequences:** with the wider op mix, the
  *blank*-doc fuzz hit a paragraph-identity divergence on an `InsertText` of U+2028
  (`SplitParagraph`/`SetRunStyles` reach a state the old paragraph-only sequence
  didn't). The structural fuzz did not hit it (different sequence). This is a real
  materializer bug around soft breaks + paragraph structure — reproduce by re-adding
  `SetRunStyles` and running `npeer_fuzz_blank_paragraph_ops`. Investigate whether a
  run-style mark spanning a boundary/soft-break, or a soft-break split, is the trigger.

The generator extension was reverted to keep the fuzz a green gate; re-apply it
incrementally (SAFE ops first, then grind BUG #1 and BUG #2) next session.
