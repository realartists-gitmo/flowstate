# Handoff: Adjustment Plan Implementation Status

> Generated: 2026-06-18
> Source: `adjustmentplan.md` (1283 lines, the "Platonically ideal" production Loro-native architecture)
> Companion: `FIX_LORO_ROOT.md` (614 lines, execution plan for single-root LoroText performance; NOT yet implemented)

---

## Summary

The codebase has completed a **Loro-native foundation** (schema, projection, import/export, package format, table/equation/image objects, undo/redo, revision/restore). However, the **editor projection layer** (`gpui-flowtext::Document`) is still the old mutable document model, and the **performance infrastructure** identified in `FIX_LORO_ROOT.md` is **entirely unimplemented** — the file does not exist in the crate. Additionally, there is a **dual-runtime problem**: `CrdtRuntime` (flowstate-collab) is a clean implementation of the plan's runtime, but the app uses `CollabSession` (flowstate) which manages its own `LoroDoc` directly.

---

## Section-by-Section Status

### §1 Non-Negotiable Objective — **Mostly Complete**
- LoroDoc IS the canonical document substrate. ✓
- `.db8` extension preserved as brand identity, now uses Loro-native package format. ✓
- Object replacement character (`U+FFFC`) strategy locked in. ✓
- Tables fully in scope and implemented as structured Loro objects. ✓
- Named revisions implemented. ✓
- PDF source embeds Loro-native `.db8` bytes only. ✓
- **Gap**: The old GPTX (final-state) serializer still exists at `crates/gpui-flowtext/src/persistence/io.rs`. It uses magic `b"GPTX"`, version 6, and its own chunked binary format. The `reject_db8_path` function prevents it from handling `.db8` paths, but the format itself is still present and used by editor recovery, benchmarks, and some tests. The plan says "Code that exists only to read, write, recover, index, export, or interoperate with the old final-state `.db8` model must be removed or rewritten." The GPTX is a final-state serializer with a different extension, so it's technically outside the `.db8` prohibition — but it represents a parallel final-state persistence path.

### §1A Locked Clarifications — **Complete**
- `.db8` is Loro-native only. ✓
- No old `.db8` compatibility reader. ✓
- Rust `loro` crate API is authoritative. ✓
- `U+FFFC` object replacement character strategy used for block anchoring. ✓
- Tables are structured Loro objects. ✓
- Named revisions are time-travellable snapshot/frontier points. ✓
- PDF source embeds only new Loro-native `.db8`. ✓

### §2 Canonical Model — **Complete in intent, partial in practice**
- LoroDoc is the single canonical document model. ✓
- Everything else is derived. ✓
- The canonical pipeline (local command → resolve → Loro mutation → subscription → projection → render) is architecturally understood and partially implemented in `CrdtRuntime`. ✓
- **Gap**: The `CollabSession` in the app does NOT use `CrdtRuntime`. It owns its own `LoroDoc` and `UndoManager` directly. Editor commands flow through `apply_editor_semantic_command` (a free function), not through `CrdtRuntime::command`. The pipeline is less clean at the app level.

### §3 Runtime Ownership — **Incomplete**
- **Planned**: Dedicated CRDT runtime (thread/task/actor) owns LoroDoc; UI owns only projection.
- **Reality**: `CrdtRuntime` struct exists in `flowstate-collab::crdt_runtime` with the correct ownership model, but **it is not used by the application**. The app's `CollabSession` holds `doc: Option<LoroDoc>` directly on the UI thread. There is no thread boundary between CRDT mutation and UI. The `CrdtRuntime` is only exercised in unit tests.
- **Gap**: Dual runtime — `CrdtRuntime` (unused in production) vs `CollabSession` (production, directly manages LoroDoc). No thread isolation.

