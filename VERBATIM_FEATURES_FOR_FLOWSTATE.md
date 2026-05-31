# Verbatim Feature Inventory And Flowstate Implementation Plan

This document inventories the feature surface of `verbatim/`, which is a
Microsoft Word and Excel based debate utility, then maps each feature to a
Rust/GPUI implementation strategy for Flowstate.

The source pass covered:

- `verbatim/README.md` and `verbatim/desktop/README.md`
- `verbatim/desktop/CHANGELOG.md`
- `verbatim/docs/verbatim/**`
- Word VBA in `verbatim/desktop/template/src/*.bas` and forms
- Excel Flow VBA in `verbatim/desktop/flow/src/*.bas` and forms
- The Tauri timer in `verbatim/timer`
- Flowstate architecture notes and current Rust crates under `crates/`

This plan intentionally does not assume a debate event, format, circuit, or
speech order. Any event-specific defaults should come from a user-selected
profile, imported document metadata, or an explicit setup prompt.

## Implementation Principles

- Keep debate document semantics in the model, not only in rendered font runs.
  Word Verbatim often encodes behavior through styles, direct formatting, and
  macros. Flowstate should encode cards, cites, tags, headings, condensed text,
  emphasis, highlights, warrant comments, and speech provenance as model data
  where possible.
- Route every operation through command IDs and undoable transactions. Verbatim
  macros frequently mutate the active Word selection directly. Flowstate should
  expose the same speed, but with predictable undo, range previews for broad
  transforms, and command palette/keymap support.
- Prefer `gpui-component` controls where applicable: button, toggle, input,
  select, menu, popover, dialog, list, table, tree, tabs, sidebar, tooltip,
  checkbox, switch, slider, color picker, and virtualized scrolling surfaces.
- Put pure document and flow operations in crates, not UI modules. Existing
  natural homes are `flowstate-document`, `flowstate-docx`, and
  `flowstate-flow`. New large surfaces should become crates once they are stable
  enough to avoid a monolithic app layer.
- Make long-running work asynchronous and cancellable. Search indexing, docx
  scanning, OCR, virtual tub refresh, caselist upload, and large formatting
  cleanup should not block GPUI rendering.
- Preserve Word import/export compatibility without copying Word's internal
  constraints. Where Verbatim uses direct font size hacks or hidden text,
  Flowstate should prefer reversible semantic state and project that state into
  `.docx` only at export time.

## Recommended New Modules Or Crates

- `flowstate-debate`: debate range resolution, card parsing, cite parsing,
  Verbatim-style format transforms, statistics, speech assembly helpers, and
  snippet serialization. This could start as app modules and move to a crate
  once interfaces settle.
- `flowstate-search`: app-owned file and content index for `.db8`, `.docx`,
  `.rtf`, `.txt`, and `.fl0`. The repo already depends on `fff-search`; the
  exact division should follow current `src/file_search.rs` ownership.
- `flowstate-integrations`: optional network and OS integrations such as
  openCaselist, share rooms, OCR backends, external search tools, and audio.
  Keep this separate so core editing remains fast and offline-friendly.
- `flowstate-timer`: pure timer state, presets, alert scheduling, and window
  preferences if the timer returns to scope later.

## Current Flowstate Coverage

Flowstate already has several primitives that should be reused rather than
recreated:

- Rich document model crates: `flowstate-document` and `flowstate-docx`.
- Debate paragraph commands for Pocket, Hat, Block, Tag, Analytic, and Undertag.
- Run styling commands for Cite, Underline, Emphasis, Condensed,
  Ultracondensed, highlights, strikethrough, images, tables, and equations.
- A native flow crate, `flowstate-flow`, with `.fl0` model/actions/history,
  presets, style metadata, and persistence.
- A GPUI flow ribbon using `gpui-component` controls for setup, templates,
  speaker switching, undo/redo, response insertion, extension, deletion, fold,
  and other native flow actions.
- App settings and workspace settings infrastructure.
- Search-related dependencies and app modules.

The Verbatim work below should extend these systems, not bypass them.

## Feature Inventory And Recommendations

### 1. Debate Document Structure

Verbatim uses a strict Word style hierarchy:

- Pocket: `Heading 1`, shortcut F4.
- Hat: `Heading 2`, shortcut F5.
- Block: `Heading 3`, shortcut F6.
- Tag: `Heading 4`, shortcut F7.
- Cite: F8.
- Underline: F9.
- Emphasis: F10.
- Highlight: F11.
- Clear to Normal: F12.

Recommendation: implement and keep as core.

Flowstate already has paragraph and run style commands close to this model.
The remaining work is to make these styles first-class in document navigation,
range selection, speech assembly, statistics, snippets, search indexing, and
export.

Implementation notes:

- Keep Pocket, Hat, Block, Tag, and Cite in the document semantic model.
- Ensure imported `.docx` Verbatim styles map to Flowstate's styles.
- Ensure exported `.docx` can round-trip back to Word Verbatim where possible.
- Keep shortcut defaults configurable rather than hard-coded.
- Expose styles through the existing ribbon and command palette.

Improvement over Verbatim:

- Show structure errors inline: card without cite, cite without tag, empty
  headings, fake tags, and unsupported style mixtures.
- Provide a live outline with drag/drop, keyboard movement, and search.
- Make each card optionally addressable by a stable ID for source provenance,
  snippets, speech docs, and future collaboration.

### 2. Analytic And Undertag Styles

