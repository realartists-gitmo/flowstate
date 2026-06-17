# FIX_LORO_ROOT — Make the single-root LoroText model fast and correct

> Goal: eliminate expensive full-document rewrites/reprojections while preserving
> the single-root `LoroText` design and CRDT merge correctness. This document is
> the execution plan: scoped tasks, exact code locations, dependencies, and a
> parallelization matrix so multiple subagents can work without colliding.

---

## 0. TL;DR

The collab core already migrated to a **single-root `LoroText`** design (one root
text container `"body"`, paragraphs delimited by `\n`; the `"blocks"` movable list
holds only metadata maps). The migration is **functionally complete but performance-
and merge-incomplete**: the supporting infrastructure that makes single-root cheap
(a maintained paragraph→byte offset index, incremental span splices, delta-driven
remote reconcile) was never added. As a result, **almost every edit that is not a
single-character keystroke collapses into a whole-document rewrite**, and every
remote text change re-parses and re-compares the entire document.

There is **no second text layout in the code** (no leftover per-paragraph text
containers). The conflicts are:

1. **Performance/merge debt** that fights the single-root model (the main work).
2. **Stale design docs** (`plan.md`, `fix.md`) that still describe the *abandoned*
   per-paragraph-`LoroText` schema (v1). These are misleading but harmless to code.
3. **Dead pre-CRDT code paths** (op-replay remote apply + a postcard wire format)
   left behind by the migration.
4. **One redundant source of truth**: paragraph style is stored twice
   (block-map `STYLE` + body `pstyle` mark).

The fix is organized into three independent groups (collab core, editor model,
cleanup) that can run in parallel.

---

## 1. The invariant (read before changing anything)

The single-root model must hold these invariants after **every** operation:

- **INV-1 (text):** `body_text(doc)` UTF-8 content == `full_document_text(document)`
  (all paragraphs joined by `'\n'`). A debug helper already checks this:
  `debug_assert_body_matches_document` (`crates/flowstate-collab/src/local_apply.rs`).
- **INV-2 (marks):** for every paragraph, the body marks (`sem`/`ul`/`strike`/`hl`
  inline, `pstyle` paragraph) over that paragraph's byte range match the paragraph's
  runs/style. Whole-body rewrite is correct today *because* it reapplies all marks
  (`replace_body_from_document` → `apply_mark_intervals`, `projection.rs:148-160`);
  any incremental replacement **must** carry marks for the spliced range.
- **INV-3 (binding):** `DocBinding.rows` is 1:1 and in-order with `document.blocks`
  (kind, block id, paragraph id, version). Checked by
  `DocBinding::assert_consistent` (`binding.rs`).
- **INV-4 (convergence):** concurrent edits converge to identical state. Minimal,
  intention-preserving Loro deltas are required — a full body `delete(0,len)+insert`
  destroys concurrent peers' insertion anchoring even when text is unchanged.

Every task below must keep INV-1..4. When in doubt, fall back to the existing
whole-document path (`replace_document`) rather than risk divergence.

---

## 2. Current full-rewrite / full-reprojection map (the debt)

Verified locations (line numbers approximate; anchor on symbol names):

