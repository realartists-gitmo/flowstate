# Differential Diagnosis: Cross-Machine Benchmark Variance

**Subject:** Two `[hotpath]` profiles of `flowstate` (`app::run_standalone`) showing "wildly
different" per-function costs across two machines.

- **Run A** — 71.25 s wall, `CPU baseline avg: 107.44 µs`, 641 measured functions, threads
  named `flowstate-crdt-`, `flowstate`, `Worker-N`.
- **Run B** — 35.91 s wall, `CPU baseline avg: 0 ns`, 584 measured functions, threads named
  `main`, `thread_32716`, `hp-threads`, all with status `Unknown`.

Code examined at `crdt-materializer-refactor` (087a383) and its ancestor f712a33, plus the
`hotpath 0.16.1` profiler source (the exact version in `Cargo.lock`).

---

## TL;DR

The two profiles are **not measurements of the same experiment**. Five independent
confounders stack on top of each other, and none of them is "machine X has a slower CPU":

1. **Different builds.** Run A's binary predates the `crdt-materializer-refactor` commits;
   Run B's is newer (or at least a different commit). Proven by symbol evidence, not vibes
   (§2).
2. **Different operating systems.** Run A is Linux; Run B is Windows. The profiler itself
   behaves differently on each (§3).
3. **Different workloads.** Run B imported the DOCX once *plus two file previews*, and spent
   the session **typing** (153 edit flushes, 190 k paragraph mutations). Run A was one import
   plus a scroll-heavy, mostly idle session (§4).
4. **The single biggest consumer in Run A is invisible to the profiler.** The
   `flowstate-crdt-runtime` thread burned ~56 s of CPU and 14.4 GB of allocations, and
   `flowstate-collab` has **zero hotpath instrumentation**, so none of it appears in the
   timing/alloc tables (§6).
5. **The numbers being compared are aggregates whose cost is state-dependent**, plus top-15
   table truncation and `% Total` percentages taken against very different wall-clock/idle
   ratios (§8).

When you compare **leaf functions doing fixed work**, the two machines are within 5–40 % of
each other — ordinary hardware/OS spread, nothing "wild" (§5).

---

## 1. What the header line actually tells us

`hotpath`'s `CPU baseline avg` is not probe overhead. A background thread
(`hp-cpu-baseline`) runs a fixed 50 000-iteration integer workload every 50 ms and measures
it with `CLOCK_THREAD_CPUTIME_ID`, then reports the average
(`hotpath-0.16.1/src/lib_on/cpu_baseline.rs`):

```rust
#[cfg(unix)]
fn thread_cpu_time_ns() -> Option<u64> { /* clock_gettime(CLOCK_THREAD_CPUTIME_ID) */ }

#[cfg(windows)]
fn thread_cpu_time_ns() -> Option<u64> { Some(0) }   // <-- hard-coded stub
```

- **Run A: 107.44 µs** for 50 k iterations ≈ 2.15 ns/iteration — a normal desktop/laptop
  figure. It also averages over the *whole run*, so scheduler contention (Run A had a second
  core pegged the entire time, see §6) inflates it. It does **not** indicate a pathologically
  slow machine.
- **Run B: 0 ns** — meaningless by construction. On Windows the baseline is a stub.

**Diagnostic value:** the baseline discriminates the platforms, and tells you the two
baselines must never be compared against each other.

## 2. The two runs are different builds

Run A's alloc table contains `document::document_sections` (67 calls). That function was
**renamed** to `document_outline` in commit `0f0b9cf` ("Just checking") — the fourth-from-tip
commit of `crdt-materializer-refactor`:

```
-pub fn document_sections(document: &DocumentProjection) -> Vec<DocumentSection> {
+pub fn document_outline(document: &DocumentProjection) -> Vec<DocumentOutlineNode> {
```

So Run A's binary was built from a commit **at or before `f712a33`**, i.e. *without* the
materializer refactor (`0f0b9cf`, `439eab8` "Fixed a bunch", `d663415` "Fixy", `d278b9a`,
`087a383`). Those commits rewrote exactly the code that dominates Run A's hidden cost:
`crdt_runtime.rs` (704 lines changed), `projection_patch.rs` (row-index patches → stable-ID
patches), and `projection_apply.rs` (612 lines).

The differing measured-function counts (641 vs 584) are consistent with this, though they are
also workload-sensitive, so the rename is the load-bearing evidence.

