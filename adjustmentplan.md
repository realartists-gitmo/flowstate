# Flowstate Loro-Native Document Architecture Specification

## 1. Non-Negotiable Objective

Flowstate's document architecture must be rebuilt around Loro as the canonical document substrate.

The target architecture is not "a Flowstate document mirrored into Loro." It is "a Loro document rendered and edited by Flowstate."

Do not optimize this plan for implementation lift, prototype speed, or backwards compatibility with the current development-only `.db8` format. The target is the production architecture Flowstate should actually keep. The ABSOLUTE PLATONIC IDEAL.

The renderer, layout engine, UI command layer, DOCX/PDF/export tools, search tools, and caches may remain Flowstate-specific. The document state, durable history, undo/redo basis, collaboration state, revision timeline, and save format must be Loro-native.

The goal is a single architecture that supports:

* local editing
* collaborative editing
* durable operation history
* time travel
* branch/fork restore flows
* per-peer undo/redo
* CRDT-safe concurrent editing
* structured rich documents
* filesystem persistence
* DOCX import and export compatibility
* PDF/export generation from snapshots
* deterministic rendering from canonical state
* within-document and tub search from Loro-derived projections

The old development `.db8` document format is not a compatibility target. DOCX is still a first-class external import/export format.

## 1A. Locked Clarifications Before Implementation

These decisions are part of the architecture, not optional implementation notes.

The user-facing Flowstate document extension remains `.db8`. That extension is brand identity only. It does not imply compatibility with the old development `.db8` serializer, old `gpui-flowtext::Document` persistence, or any previous final-state native binary format. From this architecture onward, a `.db8` file is a Loro-native Flowstate package as specified in this document.

There is no old `.db8` importer, compatibility reader, migration path, fallback decoder, or dual-format open path. The previous development `.db8` format should be treated as if it never existed. Code that exists only to read, write, recover, index, export, or interoperate with the old final-state `.db8` model must be removed or rewritten onto the Loro-native package model.

The implementation must use the Rust `loro` crate APIs as authoritative. `loroapi.md` may be used as conceptual API background, but it documents the JavaScript/TypeScript binding and must not be treated as proof that a Rust API with the same name or capability exists. Before relying on a Loro feature, verify it against the Rust crate in use.

Rust Loro currently exposes rich text as text plus marks/cursors, and nested containers through maps/lists/movable lists. It does not expose a Rust-native rich-text embed/container insertion API for placing child containers directly inside `LoroText`. Therefore object and structural block ordering is locked to the object replacement character strategy: use `U+FFFC` (`\u{FFFC}`) placeholders inside flow `LoroText`, and store the corresponding structured object metadata in Loro maps such as `blocks_by_id`. The placeholder is the CRDT-ordered anchor. The block map is the editable durable object record.

Tables are fully in scope for this architecture pass. They must be implemented as structured Loro objects with row identity, column identity, cell identity, spans, nested table support, and independent CRDT text flows per cell. A table may not be stored as an opaque binary payload or rewritten wholesale for normal edits.

Named revisions are time-travellable snapshot/frontier points. A named revision must preserve the ability to render, open, and fork that point in document history. It does not need to preserve every individual low-level operation that occurred before that named point. History compaction may squash unnamed low-level update history once the retained snapshots/frontiers needed for named revisions and product checkpoints remain restorable.

PDF source embedding must embed only the new Loro-native `.db8` package bytes. It must not embed the old development `.db8` payload, and it must not support both old and new source payloads.

## 2. Canonical Model

There is exactly one canonical document model: the `LoroDoc`.

Everything else is derived.

Flowstate may maintain a `DocumentProjection`, layout cache, paragraph index, glyph cache, hit-test map, section cache, pagination cache, asset cache, render tree, search index, preview cache, and export projection. None of those are authoritative. They can be discarded and rebuilt from the Loro document plus the asset store.

The following must not remain canonical:

* native final-state `.db8` serialization
* `gpui-flowtext::Document` as persistent source of truth
* Flowstate operation logs as persistent source of truth
* patch streams as document truth
* local/remote mutation paths that reconcile two document models
* binding layers whose job is to keep Flowstate state and Loro state synchronized
* object payload blobs that hide structured editable content from Loro

The canonical pipeline is:

```text
Local command
  -> resolve command against current DocumentProjection
  -> send semantic command to CRDT runtime
  -> mutate LoroDoc in grouped Loro transaction/change
  -> permanent Loro subscription emits document event/update
  -> CRDT runtime updates or invalidates DocumentProjection
  -> UI receives projection diff/snapshot
  -> render
  -> persist Loro update
  -> sync Loro update

Remote update
  -> CRDT runtime imports bytes into LoroDoc
  -> import status triggers immediate anti-entropy if dependencies are missing
  -> permanent Loro subscription emits document event/update
  -> CRDT runtime updates or invalidates DocumentProjection
  -> UI receives projection diff/snapshot
  -> render
  -> persist accepted update
```

There is no dual-write model.

## 3. Runtime Ownership

Flowstate must use a dedicated CRDT runtime that owns each live `LoroDoc`.

The UI thread must not own or mutate the canonical `LoroDoc`. The UI owns the current immutable or copy-on-write projection state needed for rendering, layout, hit testing, and command construction.