| Path | Location | Triggered by | Cost |
|---|---|---|---|
| `replace_paragraph_span` discards span, calls `replace_document` | `flowstate-collab/src/local_apply.rs:276-285` | **Almost every non-typing edit**: delete/backspace, selection-replace, IME/multi-char, Enter, paste, drag-move | full block-list + body rewrite + `DocBinding::build` |
| `replace_document` (`replace_blocks_from_document` + `DocBinding::build`) | `local_apply.rs:300-304` | `ReplaceParagraphSpan`, `ReplaceDocument`, `ReplaceBlock` fallback | O(doc) |
| `replace_body_from_document` (full body `delete+insert`+remark) | `projection.rs:148-160`, called at `local_apply.rs:207,255,270` | paragraph block insert/delete/move | O(text) CRDT churn |
| `body_paragraph_range` stringifies whole body + linear newline scan | `local_apply.rs:452-455` (`text_content`) + `paragraph_range_in_body_text:481-495` | **every** `InsertText`(fast-path keystroke), `DeleteRange`, `SetRunStyles`, `SetParagraphStyle`, `split_paragraph` | O(n) per call |
| `reconcile_body_text` reprojects ALL blocks + compares EVERY paragraph; Loro `TextDelta` discarded | `remote_apply.rs:37` (`Diff::Text(_)`) + `:50-99` | every remote body change | O(doc) per remote keystroke |
| Self-check timer runs full `document_from_loro` + 2 hash passes | `flowstate/src/collab/session_timers.rs:263-289` | every 30s while attached & idle ≥2s | recurring O(doc) |
| Editor: `update_paragraph_offsets_after_len_change` → tail `to_vec` + `replace_paragraph_blocks` + `rebuild_document_sections` | `gpui-flowtext/src/edit_ops/offsets.rs:59-76` | **every keystroke/backspace** | O(n) |
| Editor: `rebuild_document_sections` (walks all paragraphs) | `gpui-flowtext/src/document/core.rs:335-378` | ~20 call sites incl. per edit | O(n) |
| Editor: `DocumentIdentityMap::reconcile` clones id vecs + walks all blocks; `paragraph_index` linear scan | `gpui-flowtext/src/collaboration.rs:29-77` | every shape-changing edit + remote batch | O(n) |
| Editor: collab structural patches → `rebuild_document_from_collab_structural_blocks` (full rebuild) | `gpui-flowtext/src/rich_text/editor/collab_apply.rs:68-117,240-265`; headless `patch_apply.rs:151-178` | remote block insert/delete/move/object-replace | O(doc) |

**Key fact:** the granular ops `DeleteRange`/`SplitParagraph`/`JoinParagraphs`
are fully implemented in `local_apply.rs:46-57,93-198` and tested
(`tests/translation.rs`, `tests/convergence.rs`) **but the editor never emits them**
— it emits `ReplaceParagraphSpan` instead. The incremental machinery exists; it is
just bypassed.

---

## 3. Conflicts / leftovers vs. single-root design (inventory)

- **No leftover per-paragraph text containers.** The only `get_text` is the root
  body (`schema.rs:140-142`); `insert_container` only ever inserts maps
  (`projection.rs:108,123`). There is **no `TEXT` key**. Schema keys are
  `meta/body/blocks`, `schema/session/title`, `kind/style/data/rev`,
  `sem/ul/strike/hl/pstyle` (`schema.rs:17-38`).
- **Schema is v2, no migration path.** `SCHEMA_VERSION = 2` (`schema.rs:23`);
  `verify_lineage` *rejects* any other version (`projection.rs:204-209`). The `2`
  implies a prior v1 (the per-paragraph design in `plan.md`). No upgrade code — old
  snapshots are refused, not migrated. (Acceptable; just document it.)
- **Stale docs:** `plan.md` (§5, §5.1, §5.2, §6.1) and `fix.md` describe the
  per-paragraph-`LoroText` schema and a `BindingRow{ text: Option<LoroText> }` that
  no longer exist, and claim "no global offset arithmetic" — the opposite of the
  implemented single-root design. → Task **T14**.
- **Dead pre-CRDT code:** editor-side op-replay remote apply
  `apply_remote_operations`/`apply_canonical_operation`
  (`gpui-flowtext/src/rich_text/editor/lifecycle.rs:314-480`, no callers) and the
  postcard wire format `WireCanonicalOperation` +
  `encode_canonical_operations`/`decode_canonical_operations`
  (`gpui-flowtext/src/collaboration.rs:144-294`) + `last_collaboration_*` accessors
  (no live callers). → Task **T12**.
- **Dual source of truth for paragraph style:** written to block-map `STYLE`
  (`projection.rs:110`, `local_apply.rs:60`) *and* as a body `pstyle` mark
  (`projection.rs:176-180`, `local_apply.rs:61-62`). On read, the map value is
  shadowed by the body mark whenever a body paragraph exists
  (`projection.rs:66-78`). → Task **T13**.

---

## 4. Fix architecture

Four pillars, mapped to tasks:

1. **Foundation — maintained paragraph offset index (T1).** Store each paragraph's
   body byte range in `DocBinding` (prefix-sum / Fenwick, mirroring the editor's
   existing `ParagraphOffsetIndex`). Replaces O(n) body stringify+scan and provides
   the offset math that incremental splices need. *Everything else builds on this.*