**Consequence:** any function whose cost the refactor touched (everything downstream of
`ProjectionPatch`, the CRDT runtime, `projection_apply`) is expected to differ between these
two profiles *because the code differs*, independent of hardware.

## 3. The two runs are different operating systems

Three profiler artifacts pin this down (all verified in hotpath 0.16.1 source):

| Artifact | Run A | Run B | Why |
|---|---|---|---|
| Thread names | `flowstate-crdt-` (exactly 15 chars) | `thread_32716`, `main` | Linux truncates pthread names to 15 chars; the Windows collector falls back to `thread_<tid>` when `GetThreadDescription` has no name |
| Thread status | `Running` / `Sleeping` | `Unknown` | `collector_windows.rs` hard-codes `status = "Unknown"` |
| Per-thread Alloc/Dealloc/Diff columns | Present (24.0 GB / 23.6 GB) | Absent | Per-thread allocation attribution is only wired up on Linux/macOS (`tid.rs` has no Windows path) |
| CPU baseline | 107.44 µs | 0 ns | Windows stub (§1) |

So **Run A = Linux, Run B = Windows**. That matters beyond cosmetics:

- **Text shaping is a different engine per OS.** `layout::shape_fragment` calls
  `window.text_system().shape_line(...)` (GPUI platform text system): cosmic-text/fontconfig
  on Linux, DirectWrite on Windows. Run A allocates **22.6 KB per shape-cache miss** vs Run
  B's **592 B** — a 40× per-call difference explained by engine + installed-font/fallback
  differences, not app code. Shaping-attributed allocations: A ≈ 3.65 GB, B ≈ 1.5 GB.
- **File I/O costs differ systematically** (NTFS + Defender vs ext4 page cache), which
  matters because of the settings-reload behavior in §7.

## 4. The two runs are different workloads

The call counts encode what the user actually did in each session:

| Signal | Run A | Run B | Reading |
|---|---|---|---|
| Wall time | 71.25 s | 35.91 s | Uncontrolled session length |
| `RichTextEditor::render` | 499 calls (7 fps avg) | 471 calls (13 fps avg) | B's session was far more interactive per second |
| `interpret_cleaned_docx` | 1 call | **3 calls** | B did 1 Loro import + **2 file previews** (`load_document_preview` → `convert_docx_to_document` also calls it) |
| `edit_ops::update_paragraph_offsets_after_len_change` | below cutoff | **153 calls** | B was **typing**; each debounced edit flush hits this |
| `document::paragraphs_mut` | below cutoff | **190 116 calls** | Same: sustained editing |
| Chunk paints per frame | 8 491/499 ≈ 17 | 14 516/471 ≈ **31** | B's viewport showed ~80 % more chunks — bigger window / higher resolution / lower zoom |
| `shape_line` | 51 837 | 97 401 | Typing forces reshaping; more visible lines |

A scroll-mostly-idle session (A) and a type-heavy, preview-clicking, larger-viewport session
(B) will *never* produce comparable per-function totals, even on identical machines and
identical builds.

## 5. Like-for-like leaf functions show the hardware is fine

For functions whose per-call work is fixed (not dependent on cache state or session
history), the machines nearly agree:

| Function (avg per call) | Run A | Run B | B/A |
|---|---|---|---|
| `workspace::load_workspace_document` (whole import) | 2.58 s | 2.66 s | 1.03× |
| `paint::paint_layout` | 290.6 µs | 308.0 µs | 1.06× |
| `VirtualParagraphChunkElement::paint` | 290.9 µs | 308.6 µs | 1.06× |
| `materialize_visible_remainders_for_scroll` | 6.00 ms | 5.75 ms | 0.96× |
| `paint::paint_line_text` | 134.3 µs | 190.4 µs | 1.42× |

Same document import within 3 %. Paint within 6 %. `paint_line_text`'s 1.42× is the largest
genuine platform gap (glyph rasterization/text-system differences, §3).

The "wildly more expensive" functions are the **aggregates**:

| Function (avg per call) | Run A | Run B | B/A |
|---|---|---|---|
| `RichTextEditor::render` | 7.94 ms | 18.93 ms | 2.4× |
| `rebuild_item_sizes_cache_with_prefetch` | 7.05 ms | 18.61 ms | 2.6× |

These are not "the same function costing more" — they are the same function doing **more
work per call** because the editor was in a different state (§8 below explains the exact
mechanism). Chasing these as if they were hardware differences is why the investigation has
repeatedly dead-ended.

## 6. Run A's real story is invisible: the uninstrumented CRDT thread