Verbatim explicitly declined to add a separate Analytics style and declined
an Undertags style because its macro parsing assumes a compact tag/cite/card
structure.

Flowstate currently exposes Analytic and Undertag commands.

Recommendation: keep compatibility, but do not make these styles part of the
paperless core until the user chooses how they should behave.

Options:

- Keep as ordinary paragraph styles.
  - Pros: simple, compatible with existing Flowstate commands, useful for users
    who want visible analytics.
  - Cons: speech assembly, stats, and send-to-flow logic need explicit rules.
- Treat as annotations attached to cards or blocks.
  - Pros: cleaner document structure, easier to hide/show, better provenance.
  - Cons: less familiar to Word users and harder to export to a flat `.docx`.
- Do not expose prominently by default.
  - Pros: matches Verbatim's proven parsing assumptions.
  - Cons: gives up a possible Flowstate differentiator.

Recommendation: support them as optional semantic paragraphs with explicit
settings for whether they count in stats, speech send, snippets, and flow send.

### 3. Range Resolution

Many Verbatim commands operate on:

- The selected text if text is selected.
- The current card or heading when the cursor is inside one.
- A larger heading range when the cursor is on a heading.
- The whole document when the cursor is at the top or no local range applies.

Recommendation: implement as a shared range resolver.

Implementation:

- Add a `DebateRangeResolver` that returns typed ranges such as selection,
  current card, current heading subtree, current block, current hat, current
  pocket, whole document, and active flow selection.
- Every formatting macro, speech send, stat count, and cleanup command should
  call this resolver rather than reimplementing selection heuristics.
- The resolver should be pure and covered by tests using synthetic document
  structures.

Improvement over Verbatim:

- Add a small range preview in destructive commands: "Current card", "Current
  block", "7 cards", or "Whole document".
- Make broad transforms single undo transactions.

### 4. Paste Text And Condense On Paste

Verbatim's F2 Paste Text pastes unformatted text. If Condense On Paste is
enabled, it immediately condenses the pasted card text.

Recommendation: implement.

Implementation:

- Add a paste mode command that reads plain text from the clipboard, normalizes
  whitespace, and inserts it as plain document content.
- If the setting is enabled, run the same condense transform used by F3.
- Preserve links to source clipboard metadata only if that can be done without
  surprising the user.

Options:

- Plain text only.
  - Pros: predictable, fast, closest to Verbatim's paste-text command.
  - Cons: loses source formatting that may occasionally be useful.
- Sanitized rich paste.
  - Pros: can preserve italics, small caps, citations, and links.
  - Cons: more complicated and can import unwanted web styling.

Recommendation: default to plain text, with a separate "Paste And Sanitize"
command later if users need it.

### 5. Condense, Pilcrows, And Paragraph Integrity

Verbatim's Condense command removes or replaces paragraph breaks inside cards,
normalizes spacing, removes tabs and breaks, and can insert visible pilcrow-like
markers so paragraph boundaries remain visible after condensing. It also has
Uncondense and Remove Pilcrows commands.

Recommendation: implement differently.

Options:

- Physical text transform.
  - Pros: closest to Verbatim and easy to export to `.docx`.
  - Cons: destructive, fragile around undo, less suitable for collaborative
    editing, and can lose source structure.
- Semantic condense state.
  - Pros: reversible, better for WYSIWYG rendering, faster to toggle, and can
    export to Word formatting only when needed.
  - Cons: requires renderer support and export projection.
- Hybrid semantic state plus explicit "bake condense" command.
  - Pros: best editing model while preserving compatibility.
  - Cons: more states to explain and test.

Recommendation: use the hybrid model. Store paragraph integrity and condensed
state semantically. Render compactly in GPUI. Export or "bake" to Verbatim-style
text only on request.

Implementation details:

- Settings: paragraph integrity, visible paragraph markers, condense on paste.
- Commands: Condense, Condense Without Markers, Condense With Markers,
  Uncondense, Remove Markers.
- Preserve omission notes according to a setting, matching Verbatim's
  ShrinkOmissions behavior.

Improvement over Verbatim:

- Render paragraph markers as UI glyphs, not inserted document characters,
  unless the user explicitly bakes them into text.
- Keep original paragraph boundaries in model history.

### 6. Shrink And Unshrink

Verbatim shrinks non-underlined card text through a font-size cycle, down to a
minimum of 4pt, with commands to shrink all, unshrink current card, and unshrink
all cards.

Recommendation: implement differently.

Options:

- Direct font-size mutation.
  - Pros: direct `.docx` compatibility.
  - Cons: hard to reason about, hard to theme, and can degrade readability.
- Semantic density styles such as Condensed and Ultracondensed.
  - Pros: already aligns with Flowstate run styles, reversible, themeable, and
    renderer-controlled.
  - Cons: export needs mapping to Word font sizes.

Recommendation: use semantic density styles. Do not support shrinking below
4pt. Provide an export mapping for Word users who need the old visual result.

Commands:

- Shrink current card.
- Shrink selected range.
- Shrink all cards.
- Unshrink current card.
- Unshrink all.
- Preserve omission notes setting.

Improvement over Verbatim:

- Add "spoken text density" rendering that keeps card text readable on screen
  while still exporting compactly.

### 7. Underline, Highlight, Emphasis, And Clear

Verbatim has fast commands for underline, emphasis, highlight, and clear:

