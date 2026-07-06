# Decision: object-adjacent empty-paragraph coalescing parity

**Status:** DECIDED — **Fork B** (preserve real empties). Half implemented; incremental half remains.
**Found by:** the new N-peer convergence fuzz harness (`multi_peer_convergence_tests.rs`, commit d4b11ae).
**Reproduces:** `cargo test -p flowstate-collab npeer_fuzz_structural_fixture_paragraph_ops -- --ignored` (fails). Minimal: single peer, `structural_fixture`, seed `0xB2`, 15 ops.

## Progress (Fork B)

- **DONE — full-rebuild half:** `push_flow_blocks` (`loro_projection.rs:279`) now coalesces ONLY the phantom (the first `\n` after an object, `current_boundary == None`) and KEEPS real, durably-recorded empty paragraphs. An empty line next to an image now survives in the authoritative `document_from_loro` projection. **Zero regressions** across all 240+ collab/document/gpui-flowtext tests.
- **REMAINING — incremental half (in your active rework):** the incremental replay in `gpui-flowtext` (`replay_semantic_command_on_projection` / `projection_apply.rs`) must produce the same segmentation. The structural fuzz now surfaces a *second, deeper* bug there: a **merge op where the Loro mutation merged two paragraphs but the incremental replay did not** (seed 0xA1: canonical shows `"…aboveme.cClosing…"` as one paragraph, incremental keeps them split). That's a JoinParagraphs/cross-paragraph-DeleteRange replay-vs-Loro mismatch, likely the same coordinate/atomicity family as the fixes in d4b11ae but on the incremental side. Fix that + mirror the phantom-coalescing, then un-ignore the structural fuzz.

The sections below are the original analysis that led to choosing Fork B.

---

This is the last convergence bug the harness surfaces after the fixes already landed this session. I'm isolating it here rather than guessing because the correct fix depends on a **product question** and touches the incremental-materializer rework you're actively editing.

---

## TL;DR

`document_from_loro` (the authoritative full rebuild, via `push_flow_blocks`) **coalesces** an empty paragraph that sits immediately after an object (image/table/equation) — it drops it from the projection. The **incremental** projection (the per-edit replay in `gpui-flowtext`) does **not** coalesce. So the moment an edit *creates or exposes* an object-adjacent empty paragraph, the two projections disagree on paragraph count/identity, the preflight assert (`crdt_runtime.rs:1290`) fires, and in a release build the incremental patch shipped to the editor/peers is wrong → "visual fuckery" / rolling-back edits.

The two must use **one** segmentation rule. The fork is *which* rule, and that hinges on a product question.

---

## Why it exists

An object is its own block. In the body flow, `[paragraph, object, paragraph]` serializes as:

```
\n before  OBJECT  \n after
```

Without any coalescing, `push_flow_blocks` would emit a **record-less empty paragraph** between `OBJECT` and `after` (the `\n` after the object, with no preceding text, produces an empty paragraph). That's a *phantom* — an artifact of the object being a block, not something the user created. Commit `3d2cb82` added the coalescing branch (`loro_projection.rs:279`) to drop it.

But that branch drops **every** empty paragraph whose previous block is an object — including **real** ones the user typed or an edit created (e.g. deleting the last char of a paragraph that happens to sit right after an image). The incremental replay keeps those. Hence the divergence.

### Concrete repro (seed 0xB2, single peer)

Fixture has `… alpha, IMAGE, "Two empties above me." …`. Edits split `"Two empties above me."` into `"T"` + `"wo empties above me."`, then delete the `"T"`:

- **Incremental** (predicted): keeps the now-empty `""` paragraph → **9 paragraphs**.
- **Full** (`document_from_loro`): coalesces the object-adjacent `""` → **8 paragraphs**.

`debug_assert_eq!` on `paragraph_ids` fires.

---

## Why the "obvious" one-line fix (Fork B) is NOT enough

I tried gating the coalescing on `current_boundary.is_none()` (coalesce only the *first* `\n` after an object = the phantom, keep subsequent empties). It gives the correct paragraph **count per body `\n`** and passed the 104 unit tests — but the fuzz still failed, because the phantom/real distinction is **position-ambiguous**: an edit-created **real** empty can itself be the first `\n` after an object, and a bare `[image, paragraph]` shares one `\n` that is simultaneously "the phantom slot" and "the next paragraph's boundary." Position alone can't tell them apart, and the incremental replay has no equivalent notion at all. Reverted (see git history of this doc's commit).