The CRDT runtime:

* owns the `LoroDoc`
* owns the permanent Loro subscriptions
* owns the Loro `UndoManager`
* owns import/export/update persistence coordination
* receives semantic editor commands over channels
* returns projection diffs, projection snapshots, selection updates, asset availability changes, and status events
* batches projection work by Loro commit/frontier
* isolates CRDT work from UI frame timing

The preferred shape is:

```text
UI thread
  DocumentProjection
  LayoutEngine
  Renderer
  HitTest
  Command construction
  Presence rendering

CRDT runtime thread/task
  LoroDoc
  UndoManager
  Loro subscriptions
  Persistence writer
  Sync import/export
  Projection builder/incremental projector
  Asset manifest coordination
```

The runtime may use a thread, task, or actor implementation, but the ownership boundary is mandatory: canonical Loro mutation happens in one place.

## 4. Command Layer

The old Flowstate collaboration API and `CanonicalOperation` path must be retired and removed.

Flowstate should keep semantic editor commands, but those commands must target the Loro-native schema directly. They are user intent, not a second canonical operation language.

The command architecture is locked:

> UI sends semantic commands; the CRDT runtime resolves them into Loro schema mutations.

The UI must not construct low-level Loro schema mutations directly as the normal command path. Schema mutation authority lives in the CRDT runtime.

Preserving the old `CanonicalOperation` API as a durable or canonical path is explicitly rejected.

```text
UI input
  -> hit test/projection coordinate
  -> semantic command
  -> CRDT runtime validates against current frontier
  -> Loro schema mutation(s)
  -> grouped commit
  -> projection diff
```

Examples:

```text
insert_text(flow_id, cursor, text, style_state)
split_paragraph(flow_id, cursor, inherited_attrs)
join_paragraphs(flow_id, boundary_id)
set_run_semantic_style(flow_id, range, style_id)
set_highlight_style(flow_id, range, highlight_id)
insert_table(anchor, rows, columns)
insert_image(anchor, asset_ref, attrs)
edit_image_alt_text(image_id, text_command)
edit_equation_source(equation_id, text_command)
move_block(block_id, target_cursor)
undo()
redo()
open_revision(frontier)
fork_revision(frontier)
```

## 5. Projection And Editor Model

The ideal target may substantially replace or rearchitect `gpui-flowtext::Document`.

The editor/render model must replace the current mutable `gpui-flowtext::Document` entirely.

Keeping a type named `Document` while redefining it as projection-only is not the target. Keeping the current mutable `Document` plus Loro binding/reconciliation is rejected.

`DocumentProjection` is a frontier-scoped view:

```text
DocumentProjection
  frontier
  flows
  paragraphs
  blocks
  sections
  table_grids
  inline_style_runs
  object_metadata
  asset_refs
  search_units
  cursor_position_maps
  byte/grapheme/UTF-16 indexes for the projection only
  dirty_ranges
```

The projection must be rebuildable from:

```text
LoroDoc at frontier + asset manifest
```

The projection may cache:

* paragraph boundaries
* paragraph IDs
* block order
* style runs
* table layout inputs
* glyph layout inputs
* pagination keys
* hit-test data
* search units
* export units

Projection coordinates are not canonical. Canonical command positions should use Loro cursors wherever possible. Byte, grapheme, UTF-16, row, column, and paragraph indexes are local coordinates within a specific projection frontier.

If projection encounters a malformed paragraph boundary without a paragraph-style mark, it should default that paragraph to Normal and immediately schedule a repair mutation in the CRDT runtime. Repair should modify the underlying Loro buffer so the document is safe before further p2p sharing propagates the malformed state.

## 6. Canonical Text Model

The document must not use one independent `LoroText` per paragraph as the primary body text model.

That design breaks expected word-processor semantics for concurrent paragraph splits and inline edits.

Example:

```text
Initial:
ABC

Alice inserts "y" between B and C.
Bob presses Enter between A and B.
```

The semantically correct result is:

```text
A
ByC
```

Not:

```text
Ay
BC
```

Therefore, paragraph content must live inside a continuous CRDT text sequence.

The rule is:

> Use one canonical `LoroText` per continuous text flow.

A text flow is a sequence where inline edits and paragraph boundary changes must commute naturally.

Independent text flows include:

* main document body
* table cell contents
* image caption
* image alt text
* equation source
* footnote body, if footnotes are added
* endnote body, if endnotes are added
* header text flow
* footer text flow
* sidebar text flow, if sidebars are added

Annotations, comments, bookmarks, and suggestion systems are out of scope for this plan and should not appear in the target schema.

Paragraph styles apply to paragraph-bearing flows: body flows, table cell flows, caption flows, headers, footers, and similar prose flows.

Equation source and image alt text flows do not have paragraph styles. They are plain/specialized text flows, not Flowstate paragraph flows.

## 7. Paragraph Boundaries

The preferred paragraph boundary representation is literal newline characters inside the flow `LoroText`.

Rationale:

* It matches ordinary text semantics.
* It keeps paragraph split/join as native text edits.
* It avoids private-use marker leakage into clipboard/search/export.
* It already matches the rough direction of the current root body text model.
* It makes body text easier to inspect, search, import, and export.

