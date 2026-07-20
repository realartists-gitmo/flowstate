# Flowstate — Agent Map

Orientation for coding agents. Flowstate is a **performant multiplayer word processor for competitive debate**. Agents get lost when they treat product language as free-form English; many everyday words are **load-bearing domain nouns** that point at exact crates and types. Use this file as the first routing table.

If a term below does not match what you see in code, trust the code and update this map.

---

## 30-second architecture

```
┌──────────────────────────────────────────────────────────────────────┐
│  flowstate (app shell: Workspace, ribbons, tabs, collab UI)          │
│    crates/flowstate/src/{workspace,collab,flow,ribbon,app_settings}  │
└───────────────┬───────────────────────────────┬──────────────────────┘
                │                               │
     ┌──────────▼──────────┐         ┌──────────▼──────────┐
     │  gpui-flowtext      │         │  flowstate-flow     │
     │  rich-text editor   │         │  .fl0 format +      │
     │  + projection model │         │  board materializer │
     └──────────┬──────────┘         └──────────┬──────────┘
                │                               │
     ┌──────────▼───────────────────────────────▼──────────┐
     │  flowstate-collab  (GPUI-free authority core)         │
     │  local_write / crdt_runtime / flow/ / net/ / doc_io   │
     │  ONE WriteGate per open doc · LoroDoc is truth        │
     └──────────┬───────────────────────────────┬──────────┘
                │                               │
     ┌──────────▼──────────┐         ┌──────────▼──────────┐
     │  flowstate-document │         │  iroh / iroh-gossip │
     │  .db8 package +     │         │  P2P mesh (opaque   │
     │  Loro schema/proj.  │         │  Loro update bytes) │
     └─────────────────────┘         └─────────────────────┘
```

**Law of one authority:** the only write path for local edits is typed **intents** through a **write gate** into a **Loro** document. The UI never owns truth; it consumes **projections** (and ordered projection streams). Raw projection-space mutation is condemned (“raw authority”).

---

## Crate map (where things live)