Run A's thread table is the smoking gun everyone walks past because it isn't in the timing
table:

```
| flowstate-crdt- | Running | 99.8% | 103.8% | 79.4% | 14.4 GB | 14.4 GB | 23.9 MB |
```

79.4 % average CPU over a 71.25 s run ≈ **56 s of CPU**, and **14.4 GB of the process's
24 GB total allocations** — 60 % of all memory traffic — on one thread. Meanwhile the
timing/alloc tables attribute only 9.4 GB total. Why the discrepancy? **`flowstate-collab`
(the crate that owns `CrdtRuntime` and the `flowstate-crdt-runtime` actor thread) has no
hotpath instrumentation at all** — the `hotpath` feature in `crates/flowstate/Cargo.toml`
forwards only to `flowstate-document`, `flowstate-docx`, and `flowstate-flow`. Every cycle
that thread burns is unattributed.

What was it doing? On Run A's **pre-refactor build**, each editor edit batch goes through
`apply_editor_transaction` → and when `incremental_projection_patches_for_command` returns
`None`, the fallback is (old `crdt_runtime.rs`):

```rust
let before_projection = self.projection.clone();   // full-document clone
self.refresh_projection()?;                        // document_from_loro: full re-materialization
                                                   // + ProjectionRuntimeIndex::from_projection
events.push(self.projection_change_event(&before_projection, invalidation)?);
                                                   // full-document diff (projection_patches_between)
```

For a debate-sized document (the import alone interprets ~200 MB of paragraph XML), a single
fallback costs a full projection clone + full Loro re-materialization + full index rebuild +
full diff — tens-to-hundreds of MB and hundreds of ms, **per edit batch**, on the CRDT
thread. 14.4 GB / ~100–200 events lands exactly in that range. This is precisely what the
`crdt-materializer-refactor` commits rewrote (stable-ID patches so the incremental path
stops bailing on row-index ambiguity).