Each text flow should begin with a persistent sentinel newline character.

That sentinel newline is not rendered as a visible blank line and is not user-deletable. It exists so every editable flow always has a paragraph boundary character before the first user-visible paragraph.

Paragraphs are signified by the boundary character that sets them off:

```text
\nParagraph one\nParagraph two\nParagraph three
^              ^              ^
sentinel       boundary       boundary
```

The initial sentinel boundary carries the first paragraph's paragraph-style mark. Each later paragraph's style is carried by the boundary newline immediately before that paragraph. This is the predecessor-boundary model.

The plan rejects custom paragraph marker characters unless a later Loro limitation makes newline anchoring insufficient.

Paragraph identity metadata must still be Loro-native and durable. Newlines alone are enough for paragraph boundaries, but paragraph IDs and stable paragraph anchors still need Loro-native representation.

Each paragraph is represented by identity metadata anchored to its start boundary:

```text
ParagraphMap
  id: ParagraphId
  flow_id: FlowId
  start_cursor: Loro cursor at paragraph start / preceding boundary
  boundary_cursor: Loro cursor for paragraph-ending newline when available
```

Empty paragraphs are representable because an empty paragraph is a boundary interval. A blank line between two newline characters is a real paragraph and carries style on its predecessor boundary newline.

Paragraph style storage is locked to Loro marks.

Flowstate should use Loro's rich-text mark APIs for paragraph style. The canonical paragraph style is attached to paragraph boundary text, normally the newline immediately before the paragraph. This keeps Flowstate aligned with Loro's intended rich-text API instead of inventing a parallel paragraph-style substrate.

The mark model must still define word-processor semantics explicitly:

* Every flow starts with a non-rendered, non-deletable sentinel newline.
* The first visible paragraph gets its style from the sentinel newline's paragraph-style mark.
* Every later paragraph gets its style from the newline immediately before it.
* A blank paragraph between two newline characters gets its paragraph style from the predecessor newline.
* Splitting a paragraph in the middle makes both resulting paragraphs inherit the origin paragraph's style.
* Pressing Enter at the end of a styled paragraph creates a following blank paragraph with Normal style.
* Pressing Enter at the start of a styled paragraph creates a preceding blank paragraph that inherits the original paragraph's style; the original paragraph keeps its style.
* Joining paragraphs keeps the higher/earlier paragraph's style.
* Deleting across paragraphs keeps the higher/earlier surviving paragraph's style.
* Applying paragraph style at a collapsed caret applies to the current paragraph.
* Applying paragraph style to any selection applies to every paragraph touched by the selection, including partially touched first/last paragraphs.
* Rich paste preserves pasted paragraph styles unless pasted onto a styled paragraph in a way that semantically replaces that paragraph; in that replacement case, pasted paragraphs inherit the target paragraph's style.
* Plain-text paste with newlines inherits the insertion paragraph's style for all inserted paragraphs.

Do not create a competing paragraph-style field in paragraph/block metadata. If a projection caches paragraph style, that cache is derived from Loro marks.

Paragraph style uses one Loro mark key:

```text
paragraph_style = <enum slot>
```

Paragraph style IDs are stable enum slots with holes reserved for future styles. User-facing labels can change freely in the app/theme without changing document semantics. The document stores style identity, not display names and not appearance attributes.

Paragraph-style marks should use the least-sticky Loro mark expansion setting that supports boundary-only paragraph marks. Normal paragraph-style inheritance should not rely on automatic mark expansion. Commands such as Enter, split paragraph, paste, import, and repair must explicitly set paragraph-style marks on the affected boundary newlines.

If two peers concurrently set different paragraph-style enum values on the same boundary under the same `paragraph_style` key, Flowstate accepts Loro's deterministic visible winner. Projection should use the value Loro exposes as current. This is normal CRDT conflict resolution, not corruption.

Other paragraph-level properties are out of scope unless Flowstate later introduces them as real editor features. If future paragraph-level properties cannot be represented cleanly as Loro marks, they need a separate design decision at that time.

The renderer strips no custom paragraph marker. It renders newline-delimited paragraphs from the projection.

## 8. Flow Structure

Each text flow is stored as:

```text
FlowMap
  id: FlowId
  kind: "body" | "table_cell" | "caption" | "alt_text" | "equation_source" | "header" | "footer" | ...
  text: LoroText
  attrs: LoroMap
```

Flow maps are reachable from:

```text
RootMap
  flows_by_id: LoroMap<FlowId, FlowMap>
```

A flow may contain:

* normal Unicode text
* newline paragraph boundaries
* object/embed placeholders when the flow supports embedded objects

Paragraphs are ranges in a flow. Paragraph split inserts `\n`. Paragraph join removes `\n`. Inline insert inserts into the same flow text.

Nested flows remain in Loro history even after the current document frontier no longer reaches them. They are not deleted from history merely because a block was deleted. Package-level asset/cache garbage collection may remove unreachable binary assets according to retention policy, but Loro document history remains history unless a deliberate lineage-compaction/export operation creates a new document lineage.

## 9. Block And Object Ordering

Block order is canonical in the text flow itself.

The main body flow is the canonical ordered sequence for body paragraphs and body objects. A separate ordered block list must not become a second source of truth.