- Underline toggles underline style or clears it.
- Underline Mode applies underline as the selection changes.
- Highlight applies a configured color.
- Emphasis adds configured bold/italic/box formatting.
- Clear resets selection/current card to Normal.
- Standardize Highlighting normalizes highlight colors.
- Standardize Highlighting With Exception preserves a configured exception
  color.
- Remove Emphasis converts emphasis to underline.
- Remove Non-Highlighted Underlining clears underlining that is not also
  highlighted.

Recommendation: implement.

Implementation:

- Use existing run style commands where possible.
- Add a modal underline/highlight state in editor state, not global mutable
  macros.
- Store configured emphasis style in app settings.
- Add cleanup transforms in `flowstate-debate` or `flowstate-document`.

Options for Underline Mode:

- Mode toggled in ribbon.
  - Pros: familiar and visible.
  - Cons: can surprise users if left on.
- Press-and-hold command.
  - Pros: less accidental formatting.
  - Cons: less like Verbatim.
- Both.
  - Pros: covers speed and safety.
  - Cons: extra settings.

Recommendation: support a visible toggle with an obvious active state and an
optional temporary keybinding.

Improvement over Verbatim:

- Show active modes in the status bar.
- Prevent mode changes from altering headings and cites unless explicitly
  allowed.

### 8. Auto Emphasis And Auto Underline

Verbatim can automatically emphasize first letters/words and can auto-underline
card text based on tag words and synonyms.

Recommendation: implement as optional assistive commands, not automatic
background mutation.

Options:

- Simple lexical scorer, similar to Verbatim.
  - Pros: fast, local, deterministic, easy to explain.
  - Cons: shallow and can miss warrants or overfit tag wording.
- Configurable keyword scorer with stopwords and stemming.
  - Pros: still local, better accuracy, testable.
  - Cons: more settings.
- Embedding or ML assisted scoring.
  - Pros: potentially better warrant selection.
  - Cons: heavier, less deterministic, privacy concerns, and not necessary for
    first implementation.

Recommendation: start with a local configurable scorer. Never send evidence
text to a network service without explicit user opt-in.

Improvement over Verbatim:

- Provide a preview of proposed underlines before applying to a whole card or
  block.
- Learn per-document stopwords from tags and headings.

### 9. Citation Formatting

Verbatim expects one-line citations, usually only last name/date in Cite style.
It provides:

- Cite style command.
- Duplicate previous cite.
- Auto Format Cite.
- Reformat All Cites.
- Get From Cite Creator.
- Warnings and docs around predictable citation formatting.

Recommendation: implement core cite tools; make external Cite Creator optional.

Implementation:

- Add a citation parser that extracts author/date/page/source metadata when
  available but preserves original text.
- Add commands:
  - Mark Cite.
  - Duplicate Previous Cite.
  - Normalize Current Cite.
  - Normalize All Cites.
  - Select Cite.
  - Show Cite Diagnostics.
- Export normalized cite style to `.docx`.

Options for Cite Creator integration:

- External helper integration.
  - Pros: compatible with existing user workflows.
  - Cons: platform-specific, brittle, and outside Flowstate control.
- Native citation form.
  - Pros: cross-platform, testable, no browser automation.
  - Cons: requires building a new workflow.

Recommendation: build native citation cleanup first. Add external helper support
only through a plugin or configurable command.

Improvement over Verbatim:

- Store cite diagnostics without changing visible text.
- Support multiple citation formats per workspace profile.

### 10. Formatting Cleanup Commands

Verbatim includes many document cleanup commands:

- Fix Fake Tags.
- Convert Analytics To Tags.
- Fix Formatting Gaps.
- Convert To Default Styles.
- Remove Extra Styles.
- Auto Number Tags.
- De-Number Tags.
- Insert Header.
- Remove Emphasis.
- Remove Non-Highlighted Underlining.
- Remove Blanks.
- Remove Pilcrows.
- Remove Hyperlinks.
- Remove Bookmarks.
- Update Styles.
- Select Similar Formatting.
- Show All Formatting.

Recommendation: implement most as audit/fix commands.

Implementation:

- Build a "Document Audit" panel listing detected issues and one-click fixes.
- Keep individual commands for speed and keybindings.
- Use typed transforms and diagnostics rather than direct selection mutation.
- Add before/after counts for destructive transforms.

Feature-specific notes:

- Fix Fake Tags: convert visually tag-like normal paragraphs to Tag style.
- Convert Analytics To Tags: only if the user enables analytic conversion.
- Fix Formatting Gaps: normalize unintended formatting gaps inside card text.
- Convert To Default Styles: map imported Word styles to Flowstate semantics.
- Remove Extra Styles: strip unknown imported styles when safe.
- Auto Number Tags and De-Number Tags: implement, but store numbering as a
  view/export setting where possible.
- Insert Header: implement as export/page layout metadata, not always visible
  body text.
- Remove Hyperlinks/Bookmarks: implement for imported `.docx`.

Improvement over Verbatim:

- Make cleanup previewable and reversible.
- Show exactly which cards or headings will change.

### 11. Comments And Warrants

Verbatim has commands for adding warrant comments and deleting all warrants.

Recommendation: implement.

Implementation:

- Add comment/annotation support in the document model if not already complete.
- Provide commands:
  - Add Warrant Comment.
  - Show/Hide Comments.
  - Delete Warrant Comments.
  - Export Comments To `.docx`.

Improvement over Verbatim:

- Support named comment categories: warrant, cite issue, formatting issue,
  speech note, and research TODO.

### 12. Paperless Speech Documents

