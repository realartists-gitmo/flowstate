# Flowstate Roadmap Features From CardMirror Gap Analysis

- **Web build**
  - Gap: Flowstate is currently a native GPUI desktop app; CardMirror has a browser/web edition.
  - Implementation: compile Flowstate through the experimental GPUI WASM target, keep the editor/runtime core shared, and provide a web host adapter for platform services.

- **Web filesystem replacement**
  - Gap: desktop Flowstate assumes local file access for open/save/recent-document workflows; CardMirror can run from constrained web environments.
  - Implementation: add storage providers behind a filesystem-like trait: browser download/upload for baseline, Google Drive SDK and Dropbox SDK for persistent web-backed documents, and local desktop filesystem as the existing provider.

- **Web collaboration**
  - Gap: native collaboration can rely on desktop networking/runtime assumptions; web needs a WASM-compatible transport.
  - Implementation: compile the existing iroh-based collaboration stack to WASM and keep the Loro/CRDT document runtime shared; avoid a separate WebRTC/WebSocket collaboration architecture unless iroh WASM proves insufficient.

- **Mobile app**
  - Gap: Flowstate does not currently have a mobile surface; CardMirror has a mobile browser layout.
  - Implementation: target `gpui-mobile`, reuse the document engine and command catalog, and build a touch-first shell with slim top bar, full-screen editor, outline drawer, read button, pinch zoom, and touch-sized settings.

- **Responsive web/mobile layout**
  - Gap: Flowstate’s workspace is desktop-oriented: ribbon, side panels, tabs, outline, toolkit.
  - Implementation: add responsive workspace modes selected by host/device width: desktop ribbon mode, compact web mode, and mobile read/edit mode. Reuse command IDs so shortcuts, buttons, and settings stay aligned.

- **`.cmir` import/export**
  - Gap: Flowstate uses `.db8` as its native package and supports DOCX/PDF conversion, but does not read/write CardMirror’s native `.cmir`.
  - Implementation: add a converter crate that maps `.cmir` structures into `flowstate_document` projections and back. Treat `.cmir` as an interchange format, not Flowstate’s canonical storage; preserve unsupported metadata in an extension blob when possible for round-trip fidelity.

- **Interactive onboarding document**
  - Gap: Flowstate has a demo document, but not a first-launch/new-document guided live tutorial.
  - Implementation: ship a structured onboarding `.db8` template with Pockets/Hats/Blocks/Tags/Analytics/Undertags and walkthrough content. Add a setting for “new documents start from onboarding template.”

- **Shortcut reference UI**
  - Gap: Flowstate has a command catalog/keymap model, but no polished in-app shortcut reference.
  - Implementation: render `COMMAND_SPECS` plus user keymap overrides into a searchable shortcut modal grouped by command category.

- **Command palette**
  - Gap: Flowstate has command IDs and search overlays, but not one global palette that searches commands, settings, files, tub evidence, open documents, and insertion targets.
  - Implementation: build a unified palette over existing command specs, settings schema, recent docs, tub index, document outline, and future Quick Cards/dropzone indexes.

- **Quick Cards**
  - Gap: Flowstate lacks a tagged personal snippet library for reusable evidence.
  - Implementation: store snippets as rich fragments in local app data, index name/tags/body text, expose Add/Search/Insert/Manage commands, and make insertion use the same rich-fragment path as paste/send-to-speech.

- **Dropzone shelf**
  - Gap: Flowstate can send to speech documents, but lacks a staging shelf for evidence before final placement.
  - Implementation: add a local dropzone model storing rich fragments and source metadata; expose Send to Dropzone, Insert from Dropzone, Clear, and palette integration.

- **Three-pane multi-doc workspace**
  - Gap: Flowstate supports multiple document panels/tabs, but not CardMirror-style simultaneous three-pane editing with per-pane outlines/history.
  - Implementation: extend workspace layout to split active documents into up to three visible slots, each with its own editor entity, outline state, focus shortcuts, and drag/copy insertion path.

- **Drag-copy between panes**
  - Gap: editor supports rich selection/drag primitives, but cross-pane card movement is not productized.
  - Implementation: serialize dragged cards/sections as rich fragments with structural metadata and allow copy/drop into another visible editor while refusing invalid structural drops.

- **Outline drag reorder and multi-select**
  - Gap: Flowstate outline supports tree display/collapse/jump, but not full reorder/copy interactions.
  - Implementation: add outline selection state, structural range extraction, schema-aware drop target validation, and runtime edit commands for moving/copying heading subtrees.

- **Read mode**
  - Gap: Flowstate has send/read-related concepts, but no explicit locked podium mode that hides unread content and prevents accidental edits.
  - Implementation: add an editor mode that renders only read-aloud structures/runs, disables mutating commands, supports marker placement, and optionally jumps to document top on entry.

