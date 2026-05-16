# Plan: Make Text Editable Without Touching Rendering

## Goal
Add keyboard-driven text editing (insert / delete / arrow-key navigation, including
across line wraps and paragraph boundaries) on top of the existing `RichTextEditor`,
**without modifying any layout or paint code beyond a tiny caret quad**. All editing is
purely model mutation; the existing dirty-notify → re-layout → re-paint pipeline does
the rest.

## Why this is safe for rendering
The renderer in `src/rich_text_element.rs` is a pure function of `Document` +
`LayoutState`. It re-runs every time `cx.notify()` is called. Today,
`apply_run_style_to_selection` (line 272) already mutates the document and works
correctly. We just need to add mutations that change *characters* instead of *styles*,
plus key bindings to drive them. No layout or shaping APIs change.

The fragile part is **caret/selection arithmetic across line wraps**, because lines are
a *layout* concept (`LaidOutLine`, line 487) while `DocumentOffset` (line 218) is a
*model* concept (paragraph + byte offset into the paragraph's concatenated run text).
The plan handles this by:

- For horizontal motion (Left/Right, Backspace, typing): operate purely on
  `DocumentOffset` byte-stepping over grapheme clusters. Soft-wrap line boundaries
  don't matter — moving "right" across a soft-wrap is just incrementing the byte
  offset by one grapheme; the renderer places the caret on the next visual line
  automatically.
- For vertical motion (Up/Down): use `last_layout` to find the caret's current
  `LaidOutLine`, then call the existing `hit_test_x` on the previous/next line.
  `LaidOutLine::hit_test_x` (line 505) is already wrap-aware.

## Architecture

### 1. Key bindings & actions  (`src/rich_text_element.rs`, near the top)
Define `gpui::actions!` for: `MoveLeft`, `MoveRight`, `MoveUp`, `MoveDown`,
`MoveLineStart`, `MoveLineEnd`, `Backspace`, `Delete`, `SelectLeft`, `SelectRight`,
`SelectUp`, `SelectDown`, `SelectAll`. Bind them in `main.rs` via `cx.bind_keys([...])`
with the `RichTextEditor` context.

For raw text insertion, use GPUI's built-in `on_key_down` handler in `render` and
inspect `KeyDownEvent::keystroke.key_char` — that's the idiomatic way to get typed
characters in GPUI without an IME pipeline. If we need IME / composition later we can
swap in `EntityInputHandler`, but that's out of scope for v1.

### 2. Focus & input plumbing  (in `RichTextEditor::render`, line 366)
- Add `.key_context("RichTextEditor")` to the root div so actions resolve.
- Add `.on_action(cx.listener(Self::on_move_left))` etc. for each action.
- Add `.on_key_down(cx.listener(Self::on_key_down))` to capture printable chars and
  `Enter`.

The focus_handle is already tracked (line 369) — no changes needed there.

### 3. Editing primitives (new private fns on `RichTextEditor`)
All operate on `&mut self.document` and call `cx.notify()`:

- `insert_text(&mut self, text: &str, cx)` — if selection non-empty, delete it first;
  then insert `text` into the paragraph at the caret's byte offset, inheriting the
  styles of the run that contains the caret (or the run immediately to the left at run
  boundaries). Caret advances by `text.len()`.
- `delete_selection(&mut self, cx) -> bool` — deletes the normalized selection range,
  joining paragraphs if it spans paragraph boundaries. Returns true if anything was
  deleted. Sets caret to the range start.
- `backspace(&mut self, cx)` — if caret, move one grapheme left then delete one
  grapheme; if selection, `delete_selection`. Joins paragraphs when at start-of-
  paragraph.
- `delete_forward(&mut self, cx)` — symmetric.
- `insert_paragraph_break(&mut self, cx)` — splits the current paragraph at the caret
  into two paragraphs, both with the same `ParagraphStyle`; runs straddling the caret
  are split.

Helpers (private fns, not methods):
- `split_run_at(paragraph: &mut Paragraph, byte: usize)` — ensures a run boundary at
  the given byte, splitting one run into two if needed.
- `run_containing(paragraph: &Paragraph, byte: usize) -> (run_ix, local_byte)`.
- `prev_grapheme_boundary(s: &str, byte: usize) -> usize` /
  `next_grapheme_boundary` — using the `unicode-segmentation` crate.

The existing `apply_style_to_paragraph_range` already does run-splitting math
(line 1577); factor `split_run_at` out of it or write a fresh utility.

### 4. Caret movement
- `move_horizontal(&mut self, dir, extend, cx)` — if `!extend` and selection is a
  range, collapse to the appropriate end; otherwise step `head` by one grapheme,
  crossing paragraph boundaries when at paragraph start/end.
- `move_vertical(&mut self, dir, extend, cx)` — read `self.last_layout`. Walk its
  `paragraphs[].lines[]` to locate the line whose `[start_byte, end_byte]` contains
  `head.byte` in `head.paragraph`. Find the adjacent line (in the same paragraph, or
  the last/first line of the prev/next paragraph). Compute target x as the x within
  the current line of the caret (use `segment.shaped.x_for_index` — the inverse of
  the existing `closest_index_for_x` at line 509). Then call
  `target_line.hit_test_x(target_x)` to get the new byte. Preserve a "goal x" across
  consecutive vertical moves (`goal_x: Option<Pixels>` on `RichTextEditor`, reset on
  any horizontal or mouse action).
- `MoveLineStart` / `MoveLineEnd` — same line-locating walk, then jump to
  `line.start_byte` or `line.end_byte`. **This is how we get correct soft-wrap-aware
  Home/End** without changing the renderer.

### 5. Caret painting (confirmed: minimal addition)
Add a 1px-wide black quad at the caret position in `paint_layout` when the selection
is a caret and the editor is focused. ~5 lines, very localized; does not touch
existing paint logic for glyphs, highlights, underlines, or borders.

### 6. State additions to `RichTextEditor`
- `goal_x: Option<Pixels>` — for stable vertical motion across wrapped lines.
- That's it.

### 7. Crossing line wraps safely (the user's concern)
- **Soft wraps** (a single paragraph wrapped into multiple `LaidOutLine`s): the model
  has no concept of soft wrap — it's just one byte stream. Editing operates on bytes;
  the layout pass re-wraps automatically on the next `notify`. Zero coupling between
  editing logic and `wrap_lines`. ✅
- **Hard paragraph breaks**: handled explicitly by `insert_paragraph_break` /
  paragraph-joining in delete/backspace. The selection range type already supports
  cross-paragraph ranges (proven by `apply_run_style_to_selection`, lines 277–293). ✅
- **Caret at exactly a wrap point**: `DocumentOffset { byte: N }` is ambiguous between
  "end of line k" and "start of line k+1". For v1 bias to "start of next line"
  (matches Word). `hit_test_x` at line 505 already does this implicitly.

## Confirmed decisions
- **Caret painting:** Add minimal caret quad in `paint_layout`.
- **Enter behavior:** Splits paragraph, new paragraph inherits current
  `ParagraphStyle`.
- **Grapheme stepping:** Use `unicode-segmentation` crate for correct one-visible-
  character-per-keystroke behavior on combining marks and compound emoji.

## Files to change
- `Cargo.toml`: add `unicode-segmentation = "1"`.
- `src/rich_text_element.rs`: add actions, key handlers, edit primitives, movement
  methods, `goal_x` field, and the small caret quad in `paint_layout`. **No changes**
  to `build_layout`, `wrap_lines`, `shape_line`, `paint_line_text`, or the styling
  pipeline.
- `src/main.rs`: register key bindings.

## Out of scope (v1)
- IME / dead keys / composition.
- Undo/redo (straightforward to add later: snapshot `Document` per edit).
- Clipboard (copy/paste).
- Caret blink animation (static caret only).

## Verification
- Run `cargo check` after each meaningful edit; fix errors before continuing.
- Manual checks: type across a soft-wrap boundary; backspace across a paragraph
  boundary; Up/Down preserves x; Home/End jump to visual line ends, not paragraph
  ends; selection styling (existing `apply_run_style_to_selection`) still works on
  edited text.
