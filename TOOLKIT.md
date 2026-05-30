# Toolkit, Tub, And Index Architecture

This document records the locked architecture for the next Flowstate feature:
the tub, the left-side tub tree, the right-side toolkit search panel, and the
indexing foundation behind both.

The goal is not to ship an intermediate file picker. The goal is to put the
correct architecture in place now, even if the first UI exposes only a smaller
surface.

## Product Model

The user sets a `tub`: one debate prep root directory. The tub means:

- These files are intended to be indexed while closed.
- These files are visible in the left-side tub tree.
- These files are searchable by filename and by semantic document content.
- These files can produce drag/drop fragments for insertion into the active
  document or flow.

The first implementation should support one active tub root. The storage model
should not make multiple tub roots impossible later, but the UI should not
pretend that multi-root behavior exists yet.

## Locked Decisions

- Replace the current `DocumentFileSearch` infrastructure for tub search. It is
  acceptable as a small overlay prototype, but not as the tub foundation.
- Use a real `TubIndex` service with a persistent file catalog, a Tantivy search
  index, and background indexing workers.
- Use Tantivy for the persistent search index for both filename/path fields and
  semantic document content.
- Use SQLite only as the durable operational catalog: files, directories,
  indexing jobs, errors, timestamps, hashes, frecency, and mappings to Tantivy
  documents. SQLite is not the document format and is not the content search
  engine.
- Move `.db8` to a VNext raw binary format that persists stable paragraph IDs,
  block IDs, and semantic section records.
- Keep `.db8` fast: VNext remains compact, sequential, length-prefixed binary.
  It must not become JSON, SQLite, or a random-access database format.
- Build toolkit previews from semantic search units, not from ad hoc text
  snippets.
- Render toolkit results with virtualization and lazy read-only document
  previews. Do not mount a full live editor for every search result.

## Current Code To Replace Or Extend

The current filename search lives in `src/file_search.rs`. It recursively scans
a root, stores lowercased file names and full paths, and ranks substring or
fuzzy-subsequence matches. It has no tub setting, no persistent catalog, no
watcher, no semantic file metadata, and no content indexing.

The current overlay lives in `src/workspace/file_search_overlay.rs`. It rebuilds
the file index in the background and opens selected files. This should be
removed from the right-side toolkit and replaced by tub search entry points.

The right-side toolkit is currently functionally collapsed. In
`src/workspace/toolkit_panel.rs`, the "expanded" width is still 40px and
`render_toolkit_expanded` is dead code. The toolkit feature must make the panel
actually expandable and useful.

The left outline panel currently switches contextually between document outline
and flow outline. The tub view should become an explicit left-nav mode, not a
separate overlay.

## Left Panel UI

The left panel has two top-level visible modes:

- `Tub`
- `Outline`

Both labels are buttons. `Tub` is on the left. `Outline` is on the right. Only
one is active at a time.

`Outline` means the current contextual outline:

- Document outline when a document is active.
- Flow outline when a flow is active.

`Tub` means the configured tub file tree. It is accessible from all document and
flow views.

Implementation shape:

```rust
enum LeftNavMode {
  ContextualOutline,
  Tub,
}
```

The tub tree is backed by the tub catalog, not by direct filesystem reads during
render. Directory expansion state is UI state. File and directory rows use
`gpui-component` tree/list primitives where applicable.

Tub tree icons:

- Closed directory: `IconName::FolderClosed`
- Open directory: `IconName::FolderOpen`
- File rows: use file-kind icon once available, otherwise a simple document
  icon.
- Filename search: small `IconName::Search` button in the tub header.

The old right-side "Find DB8 File" button moves here as a small icon-only
filename search action.

## Right Toolkit UI

The right-side panel is the content toolkit. It is not a file picker.

Primary UI:

- A search bar at the top.
- A semantic style/unit selector near the search bar.
- A virtualized vertical result list.
- Each result is a mini-window: a rendered read-only preview of the result unit.
- Each mini-window has its own internal scroll area.
- The result list has external scrolling across results.
- Result units support drag/drop into the active document or flow.

Initial searchable unit filters:

- Block sections
- Tag sections
- Analytics

Architecture must also support:

- Cards
- Cites
- Paragraphs
- Flow nodes
- Full documents
- Future custom semantic units

Do not implement previews by keeping every result as a live editable
`RichTextEditor`. Use a read-only preview renderer over a rich fragment or
document slice, materialized lazily when the result is visible or expanded.

## DB8 VNext

Current `.db8` is already a fast raw binary format: magic, version, text blob,
assets, and block records. VNext keeps that philosophy but adds durable IDs and
semantic section records.

VNext should be chunked binary:

```text
DB8 magic
version
chunk table
  chunk_kind
  flags
  offset
  byte_len
chunks
  text
  assets
  blocks
  paragraph_ids
  block_ids
  sections
  document_metadata
  optional_search_summary
```

Mandatory chunks:

- `text`
- `assets`
- `blocks`
- `paragraph_ids`
- `block_ids`
- `sections`

Optional chunks:

- `document_metadata`
- `optional_search_summary`

The core editor open path may skip optional chunks. Search/indexing code may
read section chunks without loading asset bytes where possible.

### Persistent IDs

Persist paragraph and block IDs in the document format. They should no longer be
only an editor-side `DocumentIdentityMap` generated on open.

Invariants:

- `paragraph_ids.len() == paragraphs.len()`
- `block_ids.len() == blocks.len()`
- IDs survive ordinary edits.
- Split/insert creates new IDs.
- Delete removes IDs.
- Move preserves IDs.
- Import from older `.db8` or `.docx` generates IDs once, then persists them.

The document model should own these IDs, likely as parallel arrays rather than
inflating every paragraph and block payload:

```rust
pub struct Document {
  pub text: Rope,
  pub paragraphs: Arc<Vec<Paragraph>>,
  pub blocks: Arc<Vec<Block>>,
  pub assets: AssetStore,
  pub ids: DocumentIds,
  pub sections: Arc<Vec<DocumentSection>>,
  pub offset_index: ParagraphOffsetIndex,
  pub theme: DocumentTheme,
}
```

Exact field names can change, but the ownership should not: IDs and sections
belong to the document model and persistence layer, not only to the UI editor.

### Semantic Sections

Persist section records as offsets into existing paragraphs. Do not duplicate
heading or body text in `.db8`.

```rust
pub struct DocumentSection {
  pub id: SectionId,
  pub parent_id: Option<SectionId>,
  pub kind: SectionKind,
  pub heading_paragraph: Option<ParagraphId>,
  pub start_paragraph: ParagraphId,
  pub end_paragraph_exclusive: ParagraphId,
}

pub enum SectionKind {
  Pocket,
  Hat,
  BlockSection,
  TagSection,
  Analytic,
  Card,
}
```

The exact storage can use paragraph indexes for compactness if the reader
validates them against the persisted paragraph IDs. The semantic identity should
still be stable across ordinary edits.

Initial unit semantics:

- `BlockSection`: a Block heading and all content until the next Block, Hat, or
  Pocket boundary.
- `TagSection`: a Tag heading and all content until the next Tag, Block, Hat,
  or Pocket boundary.
- `Analytic`: an individual Analytic paragraph.
- `Card`: future unit, likely Tag plus Cite plus body card text when card
  parsing is formalized.

### VNext Performance

VNext should not meaningfully hurt read/write speed if implemented as compact
sequential binary.

Expected costs:

- Paragraph IDs: 16 bytes per paragraph if stored as `u128`.
- Block IDs: 16 bytes per block if stored as `u128`.
- Section records: approximately 32-64 bytes per section.

That is small relative to text, assets, layout, docx conversion, and search
indexing. The benefit is that closed-file indexing, toolkit previews, drag/drop
provenance, and future collaboration get stable anchors immediately.

Avoid:

- Duplicating section text in `.db8`.
- Recursive variable-length section trees in the hot read path.
- Compression in the normal open path.
- Persisting per-token search indexes inside `.db8`.
- Putting SQLite inside `.db8`.

## Tub Catalog

The tub catalog is durable operational state. It is separate from both `.db8`
documents and the Tantivy index.

Use SQLite for the catalog because it gives transactions, resumable background
jobs, ad hoc diagnostics, stale-file tracking, and safe updates without
inventing a database.

SQLite stores:

- Tub settings.
- Directory rows.
- File rows.
- Canonical paths and display paths.
- Parent directory relationships.
- File type.
- Size.
- Modified timestamp.
- Fast content hash.
- Last indexed timestamp.
- Index status.
- Last index error.
- Mapping from file IDs to Tantivy documents.
- Frecency and last-opened metadata.
- Background indexing job state.

SQLite does not store:

- `.db8` document contents as source of truth.
- Full text search postings.
- Rendered mini-window previews.

Suggested catalog shape:

```text
tub_roots(
  root_id,
  path,
  created_at,
  last_scan_at
)

tub_files(
  file_id,
  root_id,
  path,
  display_path,
  parent_path,
  file_name,
  extension,
  file_kind,
  size_bytes,
  modified_ns,
  content_hash,
  index_status,
  last_indexed_at,
  last_error,
  last_opened_at,
  frecency_score
)

index_jobs(
  job_id,
  file_id,
  job_kind,
  priority,
  status,
  attempts,
  last_error,
  created_at,
  updated_at
)
```