Object blocks are represented by object placeholders in a flow. Their editable metadata lives in a block registry:

```text
RootMap
  blocks_by_id: LoroMap<BlockId, BlockMap>
```

Block map:

```text
BlockMap
  id: BlockId
  kind: "paragraph" | "image" | "equation" | "table" | "divider" | ...
  flow_id: FlowId
  anchor_cursor: Loro cursor for position in parent flow
  attrs: LoroMap
  nested_refs: LoroMap
```

Paragraph blocks are metadata records over newline-delimited flow ranges.

Object blocks are metadata records anchored to object placeholders in the flow.

Images and equations do not have paragraph styles.

Tables have independent table styling. Table styling is not inherited from surrounding paragraphs.

Object placeholders use the Unicode object replacement character `U+FFFC` (`\u{FFFC}`) in the parent flow `LoroText`, plus a block ID mapping in `blocks_by_id`. This is locked because the Rust `loro` crate does not currently expose a native rich-text embed/container insertion API for placing child containers directly inside `LoroText`.

Object placeholder characters are not user-authored document text. They are structural anchors. Clipboard, search, DOCX export, PDF export, plain-text export, and visible text rendering must project them as the corresponding object, not leak the raw replacement character as ordinary content.

All durable IDs must be stored in Loro. Projection-generated IDs must not become canonical.

## 10. Semantic Styling

Flowstate does not allow arbitrary user-authored formatting as the canonical editing model.

Inline styling must remain semantic-bound. Loro marks should store style identities and booleans from Flowstate's semantic style system, not freeform font/color/CSS values.

Examples:

```text
run_semantic_style_id
highlight_style_id
direct_underline
strikethrough
emphasis_style_id, if introduced
```

Paragraph style is a Loro mark on paragraph boundary text:

```text
paragraph_style_id
```

The exact style model may continue to use overlapping semantic categories, as Flowstate does today:

* paragraph style IDs stored as Loro paragraph-boundary marks
* run semantic style IDs
* highlight style IDs
* underline flag/style
* strikethrough flag/style
* future semantic style families

The user-facing style catalog is app/theme data only. A style ID in Loro references a semantic enum slot; the appearance of that slot is resolved by the client theme/style catalog.

DOCX import remains supported. Imported direct formatting must be interpreted into Flowstate's predefined semantic model using the existing Verbatim/heuristic import architecture, evolved as needed.

DOCX import must not create new document-local styles. It maps recognized DOCX styles and formatting heuristics into Flowstate paragraph style slots, run semantic style slots, highlight slots, underline, and strikethrough. Unknown DOCX formatting may be reported or dropped, but it must not expand the live document's semantic style universe.

Imported formatting should not turn the live editor into a freeform arbitrary formatter.

URLs are ordinary document text. This plan does not model links as annotations or comments.

Comments, annotations, bookmarks, and suggestions are out of scope.

## 11. Sections And Page Structure

Sections are structural metadata anchored to body flow positions.

```text
sections_by_id: LoroMap<SectionId, SectionMap>
section_order: derived from body flow anchors unless later proven insufficient

SectionMap
  id
  start_cursor
  attrs:
    page_size
    margins
    columns
    header_flow_id
    footer_flow_id
    page_numbering
    orientation
```

A section boundary should be a CRDT-stable position in the body flow.

Headers and footers are independent text flows.

Section/order metadata must not duplicate paragraph ordering. The body flow remains canonical for body order.

Heading and outline computation reads paragraph style exclusively from paragraph-boundary marks. A paragraph participates in outline/section projection when its semantic paragraph style slot maps to a heading/section role in the app/theme style system.

## 12. Tables

Tables must be structurally CRDT-native.

A table must not be stored as a single binary blob whose entire payload is rewritten for edits.

The target table schema must support:

* row identity
* column identity
* row insertion/deletion/movement
* column insertion/deletion/movement
* cell identity
* merged cells
* row spans
* column spans
* nested tables
* independent rich text flows per cell
* concurrent edits in different cells without conflict
* concurrent structural edits represented by Loro containers

Preferred table schema:

```text
TableBlock
  id: BlockId
  kind: "table"
  anchor_cursor
  attrs: TableAttrs
  row_order: LoroMovableList<RowId>
  rows_by_id: LoroMap<RowId, RowMap>
  column_order: LoroMovableList<ColumnId>
  columns_by_id: LoroMap<ColumnId, ColumnMap>
  cells_by_id: LoroMap<CellId, CellMap>

RowMap
  id: RowId
  attrs: RowAttrs

ColumnMap
  id: ColumnId
  attrs: ColumnAttrs

CellMap
  id: CellId
  row_id: RowId
  column_id: ColumnId
  row_span: u32
  column_span: u32
  attrs: CellAttrs
  flow_id: FlowId
  nested_table_ids: LoroMovableList<BlockId>
```

Each table cell owns an independent text flow.

Inside a cell flow, paragraphs follow the same rule as the main body: one continuous `LoroText` per cell flow with newline paragraph boundaries.

Use `LoroMovableList` for rows and columns. Tables are ordered structures with semantic movement.

Do not use `LoroTree` as the main table representation unless arbitrary recursive tree moves become required. Tables are ordered row/column grids with cell flows, not general trees.