2. **Incremental local apply (T2, T3).** Replace the `ReplaceParagraphSpan`→
   `replace_document` shortcut and the three `replace_body_from_document` block-op
   calls with span-scoped body splices + targeted block-row reconcile, with minimal
   sub-paragraph text deltas and mark carry-over. Keep `replace_document` as a
   safety fallback.
3. **Delta-driven remote reconcile (T4).** Consume the Loro `TextDelta` instead of
   discarding it; reconcile only the affected paragraph ordinals (+ neighbors on
   newline changes) instead of reprojecting and comparing the whole document.
4. **Editor-model incrementalization (T6–T11).** Make sections/offset/identity/
   structural-patch updates range-scoped instead of O(n) per edit.

Plus cleanup (T12–T15).

---

## 5. Execution plan (parallelization)

### File-ownership matrix (avoid edit collisions)

| Owner / Agent | Crate | Files exclusively owned |
|---|---|---|
| **A — collab offset** | flowstate-collab | `binding.rs`, **new** `body_index.rs`, the 3 range helpers in `local_apply.rs` |
| **B — collab local** | flowstate-collab | `local_apply.rs` op handlers (after A lands) |
| **C — collab remote** | flowstate-collab | `remote_apply.rs` |
| **D — editor model** | gpui-flowtext | `document/core.rs`, `edit_ops/offsets.rs`, `edit_ops/insert_delete.rs`, `edit_ops/text.rs`, `edit_ops/split_delete.rs` |
| **E — editor identity** | gpui-flowtext | `collaboration.rs` (identity map) |
| **F — editor patches** | gpui-flowtext | `rich_text/editor/collab_apply.rs`; flowstate-collab `patch_apply.rs` |
| **G — cleanup** | both | `rich_text/editor/lifecycle.rs` (dead-path removal), `plan.md`, `fix.md` |
| **H — editor emission** | gpui-flowtext | `rich_text/editor/edit_pipeline.rs`, `hit_testing.rs` (after B stable) |

> Note: Agents **A** and **B** both touch `local_apply.rs`. Run A → B sequentially
> on the **same** worktree/agent, or land A first. Agents **C**, **D**, **E**, **F**,
> **G** are file-disjoint and can run fully in parallel.

### Waves

**Wave 1 (parallel):**
- A: **T1** (offset index) — *unblocks B and C.*
- D: **T6** (incremental `rebuild_document_sections`) — *unblocks T7/T8/T9.*
- E: **T10** (identity map diff + hashmap).
- F: **T11** (in-place structural patches).
- G: **T12** (remove dead pre-CRDT paths) + **T14** (retire stale docs).

**Wave 2 (parallel, after Wave 1 deps):**
- B: **T2** (incremental `ReplaceParagraphSpan`) → then **T3** (block body splices).
- C: **T4** (delta-driven remote reconcile).
- D: **T7** → **T8** → **T9** (editor model splices).
- G: **T13** (style single-source-of-truth) + **T15** (self-check cost).

**Wave 3 (after B stable):**
- H: **T5** (granular local op emission for delete/Enter).
- All: integration + convergence + clippy pass.

---

## 6. Tasks

Each task: **Priority · Depends · Files · Problem · Target · Steps · Acceptance ·
Tests · Risk/Fallback.**

---

### T1 — Maintained paragraph byte-offset index in `DocBinding`  *(FOUNDATION)*

- **Priority:** Critical · **Depends:** none · **Owner:** A
- **Files:** `crates/flowstate-collab/src/binding.rs`; **new**
  `crates/flowstate-collab/src/body_index.rs` (+ register `mod body_index;` in
  `crates/flowstate-collab/src/lib.rs`); the three range helpers in `local_apply.rs`
  (`body_byte_for_paragraph_byte:435`, `body_range_for_paragraph:444`,
  `body_paragraph_range:452`).

**Problem.** `body_paragraph_range` calls `text_content(&body_text(doc))` (full
`to_delta` → `String`) then `paragraph_range_in_body_text` linear-scans for the Nth
`\n` — O(n) per call, on the keystroke hot path (`local_apply.rs:85,103-104,151`,
`66-67,61`). `DocBinding` (`binding.rs:28-34`) stores no offsets.

**Target.** O(log n) (or amortized O(1)) paragraph→body-byte mapping with no
whole-body stringification on edits.