Verbatim can create speech documents, autosave them, name them by round/speech,
and send selected text/current card/current heading to a speech document. If the
active document is a speech document, Send To Speech inserts a red timestamp
marker instead.

Recommendation: implement as a core Flowstate workflow.

Implementation:

- Add an explicit "speech target" document concept in workspace state.
- Commands:
  - New Speech Document.
  - Set Active Speech Target.
  - Send To Speech.
  - Send To Speech End.
  - Insert Speech Marker.
  - Clear Speech Target.
- Autosave directory and naming templates belong in settings.
- Speech documents should retain source provenance for sent cards.

Options for speech target detection:

- Filename heuristic such as documents containing "Speech".
  - Pros: easy import from Verbatim habits.
  - Cons: brittle and ambiguous.
- Explicit workspace metadata.
  - Pros: reliable and UI-visible.
  - Cons: needs migration and UI.
- Hybrid.
  - Pros: familiar import behavior plus reliable native state.
  - Cons: more implementation work.

Recommendation: hybrid. Use filename heuristics only to suggest a target, then
store explicit metadata.

Improvement over Verbatim:

- Add card provenance links: jump from speech text back to the source card.
- Detect duplicate sends.
- Support side-by-side source and speech panes inside the same GPUI workspace.

### 13. Organizing Cards And Headings

Verbatim heavily relies on Word's Navigation Pane and keyboard commands:

- Move heading up.
- Move heading down.
- Move heading to bottom.
- Delete current heading.
- Select heading and content.
- Select current card.
- Drag/drop headings in the outline.

Recommendation: implement as core editor and outline behavior.

Implementation:

- The outline pane should understand Pocket/Hat/Block/Tag hierarchy.
- Drag/drop should move whole subtrees.
- Keyboard commands should use the shared range resolver.
- Moves should be structural operations on the document model, not cut/paste.

Improvement over Verbatim:

- Show card counts and speech-time estimates per heading.
- Provide drop previews and invalid-drop feedback.

### 14. Auto-Open Folder

Verbatim watches a configured folder and opens new `.doc`, `.docx`, and `.rtf`
files as they arrive.

Recommendation: implement later if users still use browser download workflows.

Options:

- OS file watcher.
  - Pros: immediate and efficient.
  - Cons: platform edge cases and permissions.
- Polling.
  - Pros: simpler and robust.
  - Cons: less efficient and delayed.

Recommendation: OS watcher with polling fallback. Keep it off by default and
surface active monitoring clearly.

### 15. Combine Documents

Verbatim can combine selected recent/manual documents into one new document,
inserting each as a new Pocket titled by filename or round metadata.

Recommendation: implement.

Implementation:

- Add an import wizard that accepts `.docx`, `.rtf`, `.txt`, and Flowstate docs.
- Each source becomes a Pocket by default.
- Allow user to choose whether imported top-level headings are preserved or
  nested under the new Pocket.

Improvement over Verbatim:

- Show a pre-import outline.
- Deduplicate identical cards by cite/tag hash if the user asks.

### 16. Virtual Tub

Verbatim Virtual Tub creates a JSON/bookmark index of a configured folder and
lets users insert heading blocks without opening each source file. It supports
files and one subfolder level, is intended for a small number of files, and
indexes down to Block/Heading 3 rather than individual cards.

Recommendation: implement, but as a native indexed evidence library.

Options:

- Verbatim-style generated JSON plus bookmarks.
  - Pros: simpler and close to existing behavior.
  - Cons: shallow, fragile, and tied to `.docx` bookmark mechanics.
- Native app-owned library index.
  - Pros: supports `.db8`, `.docx`, `.rtf`, `.txt`, `.fl0`, card-level search,
    previews, and faster inserts.
  - Cons: larger implementation.
- Snippet-only replacement.
  - Pros: much simpler.
  - Cons: does not replace tub browsing and evidence library workflows.

Recommendation: build a native library index. A first version can index only
headings and cards from configured folders, then expand to full-text search and
preview.

Improvement over Verbatim:

- Index individual cards, not only headings.
- Add live file watching, stale index warnings, and preview before insert.
- Preserve source file, heading path, and import time as provenance.

### 17. Quick Cards

Verbatim Quick Cards saves selected cards/blocks under user shortcuts in one of
ten profiles, then inserts them by typing a shortcut or using a menu. It stores
the snippets in Word building blocks and warns that too many can slow Word.

Recommendation: implement.

Options:

- Store snippets in app settings.
  - Pros: simple.
  - Cons: can bloat settings and is awkward for large rich fragments.
- Store snippets as files in workspace or app data.
  - Pros: portable, inspectable, sync-friendly.
  - Cons: more file management.
- Store snippets in SQLite or another app database.
  - Pros: fast lookup, scalable, metadata-rich.
  - Cons: more infrastructure.

Recommendation: use a small app-owned snippet store with stable IDs and rich
fragment serialization. Export/import as portable files.

Features:

- Ten profiles for Verbatim compatibility.
- Shortcut insertion.
- Command palette lookup.
- Ribbon/menu insert.
- Delete one, delete all, rename, profile switch.
- Preview card before insert.

Improvement over Verbatim:

- Add tags, fuzzy search, source provenance, and per-workspace snippet packs.

### 18. Search

Verbatim search options include:

- Mac Spotlight content search.
- Windows SystemIndex content search.
- Optional Everything Search plugin.
- A ribbon search box returning up to 25 matching documents.
- Open containing folder or external search app.

Recommendation: implement as a native app search index with optional external
backends.

Options:

- OS search only.
  - Pros: fast setup, uses platform indexes.
  - Cons: inconsistent, may miss Flowstate formats, limited semantic filtering.
- App-owned index.
  - Pros: consistent, can index card structure, cites, tags, flow cells, and
    snippets.
  - Cons: requires indexing work and storage.
- External tool integration.
  - Pros: useful for users already relying on Everything or similar tools.
  - Cons: platform-specific and not core.

Recommendation: app-owned index first, external search as optional integration.

Improvement over Verbatim:

- Search by tag, cite, author, source file, heading path, date, side, speech,
  and flow metadata.
- Insert from search results without opening the source document.

### 19. OCR

Verbatim OCR uses Tesseract on Mac and Capture2Text on Windows, with custom
path overrides. Windows flow uses the snipping tool and clipboard.

Recommendation: implement as an optional integration, not a core dependency.

Options:

- OS OCR APIs.
  - Pros: native install experience and often good performance.
  - Cons: platform-specific behavior.
- Tesseract CLI or bindings.
  - Pros: cross-platform and familiar from Verbatim.
  - Cons: installation and language data can be painful.
- External command plugin.
  - Pros: flexible for power users.
  - Cons: less polished.

Recommendation: start with external command configuration plus a clean insertion
pipeline. Add native OS OCR later if demand justifies it.

Improvement over Verbatim:

- OCR into a preview panel first, then insert as plain text, cite text, or a new
  card.

### 20. Timer

Verbatim includes a separate Tauri timer with:

- Speech, affirmative/pro/government prep, and negative/con/opposition prep
  timers.
- Presets for multiple event families.
- Constructive, rebuttal, cross-examination, and prep durations.
- Start/pause/stop/reset.
- Alerts at configured remaining times.
- Flash/audio alert types.
- Transparent, always-on-top, autoshrinking mini window.
- Side-name modes.
- Persistent current timer state and window size.

Current Flowstate notes defer the timer.

Recommendation: defer until core editing and flow are stable, but keep the
state model separate so it can be added cleanly.

Options:

- Embedded GPUI timer panel.
  - Pros: integrated with workspace, presets, speech docs, and flow.
  - Cons: less useful when another app is fullscreen.
- Separate GPUI helper window.
  - Pros: closer to Verbatim always-on-top timer.
  - Cons: more window-management work.
- External helper app.
  - Pros: independently usable.
  - Cons: more packaging and state sync.

Recommendation: separate GPUI helper window backed by a pure `flowstate-timer`
state crate when timer scope returns.

Source issue to avoid porting:

- The timer default settings use `alertTypes.audio`, while the runtime checks a
  `beep` key. A native port should normalize alert setting names before adding
  compatibility import.

### 21. Statistics And Reading Time

Verbatim's stats popup estimates speech time from highlighted words plus tag
words, card count, user WPM, and event defaults. It auto-refreshes and colors
time estimates based on speech length.

Recommendation: implement.

Implementation:

- Add a live stats panel for the selected range, current heading, and active
  speech document.
- Count highlighted words, tag words, cites, cards, analytics if enabled, and
  total visible words.
- Use user WPM and selected debate style presets. Ask the user at setup rather
  than assuming a default format.

Improvement over Verbatim:

- Show spoken-only, evidence-only, and all-visible estimates.
- Cache counts incrementally so large documents stay responsive.
- Show per-heading time budgets in the outline.

### 22. Audio Recording

Verbatim can record audio to a configured folder using platform-specific APIs.

Recommendation: defer or implement as a plugin.

Options:

- Native cross-platform audio capture.
  - Pros: polished once complete.
  - Cons: permissions, device selection, encoding, and privacy concerns.
- External command integration.
  - Pros: flexible and lower implementation cost.
  - Cons: inconsistent UX.

Recommendation: defer from core. If implemented, require visible recording state
and explicit save location.

### 23. Invisibility Mode

Verbatim Invisibility Mode hides non-highlighted body text, keeps headings and
cites visible, and disables spelling/grammar while active. The desktop README
says destructive invisibility modes that delete content will not be added.

Recommendation: implement as a reversible view filter only.

Implementation:

- Add "Spoken Text View" or "Highlighted View" that dims or hides unspoken body
  text in rendering.
- Never delete content.
- Ensure export is unaffected unless the user explicitly exports a filtered
  speech copy.

Improvement over Verbatim:

- Provide intensity options: dim, collapse, or hide.
- Show hidden word counts and warnings when copying from a filtered view.

### 24. Window Layout And Views

Verbatim can arrange Word windows with source docs on one side and speech docs
on the other, switch windows, set Draft/Web view, zoom, and split percentages.

Recommendation: implement through Flowstate workspace layout.

Options:

- Internal split panes.
  - Pros: cross-platform, native to GPUI, better than OS window juggling.
  - Cons: less useful if users want independent OS windows.
- Detachable windows.
  - Pros: supports multi-monitor workflows.
  - Cons: more state and window lifecycle complexity.

Recommendation: internal splits first, detachable speech/timer windows later.

Features:

- Source/speech split.
- Flow/source split.
- Saved workspace layouts.
- Zoom controls.
- Focus next document / focus speech target commands.

### 25. Keyboard Shortcuts And Keymap UI

Verbatim allows F2-F12 command remapping, resets defaults, and includes a long
shortcut list.

Recommendation: implement through Flowstate's command system.

Implementation:

- Keep default Verbatim-compatible shortcuts as a preset.
- Add keymap settings UI with conflict detection.
- Support profile import/export.
- Offer per-workspace overrides for teams with custom workflows.