## 13. Equations

Equation source must be stored as `LoroText`.

Equation metadata should be stored in `LoroMap`.

```text
EquationBlock
  id: BlockId
  kind: "equation"
  anchor_cursor
  source_flow_id
  attrs:
    syntax: "latex" | "mathml" | ...
    display: "inline" | "block"
    numbering
    alignment
```

If two users edit the equation source concurrently, the source should merge like text.

Do not store equation source as a map string.

Render output is not synchronized between peers. Each client renders equation source locally using Flowstate's live rendering system. Any rendered SVG/bitmap/layout result is a disposable client cache.

Inline equations and block equations are both represented as object placeholders inside a parent text flow. The renderer decides whether the object is rendered inline or block-level from metadata.

## 14. Images And Assets

Images do not need a special image CRDT.

Image metadata should be stored in `LoroMap`. Image bytes should be stored outside Loro in a content-addressed asset store.

```text
ImageBlock
  id: BlockId
  kind: "image"
  anchor_cursor
  asset_id
  content_hash
  mime_type
  byte_length
  dimensions
  crop
  sizing_mode
  alignment
  alt_text_flow_id
  caption_flow_id
```

Image metadata should use field-level map keys. Do not rewrite the whole image object for a metadata edit.

Image caption and alt text must be CRDT-editable text flows.

Asset bytes are stored in the package asset store, not in Loro.

Asset content addressing must use BLAKE3 canonically.

The canonical asset hash is a BLAKE3 digest. Short local cache keys may be derived for in-memory lookup, but they are never canonical and must not be used for integrity or package identity.

Remote asset behavior:

* CRDT updates may be accepted before referenced asset bytes arrive.
* The document must render a visible placeholder for missing assets.
* Missing assets should trigger prioritized asset pulls.
* Asset availability should participate in sync/anti-entropy status so peers can discover missing bytes, not just missing Loro ops.
* Local image insertion may commit Loro metadata before the full asset bytes are available.
* If asset bytes never arrive or remain incomplete, the document should retain the reference and render an explicit incomplete-asset placeholder.
* Remote metadata may arrive before bytes; that is acceptable and should render as a recoverable placeholder.

## 15. Presence, Peers, And Author Metadata

Presence is not document history.

Remote carets, live selections, transient names/colors, and typing/live state belong in Loro ephemeral state or equivalent ephemeral sync state.

Durable authorship metadata is separate.

The schema should distinguish:

```text
UserId
  stable user-facing identity for blame/history/UX

ReplicaId / PeerId
  unique Loro editing replica identity
  may differ for each device, app instance, tab, or session participant
```

The same user may have multiple devices. The same device or app instance may open multiple tabs into the same session. Those must be distinct Loro replicas even if they share one user-facing identity.

Never set a shared or user-stable Loro peer ID. Loro peer IDs identify editing replicas and must be unique per active replica.

The document may store durable author metadata keyed by user identity and/or by observed Loro peer IDs. Live roster/presence remains ephemeral.

## 16. Selection, Cursors, And Affinity

Canonical selection endpoints should use Loro cursors wherever possible.

The selection model must include explicit affinity/gravity information:

```text
SelectionEndpoint
  cursor
  affinity: before | after | neutral
  visual_gravity

Selection
  anchor: SelectionEndpoint
  head: SelectionEndpoint
  direction
```

This is stronger than merely choosing `Side::Before` or `Side::After` at cursor creation. The editor needs to preserve the user's visual intent across:

* concurrent inserts at the same position
* undo/redo cursor restoration
* bidi text
* object boundaries
* line wrapping
* selection extension
* collapsed caret movement

Clarification:

* A simple side choice says where the cursor sits relative to concurrent inserted content.
* A full affinity/gravity model stores why that side was chosen and how the caret should behave visually as text/layout changes.

`Side::Middle` should not be hardcoded as the only selection side. It can remain available for genuinely neutral anchors, but normal caret/selection behavior should choose side from stored affinity.

UndoManager cursor storage should save and restore selection cursors through Loro's cursor transformation support.

## 17. Undo And Redo

Undo/redo must be Loro-native for all documents, local or collaborative.

Flowstate must not maintain a separate canonical undo/redo stack for document mutations.

Undo is per-peer/per-replica local undo. If this user edits, then receives another peer's operation, pressing undo should reverse this user's edit, transformed over the remote change. It should not undo the other peer's operation.

The CRDT runtime owns one Loro `UndoManager` per live document replica.

Semantic undo units include:

* typed word or typing burst
* paragraph split
* paste
* style change
* table row/column insert
* table structural edit
* object resize
* image insert
* equation edit
* section break insert

Those undo units must correspond to grouped Loro changes.

Use Loro-native mechanisms:

* explicit commits for command boundaries
* Loro change merge interval for continuous local edits when appropriate
* UndoManager merge interval
* UndoManager group start/end for compound semantic commands
* UndoManager cursor onPush/onPop to restore selection

Time travel is not undo. Time travel is checkout/fork/navigation over durable Loro frontiers.

## 18. Time Travel, Revisions, And Restore

The operation history is the durable basis for time travel.

The document package should maintain a revision index:

```text
Revision
  id
  title
  timestamp
  author/user
  replica_id
  frontier/version_vector
  summary
  thumbnail optional
  parent revision optional
```

A revision records a named frontier into Loro history and/or a retained package snapshot sufficient to materialize that frontier. It is a time-travellable document point, not a promise to retain every low-level operation that happened before that point.

Opening a revision checks out or materializes the Loro document at that frontier and renders the projection for that state.

Restore behavior:

* Opening a historical revision should open a new tab with that historical version.
* That tab should be dirty.
* Saving that tab should save as a branch/fork/new document according to product flow.
* Forking into a new branch/document must remain available as the preferred restore path.

History compaction is mandatory. Full history forever is rejected.

The compaction policy is locked:

> Keep named revision snapshots/frontiers, compact unnamed update history.

Compaction must preserve user-meaningful and product-meaningful revision frontiers as restorable/forkable points while squashing low-level update history that has no revision identity. A named revision must remain openable, renderable, and forkable after compaction, but the implementation may discard unnamed per-keystroke or per-command update detail beneath a retained revision snapshot/frontier.

Natural automatic chunk points include:

* app tab session boundaries
* save instances, especially explicit `Ctrl+S`
* named revision creation
* import completion
* major document-structure operations

Each explicit save is a natural revision/checkpoint boundary. Future UX may allow users to name or preserve selected revision frontiers. Unnamed intra-session operation detail may be compacted once covered by retained snapshots/frontiers. Named revisions are snapshot/frontier preservation points, not permanent full-operation-log retention points.

Creating a new explicit document lineage is not the preferred normal compaction strategy.

## 19. Filesystem Package

The user-facing artifact remains a single `.db8` document file.

Internally, it must not be the old final-state native serializer.

The `.db8` extension names the new Loro-native Flowstate package. It must not be used as a compatibility excuse for retaining the old development `.db8` format. Old `.db8` read/write/import/recovery/indexing code paths must be removed or rewritten to this package format.

The ideal filesystem object is a Loro document package containing:

```text
DocumentPackage
  manifest
  loro_snapshots
  loro_update_segments
  asset_store
  revision_index
  projection_cache
  search_projection_cache
  thumbnails
  integrity_index
```

The package format is locked:

> Flowstate uses a custom chunked binary container that is both Loro-native and Flowstate-native.

Reasons:

* single user-facing file
* append-friendly update storage
* Flowstate-controlled chunk layout and indexing
* efficient manifests and internal lookup indexes
* direct storage for snapshots, update segments, assets, thumbnails, and projection/search caches
* fast unopened-file indexing for tub search
* no dependency on a general SQL database file format for document storage
* room for Loro-aware and Flowstate-aware chunk types, compression, checksums, and partial reads

The package must support external workspace/tub indexing. The package itself should expose enough projection/search metadata for Flowstate to index it quickly without fully opening the document or replaying Loro history.

Logical chunk classes:

```text
manifest chunk
  package_format_version
  loro_schema_version
  document_id
  latest_frontier
  latest_snapshot_id
  update_segment_index
  asset_index
  projection_cache_frontier
  search_cache_frontier
  created_at
  modified_at

loro snapshot chunks
  snapshot_id
  frontier
  bytes
  created_at

loro update segment chunks
  segment_id
  from_frontier
  to_frontier
  bytes
  checksum
  created_at

asset chunks
  asset_id
  content_hash
  mime_type
  byte_length
  bytes
  metadata

revision index chunks
  revision_id
  frontier
  title
  summary
  author_user_id
  replica_id
  created_at

projection cache chunks
  frontier
  bytes

search unit chunks
  frontier
  unit_id
  unit_kind
  heading_path
  heading
  body
  insert_text
  paragraph_start_cursor
  paragraph_end_cursor
```

Only Loro snapshot/update data and asset bytes/references are canonical.

Projection caches and search units are disposable.

## 20. Read Path

Opening a document:

```text
open package
read manifest
load latest complete Loro snapshot
apply complete update segments after snapshot
construct LoroDoc in CRDT runtime
verify package format version
verify Loro schema version
verify document lineage/integrity
load projection cache if frontier matches
otherwise rebuild projection from Loro
load search cache if frontier matches
otherwise rebuild search units from projection/Loro
open renderer on DocumentProjection
```

The renderer never reads the old final-state document format as authoritative state. There is no legacy `.db8` reader in the target app.

DOCX import is external import, not document storage.

## 21. Write Path

Saving is append-first and crash-safe.

On each committed local Loro change:

```text
receive local update bytes
append update segment in package transaction
record new frontier
update manifest
invalidate stale projection/search caches
schedule snapshot compaction if thresholds are crossed
```

Periodically, or when update history crosses thresholds:

```text
export fresh Loro snapshot
write snapshot
mark older update segments compactable if revision policy permits
write projection cache for fast open
write search projection cache for tub/search
update manifest transactionally
```

A committed update segment must either be fully visible or ignored. The manifest must point only to complete, verified segments.

## 22. Sync And Anti-Entropy

Remote updates are imported into the CRDT runtime's `LoroDoc`.

Remote updates must not be translated into Flowstate operations.

Import status must be consumed. If import reports missing/pending dependencies, Flowstate should immediately trigger update pull/anti-entropy rather than waiting only for periodic digest.

