Implementation details:
- This app is still in dev. Do not worry about 'legacy,' 'fallbacks,' or 'backwards compatibility'---we strive for a single source of truth unconditionally.
- Performance decrease, latency increase, responsiveness decrease---all of these are unconditionally prohibited---we must NEVER compromise.
- A 'minimum viable prototype,' or 'first foundation' is not permitted. We need production-ready, idiomatic, non-hacked Rust infrastructure. It needs to be fully wired and implemented.

Verdict

  The CRDT-native direction is correct, but this implementation is seriously flawed.

  It did not simplify the system into one authoritative live model. It created:

  1. A durable Loro source.
  2. A complete Document projection owned by Db8DocumentController.
  3. Another complete Document projection owned by RichTextEditor.
  4. Patch-generation and reconciliation machinery trying to keep both projections synchronized.

  That replaces the old authority conflict with a complicated, synchronous cache-coherency problem. The new paragraph-break crash proves these mirrors already diverge during ordinary local editing.

  The regression is caused by the refactor design, not by CRDTs or Iroh inherently.

  ## Critical Findings

  ### 1. Paragraph breaks corrupt the editor projection
  **Status: FULLY RESOLVED.** Multi-impact and structural projection changes are no longer merged using invalid range arithmetic. Structural edits materialize an atomic projection from the committed CRDT source, including paragraph split, join, and undo operations.

  The panic occurs while applying a projection patch:

  > byte offset is 25, but the editor document text length is 1.

  The controller computes patches against its private projection, then the editor applies those ranges against its own projection without validating that both projections share the expected frontier/state.

  crates/gpui-flowtext/src/rich_text/editor/authoritative_projection.rs:218 directly deletes and inserts using controller-provided byte ranges.

  crates/flowstate-document/src/controller.rs:1290 merges multiple independently computed impacts by taking the minimum and maximum ranges. This is not generally valid: combining non-contiguous patches into one
  large replacement requires generating replacement content covering the entire merged range. The code instead derives content from the resulting projection ranges, whose relationship to the merged pre-edit
  range is unreliable.

  Structural edits such as splits and joins are exactly where these range assumptions fail. The panic has nothing to do with peers.

  ### 2. Local input is synchronously routed through the entire CRDT pipeline
  **Status: PARTIALLY RESOLVED.** The local path now performs bounded source-first edits and incremental projection updates, and expensive silent recovery behavior was removed. CRDT mutation and projection publication are still synchronous before the editor displays the result, so end-to-end latency work remains.

  Every keystroke performs the following on the UI thread:

  - Build typed operations.
  - Borrow the authority mutably.
  - Resolve Loro anchors.
  - Apply and commit Loro operations.
  - Summarize changes.
  - Materialize Loro windows.
  - Rebuild the controller projection.
  - Copy replacement paragraphs, blocks, IDs, and text.
  - Apply the same patch to the editor projection.
  - Reconcile identities.
  - Invalidate layout.
  - Trigger recovery/autosave observers.

  The synchronous authority call is at crates/gpui-flowtext/src/rich_text/editor/authoritative_projection.rs:279.

  This guarantees local latency even with zero peers. Networking is not involved.

  ### 3. “CRDT-only” still maintains competing mutable models
  **Status: PARTIALLY RESOLVED.** Loro is now the sole mutable authority and projection-only mutation/fallback paths have been removed from authoritative editing. The controller and editor still retain separate projection instances that deterministically derive from the same CRDT source.

  Db8DocumentController stores both:

  - source: FlowDocument
  - projection: Document

  The editor independently stores another Document.

  crates/flowstate-document/src/controller.rs:292 constructs and retains the controller projection. The editor then independently patches its own copy.

  Although Loro is nominally authoritative, two mutable projections are used operationally. Correctness now depends on every patch being perfectly derived and applied twice. That is effectively another dual-
  authority problem, just under a different name.

  ### 4. Editing is intentionally disabled while constructing the CRDT
  **Status: FULLY RESOLVED.** Normal document opening no longer installs `PreparingDb8EditorAuthority`; the real authority is constructed before the editor is exposed for editing.

  The waiting period is explicitly designed into panel creation:

  crates/flowstate/src/workspace/workspace/documents.rs:1162 installs a PreparingDb8EditorAuthority and makes the editor source-read-only until background initialization finishes.

  For documents without snapshots, initialization serializes the existing document, creates a seed, builds the entire Loro document, validates it, builds token cursors, and constructs projection indexes.

  crates/flowstate-document/src/controller.rs:263

  This is unnecessary for ordinary local editing. A local document should either already have its authority available when presented or support immediate local operations while initialization finishes.

  ### 5. New documents take the worst initialization path
  **Status: PARTIALLY RESOLVED.** New/opened documents no longer expose a read-only editor while authority construction runs. Seed construction and initial materialization still perform avoidable work for large documents.

  When no CRDT snapshot exists, the entire projection is reconstructed into Loro using per-document seeding. This delays editing and temporarily holds several complete representations of the document.

  The comments themselves describe it as a “wasteful” path:

  crates/flowstate/src/workspace/workspace/documents.rs:1145

  This explains why older or legacy documents feel especially slow.

  ## Performance And Memory Findings

  ### 7. Full source validation can happen during local edits
  **Status: PARTIALLY RESOLVED.** Ordinary supported local edits use the bounded/prevalidated source-first transaction path. Compound and exceptional transaction paths can still perform document-wide validation during interactive use.

  Only bounded single-edit transactions use transact_prevalidated.

  Other edit batches fork the entire Loro document, perform the mutation on the fork, validate the entire schema, export the update for size checking, then perform the mutation again on the real document:

  crates/flowstate-collab/src/flow_document/mod.rs:674

  This means compound local actions may:

  - Clone/fork the CRDT.
  - Apply operations twice.
  - Fully validate the document.
  - Export updates twice.

  That is an extreme amount of work for local input.

  ### 8. Window materialization still performs substantial reparsing
  **Status: PARTIALLY RESOLVED.** Projection updates use bounded materialized windows, and multi-impact structural changes use a deliberate atomic materialization strategy for correctness. Window parsing and temporary replacement construction remain optimization targets.

  Incremental projection uses Loro text slices, converts them into strings and marks, scans them for structural tokens, reads records, and recreates projected paragraphs.

  crates/flowstate-collab/src/flow_document/schema/parsing.rs:125

  Then document_from_paragraphs creates another temporary document just to obtain replacement paragraphs and text:

  crates/flowstate-document/src/controller.rs:1368

  ### 9. Multiple indexes are rebuilt after every edit
  **Status: PARTIALLY RESOLVED.** Projection work is bounded for ordinary edits and structurally unsafe patch merging is gone. The controller projection index now updates only affected windows for shape-preserving edits and rebuilds only when structural cardinality changes make positional reindexing necessary. Editor offset, section, and identity maintenance still require further consolidation.

  The controller rebuilds its projection index after mutations. Projection patching rebuilds document offsets and sections. The editor then rebuilds its own offset index and reconciles its identity map.

  Examples:

  - crates/flowstate-document/src/controller.rs:747
  - crates/flowstate-document/src/controller.rs:1470
  - crates/gpui-flowtext/src/rich_text/editor/authoritative_projection.rs:244
  - crates/gpui-flowtext/src/rich_text/editor/authoritative_projection.rs:200

  The patch is incremental in name, but much of the surrounding maintenance remains document-wide.

  ### 10. Linear scans remain in hot paths
  **Status: PARTIALLY RESOLVED.** Hydration uses the projection index for paragraph/block lookup and no longer rebuilds the index when IDs and ordering are unchanged. Rich-object child ownership is indexed instead of recursively scanned. A post-root-patch touched-paragraph membership check remains linear because it must classify against the newly mutated projection rather than the pre-edit index.

  Examples include:

  - Finding paragraph IDs with .position().
  - Checking touched paragraph IDs using vector .contains().
  - Finding block IDs using repeated .position().

  crates/flowstate-document/src/controller.rs:1253

  These turn modest edits into O(document size × touched nodes) behavior.

  ### 11. Recovery snapshots duplicate the complete state
  **Status: FULLY RESOLVED.** Authoritative recovery writes no longer clone the complete editor `Document` or embed retained outbox updates already represented by the complete Loro snapshot. The UI path captures a causally fixed Loro fork and immutable projection handle; Loro snapshot export, projection normalization, validation, cache serialization, asset-manifest construction, and native envelope encoding all run on the background executor.

  After edits, recovery scheduling eventually:

  - Exports a complete Loro snapshot.
  - Reads all retained outbox updates.
  - Builds native file bytes.
  - Clones the complete editor Document.

  crates/gpui-flowtext/src/rich_text/editor/recovery.rs:181

  This substantially increases peak memory. Because the snapshot call occurs while borrowing the authority through the editor update path, snapshot generation can also block the UI.

  ### 12. Autosave may run after each observed edit generation
  **Status: FULLY RESOLVED.** Document autosave now coalesces sustained editing by generation, waits for an idle interval, prevents overlapping saves, and schedules a later generation when edits arrive during an active save.

  The editor observer invokes maybe_autosave_document after notifications, and autosave immediately starts a save for every unseen generation.

  crates/flowstate/src/workspace/workspace/documents.rs:1197
  crates/flowstate/src/workspace/workspace/documents.rs:1948

  There is no editing debounce in this path. Depending on save duration, this can continuously serialize snapshots while typing.

  ### 13. Patch responses still copy substantial data
  **Status: PARTIALLY RESOLVED.** Ordinary edits emit bounded replacement patches, while structurally complex/multi-impact commits deliberately publish an atomic full projection to preserve correctness. Large edits can still perform substantial copying and allocation.

  Even incremental updates copy replacement blocks, paragraphs, IDs, and text from the controller projection, then copy/splice them into the editor projection.

  crates/flowstate-document/src/controller.rs:748

  Large joins, deletes, pastes, or merged impacts can effectively copy much of the document.

  ## Correctness And Design Findings

  ### 14. Incremental failures silently fall back to full materialization
  **Status: FULLY RESOLVED.** Incremental errors now propagate as hard failures. Full materialization occurs only as an explicit strategy for structural-shape or multi-impact commits, not as an error fallback.

  Any incremental projection error triggers complete source materialization:

  crates/flowstate-document/src/controller.rs:1203

  This hides correctness defects and converts them into unpredictable latency and memory spikes. A failed optimization path should not silently become the normal recovery mechanism during local typing.

  ### 15. Patch application has no version/frontier precondition
  **Status: FULLY RESOLVED.** Patch generation/application is atomic for unsafe structural and multi-impact changes, explicit no-op patches are supported, and every patch carries the exact expected pre-edit block count, paragraph count, and text byte length. An incompatible stale or reordered patch is rejected before any editor projection mutation.

  ProjectionPatch contains replacement ranges and content, but does not identify the exact editor projection version it expects.

  There is no check such as:

  - expected projection generation
  - expected source frontier
  - expected text length
  - hash of replaced content

  Therefore stale, duplicated, reordered, or incorrectly merged patches can be applied to incompatible state and panic.

  ### 16. Local authority failures are converted into broad recovery responses
  **Status: FULLY RESOLVED.** Authority and projection invariant failures now propagate explicitly as no-op error responses instead of silently replacing the editor with a cloned full projection.

  Most authority errors are transformed into a response containing a cloned full projection:

  crates/flowstate-document/src/controller/editor_authority.rs:344

  This masks root causes, causes expensive replacement behavior, and lets the application continue after serious invariant failures.

  ### 17. Unsupported operations can still fall back toward legacy mutation paths
  **Status: FULLY RESOLVED.** Authoritative editing no longer permits unsupported operations to mutate the editor projection independently; missing source-first support is treated as a real error.

  Many editor commands first try the source-first path, then fall back to projection-first editing when it returns false.

  For example, paragraph insertion falls back at crates/gpui-flowtext/src/rich_text/editor/commands.rs:408.

  This is dangerous during a CRDT-only transition. If the authoritative controller exists, unsupported source operations should fail explicitly, not mutate the projection independently.

  ### 18. Authority installation clears editor undo/redo state
  **Status: FULLY RESOLVED.** Normal editing no longer races asynchronous authority installation, and undo/redo ownership is consolidated in Loro. Undo-generated inverse operations are explicitly committed and replicated.

  Installing or replacing the authoritative controller clears editor undo and redo:

  crates/gpui-flowtext/src/rich_text/editor/authoritative_projection.rs:151

  Because authority construction is asynchronous, opening a document and editing around initialization can create confusing history behavior. More broadly, undo ownership is split between old editor history and
  Loro history.

  ### 19. Local and remote operations use the same expensive projection machinery
  **Status: PARTIALLY RESOLVED.** Local and remote operations now share one authoritative CRDT semantics and bounded incremental projection where safe. Retained causal updates replay as one validated source batch and one projection publication, regardless of journal size. The interactive local path still awaits CRDT commit and projection publication before displaying the edit.

  The design does not sufficiently distinguish:

  - Fast, optimistic local input
  - Asynchronous durable CRDT commit
  - Remote reconciliation
  - Recovery/resync

  Treating all four as authoritative reprojection events makes the common local path pay remote-synchronization costs.

  The earlier design mutated the editor’s local document immediately and used the CRDT mainly for synchronization. That was fast because local input stayed local, but desynchronized because the editor document
  and CRDT could independently evolve.

  The new version correctly tries to make Loro authoritative, but routes every local edit through durable source mutation and synchronous reprojection before showing the result. It therefore pays for:

  - CRDT transaction processing
  - validation
  - update export
  - projection materialization
  - duplicate patching

  before completing a local keypress.

  The correct solution is not to restore the old competing authorities. It is to make the editor projection a tightly versioned, optimistic view of the authoritative CRDT, with one projection owner and
  asynchronous durability/network publication.

  ## Recommended Direction
  1. Make one component own the live projection. The editor and controller must not independently mutate separate Document instances.
     **Status: PARTIALLY RESOLVED.** There is one mutable authority, but two derived projection instances remain.
  2. Apply local edits optimistically to that projection immediately.
     **Status: OPEN.** Local edits remain source-first and synchronous before projection publication.
  8. Replace whole-document index rebuilds and linear ID searches with incrementally maintained maps.
     **Status: PARTIALLY RESOLVED.** Incremental projection indexing exists, but remaining rebuilds and scans require removal.
