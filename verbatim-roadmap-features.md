# Flowstate Roadmap Features From Verbatim Gap Analysis

- **Verbatim macro parity audit**
  - Gap: Flowstate has the core debate editor, but Verbatim has years of small Word/VBA macros users expect.
  - Implementation: create a tracked parity matrix from Verbatim `desktop/src/*.bas` and the Paperless Debate manual, map every macro to a Flowstate `CommandId`, and mark implemented/partial/missing.

- **Always-on editing mode**
  - Gap: Verbatim can run its ribbon/macros against ordinary Word files via startup templates.
  - Implementation: add an “open any DOCX as Flowstate” path that imports, applies Flowstate style recognition, and lets users save/export back without manually converting first.

- **Mini/safe mode distribution**
  - Gap: Verbatim ships a “Mini” template with risky/antivirus-prone features removed.
  - Implementation: add a reduced-capability Flowstate build/profile for school-managed machines: no network sharing, no recording/OCR plugins, minimal filesystem prompts.

- **Setup check tool**
  - Gap: Verbatim has PC/Mac setup check tools for macro security and system tweaks.
  - Implementation: add a first-run diagnostics screen for permissions, filesystem access, screen capture/OCR permissions, microphone permissions, network/collab readiness, update status, and OS hotkey conflicts.

- **Built-in tutorial/help**
  - Gap: Verbatim includes tutorial/help forms and ribbon screenshots for learning core functions.
  - Implementation: ship a guided onboarding document plus a searchable help panel sourced from local markdown, linked to command IDs and settings.

- **Cheat sheet**
  - Gap: Verbatim has an in-app shortcut cheatsheet.
  - Implementation: render Flowstate `COMMAND_SPECS` and user keymap overrides into a printable/searchable cheat sheet modal.

- **F-key alternate shortcut layer**
  - Gap: Verbatim provides Ctrl/Cmd+Alt+number equivalents for F-key-heavy workflows, useful on laptops/Mac.
  - Implementation: add default alternate bindings for core organize/format commands and expose them in keymap settings.

- **Selection mode consistency**
  - Gap: Verbatim documents a consistent macro scope order: selection, current card/heading, whole doc at top.
  - Implementation: standardize every Flowstate card/doc command around a shared scope resolver and write tests for selection/card/heading/document behavior.

- **Paste Text**
  - Gap: Flowstate has paste, but Verbatim’s F2 plain-text paste is a dedicated workflow.
  - Implementation: add Paste Text command that strips source styling, optionally condenses immediately, and preserves only paragraph breaks needed for card cutting.

- **Condense mode toggles**
  - Gap: Flowstate has condense, but Verbatim has paragraph-integrity and pilcrow toggles plus direct mode shortcuts.
  - Implementation: add persistent condense settings, ribbon toggles, Condense No Pilcrows, Condense With Pilcrows, and Uncondense commands.

- **Remove pilcrows**
  - Gap: no explicit cleanup command for pilcrow markers.
  - Implementation: add a scoped transform that removes pilcrow markers without changing surrounding card text.

- **Shrink/unshrink all**
  - Gap: Flowstate has condensed styles, but not Verbatim’s bulk shrink/unshrink card operations.
  - Implementation: add document/card scoped shrink cycles, unshrink all, shrink all cards, and shrink-pilcrows cleanup commands.

- **Omission-note shrink protection**
  - Gap: Flowstate does not expose Verbatim’s bracket/angle “Omitted” protection.
  - Implementation: implement configurable shrink skipping for `[... Omitted]` and `<... Omitted>` spans.

- **Fix Fake Tags**
  - Gap: no command to detect visually fake tags and convert them to structural tags.
  - Implementation: scan paragraphs for tag-like direct formatting/cite/bold patterns and convert high-confidence matches to `PARAGRAPH_TAG`.

- **Convert Analytics To Tags**
  - Gap: Flowstate supports Analytics natively, but Verbatim users need compatibility cleanup.
  - Implementation: add scoped command converting `PARAGRAPH_ANALYTIC` to `PARAGRAPH_TAG`, preserving body text.