Improvement over Verbatim:

- Let users search by command, key, or category.
- Show shortcuts in ribbon tooltips automatically from the command registry.

### 26. Settings

Verbatim settings cover:

- Profile: name, school, college/K12, event, WPM, Tabroom disable.
- Admin: always-on, auto update styles, suppress checks, first-run, setup,
  troubleshooter, tutorial, import/export settings, import/export custom code.
- View: default view, navigation-pane cycle startup, window split percentages,
  ribbon group visibility.
- Paperless: autosave speech, autosave directory, strip speech when sharing,
  search directory, auto-open directory, audio directory.
- Styles: fonts, sizes, style-specific formatting, spacing.
- Format: shrink omissions, paragraph integrity, markers, condense on paste,
  auto underline emphasis, highlight exception color.
- Keyboard: macro mapping.
- Virtual Tub: path and refresh prompts.
- Caselist: open-source upload default, process-cites default, login/logout.
- Plugins: timer, OCR, and search paths.
- About: version, update check, help links.

Recommendation: implement Flowstate settings in native app settings, grouped by
editing, paperless, style, keymap, flow, integrations, and privacy.

Implementation:

- Use `gpui-component` settings forms, tabs, selects, toggles, sliders, inputs,
  and color pickers.
- Support import/export as human-readable config.
- Keep integration tokens in OS credential storage where available, not plain
  settings files.

Source issue to avoid porting:

- Verbatim settings export appears to write `SpeechPct` from the docs split
  spinner instead of the speech split spinner. Flowstate should test settings
  import/export round trips.

### 27. Ribbon And Toolbars

Verbatim has ribbon groups for Speech, Organize, Format, Paperless, Tools,
View, Caselist, and Settings. Users can hide individual groups.

Recommendation: implement as native Flowstate ribbons and contextual panels.

Implementation:

- Keep editing and flow ribbons separate where context differs.
- Add group visibility settings.
- Use icon buttons and tooltips where possible.
- Show active toggles for Underline Mode, Paragraph Integrity, markers,
  recording, auto-open folder, and filtered view.

Improvement over Verbatim:

- Let the ribbon adapt to selected range: card commands when inside a card,
  heading commands when on a heading, flow commands in `.fl0`, and speech
  commands when a target exists.

### 28. openCaselist And Tabroom Upload

Verbatim logs into Tabroom/openCaselist, lists current rounds and caselist
targets, and uploads the current document as cites, open-source file, or both.
It can process a document into cite entries by largest heading.

Recommendation: implement after explicit integration approval, because this
involves credentials, network requests, and evolving external endpoints.

Options:

- Built-in HTTP integration.
  - Pros: best UX and automation.
  - Cons: endpoint drift, token storage, privacy/security burden.
- Browser handoff / export package.
  - Pros: lower security surface.
  - Cons: less automated.
- Plugin integration.
  - Pros: isolates network code and makes it optional.
  - Cons: extra plugin management.

Recommendation: plugin or optional integration crate with explicit user opt-in.

Implementation notes:

- Store tokens securely.
- Preview exactly what will be uploaded.
- Keep original file upload and processed cite upload separate.
- Preserve user confirmation before network upload.
- Allow upload-disabled builds for users who need offline-only setups.

Improvement over Verbatim:

- Add deterministic cite processing reports before upload.
- Keep upload logs with timestamps and target metadata.

### 29. share.tabroom-Style Sharing

Verbatim can upload a base64 document to a share room, use existing round room
codes, create random or custom rooms, and optionally strip "Speech" from the
filename.

Recommendation: implement only as an optional sharing integration.

Options:

- Direct share service integration.
  - Pros: close to Verbatim workflow.
  - Cons: network and endpoint dependency.
- Local export package.
  - Pros: private and robust.
  - Cons: less convenient during rounds.
- Generic "share provider" plugin interface.
  - Pros: future-proof.
  - Cons: more architecture.

Recommendation: export package first, share integration later.

Improvement over Verbatim:

- Show file size, stripped filename, and expiration/visibility if the service
  exposes it.
- Support QR code or copy-link actions from a share dialog.

### 30. USB Copy

Verbatim can copy the active document to a USB drive.

Recommendation: implement as a generic "Copy To..." export action.

Implementation:

- Let the user select destination.
- Remember recent export destinations.
- Warn if the file has unsaved changes.

### 31. Setup, Troubleshooting, And Install Checks

Verbatim checks template location, duplicate templates, Mac script install,
default save format, plugin paths, and known Word add-in issues.

Recommendation: replace Word-specific checks with Flowstate health checks.

Flowstate checks should include:

- App data directory writable.
- Autosave directory configured and writable.
- Snippet store readable.
- Search index healthy.
- `.docx` import/export available.
- Optional integration paths valid.
- Keymap conflicts.
- Credential store available for integrations.

Do not port:

- Word template location checks.
- Macro security workarounds.
- Word default save format checks.
- Word add-in disablement.

### 32. Plugin System

Verbatim has PC plugin installers for timer, OCR, Everything Search,
Cite Creator helper, and navigation pane cycling. It supports custom paths for
some external tools.

Recommendation: implement a small integration registry, not a macro plugin
system.

Options:

- Hard-code integrations.
  - Pros: easiest.
  - Cons: grows messy and platform-specific.
- Configurable external commands.
  - Pros: simple and flexible.
  - Cons: weaker UX and error handling.