**Steps.**
1. Add a prefix-sum/Fenwick index of paragraph byte widths to `DocBinding`
   (mirror `gpui-flowtext` `ParagraphOffsetIndex`, `document/core.rs:401-470`).
   Width of paragraph *k* = its UTF-8 byte length; body start of ordinal *k* =
   prefix sum + *k* (for the *k* preceding `\n` separators).
2. In `body_index.rs` put: the index type, `paragraph_start(ordinal)`,
   `paragraph_range(ordinal)`, `paragraph_ordinal_for_body_byte(byte)`, and a
   ranged body parser `input_paragraphs_in_body_range(text, byte_range)` (a scoped
   version of `schema.rs:254` `input_paragraphs_from_body_text`) for T4. Also move
   the prefix/suffix diff (`remote_apply.rs:223-234` `text_delta_for_replacement`,
   `common_prefix_bytes`, `common_suffix_bytes`) here so B and C can reuse it.
3. Maintain the index incrementally: add `DocBinding` methods to update one
   paragraph's width, insert/remove paragraph entries, and shift. Call them from the
   binding mutators (`push_row`/`insert_row`/`remove_row`/`move_row`) and add a
   `refresh_paragraph_widths(doc)` used by `build`.
4. Rewrite the three `local_apply.rs` range helpers to read from the index instead
   of `text_content`. Build the index in `DocBinding::build` (`binding.rs:37`).

**Acceptance.** No `text_content(&body_text(..))` call remains on a per-edit path;
range helpers are O(log n); INV-3 holds; `assert_consistent` extended to verify the
index matches the body.

**Tests.** `cargo test -p flowstate-collab` (esp. `tests/translation.rs`,
`tests/convergence.rs`); add a unit test: random inserts/deletes keep
`index.paragraph_range(k)` equal to a fresh scan.

**Risk.** Index drift. Mitigate with a debug assert comparing index vs. fresh scan
after each op (compiled out in release).

---

### T2 — Incremental `ReplaceParagraphSpan` in `LocalApplier`  *(highest impact)*

- **Priority:** Critical · **Depends:** T1 · **Owner:** B
- **Files:** `crates/flowstate-collab/src/local_apply.rs`
  (`replace_paragraph_span:276-285`, helpers).

**Problem.** `replace_paragraph_span` discards `before`/`after`/`start_paragraph`
and calls `replace_document` (full rewrite). This is the dominant production edit
path (delete, selection-replace, IME, Enter, paste, drag).

**Target.** Span-scoped body splice + targeted block-row reconcile; minimal text
delta; marks carried; fall back to `replace_document` only when ids/ranges can't be
reconciled.

**Inputs.** `document: &Document` (post-edit), `start_paragraph: Option<ParagraphId>`,
`before: &DocumentSpan`, `after: &DocumentSpan` where
`DocumentSpan { start_paragraph: usize, paragraphs: Vec<Paragraph>, text: String }`
(`gpui-flowtext/src/document/text.rs:74-79`). New paragraph/block ids are read from
`document.ids` (as `split_paragraph` already does, `local_apply.rs:140-150`).

**Steps.**
1. Resolve start ordinal: from `start_paragraph` id → row → ordinal (via T1); else
   `before.start_paragraph`. If unresolved → fallback `replace_document`.
2. `before_n = before.paragraphs.len()`, `after_n = after.paragraphs.len()`.
   Compute body range to replace: `start = index.paragraph_start(start_ord)`,
   `end = index.paragraph_range(start_ord + before_n - 1).end`.
3. **Text splice (minimal):** common-prefix/suffix-diff `before.text` vs
   `after.text` (shared helper from T1); `body.delete_utf8` + `body.insert_utf8`
   only the differing middle within `[start..end]`.
4. **Marks:** re-apply run styles (`set_run_styles_utf8`) and `pstyle`
   (`set_paragraph_style_utf8`) across the affected paragraphs' new byte ranges so
   INV-2 holds even for style-only changes (compute intervals like
   `projection.rs:162` `document_mark_intervals`, but only for the span).
5. **Block rows:** `delta = after_n - before_n`. If `>0` insert `delta` paragraph
   rows (`insert_paragraph_container` + `BindingRow`) at the right block index with
   ids from `document.ids`; if `<0` delete `-delta` rows; for overlapping rows
   update `STYLE`/version/`paragraph_id` if changed. (Reuse the same helpers T3 uses.)