- **Fix Formatting Gaps**
  - Gap: no Verbatim-style repair for accidental gaps in underline/emphasis.
  - Implementation: detect short whitespace/punctuation gaps between identical semantic styles and fill them in body text.

- **Convert To Default Styles**
  - Gap: imported DOCX files can carry arbitrary Word styles.
  - Implementation: extend DOCX import/cleanup to map unknown styles into Flowstate defaults based on style names, outline levels, and formatting heuristics.

- **Remove Extra Styles**
  - Gap: Flowstate does not expose a style-gallery cleanup equivalent.
  - Implementation: on DOCX export/import cleanup, drop unused/unknown Word styles after converting recognized content into Flowstate styles.

- **Auto Number Tags**
  - Gap: no command to number all tags in a block/document.
  - Implementation: add scoped command that prefixes tags with ordered numbers and stores enough metadata or heuristics to de-number cleanly.

- **De-Number Tags**
  - Gap: no inverse of tag auto-numbering.
  - Implementation: strip recognized numeric prefixes from tags in current block/document.

- **Insert Header**
  - Gap: Flowstate DOCX export lacks a user command for page headers.
  - Implementation: add export-time/document metadata for headers using user name/school/settings and write them into DOCX/PDF output.

- **Remove Emphasis**
  - Gap: no cleanup command converting emphasis to regular underline.
  - Implementation: transform `SEMANTIC_EMPHASIS` spans to underline semantics in scope.

- **Remove Non-Highlighted Underlining**
  - Gap: no command for reducing over-underlined cards to only read text.
  - Implementation: delete underline styling from spans that do not overlap spoken/highlight styles.

- **Remove Blanks**
  - Gap: no Verbatim macro for deleting empty structural headings.
  - Implementation: scan structural paragraphs with empty/whitespace labels and remove them with schema-aware cleanup.

- **Remove Hyperlinks**
  - Gap: no productized hyperlink cleanup command.
  - Implementation: strip hyperlink metadata/direct URL styling while preserving visible text.

- **Remove Bookmarks**
  - Gap: no cleanup command for imported Word bookmarks.
  - Implementation: preserve bookmarks during import only if needed; otherwise expose a command/export option to drop bookmark metadata.

- **Update Styles**
  - Gap: Verbatim can update document styles from current template settings.
  - Implementation: add “Apply Current Theme/Styles” command that rewrites document theme metadata or export style definitions from current settings.

- **Select Similar Formatting**
  - Gap: no command to select all text matching current paragraph/semantic/highlight/direct formatting.
  - Implementation: inspect style state at caret/selection, find matching ranges, and build a multi-range selection.

- **Show all formatting**
  - Gap: Flowstate has invisibility features, but no formatting-code diagnostics view.
  - Implementation: add a debug/inspection overlay showing paragraph style, semantic styles, highlights, direct formatting, note markers, anchors, and hidden metadata.

- **Standardize Highlighting**
  - Gap: Flowstate has highlight styles, but no Verbatim bulk standardization.
  - Implementation: add scoped command to convert every highlight to current highlight choice.

- **Standardize Highlighting With Exception**
  - Gap: no protected-color standardization workflow.
  - Implementation: add settings for exception highlight colors and skip those during standardization.

- **Auto-Emphasize First**
  - Gap: no acronym/source-letter emphasis command.
  - Implementation: apply emphasis to the first letter of each selected word, with future extension for custom acronym maps.

- **Auto Underline Card**
  - Gap: Verbatim can infer underlining from a card’s tag.
  - Implementation: build a heuristic highlighter/underliner that compares tag keywords against card body sentences and applies underline to likely warranted text.

- **Duplicate Cite**
  - Gap: no Alt-F8 cite duplication command.
  - Implementation: find previous card’s cite-styled runs and insert/copy them into the current card.

- **Auto Format Cite**
  - Gap: no deterministic cite formatter for existing cite lines.
  - Implementation: parse cite text for author/date/month/day and apply `SEMANTIC_CITE` to the correct substring.

- **Reformat All Cites**
  - Gap: no bulk cite restyling command.
  - Implementation: traverse cards, parse cite runs, update displayed date format based on current-year rules, and reapply cite semantics.

