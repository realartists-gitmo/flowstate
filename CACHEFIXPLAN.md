# Cache Refactor Plan

## Verdict

The prior plan is directionally correct: keep `Window`, `App`, GPUI text shaping, `ShapedLine`, `LayoutState`, and all `Rc`-backed layout caches on the UI thread, and move only pure `Send + 'static` preparation work to GPUI background tasks.

The plan needed a tighter boundary. In the current code, exact wrapping is not a pure operation: `wrap_lines_limited` calls `measure_line_width`, which calls `shape_fragment_cached`, which calls `window.text_system().shape_line`. That means a background phase can prepare text, runs, visibility projection, wrap candidates, and work queues, but it cannot decide exact line breaks unless the refactor also adds a UI-produced numeric measurement summary that background code can consume.

The ambitious target should therefore be a staged pipeline:

1. Move all paragraph snapshot/prep work off the UI thread.
2. Reuse UI-thread shaping and line-width measurements across chunks instead of throwing them away per chunk.
3. Optionally add a UI measurement-summary stage so background code can compute exact line plans while the UI thread only performs bounded shaping batches and final paint shaping.

## Current Facts

- `schedule_chunk_prefetch` and `run_chunk_prefetch_budget` in `src/rich_text_element/editor/chunk_prefetch.rs` are UI-thread budget loops.
- `materialize_visible_remainders_for_scroll` in `src/rich_text_element/editor/chunk_materialization.rs` does foreground catch-up work under an 8 ms budget.
- `ensure_current_chunk_cache_entry` already computes `paragraph_text` and `wrap_break_ends`, but it does so synchronously on the UI thread and stores them as `Rc`, so those results cannot be produced directly by a background task.
- `build_paragraph_chunk_layout_with_visibility` eventually calls `wrap_lines_limited`, and exact wrapping performs GPUI shaping through `Window`.
- `ParagraphChunkLayoutCacheEntry` stores `Rc<LayoutState>`, `Rc<str>`, and `Rc<Vec<usize>>`; it is UI-thread state.
- `FragmentShapeCache` is local to a single chunk build, so the editor may re-shape or re-measure the same paragraph fragments across adjacent chunks.
- `edit_generation` tracks document mutations, but theme changes, width changes, and invisibility toggles can invalidate layout without changing document content. Separate pure paragraph prep from width/theme-dependent layout work so useful text prep survives resize and theme-only invalidations.
- GPUI 0.2.2 supports `cx.background_spawn(future)`, `cx.background_executor().spawn(future)`, and `App::background_spawn(future)` for `Future + Send + 'static` with `Send + 'static` output. There is no `Window::background_spawn`; `Window` foreground handoff is `Window::spawn`.
- `App`, `Window`, and `Context` are `!Send`/`!Sync`; `BackgroundExecutor` is the background handle. To update editor state after a background result, use the existing `cx.spawn(async move |editor, cx| { ... background await ... editor.update(...) ... })` pattern used by recovery and file search.
- Dropping a `Task` cancels it, while `detach` lets it run without a result. Long synchronous CPU work inside a background future will still run to completion once polled, so stale-result validation and bounded batches are required even when task handles are retained.

## Non-Negotiables

- Do not move `Window`, `App`, `Context`, `LayoutState`, `ShapedLine`, `LaidOutLine`, `LaidOutSegment`, `Rc`, or GPUI text-system calls to a background task.
- Do not invent GPUI APIs. Verify any unfamiliar GPUI/Rust API with `rustdoc-inspector`.
- Do not add Rayon initially. GPUI's background executor is enough for a first refactor. Rayon only fits later if profiling shows a pure CPU stage that benefits from internal parallelism and does not compete with UI responsiveness.
- Do not add dependencies by editing `Cargo.toml` manually. Use cargo commands if a dependency becomes justified.
- Preserve the existing UI-thread budgets as safety rails. The refactor should reduce what those budgets have to do, not remove them.
- Preserve scroll-anchor restoration, visible chunk anchoring, cache invalidation, and invisibility-mode semantics.
- Prefer the existing `gpui-component` VirtualList behavior; this is an editor/layout pipeline problem, not a new component problem.

## Data Model Changes

Add a dedicated pure-prep cache and separate it from width/theme-dependent layout work:

```rust
struct ParagraphPrepKey {
  paragraph_key: ParagraphCacheKey,
  invisibility_mode: bool,
  edit_generation: u64,
}

struct ParagraphLayoutWorkKey {
  prep_key: ParagraphPrepKey,
  width: Pixels,
  layout_generation: u64,
}

struct ParagraphPrep {
  key: ParagraphPrepKey,
  paragraph_ix: usize,
  paragraph_text: Arc<str>,
  projected_runs: Arc<[TextRun]>,
  source_len: usize,
  wrap_break_ends: Arc<[usize]>,
  visible: bool,
}
```

Exact field names can change, but the separation should not: background prep stores only owned, immutable, `Send + Sync` data and is not width/theme-dependent. UI layout work uses `ParagraphLayoutWorkKey` and can convert `Arc` to `Rc` or borrow from the prep when building `ParagraphChunkLayoutCacheEntry`.

Add editor state for scheduling and validation:

- `layout_generation: u64`: increment on layout invalidation caused by theme changes, structural-block changes, and other layout-affecting resets. Do not increment it merely because a new chunk was materialized.
- `paragraph_prep_cache: Vec<ParagraphPrepSlot>` or equivalent mode-keyed slots so normal and invisibility-mode prep can coexist.
- `pending_layout_prep_task: Option<Task<()>>`
- `pending_layout_prep_request: Option<LayoutPrepRequest>` for coalescing newest/highest-priority work.
- Optional later: `paragraph_shaping_cache: Vec<Option<ParagraphShapingCacheEntry>>` for UI-thread `ShapedLine` and line-width reuse.

`ParagraphCacheKey` alone is not enough for background result validation because it intentionally tracks paragraph style/version and relies on paragraph versions being bumped correctly. Result envelopes should include `edit_generation` to reject stale snapshot results conservatively. UI layout caches should additionally include `layout_generation` or an explicit theme/layout key.

## Stage 1: Background Paragraph Prep

Create `src/rich_text_element/layout/prep.rs` and `src/rich_text_element/editor/layout_prep.rs`.

Background prep should:

- Clone a minimal snapshot on the UI thread: rope, `Arc<Vec<Paragraph>>`, relevant paragraph/block identity data, `edit_generation`, `invisibility_mode`, and requested paragraph range/priority list.
- Compute paragraph text from the rope off-thread.
- Compute `wrap_break_ends` off-thread.
- Compute invisibility projection off-thread for normal paragraphs when invisibility mode is enabled, including projected text and projected runs. This avoids building a projected `Document` on the UI thread for every chunk.
- Return a bounded batch of results. Bound by paragraph count and total text bytes so stale work cannot monopolize a background worker.

UI installation should:

- Validate every result against current `edit_generation`, `invisibility_mode`, paragraph count, and current `ParagraphCacheKey`.
- Drop stale results silently.
- Install prep results without bumping `paragraph_height_cache_revision`.
- Notify/schedule one UI-frame materialization pass when newly installed prep can advance existing prefetch work.

`ensure_current_chunk_cache_entry` should prefer installed prep. If prep is missing for a visible, immediately needed paragraph, it may still compute synchronously as a fallback so rendering never waits on background work.

## Stage 2: Scheduling Integration

Rework `schedule_chunk_prefetch` so it creates two queues:

- a background prep queue for paragraphs that need pure prep;
- a UI materialization queue for paragraphs whose prep is ready or whose chunk is urgently visible.

Priority order:

1. Current visible range and active caret/head paragraph.
2. Scroll foreground overscan.
3. Predicted visible height range.
4. Small trailing buffer after the predicted range.

Keep typing and interaction suppression for non-urgent work:

- If `recently_typed()` or `is_interacting()`, cancel/coalesce speculative background prep and clear speculative UI materialization.
- Visible/caret-critical work can still run synchronously under the existing foreground paths.
- After the typing suppression window, resume by scheduling prep first, then UI materialization on the next frame.

`materialize_visible_remainders_for_scroll` should request high-priority prep for visible remainders, but it must not await it. If visible work is missing prep, it can use the synchronous fallback under the existing 8 ms budget.

## Stage 3: UI Shaping Cache

The biggest remaining UI cost is exact wrapping and shaping. Add a UI-only cache before trying more background work:

```rust
struct ParagraphShapingCacheEntry {
  key: ParagraphLayoutWorkKey,
  fragment_shapes: FragmentShapeCache,
}
```

Refactor `FragmentShapeCache` so adjacent chunk builds for the same paragraph can reuse shaped fragments and measured line widths. It must remain UI-thread-only because it stores `ShapedLine`.

This should reduce repeated work in:

- `first_break_over_width`
- `first_overflow_line_end`
- `measure_line_width`
- final `shape_line`

Evict this cache with the same visible/offscreen policy as `paragraph_chunk_layout_cache`, and clear it on any key mismatch.