Periodic digest can remain as a fallback liveness mechanism, but the primary gap response should be actual Loro version-vector/update anti-entropy.

Target path:

```text
receive remote update bytes
import into LoroDoc
read ImportStatus
if pending/missing: request updates from peers immediately
Loro event reaches permanent subscription
project changed containers/ranges
persist accepted update
sync any newly generated update as needed
```

The only acceptable "patch" layer is a derived UI projection diff layer. There should be no `RemoteApplier` whose purpose is reconstructing canonical Flowstate document patches.

## 23. Permanent Loro Subscription

Temporary subscriptions around import/undo are not acceptable.

The CRDT runtime must own a permanent subscription for each live document and filter/process events by origin, trigger, current frontier, and runtime epoch.

This avoids relying on synchronous event timing during `import`, `checkout`, or `undo`.

The subscription should feed:

* projection invalidation
* local update publish
* persistence append
* search projection invalidation
* asset reachability checks
* undo/redo selection restoration
* revision/frontier status

## 24. Projection Invalidation

Projection invalidation is a core part of the architecture.

Open design decision:

| Option | Runtime perf | Correctness | Responsiveness | Complexity | P2P resilience | Summary |
|---|---:|---:|---:|---:|---:|---|
| A. Full projection rebuild after every Loro event | 3 | 10 | 3 | 4 | 8 | Simple and correct, but too slow for large documents and live collaboration. |
| B. Incremental projection from Loro diffs only | 9 | 7 | 9 | 9 | 7 | Fast but fragile if any event shape is missed. Needs strong fallback. |
| C. Batched frontier-based invalidation with incremental fast paths and full rebuild fallback | 9 | 10 | 9 | 8 | 10 | Preferred. Batch events per commit/frontier, apply known incremental invalidations, rebuild affected projection regions, and fall back to full rebuild on uncertainty. |
| D. Poll/diff snapshots after changes | 4 | 8 | 4 | 5 | 6 | Avoids event complexity but wastes Loro's event model and increases latency. |

Current recommendation: Option C.

The runtime should maintain projection indexes:

* flow text boundary index
* paragraph range index
* paragraph metadata index
* block anchor index
* object placeholder index
* table row/column/cell index
* style interval index
* section anchor index
* asset reference index
* search unit index
* cursor resolution cache

For each Loro event batch, the runtime should produce:

```text
ProjectionInvalidation
  frontier_before
  frontier_after
  changed_flows
  changed_text_ranges
  changed_blocks
  changed_tables
  changed_assets
  changed_sections
  rebuild_required flag
```

The UI receives stable projection diffs or a new projection snapshot. It never applies canonical document mutations itself.

Full projection rebuild is an exceptional fallback, not an ordinary edit path. The runtime must make fallback use observable with structured logging/counters. Fallback should be treated as a performance bug if it occurs repeatedly during normal typing, formatting, table editing, image edits, or remote update import.

The projection system should define explicit incremental paths for all expected common operations. Fallback is reserved for unknown event shapes, detected projection corruption, schema migration, recovery, or rare defensive repair.

## 25. Search

Within-document and tub search must be ported to the Loro-native architecture.

The current tub architecture:

* catalogs files in SQLite
* indexes searchable units in Tantivy
* extracts section/paragraph units from old final-state `.db8`
* hydrates previews from the old document model

The new architecture should:

* index Flowstate `.db8` Loro package files instead of old final-state `.db8`
* derive search units from `LoroDoc`/`DocumentProjection`
* preserve section-aware units such as block/tag/analytic/card/cite where those style semantics still exist
* store search unit cursors or paragraph IDs instead of raw old paragraph indexes as canonical references
* use raw paragraph indexes only as projection-local convenience values
* hydrate previews from the Loro projection or package search cache
* continue using Tantivy for workspace/tub full-text search unless a clearly superior crate replaces it

Search cache data inside a package is disposable but should be saved by default for fast unopened-file indexing. The external tub index is also disposable. Both must be rebuildable from Loro package contents.

## 26. DOCX, PDF, And Export

DOCX import must be migrated onto the Loro-native model immediately.

DOCX import path:

```text
DOCX
  -> interpreter/import heuristics
  -> semantic Flowstate style model
  -> Loro-native document creation
  -> DocumentProjection
  -> renderer
```

DOCX export, PDF export, and other output formats are generated from a Loro snapshot/frontier projection.

Exports should not expose or require document history. Saving a Flowstate document saves the Loro package with history; exporting produces a final-state external artifact.

PDF source recovery, when present, must embed the new Loro-native `.db8` package bytes only. It must not embed or recover the previous development `.db8` serializer payload, and it must not maintain dual old/new recovery formats.

The only permitted verification fixtures in this plan are import/export/render regression fixtures for DOCX/PDF/export behavior. The broader architecture plan should not specify a general test suite at this stage.

## 27. Schema Versioning

The root Loro document should include explicit schema metadata:

```text
MetaMap
  document_id
  loro_schema_version
  schema_features
  created_by_app_version
  last_written_by_app_version
  created_at
  modified_at
```

This is not backwards compatibility for old `.db8`. It is forward compatibility for future Loro-native schema evolution.