- **Get From Cite Creator**
  - Gap: Verbatim integrates with a Chrome Cite Creator helper.
  - Implementation: add browser-extension/native-message or clipboard import integration that pulls the current citation into Flowstate at the caret.

- **Speech document creation wizard**
  - Gap: Flowstate can open/save documents, but Verbatim has a speech-doc creation flow with round/opponent/date naming and autosave directory.
  - Implementation: add New Speech Document dialog, filename template settings, default speech directory, and optional Tabroom upcoming-round prefill.

- **Choose active speech document**
  - Gap: Flowstate has a speech document concept, but not a Verbatim-style chooser for multiple open speech docs.
  - Implementation: add active speech target selector and status pill; route Send To Speech commands through that target.

- **Send To Speech cursor/end warnings**
  - Gap: Flowstate send-to-speech exists, but Verbatim warns about inserting into the middle of a card and supports cursor/end variants.
  - Implementation: validate destination insertion point, warn on card-body insertion, and expose Send To Speech At Cursor and Send To Speech End commands.

- **Stopped reading marker**
  - Gap: Verbatim’s send key in a speech doc inserts a stopped-reading marker.
  - Implementation: when active doc is the speech target, map Send To Speech to insertion/removal of a reading marker at caret.

- **Delete current container**
  - Gap: no explicit Verbatim macro to delete current card/block/hat/pocket.
  - Implementation: detect enclosing structural container and delete it in one undoable runtime transaction.

- **Move container up/down**
  - Gap: no full Verbatim-style keyboard organization for speech docs.
  - Implementation: add schema-aware Move Container Up/Down for cards and heading subtrees.

- **Arrange windows**
  - Gap: Verbatim can arrange source docs and speech doc side-by-side.
  - Implementation: add workspace layout command that opens source/tub panels on one side and speech doc on the other, with configurable split ratio.

- **Switch open documents**
  - Gap: Verbatim has a ribbon/shortcut for cycling Word docs.
  - Implementation: expand Flowstate tab/window switcher with command palette integration and Alt/Ctrl-Tab style commands.

- **Reading view**
  - Gap: Verbatim leverages Word reading view for speeches.
  - Implementation: add a dedicated read view with larger paginated/continuous display, locked editing, keyboard page movement, and optional two-page desktop layout.

- **Invisibility mode**
  - Gap: Flowstate has a toggle command, but Verbatim’s user-facing mode hides all but headings/tags/cites/highlights.
  - Implementation: finish/polish Invisibility Mode semantics, status messaging, export safety, and stats/read-mode interactions.

- **Document stats**
  - Gap: Verbatim has a stats popup with cards, highlighted words, tag words, total, and read-time estimates.
  - Implementation: add stats panel over current document/selection with reader WPM settings and auto-refresh.

- **Cross-platform debate timer**
  - Gap: Flowstate lacks Verbatim’s standalone integrated speech/prep timer.
  - Implementation: build native GPUI timer panel/window with speech presets, prep sides, WPM integration, keyboard controls, and optional detached window.

- **Audio recording**
  - Gap: Verbatim can start/stop microphone recording from the ribbon.
  - Implementation: add recording permission setup, Start/Stop Recording command, configurable recordings directory, filename templates, and saved recording notifications.

- **OCR screenshot to text**
  - Gap: Flowstate does not have Verbatim’s OCR plugin workflow.
  - Implementation: add screen-region capture, Tesseract/Capture2Text or platform OCR backend, and paste recognized text as plain text at caret.

- **Auto-open folder**
  - Gap: Verbatim can watch a folder and auto-open incoming docs.
  - Implementation: add filesystem/Drive/Dropbox watched inbox provider that opens new DOCX/DB8/PDF files and optionally inserts them into the active speech/dropzone.

- **Search plugin integration**
  - Gap: Verbatim integrates OS search and Everything Search.
  - Implementation: let Flowstate file search use platform search backends where available, plus optional Everything integration on Windows, falling back to Flowstate’s own indexer.