## File Discovery And Watching

Initial scan:

- Walk the tub root in a background worker.
- Ignore hidden/system directories by default unless the user opts in.
- Skip known generated directories such as `target`, `node_modules`, package
  build outputs, and temporary sync-provider files.
- Do not descend through symlink/junction loops.
- Canonicalize paths for identity but preserve display paths for UI.

Watching:

- Use filesystem notifications where reliable.
- Debounce changes.
- Add a polling fallback because Dropbox, OneDrive, and network folders can
  produce incomplete or coalesced events.
- Treat rename as delete plus add unless the platform gives a reliable paired
  rename event.

Supported first-class file types:

- `.db8`
- `.docx`
- `.fl0`

Additional types can be cataloged but not content-indexed until extractors
exist.

## Tantivy Index

Tantivy is the persistent retrieval index for both filename/path search and
semantic content search.

Index one Tantivy document per semantic search unit, not one per file.

For file-only rows, also index a file-level unit so filename search can return
files that do not yet have extracted semantic units.

Core stored fields:

```text
file_id
unit_id
unit_kind
file_kind
path_exact
display_path
file_name_exact
extension
heading_path
title_text
cite_text
body_text
paragraph_start
paragraph_end_exclusive
byte_start
byte_end
modified_ns
content_hash
```

Indexed text fields:

```text
file_name_text
file_name_ngram
path_text
path_ngram
heading_text
title_text
cite_text
body_text
all_text
```

Use field boosts:

- filename exact match
- filename prefix/token match
- heading/title match
- cite match
- body match
- path match
- recency/frecency boost from catalog metadata

Filename search should use Tantivy's indexed filename/path fields. If pure
Tantivy ranking does not feel like a good file picker, add an in-memory reranker
over the returned candidates or the catalog file list. That reranker is not a
separate source of truth; Tantivy remains the persistent search index.

## Content Extraction

Build a `ContentExtractor` layer per file type.

`.db8` extractor:

- Read VNext chunks.
- Use persisted sections directly.
- Materialize unit text from paragraph ranges.
- Skip asset bytes for search unless alt text/captions are needed.

Legacy `.db8` extractor:

- Read current format.
- Generate IDs and semantic sections.
- Schedule/save migration when appropriate.

`.docx` extractor:

- Use `flowstate-docx` conversion rules.
- Generate IDs and semantic sections after conversion.
- Index the converted semantic document.
- Do not modify the source `.docx` unless the user explicitly saves/imports.

`.fl0` extractor:

- Use `flowstate-flow` persistence.
- Index flow title, speech names, argument nodes, tags/metadata, and text.
- Unit kind should support future `FlowNode`.

## Semantic Search Units

The toolkit search returns `SearchUnitHit` records.

```rust
pub struct SearchUnitHit {
  pub file_id: FileId,
  pub unit_id: SearchUnitId,
  pub unit_kind: SearchUnitKind,
  pub file_path: PathBuf,
  pub heading_path: Vec<String>,
  pub title: String,
  pub cite: Option<String>,
  pub snippet: String,
  pub score: f32,
  pub source_range: SourceRange,
}
```

`SearchUnitKind` must be open-ended:

```rust
pub enum SearchUnitKind {
  File,
  Pocket,
  Hat,
  BlockSection,
  TagSection,
  Analytic,
  Card,
  Cite,
  Paragraph,
  FlowNode,
}
```

The first toolkit UI exposes Block sections, Tag sections, and Analytics. The
index architecture must be able to return the other kinds without a rewrite.

## Search Query Pipeline

Filename search:

1. Query Tantivy filename/path fields.
2. Boost exact filename and prefix matches.
3. Boost basename matches over parent path matches.
4. Apply recency/frecency from the catalog.
5. Return file rows to the tub UI.

Content search:

1. Parse query text.
2. Apply unit-kind/style filters.
3. Query boosted Tantivy fields.
4. Return semantic units, not raw files.
5. Lazy-load rich previews only for visible results.

Style/unit filters:

- All
- Block sections
- Tag sections
- Analytics
- Cards
- Cites
- Flow nodes

The UI wording can be refined, but the index should use explicit unit kinds.

## Toolkit Mini-Windows

Each search result renders as a mini-window containing the semantic unit.

Rendering rules:

- Virtualize the external result list.
- Do not eagerly render all results.
- Each visible result may lazy-load a read-only rich fragment.
- The mini-window has a bounded height and internal scroll.
- The external list scrolls across mini-windows.
- The preview renderer should share as much text layout code as practical with
  the editor, but it must not carry full editor state unless the result is
  opened for editing.

Drag/drop:

- Dragging a BlockSection inserts that section according to the chosen drop
  behavior.