6. Update the T1 index for the changed range. `debug_assert_body_matches_document`.
7. Wrap in a result; on any `Err`/inconsistency → `self.replace_document(document)`.

**Acceptance.** A single-char backspace produces a Loro delta touching only that
paragraph's changed bytes (verify via `subscribe_local_update` byte size or a delta
assert), not the whole body. Paragraph count changes update only the affected rows.

**Tests.** Extend `tests/convergence.rs` with backspace, selection-replace across
2–3 paragraphs, paste of multiple paragraphs, Enter; assert convergence vs. a peer
and vs. the `replace_document` fallback (differential test: incremental result ==
fallback result for the same edit).

**Risk.** Off-by-one on `\n` boundaries; id assignment. Differential test against
the fallback is the safety net.

---

### T3 — Incremental body splices for paragraph block insert/delete/move

- **Priority:** Medium (currently dead-in-prod; needed for T5 & headless correctness)
  · **Depends:** T1 (and shares helpers with T2) · **Owner:** B
- **Files:** `local_apply.rs` `insert_block:200`, `delete_block:243`,
  `move_block:261` (remove the `replace_body_from_document` calls at 207/255/270).

**Problem.** Paragraph block insert/delete/move rewrite the entire body even though
text content barely changes (INV-4 violation under concurrency).

**Target.** Splice only the affected slice + separator newline; carry marks.

**Steps.**
- **insert (paragraph):** insert `text + "\n"` (or `"\n" + text` for last position)
  at `index.paragraph_start(ord)`; apply marks for the inserted text; then existing
  row insert. Empty paragraph → insert just `"\n"`.
- **delete (paragraph):** delete the paragraph's body range **plus one adjacent
  separator newline** (handle first/last paragraph asymmetry); then existing row
  delete.
- **move (paragraph):** capture slice text+marks, delete from old position
  (+separator), insert at target (+separator), re-apply marks.
- Update T1 index. Keep `replace_body_from_document` only as fallback.

**Acceptance/Tests.** `tests/convergence.rs` block insert/delete/move converge;
INV-1/2/3 asserts pass. Differential vs. `replace_body_from_document`.

**Risk.** Newline-adjacency edge cases (first/last/empty doc). Cover with unit tests.

---

### T4 — Delta-driven remote reconciliation

- **Priority:** High · **Depends:** T1 (offset mapping + ranged parse) · **Owner:** C
- **Files:** `crates/flowstate-collab/src/remote_apply.rs`
  (`apply_event:25-48`, `reconcile_body_text:50-99`).

**Problem.** `apply_event` matches `Diff::Text(_)` and throws the delta away
(`:37`), then `reconcile_body_text` calls `input_blocks_from_loro` (full reproject)
and compares **every** paragraph row. The emitted delta is even recomputed locally
(`text_delta_for_replacement:223-234`) instead of using Loro's.

**Target.** Use the Loro `TextDelta` to compute affected body ranges → affected
paragraph ordinals (+ neighbors when `\n` is inserted/deleted), and reconcile only
those.