Run B — despite *actively typing* — shows no CRDT thread in its top-5 (its busiest non-main
thread is hotpath's own sampler at 16 %). Consistent with B running the newer build where
edits stay on the incremental patch path (or, less likely, its CRDT thread staying just
below the 5-thread display cutoff).

**Verification available today, no code changes:** the old build already counts these
fallbacks — check the logs of the machine-A run for
`"Flowstate projection used a full rebuild fallback"` (a `tracing::warn!`), or query
`projection_fallback_stats()`. A high count on machine A and ~zero on machine B closes the
case.

## 7. Genuine shared bugs the profiles agree on (machine-sensitive costs)

These reproduce on **both** machines at the same per-frame rate, so they're app bugs, not
machine mysteries — but their *cost* is machine-dependent, adding variance:

1. **`app_settings::load_app_settings` — 13 047 calls (A) / 12 447 (B) ≈ 26 per frame.**
   Each call does `fs::read_to_string(settings_path())` **+ full TOML parse**
   (`app_settings/io.rs:1-16`). Dozens of disk reads + parses per rendered frame. On Linux
   this hides in the page cache; on Windows every read can pay NTFS + Defender tax. This is
   both wasted work everywhere and a machine-dependent noise source. Cache it (mtime check
   or a file watcher — `notify` is already a dependency).
2. **`Keymap::defaults` — 13 037 calls (A) / 12 435 (B), 15.3 KB fresh allocation each**
   (~190 MB per run on both machines), via `load_keymap()` falling back to
   `Keymap::defaults()` on every call because the settings keymap is empty. Same fix: cache.

The per-frame rate is identical across machines (26.1 vs 26.4 calls/frame) — a nice internal
consistency check that both binaries share this code path.

## 8. Why B's render path looks 2.4× slower: virtual-list estimation churn

`virtual_item_sizes` (`item_sizes.rs:315`) walks **every block in the document** on each
item-sizes rebuild; every paragraph whose chunk-layout cache entry is not `complete` gets a
`paragraph_remainder_estimate` call. So the cost per rebuild is
`O(paragraphs not fully laid out)` — state, not code.

- Run B: 2 090 350 estimates / 400 rebuilds ≈ **5 226 paragraphs estimated per rebuild**
  (matching the ~5 000-paragraph document scale in `latency_bench.rs`). At 1.8 µs each
  that's ~9.4 ms per rebuild — the bulk of B's 18.6 ms rebuild average.
- Typing makes it worse: `paragraph_estimated_total_height` caches per-paragraph estimates
  but keys them on `edit_generation`/`layout_generation` (`chunk_navigation.rs:96-113`), so
  **every keystroke invalidates every paragraph's cached estimate**, and B typed for the
  whole session (153 flushes → 400 rebuilds vs A's 291).
- The incremental escape hatch `try_patch_item_sizes_cache` requires a pending patch range
  and bails entirely when `document_has_object_blocks()` (`item_sizes.rs:107`) — so
  documents with any image/table/equation always take the full O(N) rebuild.

**And the key profiling trap:** `paragraph_remainder_estimate` is "missing" from Run A's
table, which made B look categorically different. It isn't. A's 291 rebuilds × ~5 000
paragraphs ≈ 1.5 M calls ≈ 2–3 s — right at Run A's top-15 cutoff (entry #15 is 2.05 s). The
function almost certainly ran millions of times on A too and was **truncated out of the
table**. The nested timings also double-count: B's 7.44 s `rebuild_…_with_prefetch` already
*contains* `virtual_item_sizes` (7.24 s), `paragraph_remainder_estimate` (3.77 s), and
`paragraph_estimated_total_height` (2.93 s) — these are one hotspot, not four.

Secondary inflation: those 2.09 M + 2.09 M + 1.0 M measured calls each pay hotpath's
per-call timing+alloc bookkeeping. Instrumentation overhead scales with call count, so the
run with more churn also gets measured as disproportionately slower.

## 9. Diagnosis summary

| Observation | Verdict |
|---|---|
| "Same function much more expensive on one machine" (render, rebuild_item_sizes) | **State-dependent work-per-call** (typing-driven estimate invalidation, viewport size, cache-completeness), amplified by different builds and workloads — not hardware |
| Run A 71 s vs Run B 36 s totals; `% Total` disagreements | Uncontrolled session length + idle fraction; `% Total` denominators aren't comparable |
| `paragraph_remainder_estimate` "only exists" on B | Top-15 truncation artifact |
| `shape_fragment` allocating 40× more per call on A | Different platform text engines (cosmic-text/fontconfig vs DirectWrite) + font availability |
| Run A's crdt thread at 99.8 % CPU / 14.4 GB | Pre-refactor materializer fallbacks doing full clone + re-materialize + diff per edit batch, in an **uninstrumented crate** |
| CPU baseline 107 µs vs 0 ns | Windows stub; baselines are never cross-platform comparable |
| `paint_line_text` 1.42×, paint 1.06×, import 1.03× | The *actual* machine/OS difference — modest and boring |

## 10. Recommendations

**Make runs comparable (protocol):**
1. Pin the commit — embed `git describe --always --dirty` in the hotpath label/report so a
   profile is never again compared across builds unknowingly.
2. Same document, same scripted interaction, fixed window size, fixed duration. Extend the
   existing `latency_bench.rs` pattern (scripted keystrokes on a 5 000-paragraph document)
   rather than profiling ad-hoc human sessions.
3. Export hotpath's **full JSON** instead of top-15 terminal tables, and diff runs with
   `hotpath-utils compare` — that kills the truncation artifact class entirely.
4. Compare per-call averages of leaf functions first; treat aggregate/parent functions and
   `% Total` as narrative, not evidence.
5. Set `HOTPATH_CPU_BASELINE_OFF` or ignore the baseline on Windows; it's a stub there.

**Close the observability hole:**
6. Add `hotpath` feature forwarding + `#[hotpath::measure]` coverage to **`flowstate-collab`**
   (`CrdtRuntime::apply_editor_transaction`, `refresh_projection`,
   `projection_patches_between`, `document_from_loro`, `projection_snapshot`). Today the
   single largest consumer in Run A is structurally invisible.
7. Track `projection_fallback_stats()` (it already exists) in the on-exit report/logs so
   full-rebuild storms are first-class metrics.

**Fix the shared waste (reduces both cost and cross-machine noise):**
8. Cache `load_app_settings`/`Keymap::defaults` (26 disk reads + TOML parses per frame).
9. Don't invalidate every paragraph's height estimate on every `edit_generation` bump —
   invalidate only the edited paragraph range; and lift the
   `document_has_object_blocks()` bail-out in `try_patch_item_sizes_cache` if object
   documents matter.
10. Confirm the materializer-refactor hypothesis empirically: run machine A's scenario on
    the `crdt-materializer-refactor` tip and watch the `flowstate-crdt-runtime` thread's
    CPU%/alloc in the threads table — it should collapse from ~80 % to near-idle.
