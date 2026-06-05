# Code Context

## Files Retrieved
1. `gpui-flowtext/src/rich_text/editor/item_sizes.rs` (lines 1-240) - builds `VirtualItem`s and the per-block/item size cache used for scrolling, hit testing, and rendering.
2. `gpui-flowtext/src/rich_text/invisibility.rs` (lines 1-210) - current visibility projection logic; useful template for hiding content while preserving a paragraph shell.
3. `gpui-flowtext/src/document/core.rs` (lines 335-390) - section graph builder (`rebuild_document_sections`) and how `section_level/kind` are derived from paragraph styles.
4. `gpui-flowtext/src/rich_text/editor/mod.rs` (lines 880-980) - `RichTextEditor` state fields; where a collapse map would live.
5. `gpui-flowtext/src/rich_text/editor/layout_access.rs` (lines 1-220) - hit-testing and offset/layout access over virtual items.
6. `gpui-flowtext/src/rich_text/editor/hit_testing.rs` (lines 1-140) - virtual item -> caret mapping; must understand collapsed items.
7. `gpui-flowtext/src/rich_text/editor/chunk_layout.rs` (lines 1-240) - paragraph chunk cache and chunk layout materialization.
8. `gpui-flowtext/src/rich_text/layout/paragraph_layout.rs` (lines 1-240) - actual paragraph/chunk layout construction and visibility-aware shaping.
9. `gpui-flowtext/src/rich_text/editor/formatting.rs` (lines 1-280) - already uses `document.sections` for section-range operations.

## Key Code
- `VirtualItem` today has only `HiddenBlock`, `ParagraphChunk`, `ParagraphRemainder`, `StructuralBlock`.
- `item_sizes.rs:187-240` builds one or more chunk items per paragraph, then a remainder item if the paragraph is incomplete.
- `item_sizes.rs:24-51` and `chunk_layout.rs:3-17` cache validity currently depend on width, edit/layout generation, and invisibility mode only.
- `document/core.rs:336-377` computes nested `DocumentSection`s from paragraph styles and stores `start_paragraph` / `end_paragraph_exclusive`.
- `layout/paragraph_layout.rs:92-168` already has a visibility-aware path that can return `None` and a projected paragraph path that keeps layout stable.
- `layout_access.rs:91-143` and `hit_testing.rs:52-80` map virtual items back to document offsets; collapsed sections will need to short-circuit here.

## Architecture
- `rebuild_document_sections()` turns heading-like paragraph styles (`ParagraphStyle::Custom(slot)` with `theme.custom_paragraph_styles[slot].section_level`) into a nested section tree.
- `RichTextEditor` then builds a flat virtual list of block-sized rows (`VirtualItem`s) in `item_sizes.rs`.
- Paragraphs are rendered and hit-tested by chunk cache entries (`chunk_layout.rs`, `layout_access.rs`, `hit_testing.rs`).
- So the minimal collapse feature can be implemented as a document-section filter applied at virtual-item construction time, plus a few guards in hit testing / caret movement.

## Start Here
Open `gpui-flowtext/src/rich_text/editor/item_sizes.rs` first: it is the narrowest place where a section-collapse filter can remove child paragraph rows while still emitting the heading paragraph as a visible row.

## Implementation plan
- Add editor state for collapsed sections, likely a `HashSet<SectionId>` or `FxHashSet<SectionId>` on `RichTextEditor`.
- Add a helper that, for a paragraph index, answers “is this paragraph inside a collapsed section descendant?” using `document.sections` + `paragraph_id_at`/`paragraph_index_for_id`.
- In `item_sizes.rs::virtual_item_sizes()` and `virtual_item_sizes_for_paragraph_range()`, when the paragraph is a heading style `Custom(0..=3)`:
  - always keep the heading paragraph itself as a `ParagraphChunk`/`ParagraphRemainder` item;
  - skip emitting items for paragraphs whose nearest enclosing section is collapsed;
  - treat skipped descendants like zero-height hidden content so `block_item_ranges` and `height_prefix_index` stay consistent.
- Extend cache invalidation keys to include collapse state (or a cheap version counter), otherwise `item_sizes_cache` / paragraph chunk caches will go stale.
- Update `layout_access.rs` and `hit_testing.rs` so clicks/selection over a collapsed region resolve to the heading paragraph (or the first visible paragraph after it), not to hidden descendants.
- Update caret navigation / paragraph traversal helpers (`paragraph_after_block`, `paragraph_before_block`, maybe vertical movement paths) to skip hidden descendant paragraphs.
- If you want a minimal viable UX, start with a toggle action on the heading paragraph only; no nested UI chrome is required initially.
- Smallest viable hiding mechanism: keep the section heading row; omit all descendant virtual items from the flattened list; do not change document text or paragraph IDs.
