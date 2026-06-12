# Flowstate P2P Collaborative Editing — Implementation Plan

**Status:** approved architecture, ready for implementation
**Target:** Google-Docs-style live co-editing of `.db8` rich-text documents
**Stack:** [iroh](https://docs.rs/iroh) 0.98.x for P2P networking · [loro](https://docs.rs/loro) 1.13.x for the rich-text CRDT
**Authoring date:** 2026-06-12 (API versions verified against crates.io/docs.rs on this date)

---

## 0. How to use this document

This plan is written for an orchestrating agent that will spawn implementation subagents. Rules of engagement:

1. **Milestones M0–M6 (§22) are the unit of delegation.** M2 (doc sync) and M1 (networking) are independent and can run in parallel. M3+ depend on M2.
2. **Do not relitigate the decisions in §20.** They were made deliberately with full knowledge of the codebase. If an implementer hits a wall that genuinely invalidates one, stop and surface it rather than silently choosing differently.
3. **§23 lists the handful of API names that must be verified against docs.rs at implementation time** (exact method names in fast-moving crates). Everything else in this document was verified against the codebase or live docs on the authoring date.
4. Per `CLAUDE.md`: new functionality goes in **new files**, keep files **under 1000 LOC** (budgets are given per file), prefer **gpui-component** widgets over raw GPUI, add dependencies **via `cargo add`**, and run **`cargo clippy`** after each milestone. This workspace denies `clippy::pedantic` and `clippy::nursery` — write code accordingly (no silent truncating casts, no `unwrap()` on `get`, document any `unsafe`, etc.). See the allow-list in the root `Cargo.toml` before fighting a lint.
5. Conventions in this repo you must follow: 2-space indent (`.rustfmt.toml`), `#[hotpath::measure]`/`#[hotpath::measure_all]` on functions/impls in hot paths (match surrounding code), modules sometimes assembled via `include!` (don't convert them), `Entity<T>`-based GPUI state.

---

## 1. Product definition (the UX contract)

* Any user with a rich-text document (`.db8` editor tab) open can **start a collaboration session** for that document. Starting a session produces a **ticket string** (one opaque token, e.g. `fscollab…base32…`) they can paste to anyone over any side channel.
* Another Flowstate user chooses **Join session**, pastes the ticket, and the document opens as a **new tab** in their workspace. Both users now see each other's edits live (sub-second on LAN, ~1 s worst case via relay), each other's carets (colored, named), and a participant list.
* **The host's on-disk file is the single source of truth.** Only the host's editor has a `document_path`; only the host saves/autosaves to disk. Joiners can *Save As* to take a snapshot copy. While the session is live, concurrent edits merge via CRDT (loro) — no locking, no "reload to see changes."
* The host can end the session (or just close the tab/app); joiners keep an orphaned local copy with a "session ended" banner.
* Sessions can be started in **Editor** mode (joiners can write) or **Viewer** mode (joiners are read-only). The editor already understands `CollaborationRole::{Owner, Editor, Viewer}` and blocks mutations for viewers.
* Out of scope for v1 (explicitly): collaborating on Flow (`.fl0`) documents, per-peer role changes mid-session, mesh topology / host migration, persistence of CRDT history into `.db8`, end-to-end key rotation, more than ~8 simultaneous peers.

---

## 2. What already exists (do not rebuild)

A previous iteration scaffolded collaboration hooks into the editor engine. Inventory, with file references:

| Capability | Where | Notes |
|---|---|---|
| Canonical edit-op stream with stable IDs | `crates/gpui-flowtext/src/collaboration.rs` | `CanonicalOperation` enum: `InsertText`, `DeleteRange`, `SplitParagraph`, `JoinParagraphs`, `SetParagraphStyle`, `SetRunStyles`, `InsertBlock`, `DeleteBlock`, `MoveBlock`, `ReplaceParagraphSpan`, `ReplaceBlock`, `ReplaceDocument` |
| Stable identity map | `collaboration.rs` → `DocumentIdentityMap` | maps `ParagraphId`/`BlockId`/`TableCellId` (u128 UUIDs) ↔ indices; reconciled after every edit |
| Per-edit op capture | `crates/gpui-flowtext/src/rich_text/editor/edit_pipeline.rs:238` | `mark_document_changed_with_reconcile` stores `last_collaboration_edit` after **every** mutation; every undo record carries `canonical_operations` |
| Remote-op application (naive) | `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs:265` | `apply_remote_operations(&[CanonicalOperation])` — bounds-checked, ID-addressed appliers built on `edit_ops` primitives (`insert_text_at`, `delete_cross_paragraph_range`, `split_paragraph_at`, `mutate_runs_in_range`, `apply_document_span_replacement`) |
| Roles + write gating | `editor/mod.rs:182` (`CollaborationRole`), `editor/commands.rs:86,357,368,380` | every mutation command path already checks `can_write_collaboration()` |
| Remote caret rendering | `editor/mod.rs:461` (`ExternalCaret { offset, color_rgb }`), `lifecycle.rs:471` `set_external_carets` | painted per paragraph; presence layer just needs to feed it |
| Full document replace | `lifecycle.rs:274` `replace_document_from_collaboration` | used for join bootstrap / resync |
| Editor events | `crates/gpui-flowtext/src/api.rs:107` | `EditorEvent::{Changed, SelectionChanged, …}` + `EditorEventSink` |
| Document model | `crates/gpui-flowtext/src/document/*` | `Document { text: crop::Rope, paragraphs, blocks, assets, ids, sections, offset_index, theme }`. Rope = paragraph texts joined by `\n` (no trailing newline; see `edit_ops/offsets.rs:19 paragraph_width`). Non-text blocks (`Image`, `Equation`, `Table`) occupy **no** rope bytes. Table cells own their text privately (`TableCellParagraph { paragraph, text }`). |
| Run styles | `document/text.rs:192` | `RunStyles { semantic: RunSemanticStyle, direct_underline: bool, strikethrough: bool, highlight: Option<HighlightStyle> }` — `Copy`, 4 orthogonal axes → 4 CRDT mark keys |
| Save machinery | `editor/style_state.rs:233 save`, `:247 save_as`, autosave in `workspace/workspace/documents.rs:865` | autosave already skips editors with `document_path == None` — joiners get correct behavior for free |
| Wire encoding helper | `collaboration.rs:280` `encode_canonical_operations` (postcard) | **Will NOT be used for sync** (it silently drops `ReplaceParagraphSpan`/block ops). Leave it; the CRDT layer replaces it. |

**Key consequence:** the integration burden is *translation*, not editor surgery. The editor already emits a canonical op stream and already accepts externally-driven document mutations. What's missing: (a) a CRDT document that both sides converge on, (b) a network layer, (c) session orchestration + UI.

Other load-bearing facts about the host app:

* **No tokio anywhere.** GPUI provides its own (smol-based) executors: `cx.background_executor().spawn(...)`, `cx.spawn(async move |entity, cx| …)`. iroh requires tokio → we run a dedicated tokio runtime thread (§12.1).
* `Workspace` (`crates/flowstate/src/workspace/workspace/mod.rs:65`) owns `document_panels: Vec<Entity<DocumentPanel>>`, `active_editor: Option<Entity<RichTextEditor>>`, and an `editor_subscriptions: Vec<(Uuid, Subscription)>` list — panels are keyed by `Uuid`.
* `cx.observe(&editor, …)` per panel (`documents.rs:178`) is how autosave is driven — the collab session uses the same mechanism to drain local edits.
* The document **render theme is a local user preference** (`documents.rs:153`: "DB8 stores style assignments, not style appearance") — **do not sync `DocumentTheme`**. Only style *slot assignments* (which are part of `ParagraphStyle`/`RunStyles`) sync.
* Commands are declared in `crates/flowstate/src/commands.rs` (`CommandId` + `COMMAND_SPECS`); top-bar menus in `workspace/workspace/top_bar.rs`; status bar in `render_status.rs`; modal/dialog/notification widgets come from `gpui-component` (`dialog.rs`, `notification.rs`, `avatar/`, `clipboard.rs`).

---

## 3. Architecture overview

```
┌────────────────────────────  Flowstate process (host or joiner)  ───────────────────────────┐
│                                                                                              │
│  GPUI main thread                                   tokio runtime thread (new, lazy)         │
│  ┌──────────────────────────────────────────┐       ┌────────────────────────────────────┐  │
│  │ Workspace                                │       │  CollabNet (flowstate-collab::net) │  │
│  │  └─ DocumentPanel ── RichTextEditor      │       │   • iroh Endpoint (one per app)    │  │
│  │        ▲ collab_apply / take_edits       │       │   • protocol::Router, ALPN         │  │
│  │        │                                 │       │     b"flowstate/collab/0"          │  │
│  │  CollabSession (Entity, per panel)       │       │   • per-peer send/recv tasks       │  │
│  │   • LoroDoc + EphemeralStore             │ async │   • star topology:                 │  │
│  │   • DocBinding (ids ↔ containers)        │ chan  │       host accepts N conns         │  │
│  │   • LocalApplier / RemoteApplier         │◄─────►│       joiner dials 1 conn          │  │
│  │   • participant roster, role             │       │   • frame codec (postcard+len)     │  │
│  │  CollabManager (Global)                  │       └────────────────────────────────────┘  │
│  │   • owns runtime handle + NetCommand tx  │                                               │
│  └──────────────────────────────────────────┘                                               │
└──────────────────────────────────────────────────────────────────────────────────────────────┘

      host ──── QUIC (iroh: direct holepunched, or n0 relay fallback) ──── joiner(s)
```

**Topology: star.** The host is the hub; joiners connect only to the host. The host imports every received update into its own `LoroDoc` and relays the *raw bytes* to all other peers (loro updates are idempotent and commutative, so blind relay is safe). This matches the "host owns the file" authority model, makes permission enforcement a host-side concern, and avoids gossip/mesh complexity. iroh-gossip is deliberately **not** used.

**Data flow for one keystroke (host or joiner):**

1. User types → existing edit pipeline mutates `Document`, pushes an `EditRecord`, queues a `CollaborationEdit` (canonical ops).
2. `cx.observe(editor)` fires → `CollabSession::flush_local_edits` drains the queue → `LocalApplier` translates canonical ops into loro container ops inside one `LoroDoc` commit.
3. loro's `subscribe_local_update` callback hands the session a `Vec<u8>` update blob → session sends `NetCommand::Broadcast` over the async channel → tokio thread writes a `Frame::DocUpdate` to peer connection(s).
4. Remote side: tokio thread reads frame → `NetEvent::Frame{peer, bytes}` → session entity `doc.import(bytes)` → loro fires container diff events → `RemoteApplier` maps diffs to paragraph/block mutations → editor repaints, remote selection remapped.

The CRDT doc lives **on the GPUI main thread inside the session entity** (loro ops are microseconds; only network I/O leaves the thread). `LoroDoc` is `Send + Sync`, which we exploit only for the one-time snapshot export/import on join (background executor).

### 3.1 Crate layout (new code)

```
crates/flowstate-collab/            # NEW crate. Core logic. NO gpui dependency → headless-testable.
  Cargo.toml
  src/lib.rs                        # pub mod …, shared error type (thiserror not in tree: use anyhow per workspace norm)
  src/protocol.rs                   # Frame enum, PROTOCOL_VERSION, frame codec (≤400 LOC)
  src/ticket.rs                     # SessionTicket (iroh-tickets Ticket impl) (≤150 LOC)
  src/schema.rs                     # loro container layout constants + builders + projection types (≤500 LOC)
  src/binding.rs                    # DocBinding: container ↔ ParagraphId/BlockId table (≤350 LOC)
  src/local_apply.rs                # CanonicalOperation → loro ops (≤700 LOC)
  src/remote_apply.rs               # loro DiffEvent batch → DocPatch list (≤700 LOC)
  src/projection.rs                 # LoroDoc → Vec<InputBlock> (+ reverse: Document → fresh LoroDoc) (≤450 LOC)
  src/presence.rs                   # EphemeralStore wrapper: PresenceState, cursors codec (≤300 LOC)
  src/integrity.rs                  # projection hash (xxhash via rustc-hash? no → use `twox-hash`), resync decision (≤150 LOC)
  src/session_core.rs               # role/roster/session-id types shared by host & joiner (≤250 LOC)
  src/net/mod.rs                    # NetCommand/NetEvent enums, channel types (≤200 LOC)
  src/net/runtime.rs                # tokio runtime thread bootstrap, Endpoint construction (≤250 LOC)
  src/net/host.rs                   # accept loop, per-peer tasks, relay fan-out, auth (≤500 LOC)
  src/net/joiner.rs                 # dial, handshake, reconnect-with-backoff loop (≤400 LOC)
  src/net/framing.rs                # length-prefixed read/write on iroh send/recv streams (≤150 LOC)
  tests/convergence.rs              # multi-peer fuzz (THE acceptance test, §21.2)
  tests/translation.rs              # golden tests per canonical op
  tests/loopback.rs                 # real iroh sockets on localhost, 2–3 peers

crates/flowstate/src/collab/        # NEW module in the app crate. GPUI glue + UI.
  mod.rs                            # CollabManager global, init, plumbing (≤300 LOC)
  session.rs                        # CollabSession entity (per panel): owns LoroDoc side, drains edits,
                                    #   applies remote patches via editor API, roster state (≤700 LOC)
  share_dialog.rs                   # start/join/manage modal (gpui-component Dialog) (≤500 LOC)
  status.rs                         # status-bar pill + tab badge elements (≤250 LOC)

crates/gpui-flowtext/src/rich_text/editor/collab_apply.rs   # NEW: editor-side patch application (≤600 LOC)
```

`flowstate-collab` depends on: `loro`, `iroh`, `iroh-tickets`, `tokio`, `async-channel`, `postcard`, `serde`, `uuid`, `anyhow`, `rand`, `twox-hash`, and `gpui-flowtext` (for `CanonicalOperation`, `Document`, `InputBlock` types only — this is a deliberate dependency direction: collab knows the document model; the document engine does *not* know collab/loro).

---

## 4. Dependencies (exact)

Run from workspace root (per CLAUDE.md, use the CLI — these land in `[workspace.dependencies]` then get referenced from member crates):

```sh
cargo add --package flowstate-collab iroh@0.98                                         # default features; relay/discovery wiring per §23-V1
cargo add --package flowstate-collab iroh-tickets@0.98
cargo add --package flowstate-collab loro@1.13
cargo add --package flowstate-collab tokio@1 --features rt-multi-thread,macros,time,sync
cargo add --package flowstate-collab async-channel@2
cargo add --package flowstate-collab postcard@1 --features use-std
cargo add --package flowstate-collab rand@0.9
cargo add --package flowstate-collab twox-hash@2
cargo add --package flowstate-collab --dev proptest@1
```

Version rationale:

* **iroh 0.98.2** is the latest stable (1.0.0-rc.1 exists as of 2026-05-27). Pin 0.98; the 1.0 surface is the same post-rename API (`Endpoint`, `EndpointId`, `EndpointAddr`, `endpoint.addr()`, `iroh::protocol::{Router, ProtocolHandler, AcceptError}`). Migration to 1.0 final is a version-bump task later, not an architecture change.
* **iroh-tickets** is the post-0.94 home of ticket types (`EndpointTicket`, the `Ticket` trait); keep its version in lockstep with iroh.
* **loro 1.13.1** — stable 1.x line; has everything we need: `LoroDoc`, `LoroText` (+`*_utf8` APIs), `LoroMap`, `LoroMovableList`, `ExportMode::{Snapshot, updates(&vv), all_updates()}`, `subscribe_local_update`, `subscribe` (diff events), `UndoManager`, `Cursor`, `awareness::EphemeralStore`.
* **async-channel** — runtime-agnostic MPMC; both smol-side (GPUI) and tokio-side can await it. This is the only inter-runtime bridge primitive. Do not use `tokio::sync::mpsc` across the boundary.
* **twox-hash** — fast non-crypto hash for projection integrity checks.

`crates/flowstate` adds `flowstate-collab` as a workspace dep. `gpui-flowtext` gains **no** new dependencies.

---

## 5. CRDT schema (loro container layout)

The loro document is the shared, convergent representation. Each peer's flowtext `Document` is a *projection* of it. Schema version is stored in the doc so future formats can migrate.

```
LoroDoc
├─ "meta": LoroMap
│   ├─ "schema"      : i64    = 1                 (bump on breaking layout change)
│   ├─ "title"       : string = host display name of the doc (informational)
│   └─ "created_by"  : string = host EndpointId (z32/hex, informational)
└─ "blocks": LoroMovableList                       (one entry per flowtext Block, same order)
    └─ [i]: LoroMap                                (block container)
        ├─ "kind"  : string  ∈ {"p", "image", "equation", "table"}
        ├─ for "p" (paragraph):
        │    ├─ "text"  : LoroText                 (rich text w/ marks, see below)
        │    └─ "style" : i64                      (ParagraphStyle encoded: Normal = -1, Custom(s) = s)
        └─ for "image" | "equation" | "table":
             ├─ "data" : binary                    (postcard-encoded InputBlock payload, §5.3)
             └─ "rev"  : i64                       (monotonic bump; makes replaces visible in events)
```

### 5.1 Rich text marks (per-paragraph `LoroText`)

`RunStyles` has 4 orthogonal axes → 4 mark keys:

| Mark key | Value | Absent means |
|---|---|---|
| `"sem"` | i64 slot of `RunSemanticStyle::Custom(slot)` | `RunSemanticStyle::Plain` |
| `"ul"` | bool `true` | `direct_underline == false` |
| `"strike"` | bool `true` | `strikethrough == false` |
| `"hl"` | i64 slot of `HighlightStyle::Custom(slot)` | `highlight == None` |

**Mark expansion: configure ALL four keys as `ExpandType::None`** via `LoroDoc::config_text_style` on every doc before first use (factor into `schema::configure_text_styles(&LoroDoc)`). Rationale: flowtext computes the styles of every inserted character explicitly (`pending_styles` / run inheritance in `edit_pipeline.rs`), so the CRDT must not "helpfully" expand neighboring marks — the LocalApplier always marks inserted ranges itself. This makes mark state deterministic and identical to flowtext's run model.

Reading a paragraph back: `text.to_delta() -> Vec<TextDelta>` (insert segments + attribute maps) → fold into `Vec<InputRun>` (adjacent equal-style segments merge, mirroring `edit_ops` run normalization).

**Soft line breaks** (`\u{2028}`, `document/core.rs:12`) are ordinary characters inside paragraph text — no special handling.

### 5.2 Why per-paragraph texts in a list (not one giant LoroText)

Decision (final — see §20): one `LoroText` per paragraph inside a `LoroMovableList`, **not** a single body text with `\n` separators.

* The canonical op stream already speaks `(ParagraphId, utf8 byte)` — translation to `(container, offset)` is direct, with **no global offset arithmetic** anywhere. Global-offset bookkeeping across interleaved non-text blocks is where multi-week debugging cycles go to die.
* Blast radius: a translation bug corrupts one paragraph, not the document; per-paragraph regeneration makes remote apply trivially correct.
* Block move/insert/delete map 1:1 onto `LoroMovableList` ops (true move semantics, no delete+reinsert identity loss).
* Known accepted anomaly: if peer A splits a paragraph at offset k while peer B concurrently types at offset j > k in the same paragraph within one round-trip, B's text converges to the end of the first half rather than inside the new second paragraph. No data loss, positions slightly off, self-consistent on all peers. This is the standard per-block CRDT trade-off (Notion-class behavior). Document it in code; revisit only with evidence it bites in practice.

### 5.3 Opaque block payloads (`"data"`)

`Image`, `Equation`, `Table` blocks are **atomic last-writer-wins values** in v1. Justification from the codebase: flowtext itself records table/equation/image edits as whole-block replaces (`CanonicalOperation::ReplaceBlock` carries no finer payload — see `editor/tables.rs:174`, `table_equation_editing.rs:66,280,336`, `media.rs:164,224`, `object_selection.rs:250`). A cell-level table CRDT would exceed the granularity of the host editor's own edit stream — pure cost, no fidelity gain, until the editor itself gets finer-grained table ops.

Payload type (in `schema.rs`):

```rust
#[derive(Serialize, Deserialize)]
pub enum BlockPayload {              // postcard-encoded into the "data" value
  Image { asset_id: u128, mime: String, original_name: Option<String>,
          content_hash: u64, byte_len: u64,        // asset BYTES ARE NOT IN THE CRDT (§16)
          alt_text: String, caption: Option<InputParagraph>,
          sizing: InputImageSizing, alignment: InputBlockAlignment },
  Equation { source: String, display: InputEquationDisplay },   // syntax: only Latex exists today
  Table(InputTableBlock),
}
```

Conversions `Block ↔ BlockPayload` reuse the existing `Input*` types (`document/text.rs:105-189`) and whatever `InputBlock → Block` builders the persistence/clipboard paths already use (locate via `RichClipboardFragment` consumers in `edit_ops/rich_fragment.rs`; do not write a second converter).

Concurrent edits to the same table = LWW on `"data"` (one peer's table edit wins; the other's is lost *for that block only*). The `"rev"` bump guarantees a map-diff event fires even if values were equal-ish. Acceptable v1 semantics; logged as a known limitation.

### 5.4 Identity & the DocBinding

Cross-peer identity = **loro container identity** (each block map / text container has a `ContainerID`). Flowtext's `ParagraphId`/`BlockId` are **peer-local** (they are generated fresh on load and never serialized to db8 — see `document_ids_for_shape` / `reconcile_document_ids` in `document/core.rs`). Never ship them over the wire; never store them in the CRDT.

Each session maintains the bridge table (in `binding.rs`):

```rust
pub struct DocBinding {
  rows: Vec<BindingRow>,                          // parallel to BOTH document.blocks and loro "blocks" list
  by_paragraph: FxHashMap<ParagraphId, usize>,    // rebuilt on structural change
  by_block: FxHashMap<BlockId, usize>,
  by_container: FxHashMap<ContainerID, usize>,
}
pub struct BindingRow {
  pub map: LoroMap,                                // handle to blocks[i]
  pub text: Option<LoroText>,                      // Some for kind=="p"
  pub kind: BlockKind,                             // P | Image | Equation | Table
  pub block_id: BlockId,                           // local flowtext id at this position
  pub paragraph_id: Option<ParagraphId>,           // local flowtext id (paragraph blocks only)
}
```

**Invariant (assert in debug builds after every local flush and every remote apply):**
`binding.rows.len() == document.blocks.len() == loro_blocks.len()`, `rows[i].kind` matches `document.blocks[i]`, `rows[i].paragraph_id == document.ids.paragraph_ids[paragraph_ordinal(i)]`, and `rows[i].block_id == document.ids.block_ids[i]`.

When a *remote* change creates a block, the RemoteApplier generates **fresh local ids** (`new_paragraph_id()` / `new_block_id()`) and writes them into both `Document.ids` and the binding row. When a *local* edit creates one, the LocalApplier reads the id flowtext just minted (via `identity_map` / `document.ids`) and records it next to the new container.

### 5.5 Offset discipline (read this twice)

* flowtext: **UTF-8 byte offsets** everywhere (`DocumentOffset.byte`, run `len`s, canonical ops).
* loro `LoroText`: default API is **Unicode scalar positions**; UTF-8 variants exist (`mark_utf8`, `len_utf8`, `convert_pos` between `Utf8`/`Unicode`/`Utf16`/`Event` coordinate systems; insert/delete have unicode signatures — use the `*_utf8` variants where provided, otherwise `convert_pos` first; see §23-V3).
* loro **events** report positions in *event* coordinates (unicode by default). The RemoteApplier must convert event deltas to UTF-8 for selection remapping (`convert_pos(.., PosType::Event, PosType::Utf8)` or by walking the materialized string — pick one, write one helper, test it on multibyte text: `"é🌍\u{2028}x"`).
* Every module that crosses the boundary funnels through two helpers in `schema.rs` — `to_loro_pos(text, utf8_byte) -> loro_pos` and `to_utf8_byte(text, loro_pos) -> usize` — **no inline conversions anywhere else**.
* Debug assertion after each paragraph mutation: `text.len_utf8() == paragraph_text_len(paragraph)`.

---

## 6. LocalApplier: `CanonicalOperation` → loro

`local_apply.rs`. Entry point:

```rust
pub struct LocalApplier<'doc> { doc: &'doc LoroDoc, binding: &'doc mut DocBinding }
impl LocalApplier<'_> {
  /// Apply ONE editor edit-record's ops, then `doc.commit_with(origin = "local-edit")`.
  /// One EditRecord == one loro commit == one undo unit.
  pub fn apply(&mut self, document: &Document, ops: &[CanonicalOperation]) -> anyhow::Result<()>;
}
```

`document` is the **post-edit** flowtext document (canonical ops reference post-edit identity — `identity_map.reconcile` has already run). Per-op translation:

| Canonical op | Loro translation |
|---|---|
| `InsertText { paragraph, byte, text, styles }` | row = `binding.by_paragraph[paragraph]`; `row.text.insert` at converted pos; then apply non-default marks over the inserted range (`sem`/`ul`/`strike`/`hl` per §5.1). Expansion is `None`, so a plain insert needs marking only when `styles != RunStyles::default()`. |
| `DeleteRange { start_paragraph, start_byte, end_paragraph, end_byte }` | same paragraph → single `delete`. Cross-paragraph (this is a **join**): (1) read tail delta of `end_paragraph` from `end_byte`; (2) delete `[start_byte..]` of start text; (3) delete the whole-paragraph containers strictly between start and end (movable-list `delete`); (4) append the saved tail (text+marks) to start text; (5) delete end container. Update binding rows + `Document`-side ids are already updated by flowtext. |
| `SplitParagraph { paragraph, byte, new_paragraph }` | (1) read tail delta `[byte..]`; (2) delete tail from original; (3) `blocks.insert_container(idx+1, LoroMap)` → set `"kind"="p"`, `"style"` copied from original; create `"text"` and insert tail delta with marks; (4) insert `BindingRow` with `paragraph_id = new_paragraph` and the `BlockId` flowtext minted (read from `document.ids.block_ids[idx+1]`). |
| `JoinParagraphs { first, second }` | degenerate `DeleteRange` (tail append + container delete), same code path. |
| `SetParagraphStyle { paragraph, style }` | `row.map.insert("style", encode(style))`. |
| `SetRunStyles { paragraph, range, styles }` | for each of the 4 keys: if target value non-default → `mark_utf8(range, key, value)`; else → `unmark` over the range (use the utf8 variant per §5.5). |
| `ReplaceParagraphSpan { start_paragraph, before, after }` | **workhorse**, see §6.1. |
| `InsertBlock { block, block_ix }` | read `document.blocks[block_ix]` (payload is NOT in the op); paragraph → create as in split; object → create map with `"kind"`,`"data"`(=postcard `BlockPayload`),`"rev"=0`. Insert binding row with `block_id = block`. |
| `DeleteBlock { block }` | movable-list delete at `binding.by_block[block]`; remove row. |
| `MoveBlock { block, new_block_ix }` | `blocks.mov(old_ix, new_block_ix)`; rotate binding rows. |
| `ReplaceBlock { block: Some(id) }` | re-serialize `document.blocks[ix]` → `map.insert("data", payload)`; `"rev" += 1`. `block: None` → resolve via the *single changed block* by comparing kinds/versions (flowtext bumps `Block::*.version` on edit) — if ambiguous, fall back to ReplaceDocument semantics. |
| `ReplaceDocument` | clear the movable list, rebuild every container from `document` (reuse `projection::populate_from_document`), rebuild binding. Rare (e.g. recovery/import paths, `block_insertion.rs:309`, `object_selection.rs:461`). |

### 6.1 `ReplaceParagraphSpan` translation

flowtext wraps most compound edits (paste, backspace at boundary, IME, style commands across paragraphs, drag-drop) into one coarse op carrying full `before`/`after` `DocumentSpan`s (the captured range deliberately includes ±1 neighbor paragraphs — `edit_pipeline.rs:140 edit_capture_range`). Algorithm:

1. `start = binding.by_paragraph[start_paragraph]` (fallback: `before.start_paragraph` index — keep the same fallback the existing `apply_canonical_operation` uses, `lifecycle.rs:414`).
2. Positional pairing: paragraphs `0..min(n_before, n_after)` are *matched*; surplus `after` paragraphs are *inserted* at the end of the span; surplus `before` paragraphs are *deleted*. (This matches flowtext's own positional identity reconciliation, and because spans include unchanged neighbor paragraphs, edge pairs typically no-op.)
3. For each matched pair `(b, a)`:
   * if text and runs are byte-identical → skip;
   * else: `row.text.update(a_text, UpdateOptions)` (loro's built-in Myers diff produces minimal CRDT ops); then reconcile marks: compute the desired mark intervals from `a.runs`; clear-and-remark only if runs differ from `b.runs` (compare run vectors; paragraphs are small — full remark of one paragraph is acceptable and *simple*); update `"style"` if changed.
4. Inserted paragraphs: create containers as in `SplitParagraph`, reading `style`/runs/text from the `after` span, binding to the ids flowtext minted at those indices (`document.ids.paragraph_ids[start + i]`).
5. Deleted paragraphs: movable-list delete + drop rows.
6. Debug-assert binding invariant (§5.4).

**Correctness note:** `text.update()` + concurrent remote edits to the same paragraph can interleave at diff granularity rather than keystroke granularity. That is normal CRDT behavior (same class as Google Docs' transform granularity) and converges; do not try to be cleverer.

### 6.2 The double-apply trap (must implement exactly)

`insert_single_grapheme_fast_path` (`edit_pipeline.rs:44-73`) **merges** consecutive single-grapheme inserts into the *previous* undo record by mutating its `canonical_operations[0].text` in place. If the collab layer naively re-reads "the last record's ops" after each notify, the second keystroke would re-apply the whole accumulated string ("h", then "he", then "hel"…).

Fix (part of M3's editor changes): the editor gets an explicit **pending collab queue** that receives exactly the delta of each mutation:

```rust
// editor/mod.rs additions
pending_collab_edits: Vec<CollaborationEdit>,     // drained by the session
```

* In the fast path: push `CollaborationEdit { operations: vec![InsertText{ …, text: just_this_grapheme }] }` regardless of undo-record merging.
* In `mark_document_changed_with_reconcile`: push the new record's canonical ops **only when a new record was pushed this call** (pass the ops in, don't re-read `undo_stack.last()`).
* Undo/redo paths must **not** push (they're handled by the loro UndoManager, §8) — gate pushes on "not currently restoring history" (`after_history_restore` path) and "not currently applying remote" (a `suppress_collab_capture: u32` counter the RemoteApplier increments, mirroring the existing `suppress_mutation_notify` pattern).
* `take_pending_collab_edits() -> Vec<CollaborationEdit>` drains. Keep `last_collaboration_edit` accessors untouched for compatibility.

Add a unit test in gpui-flowtext: type "a","b","c" via the fast path → drained queue is exactly three 1-char `InsertText` ops.

---

## 7. RemoteApplier: loro diff → flowtext `Document`

`remote_apply.rs` (pure: produces patches) + `editor/collab_apply.rs` (applies them via editor internals).

Subscribe once per session: `doc.subscribe_root(...)` (or `doc.subscribe(&blocks_container_id, ...)`) — the callback receives `DiffEvent`s **synchronously during `import()`/undo**, on the calling thread (main thread — good). Filter by trigger: only process events whose trigger is *Import* or *Checkout/Undo*; *Local* events are echoes of the LocalApplier and must be ignored (§23-V4 for the exact enum name).

Collect into a patch list (pure data, unit-testable):

```rust
pub enum DocPatch {
  ParagraphText { row: usize, new: InputParagraph,           // full regenerated paragraph
                  delta_utf8: Vec<TextDeltaUtf8> },           // retain/insert/delete in utf8 bytes, for selection remap
  ParagraphStyle { row: usize, style: ParagraphStyle },
  BlockData     { row: usize, payload: BlockPayload },
  Insert        { row: usize, blocks: Vec<InputBlock> },      // built from new containers
  Delete        { row: usize, count: usize },
  Move          { from: usize, to: usize },
}
```

Mapping rules:

* **Text diff on a paragraph container** → regenerate that paragraph wholesale from loro (`to_delta()` → `InputParagraph`) — never incremental-patch flowtext runs from deltas (regeneration is O(paragraph) and immune to drift). Also emit the utf8-converted delta for caret remapping.
* **Movable-list diff** → `Insert`/`Delete`/`Move` patches; for inserts, build `InputBlock`s by reading the new containers (`projection::input_block_from_container`).
* **Map diff** with `"style"` → `ParagraphStyle`; with `"data"`/`"rev"` → `BlockData`.

`collab_apply.rs` (editor side) applies a batch:

```rust
impl RichTextEditor {
  pub fn apply_collab_patches(&mut self, patches: &[DocPatch], cx: &mut Context<Self>) { … }
}
```

Implementation requirements (use existing machinery, don't reinvent):

1. Wrap the whole batch in `suppress_collab_capture += 1` (so these mutations never echo back) and reuse the `layout_invalidation_hint` + `after_text_mutation(cx)` pattern from `apply_remote_operations` (`lifecycle.rs:265-272`) for cache invalidation.
2. `ParagraphText`: build the replacement via existing span machinery — `capture_document_span` + `apply_document_span_replacement` for the single paragraph, or a focused `replace_paragraph_content(document, ix, InputParagraph)` helper added to `edit_ops` (new file `edit_ops/collab.rs` if it doesn't fit an existing one). Preserve `ParagraphId` (identity unchanged), bump paragraph `version`.
3. Structural patches: splice `document.blocks`/`paragraphs`/`ids` with **explicitly provided ids** (from the binding rows the RemoteApplier created) — do not let `reconcile_document_ids` invent them (call it only to assert no-op afterwards in debug).
4. **Selection remap:** if `self.selection` head/anchor sits in a patched paragraph, walk `delta_utf8` (retain/insert advance, delete clamps) to the new byte offset; if its paragraph was deleted, clamp to the nearest surviving offset. Same for `table_cell_*`/`equation_source_*` editing offsets: if the active table/equation block got `BlockData`-replaced remotely, exit the cell/equation editing mode gracefully (clear `selected_block`, keep selection at block boundary).
5. **Undo safety:** remote patches invalidate positional undo records. Per §8, during a session the local undo stack is unused (undo routes through loro). On session start, clear `undo_stack`/`redo_stack`; in `apply_collab_patches`, debug-assert they stay empty.
6. **Deferral:** if `self.selecting || self.active_text_drag.is_some() || self.image_resize_drag.is_some() || self.table_column_resize_drag.is_some()` or IME composition is active (marked-text state in `editor/platform.rs`), the session queues patches and retries on the next observe tick / 16 ms timer instead of mutating mid-gesture.

### 7.1 Bootstrap & full resync

Join: import host snapshot into a fresh configured `LoroDoc` (background executor — `LoroDoc` is Send), then on the main thread run `projection::document_from_loro(&doc, theme)` → `Vec<InputBlock>` → build `Document` (existing input-builder path) → `editor.replace_document_from_collaboration(document, cx)` → build `DocBinding` by walking containers. Resync (integrity failure or reconnect-too-stale): identical path, plus selection clamp.

`projection.rs` also provides the inverse, `populate_from_document(&LoroDoc, &Document)` — used by the **host once at session start** (and by `ReplaceDocument`). Round-trip property test: `document → loro → document` is identity on (text, runs, styles, block structure, table/equation/image payloads), modulo `AssetRecord.bytes` (§16).

### 7.2 Integrity checking (belt & suspenders)

`integrity.rs`: `projection_hash(&Document) -> u64` — twox-hash over (paragraph styles, run vectors, paragraph texts, block kinds + payload bytes, in order; explicitly NOT ids/theme/sections/offset-index). Host piggybacks `Frame::IntegrityProbe { vv: Vec<u8>, hash: u64 }` every 30 s of activity or every 256 relayed updates. Joiner compares **only when version vectors are equal** (otherwise skip — they're mid-flight); on mismatch → log loudly, send `Frame::ResyncRequest`, host responds with fresh snapshot, joiner rebuilds (§7.1) and posts a non-blocking notification ("Document resynced"). This converts any residual translation bug from "silent divergence forever" into "blip + log we can fix."

---

## 8. Undo/redo during a session

Positional `EditRecord` replay is unsound once remote edits interleave. While a session is active:

* Session creates `loro::UndoManager::new(&doc)` (tracks only this peer's ops — exactly Google-Docs semantics), with merge interval ≈ 500 ms to mirror flowtext's grapheme coalescing; configure `set_max_undo_steps(undo-stack depth ~ 300)`.
* Editor gains a redirect hook (set by the session, cleared on session end):

```rust
// editor/mod.rs
pub enum UndoRedirect { Undo, Redo }
collab_undo_redirect: Option<std::rc::Rc<dyn Fn(UndoRedirect)>>,   // Rc: entity is single-threaded
```

`commands.rs:191 undo()` / `:205 redo()`: if redirect set → invoke it and return. The session handler calls `undo_manager.undo()` / `.redo()`; resulting doc changes surface through the same DiffEvent → `DocPatch` → `apply_collab_patches` pipeline (trigger = checkout/undo, so they pass the §7 filter; ensure the LocalApplier echo-suppression doesn't eat them).
* Caret restore: wire `UndoManager`'s cursor callbacks (`set_on_push`/`set_on_pop` storing the selection as two loro `Cursor`s; resolve with `doc.get_cursor_pos` after pop). If the cursor API fights back, v1 fallback: leave selection where the patch remap puts it. (§23-V5)
* On session start: clear editor undo/redo stacks. On session end: stacks start fresh from the final state (history across the boundary is intentionally dropped — banner copy mentions it).
* Host before session / after session uses the existing native undo unchanged.

---

## 9. Presence (carets, names, roster)

`presence.rs` wraps `loro::awareness::EphemeralStore` (timestamped LWW KV with per-key timeout, made for exactly this):

* Key = peer's `EndpointId` string. Value (postcard → loro binary value):

```rust
pub struct PresenceState {
  pub name: String,                 // display name
  pub color_ix: u8,                 // palette index assigned by host in Welcome
  pub role: Role,                   // Owner | Editor | Viewer (informational; enforcement is host-side)
  pub selection: Option<PresenceSelection>,
}
pub struct PresenceSelection {
  pub container: String,            // ContainerID of the paragraph text (head)
  pub head: Vec<u8>,                // loro Cursor::encode() — survives concurrent edits
  pub anchor_container: String,
  pub anchor: Vec<u8>,
}
```

* Timeout 30 s; the store's local-update subscription yields raw bytes → `Frame::Ephemeral(bytes)`; inbound frames → `store.apply(bytes)`; host relays ephemeral frames like doc updates.
* Update cadence: on `EditorEvent::SelectionChanged` (subscribe via `cx.subscribe` — `RichTextEditor` must be an `EventEmitter<EditorEvent>`; if it currently only pushes to `EditorEventSink`, add the `EventEmitter` impl in M3), debounced 50 ms; plus a 10 s keepalive refresh.
* Rendering: on ephemeral merge events + after every remote apply, resolve each peer's cursors: `Cursor::decode` → `doc.get_cursor_pos` → unicode pos → utf8 byte → `binding.by_container` → `DocumentOffset` → `editor.set_external_carets(vec![ExternalCaret { offset, color_rgb }], cx)`. v1 renders caret + name-on-hover only (selection-range highlight is v1.1; `ExternalCaret` keeps its current shape).
* Color palette: 8 hardcoded high-contrast RGBs in `session_core.rs`; host assigns by join order, reuses freed slots.

---

## 10–13. Networking, protocol, tickets, lifecycle

### 10.1 Runtime bridge (`net/runtime.rs`)

* Lazy global: first share/join spawns `std::thread::Builder::new().name("flowstate-collab-net")` running a `tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all()` runtime; thread parks on `runtime.block_on(net_main(cmd_rx, evt_tx))`.
* `net_main` builds **one iroh `Endpoint` for the whole app** (shared by hosting and joining):

```rust
let endpoint = Endpoint::builder()                  // use the n0 preset: relays + address lookup
    .alpns(vec![ALPN.to_vec()])                     // ALPN = b"flowstate/collab/0"
    /* preset/address-lookup wiring per §23-V1 */
    .bind().await?;
```

Discovery is NOT on by default in iroh 0.98 — apply the n0 preset (`presets::N0` / `Builder::new(preset)`) so relay + address lookup work; additionally the ticket embeds the full `EndpointAddr` (relay URL + direct addrs) so joining works even if lookup is cold (§23-V1).
* Channels: `async_channel::unbounded::<NetCommand>()` (UI→net) and `::<NetEvent>()` (net→UI). The UI side pumps events with a detached `cx.spawn` loop in `CollabManager` that routes to the right `CollabSession` entity.

```rust
pub enum NetCommand {
  HostSession   { session: SessionId, token: SessionToken, default_role: Role, reply: oneshot<SessionTicket> },
  CloseSession  { session: SessionId },                          // host: broadcast Bye, close conns
  Join          { ticket: SessionTicket, hello: HelloInfo },
  LeaveSession  { session: SessionId },
  SendTo        { session: SessionId, peer: EndpointId, frames: Vec<Frame> },
  Broadcast     { session: SessionId, frame: Frame, except: Option<EndpointId> },
  Shutdown,
}
pub enum NetEvent {
  HostReady       { session: SessionId, ticket: SessionTicket },
  PeerConnected   { session: SessionId, peer: EndpointId, hello: HelloInfo },   // host side, post-auth
  Frame           { session: SessionId, peer: EndpointId, frame: Frame },
  PeerDisconnected{ session: SessionId, peer: EndpointId, reason: String },
  JoinEstablished { session: SessionId },                                       // joiner side, post-Welcome (Welcome arrives as Frame)
  JoinFailed      { session: SessionId, reason: JoinError },
  Reconnecting    { session: SessionId, attempt: u32 },
  SessionClosed   { session: SessionId, reason: CloseReason },
}
```

### 10.2 Host accept path (`net/host.rs`)

`Router::builder(endpoint).accept(ALPN, FlowstateProtocol{state}).spawn()` with `impl ProtocolHandler` (`async fn accept(&self, connection: Connection) -> Result<(), AcceptError>`). Per connection: `accept_bi()` → read `Frame::Hello` (5 s deadline) → validate `protocol_version`, `session_id` exists, `token` matches (constant-time compare), peer not already connected, roster < MAX_PEERS(8) → register peer (writer task owns the send half + an mpsc inbox; reader task forwards frames as `NetEvent::Frame`) → emit `PeerConnected`. The **session entity** (not the net thread) decides the `Welcome` content because the snapshot must be exported atomically with update-stream attachment: on `PeerConnected`, the session synchronously (a) exports `ExportMode::Snapshot` (or `updates(vv)` for a reconnect Hello carrying `last_vv`), (b) marks the peer live for future broadcasts — both under the same borrow of the session state so no update can fall between snapshot and stream. Then `SendTo(Welcome{...})`.

Relay rule: every authenticated inbound `DocUpdate`/`Ephemeral` from peer P → import/apply on host AND `Broadcast{except: P}` *of the same bytes*. Updates from a `Viewer` peer: **dropped, not imported, not relayed** (and the host replies `Frame::Rejected{reason: ReadOnly}` so a buggy client can self-correct via resync).

### 10.3 Joiner path (`net/joiner.rs`)

`endpoint.connect(ticket.addr, ALPN)` → `open_bi()` → send `Hello` → await `Welcome` (15 s deadline) → emit `JoinEstablished` + deliver Welcome frame → then a read loop + write loop. Reconnect: on connection loss (not explicit `Bye`/`SessionClosed`), loop with backoff 0.5/1/2/4/8…30 s; each attempt sends `Hello { last_vv: Some(doc.oplog_vv().encode()) }`; host answers `Welcome { doc: Updates(...) }` (cheap catch-up) or `Snapshot` if it can't serve the delta. UI shows "Reconnecting…" via `NetEvent::Reconnecting`; user can cancel (→ orphan copy).

### 11. Wire protocol (`protocol.rs`)

Framing (`net/framing.rs`): `u32-le length` + postcard bytes, max frame 2 MiB (`FrameTooLarge` error closes the connection); one long-lived bi-stream per peer for control/doc/presence; assets use separate per-transfer bi-streams (§16).

```rust
pub const PROTOCOL_VERSION: u16 = 1;
pub const ALPN: &[u8] = b"flowstate/collab/0";

#[derive(Serialize, Deserialize)]
pub enum Frame {
  Hello    { protocol: u16, app: String, session: SessionId, token: SessionToken,
             name: String, last_vv: Option<Vec<u8>> },
  Welcome  { roster: Vec<PeerInfo>, your_color: u8, your_role: Role,
             doc: DocBootstrap /* Snapshot(Vec<u8>) | Updates(Vec<u8>) */,
             asset_manifest: Vec<AssetMeta> },
  Rejected { reason: RejectReason },     // BadToken | Version{min,got} | Full | ReadOnly | UnknownSession
  DocUpdate(Vec<u8>),                    // loro export bytes (local update or relay)
  Ephemeral(Vec<u8>),                    // EphemeralStore encode bytes
  IntegrityProbe { vv: Vec<u8>, hash: u64 },
  ResyncRequest,
  ResyncSnapshot(Vec<u8>),
  AssetRequest { id: u128 },             // answered on a NEW bi-stream with AssetHeader+chunks
  AssetOffer   { meta: AssetMeta },      // joiner → host: "I have a new asset, pull it"
  RosterUpdate { roster: Vec<PeerInfo> },
  Bye { reason: CloseReason },           // SessionEnded | Kicked | HostShutdown
  Ping(u64), Pong(u64),                  // 15 s heartbeat (also drives presence staleness)
}
```

Version policy: exact `PROTOCOL_VERSION` match required; mismatch → `Rejected::Version` with a human-readable "host is running a newer/older Flowstate" surfaced in the join dialog.

### 12. Ticket (`ticket.rs`)

```rust
pub struct SessionTicket {
  pub addr: EndpointAddr,        // includes relay URL + direct addresses
  pub session: SessionId,        // u128 random
  pub token: SessionToken,       // [u8; 16] random (rand::rngs::OsRng) — the join secret
  pub title: String,             // doc display name, shown in join dialog before connecting
}
```

Implement the `iroh-tickets` `Ticket` trait (postcard payload, base32 display) with prefix/kind `"fscollab"`. The string is the only thing users exchange. Threat model note for implementers: the token gates membership; anyone with the ticket can join until the host ends the session — that *is* the product. The host's authority means a malicious joiner can at worst vandalize the shared doc (undoable; host can end session), never touch the host FS.

### 13. Session lifecycle state machines

`crates/flowstate/src/collab/session.rs` — `CollabSession` GPUI entity, one per panel, states:

```
Host:    Idle → Starting(net spawn + populate loro from Document + HostSession cmd)
              → Live { ticket, peers } ⇄ (peer churn)
              → Ended (user ends / tab closes / app quits → broadcast Bye, drop)
Joiner:  Connecting(ticket) → Bootstrapping(snapshot import, build Document, open tab)
              → Live ⇄ Reconnecting(attempt n)
              → Orphaned(reason)   // session over; editor stays, banner shown, collab hooks removed
Errors:  Starting/Connecting failures → toast + back to Idle (no tab opened for failed join)
```

Wiring on session start (host) / bootstrap (joiner):
1. `editor.set_collaboration_role(Some(role))`, clear undo stacks, set undo redirect (§8).
2. `cx.observe(editor)` → `flush_local_edits` (drain → LocalApplier → commit; the loro local-update subscription then emits broadcast bytes).
3. `cx.subscribe(editor)` for `SelectionChanged` → presence (§9).
4. Register session with `CollabManager` (global) for net-event routing by `SessionId`.
Teardown reverses everything: role `None` (host keeps `Owner`→`None`), redirect cleared, carets cleared, subscriptions dropped. Closing a hosting tab (`remove_document_panel`, `documents.rs:216`) and `request_close_window` must check `CollabManager::session_for_panel(id)` and confirm: "End live session with N participants?".

---

## 14. Save semantics & source of truth

* **Host:** unchanged save/autosave (`save_active`, `maybe_autosave_document`). Its `Document` projection *is* the file content. CRDT history is **not** persisted into `.db8` — a saved file is a plain snapshot; a future session starts a fresh loro doc (`populate_from_document`). No format changes.
* **Joiner:** editor opens with `document_path = None` → Save triggers Save-As (existing untitled flow), autosave skips, recovery writes skip. The panel title = host's title + " (shared)".
* **Host recovery files** (`schedule_recovery_write`) keep working — they snapshot the projection, which is exactly right.

---

## 15. Editor changes (gpui-flowtext) — complete enumerated list

All hidden behind no feature flag (inert without a session). Keep each change minimal; **no loro/iroh types may appear in this crate.**

1. `editor/mod.rs`: add fields `pending_collab_edits: Vec<CollaborationEdit>`, `suppress_collab_capture: u32`, `collab_undo_redirect: Option<Rc<dyn Fn(UndoRedirect)>>`; add `UndoRedirect` enum. (~20 LOC)
2. `edit_pipeline.rs`: per §6.2 — push exact deltas into `pending_collab_edits` (fast path pushes the single grapheme; `mark_document_changed_with_reconcile` gains an `ops: Option<&[CanonicalOperation]>` parameter or a small wrapper so it only pushes when a new record was created and `suppress_collab_capture == 0`). (~40 LOC)
3. `lifecycle.rs`: `take_pending_collab_edits()`, `set_collab_undo_redirect`, `clear_collab_session_state` (clears queue+redirect+external carets). Clear the queue in `dispose_for_close`/`release_transient_memory`. (~40 LOC)
4. `commands.rs` `undo()`/`redo()`: redirect check first. (~10 LOC)
5. **New** `editor/collab_apply.rs`: `apply_collab_patches` + selection remap helpers per §7 (≤600 LOC). Reuses `edit_ops` primitives; add `edit_ops/collab.rs` only if a `replace_paragraph_content` helper doesn't fit existing files.
6. `api.rs`/events: ensure `impl EventEmitter<EditorEvent> for RichTextEditor` exists so `cx.subscribe` works (it may already; verify). `EditorEvent` already carries `SelectionChanged`.
7. `DocPatch` lives in `flowstate-collab`; to keep the dependency direction (editor must not depend on collab), `apply_collab_patches` takes a **gpui-flowtext-owned** patch type: define `CollabPatch` (mirror of §7's `DocPatch`) in `collaboration.rs` (it already holds the collab vocabulary) and have `flowstate-collab` emit that type. (`flowstate-collab` already depends on gpui-flowtext — clean.)
8. Image placeholder: in the image layout/measure path (`rich_text/editor/media.rs` + `layout/block_layout.rs`), treat an `AssetRecord` with empty `bytes` as "loading": fixed 240×160 placeholder box (theme muted bg + spinner glyph). Triggered while an asset transfer is in flight (§16). (~40 LOC)

Everything else (roles gating, external carets, replace-document) already exists.

---

## 16. Assets (images) over the wire

Asset bytes never enter the CRDT (snapshots would carry every image forever). Flow:

* `BlockPayload::Image` carries `asset_id`, `mime`, `content_hash`, `byte_len` (metadata only).
* Host is the **asset authority**: `Welcome.asset_manifest` lists all assets; joiner requests the ones referenced by visible blocks first (then the rest lazily) via `Frame::AssetRequest` → host opens a fresh bi-stream: postcard `AssetHeader { id, mime, len, hash }` then raw 256 KiB chunks → joiner inserts `AssetRecord` into `document.assets` (via a small `collab_apply` patch `AssetArrived { id, record }`) → repaint replaces placeholder.
* A joiner pasting an image: image lands in its local `Document.assets` (existing paste path), block syncs via CRDT with metadata; joiner sends `AssetOffer`; host `AssetRequest`s it back over a stream, stores it, and other peers fetch from host on demand. (Star topology keeps transfers host-centric — no peer-to-peer asset mesh.)
* Dedup by `content_hash` is an optimization, not v1.

---

## 17. UI specification (gpui-component widgets)

1. **Commands** (`commands.rs`): add `CommandId::ShareDocument` ("Share / Collaborate…", APP context, no default key) and `CommandId::JoinSession` ("Join Collaboration Session…", APP). Add to `COMMAND_SPECS`; wire dispatch in the workspace command handler (`workspace/workspace/keybindings.rs` routing).
2. **Top bar** (`top_bar.rs`): File menu gains "Share Document…" (enabled when active panel is a rich-text doc) and "Join Session…". Plus a dedicated share `icon_button` on the `TitleBar` right cluster (next to settings) showing a filled/colored state while a session is live.
3. **Share dialog** (`collab/share_dialog.rs`, gpui-component `Dialog` + `Input` + `Button` + `clipboard` copy button + `avatar::AvatarGroup` + `switch` for Editor/Viewer default role):
   * Idle: role switch + "Start session".
   * Live (host): read-only `Input` with ticket string + Copy button; participant list (color dot, name, role); "End session" (danger variant, confirm).
   * Join tab: `Input` (paste ticket) + "Join" with inline validation (parse before dialing; show doc title from ticket); progress states Connecting → Receiving document (n KiB) → done (dialog closes, tab opens).
4. **Status bar** (`render_status.rs`): when the *active* panel has a session: pill `● Hosting · 3` / `● Connected` / `◌ Reconnecting…` / `● Session ended`, colored by state; click opens the share dialog. Follow the zoom/status element patterns already in that file.
5. **Tab badge** (`document_panel.rs` render): small colored dot + peer count on collaborating tabs.
6. **Notifications** (gpui-component `notification.rs`): peer joined/left, resync occurred, read-only edit attempt ("You're a viewer in this session").
7. **Orphan banner**: when a joiner session ends, a slim banner above the editor: "Session ended — this is your local copy. Save As to keep it." with a Save As button. Implement as a `DocumentPanel` child element gated on session state.
8. Errors: join failures via dialog inline error; mid-session fatal errors via `window.prompt` only if the user must act, otherwise notification + status pill.

---

## 18. Failure-mode catalog (each requires explicit handling + a test or QA item)

| # | Scenario | Required behavior |
|---|---|---|
| F1 | Garbage/truncated ticket pasted | Parse error inline in dialog; never dials |
| F2 | Host unreachable (offline, NAT+relay down) | Connect timeout 15 s → JoinFailed with actionable text |
| F3 | Token wrong / session unknown (stale ticket) | `Rejected{BadToken/UnknownSession}` → "Session no longer available" |
| F4 | Version skew | `Rejected{Version}` both directions, clear message |
| F5 | Host ends session / closes tab / quits app | `Bye{SessionEnded/HostShutdown}` (best-effort on quit) → joiners orphan with banner; net thread closes conns |
| F6 | Joiner vanishes (crash, network) | host: `connection.closed()` → roster update + notification; presence entry times out |
| F7 | Transient disconnect | joiner auto-reconnect w/ `last_vv` delta catch-up; edits made while offline are queued in loro and sync on reconnect (CRDT gives offline-tolerance for free) |
| F8 | Reconnect after host restarted session (new SessionId) | `UnknownSession` → orphan, do not silently join a different session |
| F9 | Self-join (pasting own ticket) | detect `ticket.addr.endpoint_id == endpoint.id()` → friendly error |
| F10 | Session full (>8) | `Rejected{Full}` |
| F11 | Viewer client sends DocUpdate (bug/malice) | host drops + `Rejected{ReadOnly}`; never relayed |
| F12 | Integrity hash mismatch | auto-resync (§7.2), notification, `eprintln!` diagnostics incl. both hashes + vv |
| F13 | Giant document snapshot | frames ≤2 MiB → snapshot sent as one frame up to that, else chunked `ResyncSnapshot`-style continuation (add `DocBootstrap::SnapshotChunk{n,of}` if needed); progress UI |
| F14 | Remote patch arrives mid-drag/IME | deferral queue (§7 item 6) |
| F15 | Asset referenced but transfer fails | placeholder persists + retry on next request; never blocks text sync |
| F16 | Two sessions on one doc / share twice | second Share opens existing session dialog (sessions keyed by panel Uuid) |
| F17 | Joiner opens Share on a joined doc | allowed to *view* roster, cannot start nested session (button disabled, tooltip) |
| F18 | App quit while hosting | window-close confirm includes session warning (extends existing dirty-check prompt flow in `documents.rs:734`) |

---

## 19. Performance budgets

* Keystroke→local paint: **zero added frame cost** — LocalApplier work happens in the observe callback after paint scheduling; loro insert ≈ 1–5 µs. Assert no network I/O on the main thread (channel sends only).
* Remote keystroke→repaint: ≤ 1 frame after import; per-paragraph regeneration only for touched paragraphs (the existing `layout_invalidation_hint` machinery keeps relayout scoped).
* Join bootstrap on a 5 MB doc: snapshot export+import off-main-thread; only `replace_document_from_collaboration` on-thread (same cost as opening the file — already acceptable).
* Memory: LoroDoc history grows with session length; fine for sessions (hours), irrelevant after close (dropped). No persistence.
* Add a micro-benchmark (optional, `rich_text/benchmarks/` style) only if M6 profiling shows surprises; do not pre-optimize.

---

## 20. Decisions already made — DO NOT relitigate

1. **loro schema = per-paragraph `LoroText` in a `LoroMovableList`** (§5.2), not a single body text. Accepted split-race anomaly documented.
2. **Tables/equations/images = atomic LWW payloads** matching the editor's own `ReplaceBlock` granularity (§5.3).
3. **Star topology, host authority, host relays raw update bytes.** No iroh-gossip, no mesh, no host migration in v1.
4. **CRDT lives on the main thread inside the session entity**; tokio thread is transport-only.
5. **Wire = loro update bytes**, never `CanonicalOperation`s (the existing `WireCanonicalOperation` codec stays unused).
6. **Theme/zoom/invisibility are local-only**; never synced.
7. **Undo in-session = loro UndoManager** with editor-level redirect; native undo stack cleared at session boundaries.
8. **Asset bytes out-of-band** over dedicated streams; host is asset authority.
9. **Joiners have no `document_path`**; host file is the only source of truth; CRDT history never touches `.db8`.
10. **Flow (`.fl0`) documents excluded from v1.**
11. New core logic in **`flowstate-collab`** crate (GPUI-free); GPUI glue in `crates/flowstate/src/collab/`; editor crate stays loro/iroh-free.

Deferred (tracked, not designed here): per-peer role changes + kick, selection-range rendering for remote peers, cell-level table CRDT, host migration, ticket QR codes, session persistence across host restarts, >8 peers, e2e-encryption hardening beyond QUIC+token.

---

## 21. Testing strategy

### 21.1 Unit / golden (in `flowstate-collab`)
* `translation.rs`: for every `CanonicalOperation` variant: apply to a `Document` fixture via the editor's real edit ops (construct via `document_from_input`), run LocalApplier, project loro back, assert equality with the mutated `Document`. Include multibyte text (`"é🌍"`, soft line breaks) in every text fixture.
* Projection round-trip property test (§7.1).
* Offset conversion helpers: exhaustive over a multibyte fixture.
* §6.2 double-apply regression test (in gpui-flowtext).

### 21.2 Convergence fuzz (THE gate for M2 — do not ship without it green)
`tests/convergence.rs` (proptest): N ∈ {2,3} simulated peers, each owning a `(Document, LoroDoc, DocBinding)` triple. Random op program per peer (insert/delete/split/join/style/block-insert/move/replace, weighted toward text); ops applied through LocalApplier; update bytes exchanged through a virtual network with random delay/reorder/duplication (loro tolerates all three); RemoteApplier applies inbound diffs to each peer's `Document`. After quiescence: **all projections byte-identical** (text, runs, styles, structure) and equal to a fresh projection of any peer's LoroDoc. Shrinkable failures print the op program.

### 21.3 Loopback integration (`tests/loopback.rs`)
Real iroh endpoints, localhost direct addrs, `RelayMode::Disabled`, no discovery: host + 2 joiners; assert handshake, welcome bootstrap, typed-edit relay, viewer rejection, reconnect-with-delta (kill joiner conn, redial with vv), Bye propagation. Mark `#[ignore]` if CI sandboxing blocks UDP; runnable locally.

### 21.4 Manual QA script (commit as `helpers/docs/collab_qa.md`)
Two app instances (`cargo run` ×2, different cwd): the 18 failure modes of §18 plus: simultaneous typing same paragraph, Enter-split race, table edit both sides (LWW visible), image paste joiner-side, undo tug-of-war, 30-minute soak with autosave on.

---

## 22. Milestones & work breakdown (for the orchestrating agent)

Dependency graph: `M1 ∥ M2` → `M3` → `M4` → `M5` → `M6`. Each milestone ends with `cargo clippy` clean (workspace lints) + its tests green.

**M0 — Scaffolding (small, do first, single agent)**
`cargo new crates/flowstate-collab --lib`; wire workspace deps (§4); empty modules compiling; `ALPN`/`PROTOCOL_VERSION` constants; CI keeps passing.

**M1 — Networking core (1 agent)** — `net/*`, `protocol.rs`, `ticket.rs`, framing. Deliverable: loopback test (§21.3) passing with an echo-level session (no loro yet — frames round-trip, auth verified, relay fan-out works, reconnect works). Verify §23-V1/V2 against docs.rs first.

**M2 — Document sync core (1–2 agents; the hard one)** — `schema.rs`, `binding.rs`, `projection.rs`, `local_apply.rs`, `remote_apply.rs`, `integrity.rs`, plus the gpui-flowtext `pending_collab_edits` change (§6.2) it needs for fixtures. Deliverables: §21.1 goldens + §21.2 convergence fuzz green. Start with text-only ops, then structure ops, then blocks. Verify §23-V3/V4 first.

**M3 — Editor integration (1 agent, after M2)** — §15 items 1–8 (`collab_apply.rs`, undo redirect, deferral, selection remap, placeholder), §8 UndoManager wiring, EventEmitter check. Deliverable: headless GPUI test (`gpui::TestAppContext` if available in gpui 0.2; otherwise unit-test patch application directly on `RichTextEditor` via `cx.new`) applying remote patches around an active selection.

**M4 — Session orchestration (1 agent)** — `collab/mod.rs` (CollabManager global + net-event pump), `collab/session.rs` (state machines §13, flush/import paths, presence §9, assets §16, resync §7.2), workspace hooks (close-tab/close-window confirms, joiner tab creation). Deliverable: two real app instances collaborate end-to-end (manual), F5/F6/F7 verified.

**M5 — UI (1 agent, parallel with late M4)** — §17 complete: commands, menus, share dialog, status pill, tab badge, notifications, orphan banner.

**M6 — Hardening (1 agent)** — full §18 sweep, §21.4 QA script run + fixes, perf sanity (§19), `helpers/docs/collaboration.md` (user-facing how-to + architecture notes for maintainers), final clippy/test pass.

Acceptance for the whole feature = QA script passes on Linux (primary) with two instances, including one relay-path run (different networks or forced `RelayMode::Relay`).

---

## 23. API verification appendix — check these exact names before coding

The architecture does not depend on any of these spellings; they are listed so implementers verify rather than guess:

* **V1 (iroh, M1):** discovery/preset wiring on `endpoint::Builder` in 0.98 — the docs show `Builder::new(preset)` / `preset()` / `address_lookup()` and `presets::N0`; relays default to n0 via `RelayMode::Default`, but **address lookup is off by default**. Confirm the preset that enables n0 DNS lookup + pkarr publish, and how to read `endpoint.addr()` for tickets (may need to await first addr / `online()` watcher). docs.rs/iroh/0.98.2.
* **V2 (iroh-tickets, M1):** exact `Ticket` trait path + required methods (kind/prefix, `to_bytes`/`from_bytes`), and `EndpointTicket` as reference implementation. docs.rs/iroh-tickets.
* **V3 (loro, M2):** UTF-8 variants available on `LoroText`: confirmed `mark_utf8`, `unmark_utf8?`, `len_utf8`, `convert_pos(index, PosType, PosType)`; check whether `insert_utf8`/`delete_utf8`/`splice_utf8` exist (if not, convert via `convert_pos`). Also `UpdateOptions` fields for `text.update`. docs.rs/loro/1.13.
* **V4 (loro, M2):** event subscription: `doc.subscribe_root` vs `doc.subscribe(&ContainerID)`; `DiffEvent` shape (container path, `Diff::{Text, List, Map}` payloads) and the trigger discriminator (`EventTriggerKind::{Local, Import, Checkout}` or similar) used to filter local echoes; `subscribe_local_update` callback signature (`&[u8]` vs `Vec<u8>`, and whether it fires per-commit).
* **V5 (loro, M3):** `UndoManager` construction + `set_on_push`/`set_on_pop` cursor types; `Cursor::encode/decode` (or `to_bytes`) for presence; `doc.get_cursor_pos` return shape.
* **V6 (loro, M2):** `LoroMovableList::{insert_container, delete, mov}` exact names; `LoroMap::{insert, insert_container, get}`; binary values (`LoroValue::Binary`) for `"data"`.
* **V7 (loro, M4):** `awareness::EphemeralStore`: constructor timeout param, `set_local_state`-equivalent (`set(key, value)`?), `encode/encode_all/apply`, subscription signatures (`subscribe_local_updates` yields outbound bytes; `subscribe` yields merge events), `remove_outdated`.
* **V8 (gpui 0.2, M3/M4):** `EventEmitter` impl pattern for `RichTextEditor`; `cx.set_global`/`Global` trait for `CollabManager`; `oneshot` equivalent for `NetCommand::HostSession.reply` (use `async_channel::bounded(1)` to stay runtime-agnostic instead of a tokio oneshot).

---

*End of plan. First action for the orchestrator: run M0, then dispatch M1 and M2 in parallel.*