| Crate | Path | Role (plain language) |
|-------|------|------------------------|
| **flowstate** | `crates/flowstate` | Application: GPUI window, workspace, tabs, ribbons, file open/save, collab dialogs, flow panels. Thin `main.rs`; real surface is `lib.rs`. |
| **gpui-flowtext** | `crates/gpui-flowtext` | Host-agnostic rich-text **editor + document projection model**. Layout, paint, selection, virtualization, local intent *types*. No debate style names of its own. |
| **flowstate-document** | `crates/flowstate-document` | Debate host meanings for styles + **`.db8` package** (Loro-native), schema constants, full/regional **materializers** (`document_from_loro`, etc.). Re-exports much of gpui-flowtext. |
| **flowstate-collab** | `crates/flowstate-collab` | **Authority core** (no GPUI): CRDT runtime, local write gate, doc I/O service, flow runtime, tickets/admission, iroh swarm, presence, Dropbox helpers. |
| **flowstate-flow** | `crates/flowstate-flow` | **Flow format crate** only: `.fl0` schema, board materializer, `FlowIntent` vocabulary, pure board ops. Runtime is *not* here. |
| **flowstate-docx** | `crates/flowstate-docx` | DOCX **interpreter** (import + stylepox cleaning), exporter, PDF, recovery. |
| **flowstate-tub** | `crates/flowstate-tub` | Squad **tub** index: filesystem watch + SQLite catalog + Tantivy search over pockets/hats/blocks/cites/… |
| **flowstate-fidelity** | `crates/flowstate-fidelity` | Cheap-when-off CRDT/projection **diagnostics** (`FLOWSTATE_TRACE_FIDELITY`). |
| **flowstate-corpus** | `crates/flowstate-corpus` | Perf/fidelity harness bins + synthetic fixtures (open_probe, collab_bench, …). |
| **flowstate-soak** | `crates/flowstate-soak` | Headless collab hotpath soak (no GPUI window). |
| **vendor/** | `vendor/{loro-internal,generic-btree,gpui-component,rdocx}` | Patched upstreams. See `Cargo.toml` `[patch.crates-io]` comments before “upgrading.” |

### App crate layout (`flowstate`)

| Dir | Meaning |
|-----|---------|
| `src/workspace/` | Tabbed shell: open docs/flows, outline, search overlays, settings, status, close prompts. `Workspace` is the top entity. |
| `src/collab/` | **UI/session glue** over `flowstate-collab`: `CollabManager`, `CollabSession`, share dialog, presence view, pump, Dropbox OAuth. Not the CRDT core. |
| `src/flow/` | Flow **UI**: `FlowEditor`, `FlowPanel`, flow ribbon, cell themes. Talks to `FlowDocHandle`. |
| `src/ribbon/` | Rich-text editor ribbon (styles, tools). |
| `src/app.rs` / `rich_text_element/` | Standalone editor bootstrap and editor host wiring. |
| `src/commands/` | Keymap / command registration. |

### Collab core layout (`flowstate-collab`)

| Dir / file | Meaning |
|------------|---------|
| `local_write/` | **THE** local mutation path for `.db8` body text: gate → resolve → commit → projection patches. |
| `crdt_runtime.rs` + `crdt_runtime/` | Owns `LoroDoc`, projection build/repair, undo stacks, import delta, table ops, publish queue. Huge; prefer submodule files when possible. |
| `doc_io.rs` | Background I/O actor for rich-text: remote import, publish pump, save, snapshot export. Acquires the same gate. |
| `flow/` | Flow twin of the above: `FlowRuntime`, `FlowDocHandle`, `FlowIoHandle`, per-cell text authority. |
| `sync_io.rs` | `SyncIoHandle { RichText, Flow }` — transport-facing enum. |
| `net/` | Swarm, direct links, anti-entropy, blobs, wire compression, auth. |
| `ticket.rs` / `admission.rs` / `discovery.rs` / `bluetooth.rs` | How peers **find and admit** each other. |
| `presence.rs` / `identity.rs` | Ephemeral carets vs durable user identity (not the same as replica/peer id). |
| `dropbox.rs` | Dropbox-adjacent helpers for squad workflows. |

### Editor core layout (`gpui-flowtext`)

| Dir | Meaning |
|-----|---------|
| `rich_text/editor/` | Input → intents, layout chunks, paint, tables, paste, zoom, projection apply. |
| `rich_text/layout/` | Virtualized layout. |
| `local_intents.rs` | **Intent vocabulary** shared with collab (`LocalIntent`, `TextAnchor`, `LocalWriteAuthority`). |
| `document/` | Projection types the editor renders. |
| `edit_ops/` | Lower-level edit op helpers (advanced surface). |
| `collaboration.rs` | Editor-side collab hooks (not the network stack). |

---

## Metaphor glossary (product language → code)

Agents fail when they search for the metaphor string and miss the real identifier. Use this table.

### Product / debate domain

| You might say… | In code it is… | Start here |
|----------------|----------------|------------|
| **Verbatim** | The legacy Word/debate template ecosystem Flowstate imports/exports. Styles are not free-form strings. | `flowstate-docx`, style constants in `flowstate-document` |
| **stylepox** | Contaminated/non-portable DOCX styles; import **cleans** into Flowstate’s fixed style slots. | `flowstate-docx/src/cleaner.rs`, `interpreter.rs` |
| **Pocket / Hat / Block / Tag / Analytic / Undertag** | Paragraph style slots (`PARAGRAPH_POCKET`, …). Outline hierarchy of a debate card. | `flowstate-document/src/lib.rs` constants; DOCX map in `helpers/docs/docx_interpreter_conversion_logic.md` |
| **Cite / Emphasis / Underline / Condensed** | Run-level semantic styles (`SEMANTIC_CITE`, …). | same + `RunSemanticStyle` |
| **Spoken / Insert / Alternative / Marked** | Highlight styles (`HIGHLIGHT_SPOKEN`, …). | same |
| **Card** | A pocket-rooted evidence unit (search unit + outline concept); also a **flow board card** (cell). Context decides. | tub `SearchUnitKind::Card`; flow `Cell` |
| **Tub** | Indexed library of a squad’s files (watch + SQLite + Tantivy). Not a document format. | `crates/flowstate-tub` |
| **db8 / .db8** | Native **rich-text** package (chunked Loro snapshot + assets + search units + projection cache). | `flowstate-document` `package.rs`, `FLOWSTATE_EXTENSION` |
| **fl0 / .fl0** | Native **flow board** document (zstd Loro snapshot). | `flowstate-flow` `persistence.rs` |
| **docx / cmir** | External interchange. DOCX supported; CardMirror (`.cmir`) is roadmap. | `flowstate-docx` |
| **Flow / flowing / the sheet** | Excel-style debate spreadsheet: sheet-global rows × speech columns, cells at (row, column) addresses. No parentage/lineage data — the user owns WHERE, like paper. | `flowstate-flow` + `flowstate-collab/src/flow` + `flowstate/src/flow`; spec `Junk/flowstate_excel_flow_spec.md` |
| **Excel flow / the grid** (.fl0 v3) | The ratified flow model (2026-07-17): placement-map cells (LWW row/col), bump-down collision normalization, autofit + override row heights, rigid-body ink anchors. Replaced the Living Grid parentage model — do not resurrect wires/pads/families. | `Junk/flowstate_excel_flow_spec.md` |
| **Bump-down / bump row** | D2 collision law: merged states putting two cells on one address keep the least-uuid cell; losers land in `bump_row_id` synthesized rows below. | `flowstate-flow/src/loro_projection.rs` `bump_row_id` |
| **Annotation / stroke** | Freeform marker drawings on a flow sheet (write-once/delete map). Rigid bodies: ONE grid anchor + stroke-local geometry; structure changes translate ink, never deform it. | `flow.annotations` schema; `AnnotationStroke`, `GridAnchor` |
| **Speech columns (1AC, 1NC, …)** | Sheet column definitions / argument side layout — product speech names, not hard-coded everywhere. | sheet type / column defs in flow format |

### Collaboration / CRDT

| You might say… | In code it is… | Start here |
|----------------|----------------|------------|
| **The document / truth / source of truth** | `LoroDoc` inside `CrdtRuntime` or `FlowRuntime`, behind `WriteGate`. | `crdt_runtime.rs`, `flow/runtime.rs` |
| **Projection** | Derived render model (`DocumentProjection`, `FlowBoardProjection`). UI-safe, not authoritative. | `loro_projection.rs` (document + flow crates) |
| **Materializer / project / reproject** | Build projection from Loro (`document_from_loro`, `board_from_loro`, regional variants). | `flowstate-document`, `flowstate-flow` |
| **Intent** | Typed mutation request (`LocalIntent`, `FlowIntent`). Sole legal write API surface. | `gpui-flowtext/src/local_intents.rs`, `flowstate-flow/src/intents.rs` |
| **Write gate / the gate** | `WriteGate<T>` — mutex with priority lanes so typing beats background import. | `local_write/gate.rs` |
| **Resolve** | Map anchors/cursors to live Loro positions **inside** the gate; reject before mutate. | `local_write/resolve.rs` |
| **Commit / local commit** | One intent → one Loro commit (origin `"local"`) + inverse record + projection patches. | `local_write/commit.rs`, flow `flow/` |
| **Authority / write authority** | `LocalWriteAuthority` trait; implementations `LocalDocHandle`, `FlowCellAuthority`. | `local_intents.rs`, `local_write/handle.rs`, `flow/cell_authority.rs` |
| **Handle** | App-facing Arc API that holds the gate for each call (`LocalDocHandle`, `FlowDocHandle`, `DocIoHandle`, `FlowIoHandle`). | same |
| **Raw authority** | **Forbidden** old path: mutate projection then try to sync CRDT. CI-scanned. | `tools/check_raw_authority.sh`, `tools/README-raw-authority-guard.md` |
| **Frontier** | Version-vector / projection epoch the editor thought it had when composing an intent. Stale → reject. | collab docs; patch `base_frontier` is stream metadata, **not** local_write validation |
| **Patch / projection stream** | Ordered UI updates after commit/import (`ProjectionStreamItem`, regional patches). | `local_intents.rs`, `crdt_runtime/projection_patch.rs` |
| **Recorded inverse** | Fast undo path: store inverse ops instead of full Loro checkout when possible. | `local_write/recorded_inverse.rs` |
| **UndoManager / slow path** | Loro undo checkout when recorded inverse is invalid (overlap, truncated history). | `crdt_runtime.rs`, heaven ledger |
| **Anti-entropy** | Version-vector healing when peers are missing ops. | `net/anti_entropy.rs` |
| **Swarm** | iroh peer mesh handle. | `net/swarm.rs` |
| **Ticket / invite** | `SessionTicket` text/link; includes `DocumentKind { RichText, Flow }` + admission HMAC. | `ticket.rs` |
| **Admission** | Session secret / bearer for join. | `admission.rs` |
| **Discovery** | Finding peers (ticket paste, Dropbox hints, Bluetooth advertise, …). | `discovery.rs`, `bluetooth.rs`, app `collab/discovery_runtime.rs` |
| **Presence** | Ephemeral carets/selections; **not** document history. Ages out. | `presence.rs`, app `session_presence.rs` |
| **Replica vs user identity** | Peer/Loro id ≠ durable person (`identity.rs`, users map in schema). | collab + schema |
| **Publish queue** | Local commits enqueue update bytes for peers; I/O service pumps them. | runtime + `doc_io` / `flow_io` |
| **Blob / asset** | Image (etc.) bytes content-addressed separately from Loro text ops. | `net/blobs.rs`, package assets |
| **Shallow / shallow history / sidecar** | Open/edit without full history in RAM; deep history may live in package sidecar. | `Junk/flowstate_shallow_open_design.md`, package code |
| **Checkpoint / revision** | Package save boundary / time-travel snapshot (`.db8`). Flows are simpler snapshot files. | package + revision dialog |
| **Coalescing (import)** | Batch remote update bytes under one gate hold; second drain patterns. | `doc_io.rs`, `flow/flow_io.rs`, `docs/collab-coalescing-parity.md` |
| **Self-check** | Detect projection drift and rebuild. | `self_check.rs` |

### Flow UX metaphors (excel grid)

| Metaphor | Meaning | Code / spec |
|----------|---------|-------------|
| **The USER owns WHERE** | Drop = set address (`SetCellAddress`, two LWW writes). Zero drop interpretation — no pads, no wires, no derived geometry. | `flowstate/src/flow/editor.rs` slot drop, excel flow spec D1/D5 |
| **The cursor** | An Excel (row, column) slot cursor over real rows + the ghost run; typing on an empty slot creates the cell seeded with the keystroke. | `editor/grid_nav.rs` |
| **Ghost rows** | Render-only rows below the last real one; first touch materializes them (rows + cell = one undo group). | `grid_nav.rs` `add_cell_at_slot` |
| **Frozen chrome** | Header (columns: drag to reorder, edge-drag to resize, double-click = autofit) + row-number gutter (drag to move rows, click to select). | `editor.rs` header/gutter overlays |
| **Answers sit to the RIGHT** | "Response" = same row, next column; alignment across speeches is manual and spatial, like paper. | `grid_nav.rs` `add_response` |
| **Silent refusal is a defect** | Occupied slots and edges refuse WITH WORDS (toast + shake); occupied drop targets tint danger while hovered. | `grid_nav.rs` `refuse` |

### Performance program language

| You might say… | Meaning | Where |
|----------------|---------|-------|
| **Heaven / quiet machine** | Perf close-out: ops at physical floor with nets (tests/oracles). | `Junk/flowstate_heaven_ledger.md` |
| **Act N / A12.x / T8.x** | Numbered perf/architecture work packages in comments (`§act-twelve`, `§perf-heaven`). | search `§act-` / ledgers in `Junk/` |
| **Floor / bounded / rare-floor** | Heaven ledger status vocabulary for latency claims. | heaven ledger |
| **Mass op chunking** | Huge remote ops sliced so typing can interrupt. | collab tests `mass_op_chunking`, A14 notes |
| **Regional rematerializer (§6-R)** | O(region) projection update for structural remote ops, not full O(doc). | `crdt_runtime` |
| **Open probe** | Stage timings for cold `.db8` open. | `flowstate-corpus` `open_probe` bin |
| **Hotpath** | Typing/import path instrumentation. | soak + benches |

### UI shell

| You might say… | In code it is… |
|----------------|----------------|
| **Workspace** | Top-level multi-tab window entity. |
| **Panel / tab** | Open document or flow surface (`document_panel`, `FlowPanel`). |
| **Ribbon** | Contextual toolbar above editor/flow. |
| **Share dialog** | Collab start/join/invite UI. |
| **Toolkit / outline** | Side chrome for nav and tools. |
| **Invisibility mode** | Hide non-spoken / condensed text for speech. | `gpui-flowtext/.../invisibility.rs` |
| **Theme is law** | Visuals from `cx.theme()` slots only. | flow architecture Part 0 |

---

## Two document kinds (do not mix stacks)

| | **Rich text (.db8)** | **Flow (.fl0)** |
|--|---------------------|-----------------|
| Format crate | `flowstate-document` | `flowstate-flow` |
| Runtime | `CrdtRuntime` + `LocalDocHandle` | `FlowRuntime` + `FlowDocHandle` |
| I/O service | `DocIoHandle` | `FlowIoHandle` |
| Cell text | body flow + tables/images/eqs | nested CRDT text **per cell** via `FlowCellAuthority` |
| Transport | same `net/`, tickets with `DocumentKind` | same mesh, opaque Loro bytes |
| UI | `RichTextEditor` / document panel | `FlowEditor` / `FlowPanel` |
| Comments / package revisions / assets | yes (package model) | mostly snapshot-only; comments/assets rich-text-centric |

`SyncIoHandle` and session attachment enums branch on kind; **do not** invent a third runtime.

---

## Canonical data flows (memorize these)

### Local rich-text edit

```
keystroke/command
  → RichTextEditor builds LocalIntent(s) against current projection
  → LocalDocHandle (WriteGate)
       resolve anchors (reject if impossible)
       mutate LoroDoc
       commit + recorded inverse
       emit projection patches
  → editor applies patches only
  → publish queue → DocIoHandle → peers (if session live)
```

### Remote rich-text edit

```
peer update bytes
  → DocIoHandle import (gate, coalesced)
  → CrdtRuntime import + derive patches / full rebuild
  → anti-entropy if deps missing
  → editor applies derived projection
```

### Flow edit

```
board gesture or cell keystroke
  → FlowIntent / nested LocalIntent on cell
  → FlowDocHandle / FlowCellAuthority (same WriteGate pattern)
  → board + cell streams
  → FlowIoHandle for network/save
```

**Solo and collab use the same handles.** Collaboration is transport + presence around the same runtime, not a second document model (`helpers/docs/collaboration.md`).

---

## “I need to change X” — routing table

| Goal | Primary locations |
|------|-------------------|
| Typing / selection / layout bugs | `gpui-flowtext/src/rich_text/editor/` |
| Intent shape / write rejection | `gpui-flowtext/src/local_intents.rs`, `local_write/{resolve,commit,handle}.rs` |
| Undo weirdness | `local_write/recorded_inverse.rs`, Loro UndoManager in `crdt_runtime.rs` |
| Projection wrong after remote | `crdt_runtime/projection_patch.rs`, `import_delta.rs`, `loro_projection.rs` |
| Table structure | `crdt_runtime/table_ops.rs`, `table_topology.rs`, editor `tables.rs` |
| Save / open / package | `flowstate-document/src/package.rs`, `doc_io.rs` |
| DOCX import fidelity | `flowstate-docx/src/interpreter*`, `cleaner.rs`, conversion helper doc |
| DOCX export / PDF | `flowstate-docx/src/exporter*`, `pdf.rs` |
| Flow grid rules / normalize | `flowstate-flow/src/{projection,mutate,loro_*}.rs` |
| Flow collab / undo / IO | `flowstate-collab/src/flow/` |
| Flow grid feel (drag/resize/cursor) | `flowstate/src/flow/editor.rs` + `editor/{grid_nav,grid_layout}.rs` |
| P2P / tickets / join | `ticket.rs`, `net/`, `flowstate/src/collab/{session,manager,share_dialog*}.rs` |
| Presence carets | `presence.rs`, app `session_presence.rs`, `presence_view.rs` |
| Tub search | `flowstate-tub` |
| Debate styles / theme catalog | `flowstate-document` constants, `ribbon/style_catalog.rs`, workspace style settings |
| Perf regression | heaven ledger, `flowstate-corpus` bins, `flowstate-soak`, comments tagged `§perf` |
| Loro upgrade | `vendor/loro-internal`, patches documented in root `Cargo.toml` |

---

## Hard invariants (agents break these constantly)

1. **Loro is truth.** Never treat `DocumentProjection` as authoritative for writes.
2. **Intents only.** New mutations = new `LocalIntent` / `FlowIntent` variants + gate path, not ad-hoc `LoroDoc` edits from the UI.
3. **No raw authority.** Do not reintroduce projection-space command batching / rebase / pending-edit flush. Run `tools/check_raw_authority.sh` when touching write paths.
4. **Same path solo and collab.** Do not special-case “local-only” mutation APIs that skip the gate.
5. **Gate holds are sacred.** No long work (disk, network, full O(doc) walks) while holding the gate on the typing path.
6. **No Loro cursor resolve inside subscription callbacks** (documented in `resolve.rs`).
7. **Transport is opaque bytes.** Prefer not inventing app-level op logs; mesh already ships Loro updates.
8. **Theme from theme slots** for new UI chrome.
9. **Vendor patches are intentional** — read patch comments before “fixing” dependencies.
10. **Silent refusal is a bug** in flow interaction design.

---

## Specs & docs index (deeper than this map)

| Document | Use when |
|----------|----------|
| `helpers/docs/collaboration.md` | Canonical collab data flow (short). |
| `helpers/docs/collab_qa.md` | Manual multi-peer QA matrix. |
| `helpers/docs/docx_interpreter_conversion_logic.md` | Style mapping rules for import. |
| `Junk/flowstate_excel_flow_spec.md` | **Authoritative** flow model: .fl0 v3 excel grid, ratified decisions D1–D6, build order. |
| `Junk/flowstate_flow_architecture_spec.md` | Flow runtime split/transport (Part 2, still law); Parts 1.1/3 (Living Grid, pads/wires) SUPERSEDED by the excel flow spec. |
| `Junk/flowstate_flow_ux_spec.md` | Flow UX details (if not fully folded into architecture). |
| `Junk/flowstate_heaven_ledger.md` | Perf floors and which tests pin them. |
| `Junk/flowstate_shallow_open_design.md` | Shallow open / history sidecar. |
| `Junk/flowstate_oom_leads.md` / `flowstate_perf_backlog.md` | Known perf debt. |
| `Junk/flowstate_act*_spec.md` + `*_ledger.md` | Historical act packages (search for your topic). |
| `docs/collab-*.md` | Focused collab decisions (coalescing, object positioning). |
| `COLLABORATION_MACOS_WINDOWS_RELEASE.md` | Platform release readiness. |
| `flow-ergonomics-guide.md` | SUPERSEDED (tested the dead Living Grid DnD); a grid-gesture battery replaces it when the surface calibrates. |
| `tools/README-raw-authority-guard.md` | Write-path CI law. |
| `crates/gpui-flowtext/README.md` | Editor library API tiers. |
| `README.md` | Product roadmap in plain English. |

`Junk/` is historical working memory — still high signal, not always “current product UI.” Prefer architecture specs + code when they disagree.

---

## Build / run / test cheatsheet

```fish
# App (release is the real editor)
cargo run --package flowstate --release

# Nightly is required (repo override)
rustup override set nightly

# Focused tests (prefer package filters — full workspace is heavy)
cargo test -p flowstate-collab --lib
cargo test -p gpui-flowtext
cargo test -p flowstate-flow
cargo test -p flowstate-document

# Raw-authority guard (write-path changes)
tools/check_raw_authority.sh

# Perf / soak (examples)
cargo run -p flowstate-soak --release -- path/to/doc.docx
# open_probe, collab_bench, etc. live under flowstate-corpus bins
```

Logs often under `flowstate-logs/`. Flow intent overlay: `FLOWSTATE_INTENT_LOG=1`. Fidelity firehose: `FLOWSTATE_TRACE_FIDELITY`.

---

## Naming collisions to watch

| Ambiguous word | Disambiguate |
|----------------|--------------|
| **flow** | (1) body text container in Loro schema (`FLOWS_BY_ID`, `BODY_FLOW_ID`) vs (2) the **board product** `.fl0`. |
| **block** | (1) document block (paragraph/table/image) vs (2) debate paragraph style **Block** vs (3) speech **Block** column. |
| **card** | Evidence card structure vs flow board cell. |
| **tag** | Paragraph style Tag vs search/tagging vs HTML. |
| **session** | Collab session vs generic “user session.” |
| **frontier** | CRDT/version frontier vs “product frontier” (roadmap). |
| **gate** | WriteGate vs UI dialog. |
| **projection** | Document projection vs GPUI “projection” (none) — always CRDT-derived view model here. |
| **runtime** | `CrdtRuntime` / `FlowRuntime` vs `net/runtime` (network actor). |

---

## How to keep this map honest

When you introduce a new domain noun, metaphor, or crate split: **add one row** to the glossary or routing table in the same PR. When you delete a path, delete its row. Agents reading only this file should still land in the right directory on the first try.