- **Read-time estimates**
  - Gap: Flowstate has speech word-count caching, but not per-reader read-time estimates as a first-class status feature.
  - Implementation: compute visible/selected read-aloud words, apply configured words-per-minute profiles, and show estimate in the status bar and speech document UI.

- **Speech and prep timers**
  - Gap: Flowstate does not expose CardMirror’s tournament timer workflow.
  - Implementation: add timer state to workspace, configurable profiles/presets, Aff/Neg prep clocks, keyboard transport commands, low-time flashing, and optional compact ribbon placement.

- **Save/export presets**
  - Gap: Flowstate exports formats and send docs, but lacks a polished preset system for Send Doc, Read Doc, Marked Cards, destinations, and filename prefixes.
  - Implementation: layer named export presets over existing DOCX/PDF/native export hooks, with filtering rules for analytics/comments/marked cards and configurable output directories.

- **Card sharing**
  - Gap: Flowstate has collaboration sessions, but not a lightweight encrypted send/receive card workflow.
  - Implementation: reuse iroh identity/transport to send rich fragments out-of-band, show Send/Receive pills near the dropzone, and insert received cards into the dropzone or active document.

- **Comments and replies**
  - Gap: Flowstate editor model does not expose Word-like anchored comments as a product workflow.
  - Implementation: add anchored annotation ranges in document metadata, render comment markers/side column, support replies/edit/delete, and export public comments to DOCX comments.

- **Private notes**
  - Gap: no local-only annotation layer equivalent to CardMirror private notes.
  - Implementation: store private notes in a per-user local overlay keyed by document identity and anchor fingerprint, with explicit “include in export” controls.

- **AI notes and threaded AI comments**
  - Gap: no integrated Ask AI workflow for selected text/images.
  - Implementation: add AI provider settings, selection/image context extraction, note insertion as local/private AI annotations, and `@AI` continuation in comment threads.

- **Spaced-repetition flashcards**
  - Gap: Flowstate has no evidence study workflow.
  - Implementation: store flashcards in local app data keyed to document/card anchors, support Q/A and cloze cards, due scheduling, review UI, and re-grounding when anchors drift.

- **AI repair text**
  - Gap: no one-command OCR/PDF cleanup.
  - Implementation: send selected text plus constrained prompt to configured AI provider, replace only the selected range, and wrap the replacement in one undoable runtime transaction.

- **AI repair formatting**
  - Gap: no AI-assisted formatting restoration for pasted/collapsed evidence.
  - Implementation: ask model for formatting spans/structure edits, validate against Flowstate document schema, then apply as semantic edit commands in one undoable transaction.

- **AI cite formatting**
  - Gap: no built-in “turn URL/pasted cite into debate cite” action.
  - Implementation: add a configurable cite prompt, model selection, and command that replaces or copies a normalized cite with Flowstate cite styling.

- **Translation**
  - Gap: no selection translation workflow.
  - Implementation: add Translate Selection command with provider abstraction: keyless backend for default, Anthropic/OpenAI/Google Cloud optional, copy result to clipboard with configurable marker.

- **Voice control**
  - Gap: Flowstate has no hands-free card editing mode.
  - Implementation: add a desktop/mobile voice subsystem with local recognition where available, command grammar over existing command IDs, visible-text targeting, dictation, paint mode, and single-step undo for each voice action.

- **Independent background color track**
  - Gap: Flowstate has highlight styles but not a separate background/shading layer that can coexist with highlight.
  - Implementation: extend run style data/model/export to carry background color separately from highlight; add ribbon controls, DOCX round-trip mapping, and display-only distinction settings.

- **Font color control**
  - Gap: Flowstate theme controls style colors, but direct font color is not exposed as a formatting command.
  - Implementation: add run-level font color style, color picker UI, “Automatic” removal, and DOCX import/export mapping.

- **Color standardization tools**
  - Gap: no bulk standardize/convert highlight/background operations.
  - Implementation: add document/card/selection commands to rewrite highlight/background colors, convert between layers, and skip configured exception colors.

- **Acronym marking**
  - Gap: no command for marking only acronym source letters.
  - Implementation: scan selected words, apply underline/emphasis/highlight only to initial or configured letters, and add settings for phrase-specific letter mappings.

- **Smart shrink protections**
  - Gap: Flowstate has condensed/ultracondensed semantics, but not CardMirror’s protected-text shrink behavior.
  - Implementation: implement shrink commands that skip omission markers, warning markers, and user regex/string protections while shrinking surrounding connective text.

- **Condense variants and pilcrow uncondense**
  - Gap: Flowstate has a condense command, but not full paragraph-integrity modes.
  - Implementation: add condense modes for preserve integrity, destructive flatten, pilcrow markers, uncondense-from-pilcrows, heading handling, and paste+condense.

- **Smart quotes/dashes**
  - Gap: no opt-in typing autoformat helpers.
  - Implementation: intercept typed quote/dash sequences in the editor input pipeline, insert corrected characters, and make the transform reversible with immediate Backspace.