Open decision: whether migrations are stored as explicit revision/history records or only as package metadata. Current recommendation is to record schema migration events in package metadata and, if they mutate the Loro document, make those mutations ordinary Loro changes with a migration origin.

## 28. Container References

Container references must store both durable Flowstate IDs and raw Loro container IDs.

The durable Flowstate ID is the semantic identity. The raw Loro container ID is stored for direct resolution and efficient runtime access.

Example:

```text
CellMap
  flow_id: FlowId

flows_by_id[flow_id]
  text: LoroText
```

The schema should not depend on user-facing code hand-authoring raw `ContainerID` strings. Container resolution should be centralized in the CRDT runtime/schema module.

## 29. Clear/Rebuild Paths

The current code has a `clear_blocks` path because it sometimes rebuilds the Loro block list from the old `Document` model. That is a symptom of the current dual-model architecture.

In the target architecture, full clear-and-repopulate paths should be rare. Normal editing should mutate Loro incrementally.

When a legitimate full rebuild is needed, such as creating a new document from DOCX import or replacing a disposable projection cache, use Loro's exposed clear API for the relevant container instead of deleting one item at a time.

Do not use full clear/repopulate as a normal local edit path, because it destroys useful incremental history and creates poor collaboration semantics.

## 30. Invariants

The architecture must preserve these invariants:

1. A document can be fully reconstructed from the Loro snapshot/update history plus asset store.
2. Flowstate render/layout/search/export caches are disposable.
3. Paragraph split and inline text insertion commute correctly.
4. Undo/redo is based on grouped Loro changes and is per-peer/per-replica.
5. Time travel is based on Loro frontiers, not undo stack state.
6. Restoring a historical revision opens a dirty fork/branch tab rather than destructively replacing current history.
7. Binary assets are referenced by Loro metadata and stored outside the CRDT operation log.
8. Image captions and alt text are CRDT text flows.
9. Equation source is a CRDT text flow.
10. Rich inline formatting uses semantic Loro marks.
11. Paragraph style lives in Loro paragraph-boundary marks, not in competing paragraph metadata.
12. Paragraph/object/table metadata lives in Loro maps/lists.
13. Tables are structured CRDT objects, not binary blobs.
14. Rows and columns have durable Loro-native identity.
15. Remote updates are imported into Loro directly.
16. Local commands mutate Loro through the CRDT runtime.
17. There is no canonical Flowstate document separate from Loro.
18. There is no local-vs-remote document mutation split.
19. Files are Loro-backed packages, not final-state-only serialized documents.
20. Projection caches declare the Loro frontier they represent.
21. Search indexes declare the Loro frontier they represent.
22. DOCX/PDF/export operate from Loro snapshot projections.
23. Comments, annotations, bookmarks, and suggestion systems are out of scope for this architecture pass.
24. General test suite requirements are out of scope for this plan; only DOCX/PDF/export regression fixtures may be specified.

## 31. Data Shape Summary

Root:

```text
RootMap
  meta: MetaMap
  flows_by_id: LoroMap<FlowId, FlowMap>
  blocks_by_id: LoroMap<BlockId, BlockMap>
  paragraphs_by_id: LoroMap<ParagraphId, ParagraphMap>
  sections_by_id: LoroMap<SectionId, SectionMap>
  assets_by_id: LoroMap<AssetId, AssetMap>
  revisions: LoroList<RevisionMap>
  users_by_id: LoroMap<UserId, UserMap>
  replicas_by_id: LoroMap<ReplicaId, ReplicaMap>
```

Flow:

```text
FlowMap
  id
  kind
  text: LoroText
  attrs
```

Paragraph:

```text
ParagraphMap
  id
  flow_id
  start_cursor
  boundary_cursor
  attrs
```

Object block:

```text
BlockMap
  id
  kind
  flow_id
  anchor_cursor
  attrs
  nested_refs
```

Table:

```text
TableBlock
  row_order: LoroMovableList<RowId>
  rows_by_id: LoroMap<RowId, RowMap>
  column_order: LoroMovableList<ColumnId>
  columns_by_id: LoroMap<ColumnId, ColumnMap>
  cells_by_id: LoroMap<CellId, CellMap>

CellMap
  row_id
  column_id
  row_span
  column_span
  flow_id
  attrs
```

Image:

```text
ImageBlock
  asset_id
  content_hash
  attrs
  alt_text_flow_id
  caption_flow_id
```

Equation:

```text
EquationBlock
  source_flow_id
  attrs
```

Asset:

```text
AssetMap
  asset_id
  content_hash
  mime_type
  byte_length
  dimensions
  metadata
```

Revision:

```text
RevisionMap
  id
  timestamp
  author_user_id
  replica_id
  frontier
  title
  summary
```

## 32. Expected Result

The final architecture should make local editing, collaboration, persistence, undo/redo, revision history, branching, import/export, and search all expressions of the same underlying Loro document history.

There should no longer be a special collaboration document model.

There should no longer be a normal local document model.

There should be one document model:

```text
LoroDoc + asset store + derived Flowstate projections
```

Flowstate becomes the editor, renderer, importer, exporter, and search/projector for a Loro-native rich document, not the owner of a separate document format that Loro shadows.