## Stage 4: Ambitious Exact Line Planning

If Stage 1-3 still leave UI-thread shaping as the bottleneck, add a two-step measurement-summary pipeline:

1. UI measurement batch: shape each effective run once and extract pure numeric metrics needed for current wrapping semantics: x positions at relevant break/char boundaries, ascent, descent, and format-derived padding. Keep `ShapedLine` on the UI thread; only numeric summaries may be sent to or stored for background use.
2. Background line planner: use those numeric summaries to compute exact line starts/ends, widths, line heights, chunk boundaries, and completion flags.
3. UI layout builder: consume `ParagraphLinePlan` and perform only final line-fragment shaping/decoration needed for `LayoutState` and painting.

This stage is more work, but it is the path to moving exact wrap decisions off-thread without violating GPUI constraints.

Important correctness note: current measurement semantics shape whole runs for width measurement and use `x_for_index` over subranges. The numeric summary must match that behavior exactly before it is allowed to replace `measure_line_width`.

## Invalidation Rules

Every invalidation path must invalidate or key out the relevant background prep and UI layout work:

- `invalidate_document_layout_caches`
- `invalidate_stale_paragraph_layout_caches`
- `invalidate_paragraph_layout_cache_range`
- `set_invisibility_mode`
- `update_document_theme`
- structural block mutations in media/object/table/equation paths
- document load/reset/dispose

On document-content or invisibility invalidation:

- clear or key out affected pure prep caches;
- clear shaping caches;
- drop `pending_layout_prep_task` if cancellation is desired;
- clear pending prep/materialization queues.

On theme, width, or structural layout invalidation:

- increment `layout_generation`;
- keep pure prep if the paragraph text/runs/visibility projection are still valid;
- clear width/theme-dependent shaping and layout caches;
- clear pending materialization queues whose `ParagraphLayoutWorkKey` no longer matches.

On range invalidation:

- clear or key out pure prep only for the expanded range when document content changed;
- increment `layout_generation` when the invalidation is layout-affecting;
- clear shaping caches for the expanded range;
- merge with `pending_item_sizes_patch_range` as current code does.

Do not use `paragraph_height_cache_revision` as the stale-result key. It changes when chunks are materialized and item sizes are rebuilt, which would make useful prep results expire for the wrong reason.

## Implementation Order

1. Add `layout_generation` and propagate it through layout-affecting invalidation paths.
2. Add background prep data types and a pure function that builds prep for a snapshot. Unit-test this function without GPUI.
3. Add scheduler state and result installation with strict validation.
4. Teach `ensure_current_chunk_cache_entry` to consume prep and fall back synchronously if needed.
5. Wire prep scheduling into `schedule_chunk_prefetch` and visible-remainder materialization.
6. Add UI-only paragraph shaping cache and refactor `FragmentShapeCache` ownership.
7. Add instrumentation and benchmark reporting for prep hits, stale drops, UI chunk build time, and foreground budget overruns.
8. Consider the measurement-summary line planner only after profiling Stage 1-3.

## Tests And Verification

Add focused tests where possible:

- prep result installation rejects stale `edit_generation` and invisibility mode;
- layout work rejects stale `layout_generation` and width;
- invisible-mode projection matches existing layout behavior for visible and hidden runs;
- empty paragraphs still produce a renderable exact row;
- Unicode char boundaries are preserved for `start_byte`, wrap breaks, and chunk ends;
- theme changes invalidate layout work and UI shaping even when `edit_generation` is unchanged, while preserving pure prep when possible;
- range invalidation clears only affected prep when using range-local invalidation;
- scroll-anchor restoration still holds when background prep lands between item-size rebuilds.

Update benchmark output to include:

- prep cache hit/miss/stale counts;
- background prep batch duration and paragraph/text-byte counts;
- UI chunk materialization duration;
- number of `shape_line` calls and `measure_line_width` calls if lightweight counters can be added without distorting timings;
- foreground budget overrun count for prefetch and scroll materialization.

Final validation should follow the repo instruction: run `cargo clippy` after edits are complete, fix significant errors/warnings, and only then return.

## Rayon Note

Rayon is not part of the first implementation. The background prep stage is naturally batchable through GPUI's background executor, and adding another CPU scheduler risks competing with UI responsiveness. Rayon becomes reasonable only if the pure prep or line-planning stage is proven CPU-bound, independent per paragraph, and large enough to benefit from work-stealing. If that happens, add it with cargo, cap parallelism, and keep all GPUI-facing work outside Rayon jobs.