- **Flip quote direction**
  - Gap: no cleanup command for wrong curly quotes.
  - Implementation: add selection transform that swaps opening/closing curly quote codepoints while preserving styles.

- **Copy Previous Cite**
  - Gap: no citation carry-forward command.
  - Implementation: find previous card boundary, extract cite-styled run(s), and insert/apply them at the current card cite position.

- **Create Reference**
  - Gap: no formatted reference-copy workflow.
  - Implementation: create a rich fragment from selected/card text, optionally prepend configurable heading, shrink body text, convert highlights to background/gray, and place result on clipboard.

- **Lock Highlighting**
  - Gap: no way to freeze existing highlights as background before re-highlighting.
  - Implementation: transform highlight runs in scope into background-color runs and clear highlight runs, preserving existing background colors.

- **Footnotes/endnotes**
  - Gap: Flowstate document primitives do not visibly expose note workflows.
  - Implementation: add note references to document model/import/export, render superscript markers, provide popover read/edit/delete, and map DOCX footnotes/endnotes both ways.

- **Richer table editing**
  - Gap: Flowstate can insert tables, but CardMirror exposes row/column add/delete and merge/split controls.
  - Implementation: surface existing table edit primitives through ribbon/context menus and fill missing runtime commands for merge/split/cell operations.

- **Image alt text UI**
  - Gap: image model has alt-text operations, but product UI is incomplete.
  - Implementation: add image context menu and inspector for alt text, caption/layout, AI alt text, and AI image-to-table.

- **Document/card cleanup macros**
  - Gap: no CardMirror-like cleanup menu for analytics conversion, hyperlink removal, similar-format selection, and formatting-gap repair.
  - Implementation: add doc/card scoped commands that operate through structured document traversal and expose them in ribbon menus and command palette.

- **Extract Undertag**
  - Gap: Undertag exists, but not the “pull selected phrase into undertag” workflow.
  - Implementation: clone selected text as a new undertag paragraph below the tag, optionally quote it, and leave the original selection in place.

- **Repair Paragraph Integrity**
  - Gap: no guided paragraph-break reconstruction workflow for collapsed PDF/paste text.
  - Implementation: build a modal/in-editor repair bar scoped to current card, highlight matching phrase starts, insert paragraph breaks on unique match, and support pending indent markers.

- **Card-boundary editing rules**
  - Gap: Flowstate may handle structural edits, but CardMirror documents more deliberate edge behavior.
  - Implementation: audit Backspace/Delete/Enter around Tag/Analytic/Undertag/body boundaries and encode structure-preserving rules with regression tests.

- **Container move commands**
  - Gap: no explicit Move Container Up/Down commands.
  - Implementation: detect enclosing card/analytic/heading subtree, compute legal adjacent target, and apply one runtime move transaction.

- **Spellcheck**
  - Gap: no visible spellcheck setting/workflow.
  - Implementation: integrate platform spellcheck or a local spellchecker into rendered visible text only, with enable/disable setting and ignored ranges for hidden/read-mode content.

- **DOCX style cleaning**
  - Gap: Flowstate imports/exports DOCX, but lacks user-facing stylepox cleanup tools.
  - Implementation: expose existing DOCX conversion/cleaning logic as Clean DOCX / batch clean commands with protected-style settings and reports.

- **Crash recovery UX**
  - Gap: Flowstate has recovery hooks, but not a visible recovery workflow.
  - Implementation: persist recovery snapshots, detect unsaved crash state on startup, show recover/discard choices, and tie recovered documents back to their source paths.

- **Updates UI**
  - Gap: no user-facing update channel/status comparable to CardMirror’s manualized update flow.
  - Implementation: add release-check provider per distribution target, update notification UI, and platform-specific installer/open-release actions.

- **Appearance settings expansion**
  - Gap: Flowstate has document theme settings, but fewer polished controls.
  - Implementation: add UI for system theme follow, document dark-mode behavior, icon style, doc name pill, undo/redo buttons, tooltip density, preview styles, nav styling, cite hover preview, interface font, and interface scale.

- **Accessibility settings expansion**
  - Gap: Flowstate lacks CardMirror’s accessibility preset breadth.
  - Implementation: add reduce motion, steady caret, screen reader mode, color-vision palette, annotation underline shapes, display-only highlight/background overrides, and color-name status reporting.

- **Library-wide search**
  - Gap: Flowstate has tub search and document search, but not a persistent whole-library index over all evidence files.
  - Implementation: build a background indexer over selected filesystem/Drive/Dropbox roots, index filenames/outlines/cards/cites/body text, and surface results in command palette and toolkit.

- **Transclusion**
  - Gap: no live references to cards stored in another file.
  - Implementation: add stable card IDs and reference blocks that resolve through the library index/storage provider, render as live embedded cards, and update when source document changes.