- Dragging a TagSection inserts the tag section.
- Dragging an Analytic inserts that analytic paragraph.
- Each inserted fragment carries provenance: source file ID, source unit ID,
  source path, and heading path.

Open actions:

- Open source file.
- Open source file at result.
- Insert result at cursor.
- Insert result at speech target later.
- Send result to flow later.

## Crate And Module Layout

Preferred architecture:

```text
crates/
  flowstate-document/
    DB8 VNext persistence
    document IDs
    semantic sections
    section extraction from document model

  flowstate-docx/
    docx to semantic document extraction

  flowstate-flow/
    flow extraction for index units

  flowstate-tub/
    tub config
    catalog
    filesystem scan/watch
    content extraction orchestration
    Tantivy schema/query
    background indexing workers
```

App-layer modules:

```text
src/workspace/
  tub_panel.rs
  toolkit_panel.rs
  toolkit_search.rs
```

The app layer renders and dispatches commands. It should not own indexing
logic, ranking rules, file watching, or DB8 extraction.

## Settings

Add settings for:

- Active tub root.
- Index location.
- Hidden/system file inclusion.
- Maximum indexed file size.
- File type inclusion.
- Search result limit.
- Toolkit panel width.
- Whether to index while on battery, if that becomes relevant.

Index location should default to app data, not inside the tub. This avoids
polluting Dropbox or other synced folders and avoids sync churn. If users later
need portable indexes, add an explicit option.

## Privacy And Safety

The tub index can contain sensitive debate prep. It must be local-only.

Rules:

- No network indexing.
- No cloud search.
- No implicit upload.
- No search telemetry.
- Index lives in local app data by default.
- Clear UI for "Rebuild Index" and "Delete Index".

## Testing Requirements

DB8 VNext:

- v5 to VNext migration.
- VNext read/write round trip.
- IDs preserved across save/load.
- IDs preserved across edit operations.
- Sections correct after edits to heading styles and paragraph boundaries.
- Old files still open.

Tub catalog:

- Initial scan.
- Rename.
- Delete.
- Modify.
- Hidden/generated directory exclusion.
- Symlink/junction loop prevention.
- Watcher fallback behavior.

Search:

- Filename golden ranking fixtures.
- Content golden ranking fixtures.
- BlockSection extraction.
- TagSection extraction.
- Analytic extraction.
- `.docx` extraction.
- `.fl0` extraction.
- Stale index invalidation.

UI:

- Tub/Outline mode switching.
- Tub tree expansion persistence.
- Toolkit panel expansion.
- Search input responsiveness.
- Virtualized result list.
- Lazy mini-window preview loading.
- Drag/drop insertion provenance.

Performance:

- DB8 VNext read/write versus current DB8.
- Initial tub scan on a large synthetic tub.
- Incremental reindex after one file change.
- Filename search latency.
- Content search latency.
- Mini-window preview render cost.

## Implementation Order

1. Add DB8 VNext document IDs and semantic sections.
2. Add migration from current DB8 to VNext.
3. Create `flowstate-tub` crate with catalog, config, and Tantivy schema.
4. Implement initial tub scan and catalog persistence.
5. Implement `.db8` semantic extraction.
6. Implement filename/path Tantivy indexing and query.
7. Add left panel Tub/Outline mode and tub file tree.
8. Move file search entry point to the tub header.
9. Implement content indexing for BlockSection, TagSection, and Analytic.
10. Expand the right toolkit panel and add semantic search UI.
11. Implement virtualized mini-window result previews.
12. Implement drag/drop insertion with provenance.
13. Add `.docx` and `.fl0` extraction.
14. Add file watching and incremental reindexing.

## Open Concerns

These are not blockers, but they need explicit answers during implementation:

- Exact max file size for content indexing.
- Whether `.docx` files should be cached as converted semantic documents.
- Whether a result drag of a BlockSection includes the block heading by default.
- Whether a TagSection drag includes cite/body by default once Card units exist.
- How provenance should be displayed in inserted content.
- How aggressively to index sync-provider conflict files.
- Whether frecency is global or per tub.
- Whether tub root changes should preserve old catalog rows as tombstones or
  drop them immediately.

## Bottom Line

The correct foundation is:

- DB8 VNext for stable document identity and semantic sections.
- A local tub catalog for durable operational state.
- Tantivy for persistent filename/path and semantic content retrieval.
- Lazy, virtualized GPUI previews for toolkit results.
- A left tub tree and right content toolkit that are distinct but backed by the
  same index service.

This architecture is larger than the current file search overlay, but it avoids
throwaway infrastructure and gives Flowstate the right base for Virtual Tub,
Quick Cards, content search, drag/drop evidence insertion, provenance, speech
assembly, and flow interop.