- **Virtual Tub**
  - Gap: Flowstate has tub search, but Verbatim has a ribbon menu for inserting blocks from a curated VTub without opening files.
  - Implementation: finish tub indexing/UI as a curated source tree, expose file-heading menus, and insert selected block fragments at caret.

- **VTub refresh/build command**
  - Gap: Verbatim has explicit Create VTub/refresh behavior for selected folders.
  - Implementation: add tub root settings, manual rebuild, watcher refresh, and stale-index diagnostics.

- **Quick Cards**
  - Gap: Verbatim has saved reusable cards/analytics insertable by shortcut.
  - Implementation: add local rich-fragment snippet library with names, categories/profiles, typed shortcut insertion, search, add, delete, and manage UI.

- **Merge documents**
  - Gap: Verbatim has a merge button for combining speech docs/post-round docs.
  - Implementation: add Merge Documents command that imports selected documents, appends structural content into a new document, and preserves source headings/comments where possible.

- **Copy to USB**
  - Gap: Verbatim has a guided USB sharing workflow.
  - Implementation: add Share to Removable Drive command: save local copy first, detect mounted drives, copy with optional filename cleanup, and confirm completion.

- **share.tabroom.com integration**
  - Gap: Verbatim can upload speech docs to share.tabroom.com.
  - Implementation: add Tabroom/share API auth, speech-doc upload, generated link display, and optional “strip Speech from filename” setting.

- **Tabroom upcoming-round integration**
  - Gap: Verbatim can create speech docs for upcoming Tabroom rounds.
  - Implementation: integrate Tabroom account auth, fetch upcoming rounds, prefill speech document metadata, and connect sharing/disclosure flows.

- **openCaselist upload**
  - Gap: Flowstate lacks direct opencaselist.com disclosure.
  - Implementation: add OpenCaselist auth, school/team/caselist selectors, cite/open-source upload form, and document parser for disclosure entries.

- **Cite Request Card**
  - Gap: no command to produce tag/cite/first-last-sentence cite-request format.
  - Implementation: add card-scoped conversion that keeps tag, cite, first words, and last words while omitting body middle.

- **Cite Request Doc**
  - Gap: no document-wide cite-request generator.
  - Implementation: create a new document/export from all parsed cards/headings using Flowstate’s disclosure/cite-request format.

- **Wikify / Word-to-Markdown for caselist**
  - Gap: no caselist-oriented Markdown conversion.
  - Implementation: add Markdown exporter that maps headings, cite style, bold/italic/underline/super/subscript, links, and removes unsupported comments/highlights per caselist rules.

- **Citeify**
  - Gap: no one-click conversion from full evidence to cite disclosure.
  - Implementation: combine Cite Request Doc with Markdown exporter and upload/copy actions.

- **Settings import/export**
  - Gap: Verbatim centralizes many debate-specific settings.
  - Implementation: expand Flowstate settings schema/UI and add import/export of settings profiles for teams/labs.

- **Hide ribbon groups**
  - Gap: Verbatim lets users hide Debate ribbon sections for small screens.
  - Implementation: add per-ribbon-group visibility settings and compact presets.

- **Speech/share filename cleanup**
  - Gap: Verbatim can strip “Speech” from shared filenames.
  - Implementation: add export/share filename rewrite rules for speech docs.

- **Default search directory**
  - Gap: Flowstate has tub root/recent docs, but not a simple general default search directory.
  - Implementation: add search roots setting with recursive indexing and command palette/file search use.

- **Audio recordings directory**
  - Gap: no setting because no recording feature yet.
  - Implementation: add once recording exists; use it for default save location and cleanup.

- **Style settings parity**
  - Gap: Flowstate has document theme settings but not all Verbatim-style controls surfaced cleanly.
  - Implementation: expose font sizes/formats for built-in styles, default font, cite/underline/emphasis appearance, paragraph spacing presets, and style preview.

- **Ribbon modernization/parity**
  - Gap: Flowstate’s ribbon is under active development; Verbatim groups workflows by Speech, Organize, Format, Paperless, Tools, View, Caselist, Settings.
  - Implementation: reorganize Flowstate ribbon around those workflow groups while keeping command IDs as the routing layer.