The lesson: **any fix that changes only one of the two paths, or distinguishes phantom-vs-real by position, will not converge.** The rule must be shared and unambiguous.

---

## The forks

### Fork A — make the incremental replay coalesce, matching the full rebuild *(recommended: lowest risk)*
Teach the `gpui-flowtext` incremental path (`replay_semantic_command_on_projection` / `rebuild_document_from_projection_patch_blocks` / the object fast-paths in `projection_apply.rs`) to drop an object-adjacent empty paragraph exactly as `push_flow_blocks` does. `push_flow_blocks` stays the authority; the incremental path is made to agree.
- **Pro:** smallest blast radius on convergence-critical code; keeps the deliberate `3d2cb82` behavior; the full rebuild is untouched and remains the source of truth; provable by the fuzz.
- **Con:** preserves the (arguably surprising) product behavior that an empty line directly after an image is silently dropped. Requires care in the rework you're editing.
- **Where:** `crates/gpui-flowtext/src/rich_text/editor/projection_apply.rs` (+ `lifecycle.rs`). The rule to mirror: `loro_projection.rs:279-286`.

### Fork B — record-aware coalescing in BOTH paths *(product-nicer, more work)*
Make the segmentation rule "an object-adjacent empty paragraph is coalesced **iff it has no durable paragraph metadata record**" (phantom = record-less; real = recorded), and apply that same rule in `push_flow_blocks` **and** the incremental path. Real empties (which always carry a record — from `ensure_paragraph_metadata`/split) survive everywhere; only true phantoms drop.
- **Pro:** correct product behavior — users can keep an empty line next to an image; matches intuition.
- **Con:** touches the convergence-critical full-rebuild segmentation *and* the incremental path; the record lookup at a boundary is subtle (the `\n` after an object is shared with the next paragraph's boundary — needs the record to be attributed to the empty, not the follower). Higher risk; more validation needed.

### Fork C — prune coalesced empties from Loro on write
When the full rebuild would coalesce an empty, also delete its `\n`/record from Loro so the body matches the projection.
- **Con:** mutation-on-projection, races with concurrent edits, can fight the CRDT. Not recommended.

---

## The product question you need to answer

**Should an empty paragraph immediately after an image/table be preserved, or silently dropped?**

- If **dropped is fine** (current behavior) → **Fork A**. Ship the safe convergence fix; done.
- If **it should be preserved** (my hunch for a word processor — people put blank lines around figures) → **Fork B**. More work, but the right end state.

My recommendation: **Fork A now** (it makes the harness green and stops the corruption with minimal risk), then **Fork B as a follow-up** product improvement once the harness is a trusted gate. Shipping A first means every future change is protected by the passing fuzz while B is developed.

---

## What's already fixed this session (so this doc is scoped)

- `17a624e` perf (OOM), `b473b1f` docx soft-break import, `5d50099` FS-170 durable-cursor offsets (InsertText/DeleteRange/SplitParagraph), `99d00f0` caret round-trip guard.
- `d4b11ae` **the harness** + JoinParagraphs atomicity + Loro-space coordinates for SetParagraphStyle/SetRunStyles/JoinParagraphs (three more mutation arms the earlier coordinate fix missed).

The harness (`npeer_fuzz_blank_paragraph_ops`, N=2–5) is **green**; the structural fuzz is `#[ignore]`d pointing here. Un-ignore it as the acceptance test for whichever fork you pick.

## Also worth knowing (not blockers)

- The generator currently covers paragraph/text ops (Insert incl. soft breaks, Delete, Split, Join, SetParagraphStyle). Block/table/object ops (InsertBlock, DeleteBlock, MoveBlock, ReplaceBlock, the 9 table ops, image/equation ops) are **not yet generated** — the construction cookbook for all 24 `SemanticEditCommand` variants is ready to wire in. Expect more bugs when object/table mutation is fuzzed; add it after the coalescing fork lands.
- All divergences found so far are **single-peer materializer-equivalence** (incremental vs a fresh `document_from_loro` on the same doc), i.e. they reproduce without concurrency — concurrency just reaches the triggering states faster.