- Real plugin API.
  - Pros: extensible.
  - Cons: significant design surface.

Recommendation: start with configurable external commands plus typed built-in
integration slots. Design a real plugin API only after the core UX settles.

### 33. Flow: Native Replacement For Verbatim Flow

Verbatim Flow is an Excel template with debate-specific flow sheets, shortcuts,
row/cell operations, highlighting, evidence borders, grouping, quick analytics,
speech send, and Word interop.

Flowstate already has a native `.fl0` model and GPUI flow ribbon. This is the
right direction.

Recommendation: keep replacing Excel Flow natively.

Features to preserve:

- Add aff/pro/government flow.
- Add neg/con/opposition flow.
- Add cross-examination flow.
- Delete current flow.
- Delete empty flows.
- Auto-fill scouting info from flow titles or metadata.
- Insert response above/below/current.
- Insert row/cell equivalent.
- Delete selected argument.
- Move selection up/down.
- Go to bottom.
- Merge adjacent cells or arguments.
- Toggle highlighting.
- Toggle evidence.
- Toggle group.
- Extend argument across future speeches, optionally with arrows.
- Enter Cell / edit selected argument.
- Insert mode where Enter creates a line break.
- Paste unformatted.
- Zoom and layout controls.
- Switch speech cursor across flows.
- Word/source to flow send.
- Flow to speech send.
- Quick Analytics profiles and shortcut insertion.
- Configurable font size, row height, column width, speech names, and automatic
  flow labels.

Options for model shape:

- Spreadsheet-like grid.
  - Pros: closest to Excel Flow and easy for row/column mental model.
  - Cons: less flexible for nested arguments and WYSIWYG integration.
- Current native argument tree.
  - Pros: better semantic model, easier folding, linking, and CRDT future.
  - Cons: users may miss exact spreadsheet gestures.
- Hybrid tree plus row/column projection.
  - Pros: preserves semantics while rendering a familiar flow.
  - Cons: more renderer and selection complexity.

Recommendation: keep the native tree and add row/column projection commands
where needed.

Implementation details:

- Add explicit evidence and group styles to `flowstate-flow` if they are not
  already semantic actions.
- Make Quick Analytics use the same snippet engine as Quick Cards, with a flow
  fragment type.
- Implement source-to-flow and flow-to-speech converters as crate-level rich
  fragment transforms, not clipboard hacks.
- Keep flow presets user-selectable.

Improvement over Verbatim:

- Link flow entries to source cards and speech document locations.
- Support undo/redo and history at the flow model level.
- Search across flows and evidence documents together.
- Avoid Excel COM and macro fragility entirely.

### 34. Word To Flow Send

Verbatim sends selected text/current Card/Block/Hat/Pocket from Word to Excel
Flow. It has modes for inserting into the active cell, splitting paragraphs into
a column, and sending headings-only summaries.

Recommendation: implement.

Options:

- Clipboard-based send.
  - Pros: easy to prototype.
  - Cons: brittle and not native.
- Typed rich fragment conversion.
  - Pros: deterministic, undoable, testable.
  - Cons: needs converter code.

Recommendation: typed rich fragment conversion.

Features:

- Send selection to active flow position.
- Send current card.
- Send current block/hat/pocket.
- Send headings-only summary.
- Split paragraphs into consecutive flow entries.
- Warn before overwriting selected flow text.

### 35. Flow To Speech Send

Verbatim Flow sends selected cells to a Word speech document at cursor or end.

Recommendation: implement using Flowstate speech targets.

Implementation:

- Convert selected flow nodes to speech document fragments.
- Preserve speaker/flow/source metadata.
- Support send to cursor and send to end.
- Warn before sending with no active speech target.

### 36. Quick Analytics

Verbatim Quick Analytics stores selected flow cells under shortcuts in one of
ten profiles, similar to Quick Cards.

Recommendation: merge with the native snippet system.

Implementation:

- Snippet type: document card, document block, flow fragment, plain analytic.
- Profiles: keep ten-profile compatibility but allow named profiles.
- Shortcut insertion in flows and speech docs.

Improvement over Verbatim:

- Snippets can be shared between document and flow contexts when types allow.

### 37. Flow Scouting Info

Verbatim Flow includes a scouting info sheet and can autofill tournament,
round, side, opponent, judge, room, and speech metadata.

Recommendation: implement as metadata, not a spreadsheet tab.

Implementation:

- Add flow/workspace metadata panel.
- Include metadata in export, share, and caselist integration.
- Allow per-flow overrides.

### 38. Flow Display And Settings

Verbatim Flow settings include:

- Font size.
- Row height.
- Column width.
- Insert mode on startup.
- Arrow extension.
- Auto label flows from a cell.
- Disable sheet popup.
- Freeze speech names.
- Alternate send-to-speech shortcut.
- Speech names presets.

Recommendation: implement the meaningful equivalents.

Mapping:

- Font/row/column settings -> flow viewport style settings.
- Insert mode startup -> editor preference.
- Arrow extension -> flow extend setting.
- Auto labels -> flow title metadata.
- Disable sheet popup -> not relevant unless Flowstate adds equivalent popups.
- Freeze speech names -> lock preset/speech headers.
- Alternate shortcuts -> keymap settings.
- Speech names presets -> existing style presets plus user customization.

### 39. Caselist Document Processing

Verbatim can process cites by largest heading, up to several levels, and upload
cite entries with first/last portions for cite requests.

Recommendation: implement as a local export/report before any upload.

Implementation:

- Build a cite packet processor that converts a document subtree to a structured
  caselist representation.
- Show generated sections and cite snippets in a preview.
- Export markdown/JSON even without network upload.

Improvement over Verbatim:

- Make the processor deterministic and test it against imported Verbatim docs.
- Show unresolved cards, missing cites, and suspicious heading levels.

### 40. Wikify / Markdown Export

Verbatim can convert cases to markdown-ish caselist text.

Recommendation: implement.

Implementation:

- Export selected range/current document to markdown with heading hierarchy,
  tags, cites, and card text.
- Add options for full text, cites only, or open-source packet.

### 41. Update Checks

Verbatim can check a remote update URL and compare versions.

Recommendation: defer unless Flowstate has a distribution/update channel.

Implementation:

- If added, use the app's actual release channel.
- Do not make startup network requests without user preference.

### 42. Mini / Network-Reduced Version

Verbatim ships a "mini" version stripping network/integration features that may
trigger antivirus or policy problems.

Recommendation: support this architecturally through optional features and
integration toggles.

Implementation:

- Keep network integrations optional.
- Provide an offline mode that disables upload/share/login/update checks.
- Keep core editing, flow, snippets, search, and stats functional offline.

### 43. Features Not To Port Directly

Do not port these Word-specific or fragile behaviors directly:

- Word COM and macro-driven selection mutation.
- Word ribbon XML callbacks as architecture.
- Hidden text as a destructive editing mode.
- Shrink below 4pt.
- Building blocks as the snippet store.
- Excel as the flow data model.
- Macro import/export as an extension system.
- Word install and template diagnostics.
- Blind network upload without preview and consent.

### 44. Existing Verbatim "Will Not Add" Items

Verbatim's own README lists several rejected features. Flowstate should treat
them carefully:

- Separate Analytics style: support only with explicit behavior settings.
- Shrink below 4pt: do not support.
- Invisibility modes that delete content: do not support.
- Undertags style: Flowstate can keep the existing command, but should not make
  paperless parsing depend on it without a user decision.

## Suggested Delivery Order

### P0: Core Debate Editing

- Shared debate range resolver.
- Verbatim-compatible style import/export mapping.
- Speech target model and Send To Speech / Send To Speech End.
- Outline drag/drop and heading move/delete/select commands.
- Semantic condense, markers, shrink, and unshrink.
- Underline/highlight/emphasis mode improvements.
- Stats panel with user-selected debate profile and WPM.
- Keymap UI with Verbatim shortcut preset.
- Document audit panel for fake tags, formatting gaps, blank headings, cite
  problems, and style normalization.

### P1: Evidence And Flow Productivity

- Native snippets for Quick Cards and Quick Analytics.
- Native evidence library replacing Virtual Tub.
- Source-to-flow and flow-to-speech conversion.
- Flow evidence/group/highlight semantics.
- Combine Documents import wizard.
- Markdown/caselist local export.
- Search index across documents, snippets, and flows.
- Warrant comments and comment export.

### P2: Integrations

- OCR integration.
- USB/export package flow.
- Optional openCaselist/Tabroom upload.
- Optional share service integration.
- Auto-open folder watcher.
- Timer helper window.
- Audio recording plugin.
- External search and citation helper integrations.

## Clear Improvements Flowstate Should Make

- Semantic model first: avoid using font size, hidden text, or inserted marker
  characters as the only source of truth.
- Reversible transforms: every broad formatting operation should be undoable as
  one transaction and previewable before application.
- Provenance: sent cards, snippets, OCR inserts, search inserts, and flow sends
  should retain source links.
- Unified snippets: Quick Cards and Quick Analytics should share storage,
  profiles, fuzzy lookup, previews, and import/export.
- Unified search: search evidence docs, current workspace, flows, snippets, and
  speech docs from one index.
- Native flow: keep replacing Excel, not embedding it.
- Privacy-first integrations: credentials in secure storage, no automatic
  uploads, no evidence text sent to services without explicit opt-in.
- Incremental stats and indexing: large tubs and long docs should stay smooth.
- Better diagnostics: rather than silently fixing formatting, explain the issue
  and show changed counts.
- Cross-format profiles: all event defaults should be selected or configured by
  the user, never assumed by the app.

## Open Product Questions

These should be asked before implementation choices that affect competitive
debate workflow:

- Which debate formats should ship as first-class presets at launch?
- Should Flowstate default to Verbatim shortcuts or a new native shortcut set?
- Should Analytic and Undertag paragraphs count as speech content, notes, or
  hidden metadata by default?
- Should condense/shrink appear visually like Word Verbatim, or prioritize a
  more readable native display with Verbatim-compatible export?
- Which integrations are acceptable in the default build: OCR, Tabroom,
  openCaselist, share service, update checks, external search, audio?
- Should snippets be global across all workspaces, workspace-local, or both?
- How important is exact `.docx` round-trip compatibility versus a cleaner
  native `.db8` model?

## Bottom Line

The highest-value Verbatim features to bring into Flowstate are not the Word
macros themselves. They are the workflow contracts:

- Fast structured cutting.
- Predictable tag/cite/card semantics.
- Range-aware formatting commands.
- Immediate speech assembly.
- Evidence library insertion.
- Snippet recall.
- Search.
- Flow and speech interop.
- Time/stat feedback.

Flowstate should implement those contracts natively in Rust and GPUI, using
semantic document and flow models, `gpui-component` UI primitives, and optional
integration crates for network or OS-dependent features.