- **Word DOCX compatibility stress tests**
  - Gap: Verbatim’s native environment is Word, so it naturally preserves Word expectations.
  - Implementation: build a DOCX fixture suite for Verbatim-created files covering styles, headings, cites, pilcrows, headers, hyperlinks, bookmarks, comments, tables, and caselist conversions.

- **Verbatim Flow replacement**
  - Gap: Flowstate does not yet replace the Excel Debate.xltm flowing workflow.
  - Implementation: build a Flowstate flow module with sheets, speech columns, debate-specific cell formatting, keyboard movement, and send-to/from speech integration.

- **Send To Flow**
  - Gap: Verbatim can send cards/headings from Word to Excel Flow.
  - Implementation: add Send To Flow Cell, Send To Flow Column, Send Headings To Flow Cell, and Send Headings To Flow Column using Flowstate’s flow panel/model.

- **Send Flow cell to speech**
  - Gap: Verbatim Flow can send selected cells back to the speech document.
  - Implementation: serialize selected flow cells as rich fragments and insert into active speech doc at cursor or end.

- **Quick Analytics**
  - Gap: Verbatim Flow has profile-based reusable analytics.
  - Implementation: add flow-side snippet profiles with shortcut-word expansion into cells.

- **Flow cell operations**
  - Gap: Verbatim Flow has insert cell above/below, merge cells, toggle highlight/evidence/group, extend argument.
  - Implementation: implement these as Flowstate flow commands with stable IDs and shortcuts.

- **Flow row operations**
  - Gap: Verbatim Flow supports insert/delete/move rows and go-to-bottom.
  - Implementation: add row-level commands over the flow grid model.

- **Flow sheet operations**
  - Gap: Verbatim Flow supports Add Aff Flow, Add Neg Flow, Add CX Flow, Delete Flow, Delete Empty Flows.
  - Implementation: add flow-sheet templates, sheet creation/deletion commands, and safeguards for non-empty deletion.

- **Autofill scouting info**
  - Gap: Verbatim Flow can populate scouting metadata from flow titles.
  - Implementation: add scouting-info model and derive fields from sheet names/round metadata.

- **Flow insert mode**
  - Gap: Verbatim Flow can make Enter insert a line break inside a cell.
  - Implementation: add flow editing mode toggle that changes Enter behavior and exposes a visible mode indicator.

- **Switch speech columns**
  - Gap: Verbatim Flow can move each sheet cursor to the selected speech column.
  - Implementation: configure speech names/columns and add Switch Speech command over all flow sheets.

- **Split with speech document**
  - Gap: Verbatim Flow can arrange Excel with Word.
  - Implementation: add Flow + Speech workspace preset with flow on one side and active speech doc on the other.

- **Auto-label flow sheets**
  - Gap: Verbatim Flow can rename sheets from cell A2.
  - Implementation: add optional sheet-title binding to a configured cell/field.

- **Flow sheet popup**
  - Gap: Verbatim Flow briefly displays sheet names on activation.
  - Implementation: add optional transient sheet-name overlay.

- **Flow settings**
  - Gap: Verbatim Flow exposes insert mode, extend-with-arrow, auto-label, speech names, font size, row height, and column width.
  - Implementation: add a dedicated Flow settings section with those controls.

- **Plugin architecture for external helpers**
  - Gap: Verbatim bundles external utilities: timer, OCR, Everything, Cite Creator, NavPaneCycle.
  - Implementation: add a Flowstate plugin/helper registry with capability detection, settings, install guidance, and command integration.

- **Antivirus/managed-device guidance**
  - Gap: Verbatim has documentation and installer choices for school IT friction.
  - Implementation: add docs and app diagnostics explaining network, filesystem, microphone, OCR, and update behavior; provide a restricted build/profile.

- **Debate-specific computer setup preset**
  - Gap: Verbatim documents and automates pieces of screen/window setup.
  - Implementation: add a “Tournament Mode” checklist/preset for sleep prevention guidance, full-screen/read mode, window layout, recording/timer readiness, and notification risk warnings.