### §4 Command Layer — **Partially Complete**
- `SemanticCommand` enum exists with most planned operations (InsertText, DeleteRange, SplitParagraph, SetParagraphStyle, SetRunStyles, InsertImage, InsertEquation, InsertTable, OpenRevision, ForkRevision, Undo, Redo). ✓
- CRDT runtime resolves semantic commands into Loro schema mutations. ✓
- `apply_editor_semantic_command` free function maps `gpui-flowtext::SemanticEditCommand` to Loro mutations (used by CollabSession). ✓
- **Gap**: The old `CanonicalOperation` API still exists in `gpui-flowtext::collaboration` alongside `WireCanonicalOperation`, `encode_canonical_operations`, `decode_canonical_operations` — dead pre-CRDT code. `SemanticEditCommand` (editor's old command enum) coexists and is the primary input to `apply_editor_semantic_command`.

### §5 Projection And Editor Model — **Incomplete**
- **Planned**: Replace `gpui-flowtext::Document` entirely with `DocumentProjection`. No dual binding/reconciliation.
- **Reality**: `document_from_loro()` projects LoroDoc into `gpui-flowtext::Document` (the old editable document type). The editor (`RichTextEditor`) still uses `gpui-flowtext::Document` as its core model. There is no `DocumentProjection` type. The editor mutates via `SemanticEditCommand` which `apply_editor_semantic_command` translates to Loro mutations. This IS a binding/reconciliation layer.
- **Gap**: The editor model is NOT replaced. `gpui-flowtext::Document` remains the projection model. The target "Keeping a type named `Document` while redefining it as projection-only" IS what's happening, which the plan explicitly rejects.

### §6 Canonical Text Model — **Complete**
- Single root `LoroText` for body flow. ✓
- `\n` paragraph delimiters. ✓
- Sentinal newline at position 0. ✓
- Independent `LoroText` per cell flow for tables. ✓
- Image caption/alt text, equation source have independent flows. ✓

### §7 Paragraph Boundaries — **Complete**
- Literal `\n` characters inside flow `LoroText`. ✓
- Sentinal newline (non-renderable, non-deletable). ✓
- Predecessor-boundary model (paragraph style on preceding `\n`). ✓
- Paragraph identity metadata in `ParagraphMap` with start_cursor, boundary_cursor. ✓
- No custom paragraph marker characters. ✓
- Loro marks (`paragraph_style`) on boundary newlines with `ExpandType::None`. ✓
- The 10 word-processor semantics rules for paragraph split/join/style application are documented in the plan but NOT verified in code behavior.

### §8 Flow Structure — **Complete**
- `FlowMap` with id, kind, `text: LoroText`, `attrs: LoroMap`. ✓
- `flows_by_id: LoroMap` in root. ✓
- Flow kinds: body, table_cell, caption, alt_text, equation_source. ✓
- Object replacement characters for embedded objects where supported. ✓

### §9 Block And Object Ordering — **Complete**
- Body flow is canonical ordered sequence. ✓
- Object replacements (`U+FFFC`) in body text. ✓
- `blocks_by_id: LoroMap` in root with `BlockMap` per block. ✓
- `BlockMap` has id, kind, flow_id, anchor_cursor, attrs, nested_refs. ✓
- All durable IDs stored in Loro. ✓

### §10 Semantic Styling — **Complete**
- Loro marks for paragraph_style, run_semantic_style_id, highlight_style_id, direct_underline, strikethrough. ✓
- Semantic enum slots (not freeform values). ✓
- Style catalog is app/theme data only. ✓
- DOCX import maps to predefined slots. ✓
- Comments/annotations/bookmarks/suggestions are out of scope. ✓

### §11 Sections And Page Structure — **Not Started**
- `sections_by_id` map is initialized in schema (`loro_schema.rs:52`) but **never populated or used**. ✓
- No section creation, reading, or projection exists in the Loro-native code paths.
- Section data comes from the old `gpui-flowtext::Document` derived from paragraph styles (section heading/outline).
- **Gap**: Full section architecture is missing: `SectionMap` with start_cursor, page_size, margins, columns, header_flow_id, footer_flow_id, page_numbering, orientation.

### §12 Tables — **Complete**
- Structured Loro objects. ✓
- `row_order: LoroMovableList`, `column_order: LoroMovableList`. ✓
- `rows_by_id`, `columns_by_id`, `cells_by_id` as LoroMaps. ✓
- Cell contents are independent LoroText flows. ✓
- Nested tables supported via `nested_table.*` keys in cell map. ✓
- InsertTable command creates full table schema. ✓
- Round-trip through projection works with tests. ✓

### §13 Equations — **Complete**
- Equation source as independent LoroText flow. ✓
- EquationBlock in `blocks_by_id` with source_flow_id, attrs (syntax, display). ✓
- Render output is local only (not synced). ✓
- Inline and block equations both use `U+FFFC` placeholder. ✓

### §14 Images And Assets — **Complete**
- Image metadata in LoroMap (asset_id, content_hash, sizing, alignment, etc.). ✓
- Asset bytes outside Loro in content-addressed asset store. ✓
- BLAKE3 content addressing. ✓
- Caption and alt text are independent LoroText flows. ✓
- Missing assets render placeholder. ✓

### §15 Presence, Peers, And Author Metadata — **Complete**
- Loro `EphemeralStore` for presence. ✓
- PresenceState with name and selection. ✓
- Roster derivation from ephemeral state. ✓
- `users_by_id` and `replicas_by_id` maps in schema (initialized but may not be populated). ✓
- PeerId for replica identity, distinct from user identity. ✓

### §16 Selection, Cursors, And Affinity — **Not Started**
- **Planned**: Full `SelectionEndpoint` model with explicit affinity/gravity (before/after/neutral, visual_gravity).
- **Reality**: Loro cursors are used with basic `Side::Left`/`Side::Right` choices. No `SelectionEndpoint` type exists. No `affinity` or `visual_gravity` model. No `Selection` type with anchor/head/direction.
- **Gap**: The full affinity/gravity model is unimplemented.

### §17 Undo And Redo — **Complete**
- Loro `UndoManager` with per-peer exclusion prefix `"remote"`. ✓
- Merge interval (600ms in CrdtRuntime, 500ms in CollabSession). ✓
- `record_new_checkpoint` for command boundaries. ✓
- Semantic undo units correspond to grouped changes. ✓
- Time travel is separate from undo. ✓

### §18 Time Travel, Revisions, And Restore — **Complete**
- `PackageRevision` stored in package with frontier, title, summary, timestamp, author. ✓
- `create_named_revision`, `load_revision_loro_doc`, `fork_revision_runtime`. ✓
- `compact_to_snapshot` for compaction. ✓
- Named revisions preserved after compaction. ✓
- `OpenRevision`/`ForkRevision` commands in `CrdtRuntime`. ✓
- Tests exist for revision preservation through compaction. ✓
- **Note**: Compaction is manual (explicit API calls). No automatic threshold-based compaction.

### §19 Filesystem Package — **Complete**
- `DocumentPackage` with custom chunked binary container. ✓
- Magic `FLOWDB8-LORO\0\0\0\0`, header version 1. ✓
- Chunk types: manifest, Loro snapshot, Loro update segment, asset, revision index, projection cache, search unit. ✓
- Postcard serialization, BLAKE3 checksums. ✓
- Atomic write via temp file + rename. ✓
- `append_update_segment` for incremental persistence. ✓
- Validation: version checks, frontier chain integrity, checksum verification. ✓

### §20 Read Path — **Partially Complete**
- Open package → read manifest → load latest snapshot → apply update segments. ✓
- Construct LoroDoc in CRDT runtime. ✓
- Verify package format and schema version. ✓
- **Gap**: Projection cache and search cache are loaded but are never populated during normal writes. The `projection_cache` frontier is always `None`. Search cache is populated via `rebuild_search_units_from_loro` during snapshot operations. Fast unopened-file indexing via package caches is not fully wired up.

### §21 Write Path — **Partially Complete**
- Append-first persistence exists (update segments appended to package). ✓
- Update segment chains validated for frontier integrity. ✓
- **Gap**: No automatic snapshot compaction when update history crosses thresholds. The current save path writes the full package every time (`package.write(path)` is called from `save_package` and `persist_update_segment` in `CrdtRuntime`, but `CollabSession` does NOT use `CrdtRuntime` — it writes via `DocumentPackage::from_loro_snapshot_with_assets` in `from_local_document` and never persists update segments incrementally). The `CollabSession` path does NOT call `append_update_segment` at all. Only the standby `CrdtRuntime` does incremental persistence.
- **Gap**: No projection cache or search cache writing during save.

### §22 Sync And Anti-Entropy — **Complete**
- Remote updates imported directly into LoroDoc via `import_with(bytes, "remote")`. ✓
- `ImportStatus` checked for pending dependencies. ✓
- Immediate anti-entropy pull triggered on missing deps. ✓
- Periodic digest fallback (10s). ✓
- Direct connection protocol with snapshot/update/asset serving. ✓
- VersionVector-based digest comparison. ✓
- Blob transfer with DirectSessionHandler. ✓

### §23 Permanent Loro Subscription — **Partially Complete**
- Permanent subscriptions exist (`_root_subscription`, `_local_update_subscription` in `CrdtRuntime`; `loro_subscriptions` in `CollabSession`). ✓
- **Gap**: Subscriptions are essentially no-ops — they only do tracing. Projection invalidation is not driven by subscriptions. Instead, the `events_after_local_change` method rebuilds the projection explicitly after each command. The subscription-backed architecture is not the actual invalidation path.

### §24 Projection Invalidation — **Not Started**
- **Planned**: Option C — batched frontier-based invalidation with incremental fast paths and full rebuild fallback.
- **Reality**: Full projection rebuild (`document_from_loro`) on every change. No `ProjectionInvalidation` type. No incremental projection indexes. No dirty-range tracking. No fallback detection/logging. This is Option A (full rebuild on every event), which the plan rates 3/10 for performance and 8-9/10 for everything else.

### §25 Search — **Partially Complete**
- `rebuild_search_units_from_loro` exists in `DocumentPackage` and derives search units from Loro body flow. ✓
- Search units have cursors (paragraph_start_cursor, paragraph_end_cursor) instead of old paragraph indexes. ✓
- **Gap**: The tub (`flowstate-tub`) still uses `read_db8(path)` which reads the full new-format `.db8` file into a gpui-flowtext Document via the Loro→Document projection, then indexes sections from the old Document model. It does NOT use `DocumentPackage`'s search units directly. **Critical gap**: `db8_index_units` at `flowstate-tub/src/lib.rs:923` reads the entire Document to get section information, which doesn't exist as Loro-native sections.
- **Gap**: Tantivy index is still used for full-text search (acceptable per plan). Section-aware units use old paragraph indexes (`paragraph_start: Option<usize>` in `IndexUnit`), not Loro cursors.

### §26 DOCX, PDF, And Export — **Complete**
- DOCX import → Document → Loro-native `.db8`. ✓
- `convert_docx_to_db8` writes Loro-native format. ✓
- PDF generation includes Loro-native `.db8` source embedding (compressed, zstd). ✓
- PDF recovery extracts Loro-native `.db8` bytes only (magic `FSL8ZST\0`). ✓
- No dual old/new payload support in PDF. ✓
- Export generates from Document projection. ✓

### §27 Schema Versioning — **Complete**
- `MetaMap` with document_id, loro_schema_version, schema_features, created_by_app_version, last_written_by_app_version, created_at, modified_at. ✓
- `LORO_PACKAGE_FORMAT_VERSION = 1`, `LORO_SCHEMA_VERSION = 1`. ✓
- Version check on package open. ✓

### §28 Container References — **Complete**
- Durable Flowstate IDs (flow_id, block_id, paragraph_id) stored as strings in Loro maps. ✓
- Loro containers created via `ensure_mergeable_*` methods. ✓
- Container resolution centralized in schema module. ✓

### §29 Clear/Rebuild Paths — **Partially Complete**
- `replace_body_from_document` does full clear+repopulate of body text and all blocks/paragraphs. This IS used during DOCX import and initial document creation. ✓
- `clear_map` and `clear_list` used in import paths. ✓
- **Gap**: Normal editing should NOT use clear/repopulate, but currently the editor emits `ReplaceParagraphSpan`/`ReplaceDocument` which trigger full body rewrites. No incremental edit paths exist for non-typing operations.

### §30 Invariants — **Mostly Met**
1. Full reconstruction from Loro snapshot/update + assets: ✓ (load_loro_doc)
2. Caches are disposable: ✓ (explicitly stated)
3. Paragraph split and inline insert commute: ~ (works at Loro CRDT level, but editor emits ReplaceDocument which is unnecessarily destructive)
4. Undo/redo is per-peer: ✓
5. Time travel via frontiers: ✓
6. Revision restore opens dirty fork: ✓ (ForkRevision command)
7. Binary assets outside CRDT: ✓
8. Caption/alt text as CRDT flows: ✓
9. Equation source as CRDT flow: ✓
10. Semantic Loro marks: ✓
11. Paragraph style in boundary marks: ✓
12. Block metadata in Loro maps: ✓
13. Tables as structured CRDT: ✓
14. Rows/columns have durable identity: ✓
15. Remote updates imported directly: ✓
16. Local commands mutate through runtime: Partially — CollabSession does direct LoroDoc mutation
17. No canonical Flowstate doc separate from Loro: Partially — gpui-flowtext::Document still exists as editor model
18. No local-vs-remote split: Partially — CollabSession has this split in practice
19. Files are Loro-backed packages: ✓
20. Projection caches declare frontier: Not implemented (always None)
21. Search indexes declare frontier: Implemented in rebuild_search_units_from_loro
22. DOCX/PDF from snapshots: ✓
23. Comments/annotations/bookmarks/suggestions out of scope: ✓
24. General test suite out of scope: ✓

### §31 Data Shape Summary — **Complete with caveats**
All root containers exist in the schema:
- meta: ✓
- flows_by_id: ✓
- blocks_by_id: ✓
- paragraphs_by_id: ✓
- sections_by_id: **Empty/unused**
- assets_by_id: ✓ (populated during import)
- revisions: ✓ (populated)
- users_by_id: ✓ (schema only, not populated)
- replicas_by_id: ✓ (schema only, not populated)

### §32 Expected Result — **In Progress**
The architecture has one model (`LoroDoc + asset store + derived Flowstate projections`) at the crate boundary, but the app still uses a dual-model approach where `CollabSession` directly manages `LoroDoc` and the editor uses `gpui-flowtext::Document` as its live model.

---

## Cross-Cutting Issues

### Issue A: Dual CRDT Runtime (Critical)

`flowstate-collab::crdt_runtime::CrdtRuntime` is the adjustment plan's canonical runtime. It is complete, tested, and unused in production. `flowstate::collab::session::CollabSession` is the application's actual runtime, managing `LoroDoc` inline.

**Impact**: The `CollabSession` path:
- Does not use `CrdtRuntime::command()` for mutation dispatch
- Does not use `CrdtRuntime` for package persistence
- Manages its own `UndoManager` directly
- Has its own subscription setup
- Mixes CRDT mutation and UI rendering on the same thread
- Does not persist update segments incrementally (only full snapshot writes)

**The `CrdtRuntime` should be the canonical path** and `CollabSession` should delegate to it.

### Issue B: Performance Infrastructure (FIX_LORO_ROOT.md) — **Not Started**

`FIX_LORO_ROOT.md` details 15 tasks (T1–T15) across 4 pillars:
1. **Foundation** — Maintained paragraph offset index (T1)
2. **Incremental local apply** — Span-scoped body splices (T2, T3)
3. **Delta-driven remote reconcile** — Use Loro TextDelta instead of full reproject (T4)
4. **Editor-model incrementalization** — O(log n) offset/section/identity/structural patches (T6–T11)
5. **Cleanup** — Dead code removal, style single-truth, doc cleanup (T12–T15)

**None of these tasks have been implemented.** The files `local_apply.rs`, `remote_apply.rs`, `binding.rs`, `patch_apply.rs`, `body_index.rs` do not exist in any crate. Every non-typing edit causes full-document rewrites.

### Issue C: Section Architecture — **Not Started**

`Sections by ID` map is initialized but never populated. No section creation or querying logic exists in the Loro-native code. Sections are derived from old Document model paragraph style → section kind mapping.

### Issue D: Selection Model — **Not Started**

The full affinity/gravity selection model from §16 is unimplemented. Only basic Loro cursor `Side` is used.

### Issue E: Projection Invalidation — **Not Started**

Full projection rebuild on every change. No incremental invalidation system exists.

### Issue F: Old GPTX Serializer Still Present

The old final-state serializer (`gpui-flowtext::persistence::io`) with magic `GPTX`, version 6, chunk types for text/assets/blocks/IDs/sections still exists. It is used by:
- Editor recovery path
- Benchmarks
- Internal tests
- Editor's `write_document_export` for `DocumentExportFormat::Native`

The plan says old final-state format must go, but this is the `.gptx` extension (not `.db8`). It's debatable whether this counts as the "old .db8 format" — it uses a different extension. However, it IS a final-state serializer that should be removed per the architecture.

### Issue G: Unused `CrdtRuntime` in Production

`CrdtRuntime` is only exercised by unit tests in `crdt_runtime.rs`. The production `CollabSession` has its own parallel implementation. This means the plan's clean runtime architecture is not used in production.

---

## Priority Summary

| Priority | Area | Status | Effort |
|----------|------|--------|--------|
| **P0** | Adopt `CrdtRuntime` in `CollabSession`, eliminate dual runtime | Not Started | Medium |
| **P0** | Implement T1 (maintained paragraph offset index) from FIX_LORO_ROOT | Not Started | Medium |
| **P0** | Implement T2 (incremental ReplaceParagraphSpan) from FIX_LORO_ROOT | Not Started | Large |
| **P0** | Implement T4 (delta-driven remote reconcile) from FIX_LORO_ROOT | Not Started | Large |
| **P1** | Implement §16 selection model (affinity/gravity) | Not Started | Medium |
| **P1** | Implement §11 section architecture (sections_by_id population) | Not Started | Medium |
| **P1** | Implement §24 projection invalidation (Option C) | Not Started | Large |
| **P2** | Remove old GPTX serializer | Not Started | Small |
| **P2** | Remove dead pre-CRDT code (WireCanonicalOperation, etc.) | Not Started | Small |
| **P2** | Implement T6-T11 (editor-model incrementalization) from FIX_LORO_ROOT | Not Started | Medium |
| **P2** | Wire up projection cache and search cache in write path | Not Started | Small |
| **P2** | Automatic snapshot compaction with thresholds | Not Started | Small |
| **P2** | Port tub search to use Loro-native search units directly | Not Started | Small |
| **P3** | Implement T5 (granular local op emission for delete/Enter) | Not Started | Small |
| **P3** | T12-T15 cleanup tasks from FIX_LORO_ROOT | Not Started | Small |
| **P3** | Populate users_by_id and replicas_by_id | Not Started | Small |