**Steps.**
1. In `apply_event`, capture the body `TextDelta` (don't discard). Keep the
   `body_changed` fast-exit when empty.
2. Walk the delta with a body-offset cursor; collect (a) changed byte ranges and
   (b) net `\n` insertions/deletions (paragraph count delta and positions).
3. Map ranges → paragraph ordinals (T1 helpers on the post-import body). Build the
   minimal affected ordinal set; expand by ±1 where a newline boundary changed.
4. Re-derive only those paragraphs via `input_paragraphs_in_body_range` (T1) and
   emit `ParagraphText`/`ParagraphStyle`/`ParagraphRuns` patches for them; for
   newline insert/delete, emit structural `InsertBlocks`/`DeleteBlocks` + reconcile
   the binding rows for just that region (reuse `reconcile_binding_to_match_blocks`).
5. Prefer Loro's delta offsets for `delta_utf8` in `ParagraphText` (still fine to
   recompute per-paragraph prefix/suffix for the editor's selection remap).
6. **Fallback:** if the delta can't be cleanly mapped (overlapping/ambiguous),
   call the existing `reconcile_body_text` (keep it) for that event.

**Acceptance.** A remote single-char insert in an N-paragraph doc parses/compares
O(1) paragraphs, not N. Convergence unchanged.

**Tests.** `tests/remote_apply.rs`, `tests/convergence.rs`: remote single-char
insert/delete, remote split/join (newline insert/delete), concurrent edits to
different paragraphs (must not touch each other's patches), style-only change.

**Risk.** Highest-complexity task. The whole-compare fallback guarantees
correctness; optimize incrementally and gate behind the fallback.

---

### T5 — Emit granular local ops for delete / Enter (optional, complements T2)

- **Priority:** Low/Medium · **Depends:** T2 stable · **Owner:** H
- **Files:** `gpui-flowtext/src/rich_text/editor/edit_pipeline.rs`,
  `hit_testing.rs`.

**Problem.** Backspace/Delete/Enter go through the generic span path. The exact ops
`DeleteRange`/`JoinParagraphs`/`SplitParagraph` already exist and are tested in
`local_apply.rs` but are never emitted, so deltas are larger than necessary.

**Target.** Add fast paths analogous to the single-grapheme insert fast path
(`edit_pipeline.rs:3-113`) that emit `DeleteRange` (in-paragraph delete),
`JoinParagraphs` (boundary backspace/delete), `SplitParagraph` (Enter), keeping
`ReplaceParagraphSpan` (now incremental via T2) as the fallback for compound edits.

**Acceptance/Tests.** `tests/collab_capture.rs`-style assertions that backspace
emits `DeleteRange`, Enter emits `SplitParagraph`; convergence unchanged.

**Risk.** Low (handlers already tested); purely shrinks deltas.

---

### T6 — Incremental `rebuild_document_sections`  *(editor foundation)*

- **Priority:** High · **Depends:** none · **Owner:** D
- **Files:** `gpui-flowtext/src/document/core.rs:335-378`.

**Problem.** Walks all paragraphs to rebuild the heading outline; called from ~20
sites including transitively per keystroke (via `replace_paragraph_blocks`).

**Target.** Dirty-range/incremental section recomputation (only rebuild sections
intersecting the changed paragraph range; reuse stable `SectionId`s).

**Acceptance.** Single-paragraph edit does not re-walk all paragraphs for sections.
**Tests.** `cargo test -p gpui-flowtext` (section/outline tests); add a test that
editing paragraph *k* leaves untouched sections' `SectionId`s stable.
**Risk.** Section nesting/stack recomputation at range edges — include boundary tests.

---

### T7 — In-place `replace_paragraph_blocks` for single-paragraph updates

- **Priority:** High · **Depends:** T6 · **Owner:** D
- **Files:** `gpui-flowtext/src/document/core.rs:158-215`.

**Problem.** Full block walk + unconditional `rebuild_document_sections` even when a
single paragraph's content changed in place (paragraph-only docs have
`blocks.len()==paragraphs.len()`, so `update_paragraph_block` alone suffices).

**Target.** Fast path: when count is unchanged and only content changed, update the
single block + the affected section range (T6) without the full walk.

**Acceptance/Tests/Risk.** As T6; differential vs. current output for mixed
paragraph/object docs.

---

### T8 — Make `update_paragraph_offsets_after_len_change` O(log n)

- **Priority:** High · **Depends:** T6, T7 · **Owner:** D
- **Files:** `gpui-flowtext/src/edit_ops/offsets.rs:59-76` (callers
  `insert_delete.rs:84,125`).

**Problem.** Per keystroke it does: O(log n) Fenwick update (good) **then** tail
`refresh_paragraph_ranges_from` + tail `.to_vec()` clone + full
`replace_paragraph_blocks` + full `rebuild_document_sections` — net O(n).

**Target.** Keep the Fenwick update; drop the tail clone and full block/section
rebuild for the in-place single-paragraph case (use T7's in-place update and
incremental `byte_range` shift).

**Acceptance.** Typing/backspace in a large doc is O(log n) at the model layer.
**Tests.** Offset-index/byte-range correctness after random edits; existing editor
tests. **Risk.** `byte_range` cache staleness for the tail — shift offsets via the
Fenwick prefix instead of rewriting each.

---

### T9 — Scope rebuilds in span replacement / paragraph-content apply

- **Priority:** Medium · **Depends:** T6 · **Owner:** D
- **Files:** `gpui-flowtext/src/edit_ops/text.rs:75-97`
  (`apply_document_span_replacement`); `rich_text/editor/collab_apply.rs:166-181`
  (`replace_paragraph_content`).

**Problem.** Both unconditionally call `rebuild_document_offset_index` (O(n)) and
`rebuild_document_sections` (O(n)) after a span/paragraph-scoped change.

**Target.** Use the incremental offset update (T8) and dirty-range sections (T6);
scope to the touched paragraph range.

**Acceptance/Tests.** Collab `ParagraphText`/`ParagraphStyle`/`ParagraphRuns` and
local span edits update only the touched range; editor tests pass.

---

### T10 — Diff-based `DocumentIdentityMap::reconcile` + hashmap lookup

- **Priority:** Medium · **Depends:** none · **Owner:** E
- **Files:** `gpui-flowtext/src/collaboration.rs:13-77`.

**Problem.** `reconcile` clones both id vectors and walks all blocks every call
(`:29-77`); `paragraph_index` is an O(n) linear scan (`:70-76`) used per remote/
canonical op.

**Target.** Maintain a `FxHashMap<ParagraphId, usize>` (and block id map); update
incrementally; make `reconcile` diff only changed ranges.

**Acceptance/Tests.** `paragraph_index` is O(1); identity tests pass; remote apply
of many small ops doesn't show O(n²) behavior.
**Risk.** Keeping the map in sync across all edit paths — centralize updates.

---

### T11 — In-place collab structural patches (no full document rebuild)

- **Priority:** Medium · **Depends:** none (coordinate with T4 output shape) · **Owner:** F
- **Files:** `gpui-flowtext/src/rich_text/editor/collab_apply.rs:68-117,240-265`;
  headless `crates/flowstate-collab/src/patch_apply.rs:46-77,151-178`.

**Problem.** `InsertBlocks`/`DeleteBlocks`/`MoveBlock`/`ReplaceObjectBlock` call
`rebuild_document_from_collab_structural_blocks` / `rebuild_document_from_structural_blocks`
— a full `Document` reconstruction (+ whole-doc layout invalidation).

**Target.** Splice blocks/paragraphs in place (insert/remove/move the affected rows,
shift offsets via T8/T6 incremental APIs), invalidate only the affected range.

**Acceptance/Tests.** Remote block insert/delete/move updates only affected rows;
`patch_apply` headless tests + convergence pass.
**Risk.** Keeping `ids`, `offset_index`, `sections` consistent during in-place
splice — assert equality vs. the rebuild path in tests.

---

### T12 — Remove dead pre-CRDT code paths

- **Priority:** Medium (low risk) · **Depends:** none · **Owner:** G
- **Files:** `gpui-flowtext/src/rich_text/editor/lifecycle.rs:314-480`
  (`apply_remote_operations`, `apply_canonical_operation`),
  `gpui-flowtext/src/collaboration.rs:144-294`
  (`WireCanonicalOperation`, `encode_canonical_operations`,
  `decode_canonical_operations`), and `last_collaboration_*` accessors
  (`lifecycle.rs:232-248`).

**Problem.** Orphaned op-replay remote path and postcard wire format from before the
Loro-update-bytes sync; no live callers (remote apply now flows through
`apply_collab_patches`).

**Steps.** Confirm no callers (`rg` for each symbol across `crates/`, excluding
tests/defs), remove, `cargo check`/`cargo clippy`. **Do NOT** remove the
`CanonicalOperation` enum or the `local_apply.rs` handlers for
`DeleteRange`/`SplitParagraph`/`JoinParagraphs` — those are the incremental local
apply machinery (kept/used by T2/T3/T5).

**Acceptance.** Build + clippy clean; tests pass; dead symbols gone.

---

### T13 — Single source of truth for paragraph style

- **Priority:** Medium · **Depends:** none (coordinate with T2/T4) · **Owner:** G
- **Files:** `schema.rs` (`pstyle` mark fns), `projection.rs:66-78,110,176-180`,
  `local_apply.rs:58-63`, `remote_apply.rs` (style diff path).

**Problem.** Style stored twice: block-map `STYLE` (write-only on the normal read
path) and body `pstyle` mark (authoritative when a body paragraph exists). Redundant
work + drift risk.

**Decision (recommended):** make the **block-map `STYLE`** the single source of
truth and remove the `pstyle` body mark. Rationale: the block-map row is the stable
per-paragraph identity handle in the single-root model; a body mark must be
re-maintained across every text splice (extra work for T2/T3) and is `ExpandType::Both`
(expansion hazards). Style changes already flow through the map diff path on remote
(`remote_apply.rs:118` `STYLE → style_changed`).

**Steps.** Drop `MARK_PARAGRAPH_STYLE` config + writes (`schema.rs:135`,
`set_paragraph_style_utf8`, `document_mark_intervals` index 4); ensure
`input_block_from_container` always reads style from the map; ensure remote style
changes are emitted from the map diff only. Bump `SCHEMA_VERSION` to 3 (breaking
layout change) and update `verify_lineage`.
*Alternative* (if mark-based is preferred for some reason): keep `pstyle`, stop
writing/reading map `STYLE`. Pick one; do not keep both.

**Acceptance/Tests.** Style set/clear + concurrent style edits converge; no `pstyle`
references remain (chosen option); convergence tests updated.
**Risk.** Behavioral; must update convergence tests and snapshot compatibility note.

---

### T14 — Retire / update stale design docs

- **Priority:** Low · **Depends:** none · **Owner:** G
- **Files:** `plan.md`, `fix.md`.

**Problem.** Both describe the abandoned per-paragraph-`LoroText` schema (v1):
`plan.md:184-241` (schema, §5.1/§5.2 "per-paragraph texts… no global offset
arithmetic"), `plan.md:266-293` (`BindingRow{ text }`), `plan.md:331-391` (op algos
creating `"text"`); `fix.md` audits against `plan.md` as "source of truth."

**Steps.** Either (a) add a prominent "SUPERSEDED — see FIX_LORO_ROOT.md; the
implementation uses a single-root `LoroText`" banner at the top of both, or (b)
rewrite §5/§6 to match single-root. Recommend (a) now, (b) later.

---

### T15 — Reduce self-check timer cost (optional)

- **Priority:** Low · **Depends:** none · **Owner:** G
- **Files:** `crates/flowstate/src/collab/session_timers.rs:263-308`,
  `self_check.rs`.

**Problem.** `run_self_check` calls full `document_from_loro` + two `projection_hash`
passes every 30s while attached/idle — recurring O(doc).

**Target.** Cheaper drift detection: maintain a rolling projection hash updated by
the incremental apply paths, or compare Loro version vectors / a cached structural
hash instead of a full reprojection; keep full reprojection only when the cheap
check disagrees.

**Acceptance/Tests.** No full reprojection on the no-drift path; drift still
detected (inject a divergence test).

---

## 7. Global acceptance criteria

- `cargo test -p flowstate-collab` and `cargo test -p gpui-flowtext` green
  (esp. `tests/convergence.rs`, `tests/remote_apply.rs`, `tests/translation.rs`,
  `tests/collab_capture.rs`).
- `cargo clippy` clean for touched crates (per `AGENTS.md`; fix non-false-positive
  lints). Avoid `cargo build`/`fmt`/`run` unless testing a binary.
- **Differential safety:** for T2/T3/T4/T11, add tests asserting the incremental
  result equals the corresponding full-rewrite/full-rebuild result for identical
  edits (the fallback paths stay in the codebase as oracles).
- **Delta-size assertions:** for T2/T4/T5, assert local/remote Loro deltas are
  proportional to the edit, not the document (e.g., via `subscribe_local_update`
  byte length on a large doc).
- INV-1..4 hold (debug asserts compiled in for tests).

## 8. Suggested validation harness (add once, reused by all)

- A "large doc" fixture (e.g. 5k paragraphs) + helper to (a) apply an edit, (b)
  measure emitted Loro update bytes and reconcile patch count, (c) assert two peers
  converge. Put in `crates/flowstate-collab/tests/perf_smoke.rs` (logic assertions,
  not wall-clock). This makes the "fast as possible" goal testable and guards
  against regressions.

## 9. Out of scope / notes

- **No disk persistence exists** (snapshots are in-memory, peer-to-peer); no schema
  migration is required for old files. If T13 bumps `SCHEMA_VERSION`, in-flight v2
  sessions simply won't interop with v3 — acceptable pre-release; note it.
- Keep `replace_document` / `reconcile_body_text` / `rebuild_document_from_*` as
  fallbacks; this plan makes them rare, not absent.
