> **SUPERSEDED — kept for history only.** The current source of truth is
> **`FIX_LORO_ROOT.md`** (read its §0–§4 first); do not implement from this file.
>
> This doc describes the *abandoned* v1 schema: one `LoroText` **per paragraph**
> held in `BindingRow { text: Option<LoroText> }`, with "no global offset arithmetic."
> The implementation now uses a **single-root `LoroText`** — one `"body"` container,
> paragraphs separated by `\n`, a maintained paragraph→byte offset index, and a
> `"blocks"` movable list of **metadata-only** maps. Treat every per-paragraph-text,
> `BindingRow { text }`, and "no offset math" claim below as obsolete.

---

# Flowstate P2P Collaborative Editing — Implementation Plan (v2)

**Status:** approved architecture, ready for implementation
**Target:** Google-Docs-style live co-editing of `.db8` rich-text documents between symmetric peers
**Stack:** [iroh](https://docs.rs/iroh) 0.98.x (QUIC P2P) · [iroh-gossip](https://docs.rs/iroh-gossip) 0.100.x (swarm membership + broadcast) · [loro](https://docs.rs/loro) 1.13.x (rich-text CRDT)
**Authoring date:** 2026-06-12. All crate versions and API shapes verified against crates.io/docs.rs/source on this date.

**v2 supersedes v1 entirely.** Headline changes from v1: star topology replaced by a **gossip mesh of fully symmetric peers** (no host, no starter role, no relay hub); **read-only/Viewer support removed** (every peer writes); session **membership and connectivity are orthogonal state axes** (involuntary disconnects keep you in the session and resync on reconnect; only an explicit Leave detaches); sessions **end by attrition** (no end-session authority or ritual); joiners get **crash-recovery files**; explicit **leave→save prompt cascades** on tab close/app quit. The CRDT document-sync core (§5–§8) is unchanged from v1 — it never depended on topology.

---

## 0. How to use this document

This plan is written for an orchestrating agent that will spawn implementation subagents. Rules of engagement:

1. **Milestones M0–M6 (§22) are the unit of delegation.** M1 (networking) and M2 (doc sync) are independent and can run in parallel. M3 depends on M2; M4 depends on M1+M3; M5 on M4; M6 last.
2. **Do not relitigate the decisions in §20.** Each one was argued out deliberately (topology, symmetry, attrition, leave semantics). If an implementer hits a wall that genuinely invalidates one, stop and surface it; do not silently choose differently.
3. **§23 lists every external API name that must be re-verified against docs.rs at implementation time.** Those are the only facts in this document I could not pin exactly; everything else was verified against this codebase (with `file:line` references) or live documentation.
4. Per `CLAUDE.md`: new functionality in **new files**; files **under 1000 LOC** (per-file budgets are given — treat them as hard caps and split files rather than exceed them); prefer **gpui-component** widgets over raw GPUI; add dependencies **via `cargo add`**; run **`cargo clippy`** when a milestone's edits are complete and fix what it reports. This workspace denies `clippy::{pedantic, nursery, correctness, suspicious, style, complexity, perf, cargo}` wholesale with a long allow-list in the root `Cargo.toml` — read that allow-list before fighting a lint. Practical consequences: no bare `unwrap()` on `get` (`get_unwrap` denied), document every `unsafe` (`undocumented_unsafe_blocks` denied), no `mem::forget`, use `if_then_some_else_none` style, every `#[allow]` needs a `reason`.
5. Repo conventions you must follow: 2-space indent (`.rustfmt.toml`); `#[hotpath::measure]` / `#[hotpath::measure_all]` attributes on functions/impls wherever the surrounding file uses them; several modules are assembled via `include!()` concatenation (`document/mod.rs`, `edit_ops/mod.rs`, `workspace/workspace/mod.rs`) — extend them the same way, do not convert to `mod`; GPUI state lives in `Entity<T>` and is mutated via `entity.update(cx, …)`.

---

## 1. Product definition (the UX contract)

* Any user with a rich-text document tab (`.db8` editor) open can **start a collaboration session** on it. Starting produces a **ticket** — one opaque pasteable string. Anyone running Flowstate can choose **Join Session**, paste the ticket, and the shared document opens as a **new tab**. Edits, carets, and the participant list propagate live between everyone (sub-second on LAN; ~1 s worst case via relays).
* **Peers are fully symmetric.** The person who started the session has no ongoing role: they minted the session and seeded its content, and that is all. Every peer can edit, every peer can invite others (tickets are re-mintable by any member), every peer can serve a joining peer the document, and the session continues regardless of who comes and goes — including the person who started it.
* **Membership is distinct from connectivity.**
  * A network drop, laptop sleep, or relay outage does **not** remove you from a session. Your tab stays attached; your edits accumulate locally in the CRDT; reconnection is automatic; on reconnect both sides exchange what the other missed and converge. The status UI makes the offline state visible ("Offline — changes will sync when reconnected").
  * Only an explicit **Leave** detaches a document from the session. On leave, the document freezes at exactly the state you last witnessed and becomes an ordinary local document (file-backed if it has a path; an unsaved in-memory document otherwise).
* **Joining never touches your existing documents.** A join always produces a fresh tab containing a fresh document built from the session's state. There is no flow, v1 or planned, that merges a local file into a session. Save As at any time writes your current view wherever you like — and **saving does not detach you**; your saved file simply becomes your personal copy (with autosave on, it keeps tracking the session until you leave).
* **Sessions end by attrition.** There is no "end session for everyone" action and no session-ended event. A session exists while at least one peer is attached; when the last peer leaves, it is gone. A peer who finds themselves alone sees "Only you" and chooses to wait (others may drop back in) or leave.
* **No canonical file.** While a session lives, the truth is the swarm's CRDT state. Files are personal copies; which copy a team treats as "the" document is social convention, not enforced by the app. (This deliberately supersedes the original "source of truth on the starter's disk" framing — discussed and decided.)
* **Closing a tab or quitting the app while attached is an implicit leave**, guarded by a two-step prompt: "Leave session?" then the save prompt — transactionally (cancel at either step cancels everything; see §13.4).
* Out of scope for v1 (explicit): collaborating on Flow (`.fl0`) documents; read-only roles; kick/ban (remedy: start a fresh session and re-invite selectively); session resume across app restarts ("Rejoin previous session" is a designed-for v1.1, see §13.6); cell-level table CRDT; >16 peers; version-history restore.

---

## 2. What already exists (do not rebuild)

A previous iteration scaffolded collaboration hooks into the editor engine. Inventory, with file references:

| Capability | Where | Notes |
|---|---|---|
| Canonical edit-op stream with stable IDs | `crates/gpui-flowtext/src/collaboration.rs` | `CanonicalOperation` enum: `InsertText`, `DeleteRange`, `SplitParagraph`, `JoinParagraphs`, `SetParagraphStyle`, `SetRunStyles`, `InsertBlock`, `DeleteBlock`, `MoveBlock`, `ReplaceParagraphSpan`, `ReplaceBlock`, `ReplaceDocument`. **Every** editor mutation path emits these (the undo system depends on it), so the local-edit tap is complete by construction. |
| Stable identity map | `collaboration.rs` → `DocumentIdentityMap` | maps `ParagraphId`/`BlockId`/`TableCellId` (u128 UUIDs) ↔ indices; reconciled after every edit |
| Per-edit op capture | `crates/gpui-flowtext/src/rich_text/editor/edit_pipeline.rs:233` `mark_document_changed_with_reconcile` | stores `last_collaboration_edit` after every mutation; every undo record carries `canonical_operations` |
| Remote-op application primitives | `crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs:265` `apply_remote_operations` | bounds-checked, ID-addressed appliers built on `edit_ops` primitives (`insert_text_at`, `delete_cross_paragraph_range`, `split_paragraph_at`, `mutate_runs_in_range`, `apply_document_span_replacement`, `capture_document_span`) — reuse these in `collab_apply.rs` |
| Roles + write gating | `editor/mod.rs:182` `CollaborationRole`, `editor/commands.rs:86` | **Unused in v1** (everyone writes — keep `collaboration_role: None` always). Do not delete the enum; do not build UI for it. |
| Remote caret rendering | `editor/mod.rs:461` `ExternalCaret { offset: DocumentOffset, color_rgb: u32 }`, `lifecycle.rs:471` `set_external_carets` | painted per paragraph already; the presence layer just feeds it |
| Full document replace | `lifecycle.rs:274` `replace_document_from_collaboration(Document, cx)` | used for join bootstrap and local self-heal rebuilds |
| Editor events | `crates/gpui-flowtext/src/api.rs:107` | `EditorEvent::{Changed, SelectionChanged, CommandDispatched, Exported}` + `EditorEventSink` trait |
| Document model | `crates/gpui-flowtext/src/document/*` | `Document { text: crop::Rope, paragraphs: Arc<Vec<Paragraph>>, blocks: Arc<Vec<Block>>, assets: AssetStore, ids: DocumentIds, sections, offset_index, theme }`. Rope = paragraph texts joined by `\n`, no trailing newline (`edit_ops/offsets.rs:19 paragraph_width`: `newline_len = usize::from(ix + 1 < paragraphs.len())`). Non-text blocks (`Image`, `Equation`, `Table`) occupy **no** rope bytes. Table cells own their text privately (`TableCellParagraph { paragraph, text }`). |
| Run styles | `document/text.rs:192` | `RunStyles { semantic: RunSemanticStyle, direct_underline: bool, strikethrough: bool, highlight: Option<HighlightStyle> }` — `Copy`, 4 orthogonal axes → 4 CRDT mark keys |
| Save machinery | `editor/style_state.rs:233 save`, `:247 save_as`; autosave `workspace/workspace/documents.rs:865 maybe_autosave_document` | autosave keys off `document_path().is_some()` + `has_unsaved_changes()` — pathless session tabs skip it automatically until the user Save-As-es |
| Recovery files | `editor/recovery.rs`, `recovery_path` set in `lifecycle.rs:25` from `document_path` | currently derived from `document_path` at construction → pathless joiner tabs get none. §15.6 adds an explicit setter. |
| Close/quit prompt flow | `documents.rs:670 close_document_panel`, `:734 request_close_window` | existing prompt→spawn→conditional-remove pattern; §13.4 extends it with the leave step |
| Wire encoding helper | `collaboration.rs:280` `encode_canonical_operations` (postcard) | **NOT used for sync** (it silently drops `ReplaceParagraphSpan`/block ops). Leave it in place; the CRDT layer replaces it. |

**Key consequence:** the integration burden is *translation between flowtext's document model and loro's containers*, not editor surgery and not distributed-systems plumbing (loro owns op history/merge/idempotency; iroh-gossip owns membership/broadcast). What we write: the two-way mirror (§5–§7), a thin transport (§10–§11), and session orchestration + UI (§13, §17).

Other load-bearing facts about the host app:

* **No tokio anywhere.** GPUI provides smol-based executors (`cx.background_executor().spawn`, `cx.spawn(async move |entity, cx| …)`). iroh requires tokio → dedicated tokio runtime thread (§10.1). Bridge with `async-channel` (runtime-agnostic) only.
* `Workspace` (`workspace/workspace/mod.rs:65`) owns `document_panels: Vec<Entity<DocumentPanel>>`, `active_editor: Option<Entity<RichTextEditor>>`, `editor_subscriptions: Vec<(Uuid, Subscription)>`; panels keyed by `Uuid`. `cx.observe(&editor, …)` per panel (`documents.rs:178`) is how autosave is driven — the session drains local edits from the same observe callback.
* The document **render theme is a local user preference** (`documents.rs:153`: "DB8 stores style assignments, not style appearance") — **never sync `DocumentTheme`**, zoom, or invisibility mode. Only style *slot assignments* (part of `ParagraphStyle`/`RunStyles`) sync.
* Commands: `crates/flowstate/src/commands.rs` (`CommandId` + `COMMAND_SPECS`). Top-bar menus: `workspace/workspace/top_bar.rs`. Status bar: `render_status.rs`. Dialog/notification/avatar/clipboard widgets: vendored `gpui-component`.

---

## 3. Architecture overview

```
┌──────────────────────────── Flowstate process (any peer — all peers identical) ───────────────────────────┐
│                                                                                                            │
│  GPUI main thread                                          tokio runtime thread (lazy, one per app)        │
│  ┌───────────────────────────────────────────────┐         ┌───────────────────────────────────────────┐  │
│  │ Workspace                                     │         │ CollabNet (flowstate-collab::net)         │  │
│  │  └─ DocumentPanel ── RichTextEditor           │         │  • one iroh Endpoint for the whole app    │  │
│  │       ▲ apply_collab_patches / take_edits     │         │  • iroh-gossip Gossip instance            │  │
│  │       │                                       │  async  │     – one subscribed topic per session    │  │
│  │  CollabSession (Entity, one per session tab)  │ channels│     – HyParView membership (≤5 active     │  │
│  │   • LoroDoc + EphemeralStore (presence)       │◄───────►│       conns/topic) + Plumtree broadcast   │  │
│  │   • DocBinding (local ids ↔ containers)       │         │  • direct protocol (own ALPN): serve/pull │  │
│  │   • LocalApplier / RemoteApplier              │         │     snapshots, update-deltas, blobs,      │  │
│  │   • SessionPhase state machine (§13)          │         │     assets over per-request bi-streams    │  │
│  │   • anti-entropy timer, self-check timer      │         │  • Router accepts BOTH ALPNs              │  │
│  │  CollabManager (gpui Global)                  │         └───────────────────────────────────────────┘  │
│  │   • runtime handle, NetCommand tx,            │                                                        │
│  │     NetEvent pump, session registry           │                                                        │
│  └───────────────────────────────────────────────┘                                                        │
└────────────────────────────────────────────────────────────────────────────────────────────────────────────┘

   peer ⇄ peer: iroh QUIC (direct holepunched; n0 relay fallback), wired into a gossip overlay per session
```

**Topology: gossip swarm of equals.** iroh-gossip (HyParView + Plumtree) maintains, per topic, a bounded **active view** (≤5 live connections by default; `active_view_capacity: 5`, `passive_view_capacity: 30` — verified in iroh-gossip source) and disseminates broadcasts along a self-healing spanning tree. At our session sizes (≤16) the overlay is effectively a near-full mesh; we still get membership maintenance, dedup, fan-out, and repair for free. There is **no hub**: every peer broadcasts its own updates, every peer can serve bootstrap/catch-up/assets over the direct protocol, and the protocol contains no role distinctions whatsoever.

**Why this is safe with zero merge code:** loro updates are commutative, idempotent, and causally self-describing. Redundant gossip delivery → deduped by loro import. Out-of-order arrival → loro queues imports with missing dependencies as *pending* and applies them when gaps fill. Missed-while-offline → repaired by anti-entropy (§10.4), which is a thin loop over `oplog_vv()` / `export(updates(vv))` / `import()`. We never parse, store, transform, or order operations ourselves.

**Data flow for one keystroke:**

1. User types → existing edit pipeline mutates `Document`, pushes an `EditRecord`, and (new, §15.2) pushes the exact op delta into `pending_collab_edits`.
2. `cx.observe(editor)` fires → `CollabSession::flush_local_edits` drains the queue → `LocalApplier` replays the ops onto the `LoroDoc` → `doc.commit_with(origin="local-edit")`.
3. loro's `subscribe_local_update` callback yields the update blob → session sends `NetCommand::Publish{session, bytes}` → tokio thread: blob ≤ `GOSSIP_INLINE_LIMIT` → `sender.broadcast(GossipMsg::Update(bytes))`; larger → blob outbox + `GossipMsg::UpdateAvailable{blob_id, len}` notice (§10.5).
4. Every other peer: gossip receiver yields the message → `NetEvent::Gossip{session, from, msg}` → session entity `doc.import(bytes)` (pulling first if it was an availability notice) → loro fires container diff events tagged as import-triggered → `RemoteApplier` maps diffs to patches → `editor.apply_collab_patches` → repaint, selections remapped.

The `LoroDoc` lives **on the GPUI main thread inside the session entity** (loro ops are microseconds; only I/O leaves the thread). `LoroDoc` is `Send + Sync`; we exploit that only for snapshot export/import on join (background executor) and for the direct-protocol servers, which hold a cloned doc handle (`LoroDoc::clone` shares the same underlying doc — verify, §23-V6; if it deep-copies, route serve requests through the session entity instead).

### 3.1 Crate layout (new code, with hard LOC caps)

```
crates/flowstate-collab/             # NEW crate. Core logic. NO gpui dependency → headless-testable.
  Cargo.toml
  src/lib.rs                         # module decls + crate-level docs (≤80)
  src/ids.rs                         # SessionId(=TopicId bytes), BlobId, PeerId aliases, color palette (≤120)
  src/ticket.rs                      # SessionTicket: mint/parse/display (iroh-tickets Ticket impl) (≤200)
  src/proto_gossip.rs                # GossipMsg enum + encode/decode + size budget consts (≤200)
  src/proto_direct.rs                # direct-protocol Request/Response enums + stream codec (≤350)
  src/schema.rs                      # loro container layout, configure_text_styles, offset helpers (≤500)
  src/binding.rs                     # DocBinding table + invariants (≤350)
  src/local_apply.rs                 # CanonicalOperation → loro ops (≤700)
  src/remote_apply.rs                # loro DiffEvent batch → CollabPatch list (≤700)
  src/projection.rs                  # LoroDoc → Vec<InputBlock>; Document → LoroDoc population (≤450)
  src/presence.rs                    # EphemeralStore wrapper, PresenceState codec, roster derivation (≤300)
  src/self_check.rs                  # local projection hash + drift detection (≤150)
  src/net/mod.rs                     # NetCommand/NetEvent enums, channel aliases (≤250)
  src/net/runtime.rs                 # tokio thread bootstrap, Endpoint + Gossip + Router construction (≤300)
  src/net/swarm.rs                   # per-session topic task: subscribe, broadcast, receive, neighbors (≤400)
  src/net/direct.rs                  # direct-protocol server (ProtocolHandler) + client helpers (≤450)
  src/net/anti_entropy.rs            # digest timer, gap detection, pull scheduling (≤250)
  src/net/blobs.rs                   # outbox ring buffer for oversized updates (≤150)
  tests/convergence.rs               # multi-peer fuzz (THE acceptance gate, §21.2)
  tests/translation.rs               # golden tests per canonical op (§21.1)
  tests/swarm_loopback.rs            # real iroh+gossip on localhost, 3 peers (§21.3)
  tests/anti_entropy.rs              # gap/heal logic against a fake transport (§21.1)

crates/flowstate/src/collab/         # NEW module in the app crate. GPUI glue + UI.
  mod.rs                             # CollabManager global, init, NetEvent pump, registry (≤350)
  session.rs                         # CollabSession entity: §13 state machine, flush/import, presence,
                                     #   assets, self-heal, prompt helpers (≤800; split session_io.rs if over)
  share_dialog.rs                    # share/join/roster modal (gpui-component Dialog) (≤500)
  status.rs                          # status-bar pill + tab badge + offline indicator elements (≤250)

crates/gpui-flowtext/src/rich_text/editor/collab_apply.rs   # NEW: patch application + selection remap (≤600)
```

Dependency direction (deliberate): `flowstate-collab` → `gpui-flowtext` (for `CanonicalOperation`, `Document`, `Input*`, `CollabPatch` types). The editor crate never sees loro or iroh. The app crate sees both.

---

## 4. Dependencies (exact, with commands)

Run from the workspace root after `cargo new crates/flowstate-collab --lib` (M0). Workspace uses `[workspace.dependencies]`; add there and reference with `{ workspace = true }` per existing convention:

```sh
cargo add --package flowstate-collab iroh@0.98
cargo add --package flowstate-collab iroh-gossip@0.100
cargo add --package flowstate-collab iroh-tickets@0.98
cargo add --package flowstate-collab loro@1.13
cargo add --package flowstate-collab tokio@1 --features rt-multi-thread,macros,time,sync
cargo add --package flowstate-collab async-channel@2
cargo add --package flowstate-collab postcard@1 --features use-std
cargo add --package flowstate-collab serde@1 --features derive
cargo add --package flowstate-collab anyhow@1 uuid@1 rand@0.9 twox-hash@2
cargo add --package flowstate-collab gpui-flowtext --path crates/gpui-flowtext
cargo add --package flowstate-collab --dev proptest@1
cargo add --package flowstate flowstate-collab --path crates/flowstate-collab
```

Version rationale (verified 2026-06-12):

* **iroh 0.98.2** — latest stable (1.0.0-rc.1 exists, 2026-05-27). Post-rename API: `Endpoint`, `EndpointId`, `EndpointAddr`, `endpoint.addr()`, `iroh::protocol::{Router, ProtocolHandler, AcceptError}`. 1.0 migration later is a version bump, not a redesign.
* **iroh-gossip 0.100.0** (2026-05-27) — pairs with the current iroh line. HyParView defaults verified in source: `active_view_capacity: 5`, `passive_view_capacity: 30`. Subscribe API: `gossip.subscribe(topic_id: TopicId /* 32 bytes */, bootstrap: Vec<EndpointId>)`.
* **iroh-tickets 0.98** — post-0.94 home of the `Ticket` trait (postcard payload, base32 display) and `EndpointTicket`.
* **loro 1.13.1** — `LoroDoc`, `LoroText` (+`*_utf8` and `convert_pos` APIs), `LoroMap`, `LoroMovableList`, `ExportMode::{Snapshot, updates(&vv), all_updates()}`, `subscribe_local_update`, diff-event subscriptions, `UndoManager`, `Cursor`, `awareness::EphemeralStore` (timestamped LWW KV with per-key timeout, encode/apply, local-update + merge subscriptions — purpose-built for presence).
* **async-channel 2** — runtime-agnostic; the only primitive allowed across the smol↔tokio boundary. Never `tokio::sync::mpsc` across it; for request/reply use `async_channel::bounded(1)` as a oneshot.
* **twox-hash 2** — fast non-crypto hash for the local self-check (§7.2).

`gpui-flowtext` gains **no** new dependencies.

---

## 5. CRDT schema (loro container layout)

The loro document is the shared convergent representation; each peer's flowtext `Document` is a *projection* of it. One loro doc per session, created once by whoever starts the session (`projection::populate_from_document`), then only ever mutated through the appliers.

```
LoroDoc
├─ "meta": LoroMap
│   ├─ "schema"  : i64    = 1                      (bump on breaking layout change)
│   ├─ "session" : string = hex(topic_id)          (lineage guard — see §12.2)
│   └─ "title"   : string = doc display name at creation (informational; shown in join dialog)
└─ "blocks": LoroMovableList                        (one entry per flowtext Block, same order)
    └─ [i]: LoroMap                                 (block container)
        ├─ "kind"  : string  ∈ {"p", "image", "equation", "table"}
        ├─ for "p" (paragraph):
        │    ├─ "text"  : LoroText                  (rich text w/ marks, §5.1)
        │    └─ "style" : i64                       (ParagraphStyle: Normal = -1, Custom(s) = s as i64)
        └─ for "image" | "equation" | "table":
             ├─ "data" : LoroValue::Binary          (postcard-encoded BlockPayload, §5.3)
             └─ "rev"  : i64                        (monotonic bump per replace; forces a map-diff event)
```

Call `schema::configure_text_styles(&LoroDoc)` on **every** doc before first use (creation, join import, resync import) — it registers the four mark keys with their expand behavior (§5.1). Factor doc construction into one function so this can't be forgotten: `schema::new_configured_doc() -> LoroDoc`.

### 5.1 Rich text marks (per-paragraph `LoroText`)

`RunStyles` (4 orthogonal axes) → 4 mark keys:

| Mark key | Value | Absent means |
|---|---|---|
| `"sem"` | i64 slot of `RunSemanticStyle::Custom(slot)` | `RunSemanticStyle::Plain` |
| `"ul"` | bool `true` | `direct_underline == false` |
| `"strike"` | bool `true` | `strikethrough == false` |
| `"hl"` | i64 slot of `HighlightStyle::Custom(slot)` | `highlight == None` |

**Configure ALL four keys as `ExpandType::None`.** Rationale: flowtext computes the styles of every inserted character explicitly (`pending_styles` / run inheritance in `edit_pipeline.rs:25-34`), so the CRDT must never auto-expand neighboring marks — the LocalApplier marks every styled insert itself. Mark state stays deterministic and bit-identical to flowtext's run model.

Reading a paragraph back: `text.to_delta() -> Vec<TextDelta>` (insert segments with attribute maps) → fold into `Vec<InputRun>`:

```rust
// schema.rs
pub fn input_runs_from_delta(delta: &[TextDelta]) -> Vec<InputRun> {
  // 1. each insert segment → InputRun { text, styles: run_styles_from_attrs(attrs) }
  // 2. merge adjacent runs with equal RunStyles (mirrors edit_ops run normalization)
  // 3. empty paragraph → vec![] (flowtext represents empty paragraphs with zero runs — check
  //    blank_document() and match its shape exactly)
}
pub fn run_styles_from_attrs(attrs: &…) -> RunStyles;   // missing key → default axis value
pub fn mark_intervals_from_runs(runs: &[TextRun]) -> [Vec<(Range<usize>, LoroValue)>; 4];  // utf8 ranges per key
```

**Soft line breaks** (`\u{2028}`, `document/core.rs:12`) are ordinary characters inside paragraph text — no special handling anywhere in the sync layer.

### 5.2 Why per-paragraph texts in a list (not one giant LoroText) — decided, final

* The canonical op stream already speaks `(ParagraphId, utf8 byte)` — translation to `(container, offset)` is direct, with **no global offset arithmetic anywhere**. Global-offset bookkeeping across interleaved non-text blocks is the classic multi-week-debugging sink; we structurally avoid it.
* Blast-radius containment: a translation bug corrupts one paragraph, not the document; per-paragraph regeneration makes remote apply trivially correct.
* Block insert/delete/move map 1:1 onto `LoroMovableList` ops (true move semantics — concurrent edits inside a moved block survive).
* Known, accepted anomaly: peer A splits a paragraph at offset k while peer B concurrently types at j > k in the same paragraph within one propagation window → B's text converges at the end of the first half instead of inside the new second paragraph. No data loss; all peers agree; B's caret looks slightly wrong once. Standard per-block CRDT trade-off. Document it in `schema.rs` module docs; revisit only with field evidence.

### 5.3 Opaque block payloads (`"data"`)

`Image`, `Equation`, `Table` blocks are **atomic last-writer-wins values**. Justification from the codebase: flowtext itself records table/equation/image edits as whole-block replaces — `CanonicalOperation::ReplaceBlock` carries no finer payload (`editor/tables.rs:174`, `table_equation_editing.rs:66,280,336`, `media.rs:164,224`, `object_selection.rs:250`). A finer-grained table CRDT would exceed the granularity of the host editor's own edit stream: pure cost, zero fidelity gain, until the editor itself ships finer table ops.

```rust
// schema.rs
#[derive(Serialize, Deserialize)]
pub enum BlockPayload {                       // postcard → LoroValue::Binary in "data"
  Image { asset_id: u128, mime: String, original_name: Option<String>,
          content_hash: u64, byte_len: u64,   // asset BYTES NEVER enter the CRDT (§16)
          alt_text: String, caption: Option<InputParagraph>,
          sizing: InputImageSizing, alignment: InputBlockAlignment },
  Equation { source: String, display: InputEquationDisplay },   // syntax: only Latex exists today
  Table(InputTableBlock),
}
pub fn payload_from_block(block: &Block) -> Option<BlockPayload>;   // None for Block::Paragraph
pub fn block_from_payload(payload: BlockPayload, assets: &AssetStore) -> InputBlock;
```

Conversions reuse the existing `Input*` types (`document/text.rs:105-189`) and the existing `InputBlock → Block` builder used by the clipboard path — find it via `RichClipboardFragment` consumers in `edit_ops/rich_fragment.rs` and call it; do **not** write a second converter. Concurrent edits to the same table = LWW on `"data"` (one peer's edit wins for that block; logged as a known limitation). The `"rev"` bump guarantees a diff event even for value-equal writes.

### 5.4 Identity & the DocBinding

Cross-peer identity = **loro container identity**. Flowtext's `ParagraphId`/`BlockId` are **peer-local** (generated fresh on every load, never serialized to db8 — see `document_ids_for_shape` / `reconcile_document_ids` in `document/core.rs:243,256`). Never put them on the wire or in the CRDT.

```rust
// binding.rs
pub struct DocBinding {
  rows: Vec<BindingRow>,                          // parallel to BOTH document.blocks and loro "blocks"
  by_paragraph: FxHashMap<ParagraphId, usize>,
  by_block: FxHashMap<BlockId, usize>,
  by_container: FxHashMap<ContainerID, usize>,    // text container AND map container both map to the row
}
pub struct BindingRow {
  pub map: LoroMap,                                // handle to blocks[i]
  pub text: Option<LoroText>,                      // Some iff kind == P
  pub kind: BlockKind,                             // P | Image | Equation | Table
  pub block_id: BlockId,
  pub paragraph_id: Option<ParagraphId>,
}
impl DocBinding {
  pub fn build(doc: &LoroDoc, document: &Document) -> anyhow::Result<Self>;  // walk containers + ids in order
  pub fn assert_consistent(&self, doc: &LoroDoc, document: &Document);       // debug_assert! the invariant below
  // splice/insert/remove/move row methods used by both appliers — keep index maps coherent
}
```

**Invariant (debug-assert after every local flush and every remote apply batch):**
`rows.len() == document.blocks.len() == loro_blocks.len()`; `rows[i].kind` matches `document.blocks[i]`'s variant; `rows[i].block_id == document.ids.block_ids[i]`; for the j-th paragraph block, `rows[i].paragraph_id == Some(document.ids.paragraph_ids[j])`.

When a **remote** change creates a block, the RemoteApplier mints fresh local ids (`new_paragraph_id()` / `new_block_id()` from `document/core.rs:225,231`) and writes them into both `Document.ids` and the new row. When a **local** edit creates one, the LocalApplier reads the id flowtext just minted (from `document.ids` at the op's index) and records it beside the new container.

### 5.5 Offset discipline (read twice; most historical CRDT-integration bugs live here)

* flowtext: **UTF-8 byte offsets** everywhere (`DocumentOffset.byte`, run `len`s, canonical ops).
* loro `LoroText`: default API positions are **Unicode scalar values**; UTF-8 variants exist (`mark_utf8`, `len_utf8`); `convert_pos(index, PosType, PosType)` converts between `Utf8`/`Unicode`/`Utf16`/`Event` coordinate systems. Use `*_utf8` variants where they exist; otherwise convert at the boundary (§23-V3 lists what to check).
* loro **diff events** report positions in *event* coordinates — convert before remapping selections.
* ALL conversions funnel through exactly two helpers in `schema.rs`; **no inline conversions anywhere else**:

```rust
pub fn loro_pos(text: &LoroText, utf8_byte: usize) -> usize;   // panics in debug on out-of-range
pub fn utf8_byte(text: &LoroText, loro_pos: usize) -> usize;
```

* Debug assertion after every paragraph mutation, both directions: `text.len_utf8() == paragraph_text_len(&document.paragraphs[ix])`.
* Mandatory test fixture for every offset-touching test: `"aé🌍\u{2028}x"` (1-, 2-, 4-byte chars + soft break).

---

## 6. LocalApplier: `CanonicalOperation` → loro

`local_apply.rs`. Entry point:

```rust
pub struct LocalApplier<'s> { pub doc: &'s LoroDoc, pub binding: &'s mut DocBinding }
impl LocalApplier<'_> {
  /// Apply ONE editor edit-record's ops, then `doc.commit_with(origin = "local-edit")`.
  /// One EditRecord == one loro commit == one undo unit == typically one gossip blob.
  pub fn apply(&mut self, document: &Document, ops: &[CanonicalOperation]) -> anyhow::Result<()>;
}
```

`document` is the **post-edit** flowtext document (canonical ops reference post-edit identity; `identity_map.reconcile` has already run). Per-op translation:

| Canonical op | Loro translation |
|---|---|
| `InsertText { paragraph, byte, text, styles }` | `row = binding.by_paragraph[paragraph]`; insert at `loro_pos(byte)`; if `styles != RunStyles::default()`, apply the non-default mark keys over the inserted utf8 range. (Expand is `None`, so an unmarked insert is plain by construction.) |
| `DeleteRange { start_paragraph, start_byte, end_paragraph, end_byte }` | Same paragraph → single `delete`. Cross-paragraph (= join): (1) read tail delta of the end paragraph from `end_byte` (`to_delta` sliced); (2) delete `[start_byte..]` from the start text; (3) movable-list-delete every whole container strictly between start and end, removing binding rows; (4) append the saved tail into the start text, re-applying its marks segment by segment; (5) delete the end container + row. All inside the one commit. |
| `SplitParagraph { paragraph, byte, new_paragraph }` | (1) read tail delta `[byte..]`; (2) delete tail from original; (3) `blocks.insert_container(idx+1, LoroMap)`; set `"kind"="p"`, `"style"` = original's style; create `"text"`; insert tail segments with marks; (4) insert `BindingRow { paragraph_id: Some(new_paragraph), block_id: document.ids.block_ids[idx+1], … }`. |
| `JoinParagraphs { first, second }` | Degenerate cross-paragraph delete — same code path as `DeleteRange`. |
| `SetParagraphStyle { paragraph, style }` | `row.map.insert("style", encode(style))`. |
| `SetRunStyles { paragraph, range, styles }` | For each of the 4 keys: target value non-default → `mark_utf8(range, key, value)`; default → unmark over the range. |
| `ReplaceParagraphSpan { start_paragraph, before, after }` | The workhorse — §6.1. |
| `InsertBlock { block, block_ix }` | Payload is NOT in the op — read `document.blocks[block_ix]`. Paragraph → as in split. Object → new map: `"kind"`, `"data"` = `payload_from_block`, `"rev"=0`. Bind with `block_id = block`. |
| `DeleteBlock { block }` | Movable-list delete at `binding.by_block[block]`; remove row. |
| `MoveBlock { block, new_block_ix }` | `blocks.mov(old_ix, new_block_ix)` (§23-V6 for exact name); rotate binding rows to match. |
| `ReplaceBlock { block: Some(id) }` | Re-serialize `document.blocks[binding.by_block[id]]` → `map.insert("data", payload)`; `rev += 1`. `block: None` → locate the single changed block by comparing `Block::*.version` fields against a kept shadow, else fall through to `ReplaceDocument` handling. |
| `ReplaceDocument` | Clear the movable list; `projection::populate_from_document(doc, document)`; `binding = DocBinding::build(...)`. Heavy but rare (recovery/import paths: `block_insertion.rs:309`, `object_selection.rs:461`). |

### 6.1 `ReplaceParagraphSpan` translation (precise algorithm)

flowtext wraps most compound edits (paste, backspace at a boundary, IME commit, multi-paragraph style commands, drag-drop) in one coarse op carrying full `before`/`after` `DocumentSpan`s. The captured range deliberately includes ±1 neighbor paragraphs (`edit_pipeline.rs:140 edit_capture_range`), so edge pairs usually no-op.

1. `start = binding.by_paragraph[start_paragraph]`, falling back to `before.start_paragraph` exactly as the existing applier does (`lifecycle.rs:414`).
2. Pair positionally: paragraphs `0..min(n_before, n_after)` are *matched*; surplus `after` → *inserted* after the matched run; surplus `before` → *deleted*. (Matches flowtext's own positional identity reconciliation.)
3. For each matched pair `(b, a)` at row `r`:
   a. If text bytes and run vectors are identical → skip.
   b. Else if text differs: `row.text.update(a_text, UpdateOptions::default())` — loro's built-in Myers diff produces minimal CRDT ops (§23-V3 for `UpdateOptions` fields).
   c. If run vectors differ: full mark refresh for the paragraph — for each of the 4 keys, unmark `[0..len)`, then re-apply that key's intervals from `mark_intervals_from_runs(a.runs)`. Paragraphs are small; simplicity beats a minimal mark diff. (Optimization later only with profiler evidence.)
   d. If `b.style != a.style`: update `"style"`.
4. Inserted paragraphs: create containers as in `SplitParagraph`, reading style/runs/text from the `after` span; bind to the ids flowtext minted at those indices (`document.ids.paragraph_ids[start + i]`, `block_ids` likewise).
5. Deleted paragraphs: movable-list delete + drop rows.
6. `binding.assert_consistent(doc, document)`.

**Convergence note:** `text.update()` interleaves with concurrent remote edits at diff granularity rather than keystroke granularity. That is normal CRDT behavior; it converges; do not attempt to be cleverer.

### 6.2 The double-apply trap (implement exactly; regression test mandatory)

`insert_single_grapheme_fast_path` (`edit_pipeline.rs:44-73`) **merges** consecutive single-grapheme inserts into the *previous* undo record by mutating `canonical_operations[0].text` in place. Any design that re-reads "the last record's ops" after each notify will re-apply the accumulated string ("h", then "he", then "hel" …). Therefore the editor gets an explicit **pending queue receiving exactly the per-mutation delta** (§15.2): the fast path pushes a one-grapheme `InsertText` regardless of undo-record merging; the general path pushes a record's ops exactly once, at record creation. The queue is the only thing the session reads.

gpui-flowtext unit test (new `rich_text/tests/collab_capture.rs`): type `"a"`, `"b"`, `"c"` through the fast path → drained queue is exactly three 1-byte `InsertText` ops; then one paste → exactly one `ReplaceParagraphSpan`; then undo → queue stays empty.

---

## 7. RemoteApplier: loro diff → flowtext `Document`

`remote_apply.rs` (pure: consumes diff events, emits patches) + `editor/collab_apply.rs` (applies patches through editor internals).

Subscribe once per session (`doc.subscribe_root` or the blocks-container subscription — §23-V4). Loro delivers `DiffEvent`s **synchronously during `import()` and undo, on the calling thread** (the main thread — exactly what we want). Filter by trigger kind: process only *Import* and *Checkout/Undo* events; *Local* events are echoes of the LocalApplier and must be ignored.

Patch type — defined in **gpui-flowtext** (`collaboration.rs`), so the editor crate never imports collab types (`flowstate-collab` already depends on gpui-flowtext; the dependency arrow stays one-way):

```rust
// gpui-flowtext/src/collaboration.rs (addition)
pub enum CollabPatch {
  ParagraphText { row: usize, new: InputParagraph,
                  delta_utf8: Vec<CollabTextDelta> },        // Retain(usize)|Insert(usize)|Delete(usize), utf8 units
  ParagraphStyle { row: usize, style: ParagraphStyle },
  ReplaceObjectBlock { row: usize, block: InputBlock },      // image/equation/table payload swap
  InsertBlocks { row: usize, blocks: Vec<InputBlock> },
  DeleteBlocks { row: usize, count: usize },
  MoveBlock { from: usize, to: usize },
  AssetArrived { id: AssetId, record: AssetRecord },         // from the asset fetcher, not from loro
}
```

Mapping rules (`remote_apply.rs`):

* **Text diff on a paragraph container** → regenerate the whole paragraph from loro (`to_delta()` → `input_runs_from_delta`) — never incremental-patch flowtext runs from deltas; regeneration is O(paragraph) and immune to drift. Also emit the event delta converted to utf8 (`CollabTextDelta`) for caret remapping.
* **Movable-list diff** → `InsertBlocks` / `DeleteBlocks` / `MoveBlock`; for inserts, read the new containers (`projection::input_block_from_container`), mint fresh local ids, splice binding rows.
* **Map diff**: `"style"` → `ParagraphStyle`; `"data"`/`"rev"` → `ReplaceObjectBlock` (decode payload; if it's an image whose asset is missing locally, also enqueue an asset fetch, §16).

`collab_apply.rs` (editor side):

```rust
impl RichTextEditor {
  pub fn apply_collab_patches(&mut self, patches: &[CollabPatch], cx: &mut Context<Self>) { … }
  pub fn collab_apply_deferred(&self) -> bool { … }   // §7.3 predicate, checked by the session before calling
}
```

Implementation requirements (reuse existing machinery, do not reinvent):

1. Wrap the batch in `suppress_collab_capture += 1` (so nothing echoes into `pending_collab_edits`) and reuse the `layout_invalidation_hint` + `after_text_mutation(cx)` pattern from `apply_remote_operations` (`lifecycle.rs:265-272`) for cache invalidation. Set `layout_invalidation_hint` to the smallest covering paragraph range per patch group.
2. `ParagraphText`: replace content via the span machinery (`capture_document_span` + `apply_document_span_replacement` scoped to one paragraph) or a focused `replace_paragraph_content(document, ix, InputParagraph)` helper added in a new `edit_ops/collab.rs`. **Preserve the existing `ParagraphId`** (identity unchanged); bump the paragraph `version` field so layout caches invalidate.
3. Structural patches: splice `document.blocks` / `paragraphs` / `ids` with the **ids provided in the patch path** (minted by the RemoteApplier) — never let `reconcile_document_ids` invent ids here; call it afterwards in debug and assert it was a no-op.
4. **Selection remap:** if `selection.head`/`anchor` sits in a patched paragraph, walk `delta_utf8` (retain advances, insert advances by inserted len if at-or-before the offset, delete clamps) to the new byte; if its paragraph was deleted, clamp to the start of the next surviving block (or end of previous). Apply the same remap to `table_cell_*` / `equation_source_*` offsets; if the actively-edited table/equation block got replaced remotely, exit the cell/equation editing mode cleanly (clear `selected_block`, selection to block boundary).
5. **Undo safety:** while attached, the native undo stack is unused (§8) — on attach both stacks are cleared; debug-assert they remain empty in `apply_collab_patches`.

### 7.1 Bootstrap & rebuild

* **Join:** `schema::new_configured_doc()` → `doc.import(snapshot_bytes)` on the background executor (`LoroDoc` is Send) → verify `meta.session == ticket.session` (§12.2) → on the main thread `projection::document_from_loro(&doc, load_document_theme())` → `Vec<InputBlock>` → the existing input-builder → `editor.replace_document_from_collaboration(document, cx)` → `DocBinding::build`. Gossip updates received while importing are buffered by the session and imported afterwards (loro pending-import absorbs ordering).
* **Local self-heal rebuild** (§7.2): identical projection path against the *existing* doc, plus selection clamp to nearest valid offset, plus a non-blocking "Document view rebuilt" notification in debug builds.
* `projection.rs` also provides the inverse, `populate_from_document(&LoroDoc, &Document)` — used once at session creation and by `ReplaceDocument`.

Round-trip property test: `document → populate → project → document'` is identity on (texts, run vectors, styles, block kinds, payload bytes) — everything except `AssetRecord.bytes` and local ids.

### 7.2 Integrity: local-first self-check (no authority needed — by design)

The flowtext `Document` is derived state; divergence therefore decomposes into two cases with different owners:

* **Projection drift** (an applier bug — the realistic case): the live `Document` no longer equals `project(own LoroDoc)`. Detect **locally**: every 30 s of idle (no edits for 2 s, timer in the session), compute `self_check::projection_hash(&Document)` and compare with the hash of a freshly projected document from the own doc (background executor; compare on main thread). On mismatch: `eprintln!` a structured diagnostic (both hashes, oplog vv, last 5 patch kinds applied), then **self-heal** via the §7.1 rebuild. No peers involved. `projection_hash` = twox-hash over, in order: paragraph styles, run `(len, styles)` vectors, paragraph text bytes, block kinds, payload bytes. Explicitly excluded: ids, theme, sections, offset index, assets.
* **CRDT-level divergence** (same oplog vv, different loro state): impossible unless lineage mixing (§12.2 prevents) or a loro bug. Guarded by the session-id check on every digest (§10.4); a mismatch there is fatal — detach with an error dialog, never attempt merge. Optional deep cross-peer state-hash comparison ships behind `FLOWSTATE_COLLAB_PARANOIA=1` for QA builds only.

### 7.3 Deferral around gestures

Remote patches must not mutate the document mid-gesture. Before applying, the session checks `editor.collab_apply_deferred()`:
`self.selecting || self.active_text_drag.is_some() || self.image_resize_drag.is_some() || self.table_column_resize_drag.is_some() || ime_composition_active()` (marked-text state lives in `editor/platform.rs` — expose a query). If true, the session queues patches and retries on the next editor observe tick or a 16 ms timer, whichever first. Queue is unbounded in theory; in practice gestures are short — add a debug log if the queue exceeds 256 patches.

---

## 8. Undo/redo while attached

Positional `EditRecord` replay is unsound once remote edits interleave. While attached:

* The session creates `loro::UndoManager::new(&doc)` at attach. It tracks **only this peer's ops** (precisely Google-Docs semantics: you can retract your own edits, never someone else's; positions transform against concurrent remote edits automatically). Configure: merge interval ≈ 500 ms (mirrors flowtext's grapheme coalescing), max steps ≈ 300.
* The editor gets a redirect hook (§15.4). `commands.rs:191 undo()` / `:205 redo()` check it first; when set, they invoke it and return. The session's handler calls `undo_manager.undo()` / `.redo()`; resulting doc changes arrive through the same DiffEvent → patch → `apply_collab_patches` pipeline (trigger = checkout/undo passes the §7 filter; verify the LocalApplier echo-filter does not eat them — they are *not* `origin="local-edit"` commits).
* Caret restore: wire `UndoManager::set_on_push` / `set_on_pop` storing the selection as two loro `Cursor`s and resolving via `doc.get_cursor_pos` after pop (§23-V5). If the cursor API resists, v1 fallback: selection stays where patch remap leaves it.
* On attach: clear `undo_stack`/`redo_stack`. On detach: stacks restart empty from the final state — history does not cross the membership boundary (mention in user docs).
* **Post-merge cleanup semantics (decided):** a reconnect merge is not an operation and cannot be "undone" as a unit. Resolution story: each author can undo their own contributions step-wise; anything else is fixed by ordinary editing (nothing is ever locked). An hour offline ≈ many undo steps — select-and-delete is often the practical tool; that's acceptable and matches the Google Docs model (minus version history, which v1 does not have).

---

## 9. Presence (carets, names, roster)

`presence.rs` wraps `loro::awareness::EphemeralStore` (timestamped LWW KV with per-key timeout — purpose-built for this):

```rust
pub struct PresenceState {                 // postcard → store value under key = hex(EndpointId)
  pub name: String,                        // self-asserted display name (app settings; default = OS username)
  pub selection: Option<PresenceSelection>,
}
pub struct PresenceSelection {
  pub head_container: String,              // ContainerID of the paragraph text holding the head
  pub head: Vec<u8>,                       // loro Cursor::encode() — survives concurrent edits
  pub anchor_container: String,
  pub anchor: Vec<u8>,
}
```

* **Colors are not negotiated**: `color_ix = hash(EndpointId) % PALETTE.len()` with `PALETTE: [u32; 8]` of high-contrast RGBs in `ids.rs`. Stable per peer, no coordination, collisions tolerable at session scale.
* Store timeout 30 s. Outbound: the store's local-update subscription yields raw bytes → `GossipMsg::Presence(bytes)` broadcast. Inbound: `store.apply(bytes)`. Refresh own entry every 10 s (keepalive) and within 50 ms (debounced) of `EditorEvent::SelectionChanged` (subscribe via `cx.subscribe`; ensure `RichTextEditor: EventEmitter<EditorEvent>` — §15.7).
* **Leave is explicit in presence:** before unsubscribing, best-effort broadcast a self-removal (delete own key / `remove_outdated` semantics — §23-V7 for the exact removal API) so others see the departure immediately rather than after 30 s.
* **Roster = presence, nothing else.** Gossip `NeighborUp/Down` events are connectivity hints for the state machine (§13.2), not membership truth — your neighbors are a random subset of the swarm. The participant list, count, and "Only you" detection all read the presence store (entries newer than timeout).
* Rendering: on presence merge events and after every remote apply batch, resolve every peer's cursors — `Cursor::decode` → `doc.get_cursor_pos` → loro pos → `utf8_byte()` → `binding.by_container` row → paragraph ordinal → `DocumentOffset` → `editor.set_external_carets(carets, cx)`. Unresolvable cursors (container deleted) → drop that caret until the peer's next selection update. v1 renders caret + name label on hover; selection-range highlight is v1.1 (`ExternalCaret` keeps its current shape).

---

## 10. Networking

### 10.1 Runtime bridge (`net/runtime.rs`)

* Lazy global, started on first share/join: `std::thread::Builder::new().name("flowstate-collab-net".into()).spawn(...)` running `tokio::runtime::Builder::new_multi_thread().worker_threads(2).enable_all()`; the thread parks in `runtime.block_on(net_main(cmd_rx, evt_tx))`.
* `net_main` constructs, once per app:

```rust
let endpoint = Endpoint::builder()            // wire the n0 preset: relays + address lookup/publish (§23-V1)
    .alpns(vec![DIRECT_ALPN.to_vec()])        // gossip registers its own ALPN via the Router
    .bind().await?;
let gossip = Gossip::builder().spawn(endpoint.clone());                  // §23-V2 for exact constructor
let _router = Router::builder(endpoint.clone())
    .accept(iroh_gossip::ALPN, gossip.clone())                           // Gossip impls ProtocolHandler
    .accept(DIRECT_ALPN, DirectProto::new(serve_state))                  // §10.3
    .spawn();
```

Discovery is **not** on by default in iroh 0.98 — apply the n0 preset (`presets::N0` / `Builder::new(preset)`) for relay + address lookup/publish. Tickets additionally embed the inviter's full `EndpointAddr` (relay URL + direct addrs) so a fresh install can join with zero warm-up. After `bind()`, await the endpoint's first reachable address before minting any ticket (§23-V1 names the watcher API).
* Channels: `async_channel::unbounded::<NetCommand>()` (UI→net) and `::<NetEvent>()` (net→UI). The UI side pumps events in `CollabManager` with one detached `cx.spawn` loop, routing by `SessionId` to the registered `CollabSession` entity.
* App quit: `CollabManager::shutdown` sends `NetCommand::Shutdown`; net thread broadcasts presence-removals for all attached sessions (best-effort, 500 ms budget), unsubscribes, closes the endpoint, exits.

```rust
// net/mod.rs
pub enum NetCommand {
  EnsureUp,                                                          // idempotent endpoint warm-up
  CreateSession { session: SessionId, reply: Tx<anyhow::Result<TicketSeed>> },   // subscribe w/ no bootstrap
  JoinSession   { session: SessionId, bootstrap: Vec<EndpointAddr> },
  LeaveSession  { session: SessionId },                              // presence-removal already broadcast by UI side
  Publish       { session: SessionId, payload: PublishPayload },     // Update(Vec<u8>) | Presence(Vec<u8>) | Digest(..)
  PullUpdates   { session: SessionId, from: EndpointId, our_vv: Vec<u8>, reply: Tx<anyhow::Result<Vec<u8>>> },
  PullSnapshot  { session: SessionId, from: EndpointId, reply: Tx<anyhow::Result<Vec<u8>>> },
  PullBlob      { session: SessionId, from: EndpointId, blob: BlobId, reply: Tx<anyhow::Result<Vec<u8>>> },
  PullAsset     { session: SessionId, from: EndpointId, asset: u128, reply: Tx<anyhow::Result<AssetBytes>> },
  MintTicketAddr{ reply: Tx<EndpointAddr> },                          // current own addr for re-minted invites
  Shutdown,
}
pub enum NetEvent {
  Gossip       { session: SessionId, from: EndpointId, msg: GossipMsg },
  NeighborUp   { session: SessionId, peer: EndpointId },
  NeighborDown { session: SessionId, peer: EndpointId },
  GossipLagged { session: SessionId },          // receiver overflow → treat as a gap; trigger §10.4 pull
  SubscribeFailed { session: SessionId, error: String },
  EndpointOnline(bool),                          // connectivity hint for §13.2
}
```

(`Tx<T>` = `async_channel::Sender<T>` with `bounded(1)` as the oneshot idiom.)

### 10.2 Swarm layer (`net/swarm.rs`)

One tokio task per attached session. Responsibilities:

* `gossip.subscribe(topic_id, bootstrap_ids)` → split into sender/receiver halves (§23-V2 for exact types: `GossipTopic` → `GossipSender`/`GossipReceiver`, events `Received/NeighborUp/NeighborDown/Lagged`). Before subscribing with bootstrap peers, register their `EndpointAddr`s with the endpoint so dialing can proceed without discovery (§23-V1 for the add-address method).
* Receive loop: decode `GossipMsg` (postcard); forward as `NetEvent::Gossip`; surface `NeighborUp/Down/Lagged`.
* Send path: `Publish` commands → `sender.broadcast(bytes)`; digests → `sender.broadcast_neighbors(bytes)` (neighbor-only, §23-V2).
* Re-subscription on total neighbor loss is **not** the swarm task's job — HyParView self-heals while ≥1 reachable peer remains; the §13.2 Offline logic only re-bootstraps when the overlay is empty *and* a known peer address is reachable again.

### 10.3 Direct protocol (`net/direct.rs`, ALPN `b"flowstate/collab-direct/0"`)

Request/response over a **fresh bi-stream per request** on a (cached) direct connection. Server side implements `iroh::protocol::ProtocolHandler` (`async fn accept(&self, connection: Connection) -> Result<(), AcceptError>`): loop `accept_bi`, read one length-prefixed `DirectRequest`, answer, finish.

```rust
// proto_direct.rs — all length-prefixed postcard (u32-le len; 2 MiB cap per frame; payload streams chunked)
pub enum DirectRequest {
  Snapshot { session: SessionId },
  Updates  { session: SessionId, have_vv: Vec<u8> },
  Blob     { session: SessionId, blob: BlobId },
  Asset    { session: SessionId, asset: u128 },
}
pub enum DirectResponseHeader {
  Ok { total_len: u64 },          // followed by raw payload in ≤256 KiB chunks on the same stream
  NotAttached,                    // serving peer left that session
  NotFound,                       // unknown blob/asset
  Busy,                           // over the concurrency limit; caller retries elsewhere
}
```

Server rules: verify the `session` is one this peer is attached to (the SessionId is the bearer secret — possessing it *is* authorization, §12.1); ≤4 concurrent serves per peer (semaphore; excess → `Busy`); snapshot/updates payloads are produced by calling back into the session's doc handle (cloned `LoroDoc` if clone-shares, else a request channel into the session entity — §23-V6 decides which).

Client helper with **peer fallback** (used by join, anti-entropy, blobs, assets):

```rust
pub async fn pull_with_fallback(req: DirectRequest, candidates: Vec<EndpointId>, per_peer_timeout: Duration)
  -> anyhow::Result<Vec<u8>>;
// try candidates in order: (1) the peer the need came from (digest sender / gossip delivered_from /
// ticket inviter), (2) current gossip neighbors, (3) any peer with a live presence entry.
// 10 s per-peer timeout; first success wins; aggregate error names every attempt.
```

### 10.4 Anti-entropy (`net/anti_entropy.rs` + session logic)

Heals every gap gossip can leave: offline windows, `Lagged` receivers, missed blob pulls, joins racing live traffic.

* Every attached peer, every **10 s ± 20% jitter** (and immediately on `NeighborUp` and on reconnect): `broadcast_neighbors(GossipMsg::Digest { session: SessionId, vv: doc.oplog_vv().encode() })`.
* On receiving a digest: compare version vectors (loro VV comparison — opaque to us beyond `partial_cmp`-style API, §23-V4). If the sender has ops we lack → `PullUpdates { from: sender, our_vv }` → import the returned `export(updates(our_vv))` bytes. **Pull-only** (we never push unsolicited): each peer pulls at most once per detected gap, so no duplicate-suppression protocol is needed. In-flight guard: one outstanding pull per session.
* If our vv ⊋ theirs: do nothing — their own digest cycle will pull from us.
* Digest carries the `SessionId`; a mismatch (≠ this topic's id) is a lineage violation: log + ignore the peer (§12.2).
* `GossipLagged` → schedule an immediate digest broadcast + opportunistic pull from any neighbor.

### 10.5 Oversized updates (`net/blobs.rs`)

Gossip messages are size-capped (configurable; default small — §23-V2). Budget: `GOSSIP_INLINE_LIMIT = 2 KiB` post-encoding.

* Update blob ≤ limit → `GossipMsg::Update(bytes)` inline (the overwhelmingly common case — typing commits are tens of bytes).
* Larger (pastes, big span replaces) → store in the session's **outbox**: `BlobId = u128 random`, ring buffer of the last 64 blobs (or 16 MiB, whichever first); broadcast `GossipMsg::UpdateAvailable { blob: BlobId, len: u64 }`; receivers `PullBlob` from `delivered_from`, fallback per §10.3. A failed/expired pull is harmless: the next anti-entropy round delivers the same ops via `Updates`. (This indirection avoids a chunk-reassembly protocol entirely — decided.)

### 11. Gossip wire format (`proto_gossip.rs`)

```rust
pub const PROTOCOL_VERSION: u16 = 1;          // first byte(s) of every gossip payload + ticket
pub const DIRECT_ALPN: &[u8] = b"flowstate/collab-direct/0";

#[derive(Serialize, Deserialize)]
pub enum GossipMsg {                           // postcard, prefixed with PROTOCOL_VERSION u16
  Update(Vec<u8>),                             // loro export bytes (one or more commits)
  UpdateAvailable { blob: BlobId, len: u64 },
  Presence(Vec<u8>),                           // EphemeralStore encode bytes
  Digest { session: SessionId, vv: Vec<u8> },  // broadcast_neighbors only
}
```

Version policy: a peer receiving a higher/lower `PROTOCOL_VERSION` ignores the message and surfaces a one-time "a participant is running an incompatible Flowstate version" notification (per-peer, deduped). Joins fail fast instead: the snapshot `DirectResponseHeader` is version-prefixed too, so an incompatible joiner gets a clear error in the join dialog rather than a silent dead session.

---

## 12. Tickets, identity, lineage

### 12.1 Ticket (`ticket.rs`)

```rust
pub struct SessionTicket {
  pub version: u16,                  // PROTOCOL_VERSION at mint time
  pub session: SessionId,            // = the 32-byte gossip TopicId = THE bearer secret
  pub inviter: EndpointAddr,         // full addr (relay URL + direct addrs) of whoever minted THIS ticket
  pub title: String,                 // doc display name, shown in the join dialog before connecting
}
```

Implements the `iroh-tickets` `Ticket` trait (postcard payload, base32 display) with prefix `"fscollab"` — the user-visible string looks like `fscollab…`. **Any attached peer can mint a ticket at any time** (share dialog → "Copy invite"); each mint embeds the minter's *current* address (`NetCommand::MintTicketAddr`), so invites never go stale while anyone is around to share them. Threat model: possession of the ticket (specifically the `session` secret) **is** membership — that is the product. The secret never appears in gossip metadata visible to non-members (TopicId is shared only with members by construction; discovery/relay infrastructure sees opaque traffic).

### 12.2 Lineage guard (the one real landmine — implement all three checks)

A loro doc merged with updates from a *different document lineage* produces interleaved garbage (both histories union), silently. Three structural guards:

1. **Topics are never reused.** `SessionId` = 32 fresh `OsRng` bytes per session creation. Starting a new session on the same file = new id, new ticket. There is no path that re-subscribes an old topic with new content.
2. **The doc carries its lineage:** `meta.session = hex(session_id)` written once at creation. A joiner verifies `imported_meta.session == ticket.session` immediately after snapshot import, **before** opening the tab; mismatch → join fails with "stale or corrupted invite."
3. **Digests carry the SessionId** (§10.4); a mismatched digest marks the sender incompatible and is ignored. Snapshot/Updates responses are only served for sessions the server is attached to, keyed by the same id.

With these, lineage mixing requires a peer to hold the right 32-byte secret *and* the wrong document — which cannot arise from any v1 flow (joins always build fresh docs; reconnects reuse the same attached doc; v1.1 "resume" must re-verify check 2).

---

## 13. Session lifecycle (the state machine — implement exactly)

### 13.1 States

```rust
// session.rs
pub enum SessionPhase {
  Creating,                                  // populate loro + subscribe; ~instant
  Joining(JoinStage),                        // §13.3
  Attached(Attachment),                      // member of the session (the steady state)
  Detached(DetachReason),                    // terminal for this tab; collab machinery torn down
}
pub struct Attachment {
  pub connectivity: Connectivity,            // §13.2
  pub peers_present: usize,                  // presence-derived (excluding self)
}
pub enum Connectivity { Online, Offline { since: Instant, retries: u32 } }
pub enum JoinStage { Resolving, Subscribing, FetchingSnapshot { got: u64, total: Option<u64> }, Building }
pub enum DetachReason { UserLeft, JoinFailed(String), Fatal(String) }   // Fatal: lineage/incompatibility
```

UI projection (status pill): `Attached{Online, n>0}` → "● n+1 in session"; `Attached{Online, 0}` → "● Only you"; `Attached{Offline,..}` → "◌ Offline — will sync"; `Joining` → progress; `Detached` → no pill (plain doc).

### 13.2 Connectivity detection (Attached only)

`Online` ⇨ `Offline` when **all** of: zero gossip neighbors for > 5 s, AND the last 2 digest rounds produced no inbound traffic, AND (`EndpointOnline(false)` OR a probe dial to the most recent known peer fails). Being merely *alone* (peers all left, but the network is fine) stays `Online` with `peers_present == 0` — "Only you," not "Offline."
`Offline` ⇨ recovery loop: backoff 1 / 2 / 4 / … / 30 s (cap), each attempt: re-register known peer addresses (passive view + last presence-seen peers, retained in the session), re-subscribe the topic with them as bootstrap, await first `NeighborUp` (10 s). On success: immediate digest broadcast (pulls what we missed; our accumulated local commits flow out via the peers' own digest pulls and normal broadcast of new edits) ⇨ `Online`. The user can Leave at any time during `Offline`.

### 13.3 Join sequence (numbered; every step has a failure edge → `Detached(JoinFailed)`)

1. Parse + version-check the ticket (reject with specific copy: malformed / newer-version / self-join when `inviter.endpoint_id == own endpoint.id()` — friendly "that's your own invite").
2. `EnsureUp` the runtime/endpoint.
3. Register `ticket.inviter` address; `JoinSession { session, bootstrap: [inviter] }`. Await first `NeighborUp` — 15 s timeout ("couldn't reach anyone in this session").
4. `PullSnapshot` via `pull_with_fallback([inviter ∪ neighbors])` — 30 s overall budget; progress streamed to `JoinStage::FetchingSnapshot`.
5. Background import into `new_configured_doc()`; verify `meta.session` (§12.2).
6. Main thread: project → `Document` → create the tab — `workspace.create_document_panel(document, /*path*/ None, Some(format!("{} (shared)", meta.title)), …)` — then `editor.set_recovery_path(Some(session_recovery_path(session)))` (§15.6).
7. `DocBinding::build`; attach all hooks (§13.5); flush gossip messages buffered since step 3 into the doc; broadcast own presence; initial digest. ⇨ `Attached`.

### 13.4 Leave, close, and quit (the prompt contracts — implement exactly)

**(a) "Leave session" button** (share dialog / status pill context): light confirm — *"Leave this session? Your copy of the document stays open."* `[Leave] [Cancel]`. On Leave: run the detach sequence (§13.5). The tab **remains open** as an ordinary document; no save prompt (the document is still there; saving stays a later decision). Transient notification: "Left session — this copy is now local."

**(b) Closing an attached tab** (`close_document_panel`, `documents.rs:670`): two-step, **transactional** — nothing detaches until every answer is in; Cancel anywhere = still attached, tab still open.
1. Prompt 1: *"Leave the collaboration session? This tab is live with N other people."* `[Leave] [Cancel]` (N from presence; copy adjusts for N = 0).
2. Prompt 2 (only if leaving): the standard save flow, branched on **`document_path().is_some()`** — NOT on any starter/joiner notion (a joiner who Save-As-ed is identical to anyone else): pathless → `[Save As…] [Don't Save] [Cancel]`; path-backed and dirty → existing `[Save] [Don't Save] [Cancel]`; path-backed and clean → skip.
3. Execute in order: await save (failure → abort, still attached) → detach (§13.5) → `remove_document_panel`. "Don't Save" on a pathless tab also deletes its session recovery file (existing `discard_recovery_file` path).

**(c) App quit / window close** (`request_close_window`, `documents.rs:734`): extend the existing dirty-panel sweep — for each attached panel run the (b) sequence; any Cancel aborts the quit. Wording for the combined first prompt when multiple sessions: *"You're in N collaboration sessions. Leave all and quit?"* then per-tab save prompts as needed. After all resolve: `CollabManager::shutdown` (presence-removals best-effort) then close.

**(d) Crash / kill:** no prompts, obviously. Peers see the presence entry age out (≤30 s). The user's protection is the recovery file (§15.6); membership is not resumable in v1 (§13.6).

### 13.5 Attach / detach hook lists (single source of truth — both must mirror)

Attach (after doc+binding exist):
1. `editor.update`: clear undo/redo stacks; `set_collab_undo_redirect(Some(…))`; set `collab_capture = true`.
2. `cx.observe(editor)` → `flush_local_edits` (drain → LocalApplier → commit; loro local-update subscription → `Publish`). Keep the `Subscription` in the session.
3. `cx.subscribe(editor)` (`SelectionChanged`) → presence update (50 ms debounce).
4. loro subscriptions: diff events (→ RemoteApplier), local-update (→ publish), undo manager creation.
5. Timers: anti-entropy digest (10 s jittered), presence keepalive (10 s), self-check (30 s idle-gated).
6. Register in `CollabManager` registry (`SessionId ↔ panel Uuid ↔ Entity<CollabSession>`).

Detach (any reason — `UserLeft`, `JoinFailed` post-tab, `Fatal`):
1. Best-effort presence self-removal broadcast; `LeaveSession` to net (unsubscribes topic, drops swarm task, clears outbox).
2. Flush any queued-but-unapplied remote patches (deterministic final state — "the state you witnessed").
3. Editor teardown: `set_collab_undo_redirect(None)`, `collab_capture = false` + drain-discard the queue, `set_external_carets(vec![], cx)`, undo stacks stay empty (fresh local history from here).
4. Drop loro subscriptions, UndoManager, timers, observe/subscribe Subscriptions, the LoroDoc, the binding.
5. Deregister from `CollabManager`. Pathless tab keeps its session recovery path until closed or Save-As (then switches to the normal derived recovery path).

### 13.6 Deferred (v1.1, designed-for): session resume across restart

Persist `{session_id, last peer addrs, loro snapshot}` per attached tab on quit; offer "Rejoin previous session" on next launch (re-verifies §12.2 check 2; rejoin = the §13.2 reconnect path with a pre-warmed doc). Not in v1 — process exit is an implicit leave via the §13.4(c) prompts. Do not build now; do not preclude (keep `SessionId` and snapshot export accessible from the session entity).

---

## 14. Save semantics

* **No canonical file exists.** Every peer's save writes *their current view* to *their* file. The app encodes no relationship between any file and the session.
* Path-backed tabs (whoever started from a file, or anyone after Save As): `save`/`save_as`/autosave all work unchanged while attached — autosave naturally tracks the live session state into the user's file. CRDT history is **never** persisted into `.db8` (a saved file is a plain snapshot; format untouched).
* **Save As while attached does not detach** (§15.5 wires `DocumentPanel::set_path` to leave the session untouched). After Save As, the tab is path-backed: autosave engages, the close-prompt branch changes, recovery path switches to the derived one.
* Pathless attached tabs: autosave/Save skip → Save As (existing untitled behavior); recovery via §15.6.

---

## 15. Editor changes (gpui-flowtext) — complete enumerated list

No feature flag; all inert without a session. **No loro/iroh types in this crate.** Each item names the exact file and insertion point.

1. **`editor/mod.rs`** — new fields on `RichTextEditor` (near `last_collaboration_edit`, line ~870):
   ```rust
   pending_collab_edits: Vec<CollaborationEdit>,
   collab_capture: bool,                                    // false ⇒ never push (no leak for solo editors)
   suppress_collab_capture: u32,                            // mirrors suppress_mutation_notify pattern
   collab_undo_redirect: Option<std::rc::Rc<dyn Fn(UndoRedirect)>>,   // Rc: entities are single-threaded
   ```
   plus `pub enum UndoRedirect { Undo, Redo }`. Initialize in `new_with_path` (`lifecycle.rs:16`); reset in `release_transient_memory` and `dispose_for_close`.
2. **`edit_pipeline.rs`** — the §6.2 delta-exact capture:
   * In `insert_single_grapheme_fast_path`: after the undo-record logic (merged or not), if `collab_capture && suppress_collab_capture == 0`, push `CollaborationEdit { operations: vec![InsertText { paragraph: paragraph_id, byte: caret.byte, text: text.to_string(), styles }] }` — **the single grapheme only**, never the merged record.
   * Change `mark_document_changed_with_reconcile(generation, reconcile, cx)` to take the ops explicitly: add a thin wrapper `mark_document_changed_with_ops(generation, reconcile, ops: Option<&[CanonicalOperation]>, cx)`; call sites that just pushed a *new* `EditRecord` pass `Some(&record.canonical_operations)`; the fast path (which already pushed its own delta) and history-restore paths pass `None`. The wrapper pushes into `pending_collab_edits` under the same gate. Keep `last_collaboration_edit` assignment as-is for compatibility.
3. **`lifecycle.rs`** — accessors:
   ```rust
   pub fn take_pending_collab_edits(&mut self) -> Vec<CollaborationEdit>;   // mem::take
   pub fn set_collab_capture(&mut self, on: bool);                          // false also clears the queue
   pub fn set_collab_undo_redirect(&mut self, hook: Option<Rc<dyn Fn(UndoRedirect)>>);
   pub fn set_recovery_path(&mut self, path: Option<PathBuf>);              // §15.6
   ```
4. **`commands.rs:191/205`** — first line of `undo()`/`redo()`: `if let Some(hook) = self.collab_undo_redirect.clone() { hook(UndoRedirect::Undo); return; }` (resp. `Redo`).
5. **`collab_apply.rs` (new)** — `apply_collab_patches` + `collab_apply_deferred` per §7, building on the `edit_ops` primitives; selection-remap helpers; ≤600 LOC. Add `edit_ops/collab.rs` with `replace_paragraph_content` if no existing helper fits (extend the `include!` list in `edit_ops/mod.rs`).
6. **Recovery for pathless session tabs**: `set_recovery_path` (item 3) + the session sets it to `std::env::temp_dir().join("flowstate-collab-recovery").join(format!("{}-{}.db8", hex(session_id_prefix8), sanitized_title))`. The existing `schedule_recovery_write` / `discard_recovery_file` machinery (`recovery.rs`) then works unchanged — verify it only requires `recovery_path.is_some()`, not `document_path`. Crash → next launch recovers a plain local document via the existing recovery-discovery flow.
7. **Events**: ensure `impl EventEmitter<EditorEvent> for RichTextEditor` exists (add if only `EditorEventSink` is wired today) and that `SelectionChanged` actually emits on selection mutation paths (check `selection.rs` / movement handlers; add the emit where missing).
8. **`collaboration.rs`** — add `CollabPatch`, `CollabTextDelta` (§7) next to `CanonicalOperation`.
9. **Image placeholder**: in the image measure/paint path (`editor/media.rs` + `rich_text/layout/block_layout.rs`), an `AssetRecord` with empty `bytes` renders a fixed 240×160 "loading" box (muted background + spinner glyph) instead of a broken image. Used while §16 transfers are in flight.
10. **Test** `rich_text/tests/collab_capture.rs` per §6.2.

---

## 16. Assets (images) over the wire

Asset bytes never enter the CRDT (snapshots would carry every image forever). Symmetric, pull-based:

* `BlockPayload::Image` carries metadata only (`asset_id`, `mime`, `content_hash`, `byte_len`).
* When a `ReplaceObjectBlock`/`InsertBlocks` patch references an asset missing from `document.assets`: insert a placeholder `AssetRecord { bytes: Arc::new(vec![]), … }` (renders as §15.9 placeholder) and enqueue a fetch: `PullAsset` via `pull_with_fallback` — candidates: the peer whose gossip delivered the block change, then neighbors, then presence-known peers. **Any peer that has the bytes serves them** (`DirectRequest::Asset` answered from the local `document.assets` of the attached tab). On arrival: `CollabPatch::AssetArrived` swaps the record in; repaint.
* A peer pasting an image needs no special path: the paste populates its local `AssetStore` (existing path), the block syncs as metadata, and everyone else pulls from *it* — or from anyone who already pulled it (fallback chain makes distribution self-spreading).
* Joins: after snapshot projection, scan blocks for missing assets and enqueue fetches, viewport-visible blocks first (the editor knows visible range; expose it or just fetch in block order — acceptable v1).
* Failed fetch: placeholder persists; retry on next reference or manual reload; never blocks text sync. Dedup by `content_hash` is v1.1.

---

## 17. UI specification (gpui-component widgets)

1. **Commands** (`commands.rs`): add `CommandId::ShareDocument` ("Share / Collaborate…", APP context) and `CommandId::JoinSession` ("Join Collaboration Session…", APP); entries in `COMMAND_SPECS` (no default keys); route in the workspace command handler (`workspace/workspace/keybindings.rs`).
2. **Top bar** (`top_bar.rs`): File menu gains "Share Document…" (enabled when the active panel is a rich-text doc) and "Join Session…". Plus a share `icon_button` in the `TitleBar` right cluster (pattern: `settings_top_bar_button`), tinted while the active tab is attached.
3. **Share dialog** (`collab/share_dialog.rs`; gpui-component `Dialog`, `Input`, `Button`, clipboard copy button, `avatar` for the roster):
   * Active tab not attached: explainer + `[Start session]` → `Creating` → flips to the attached view.
   * Attached: read-only `Input` with a **freshly minted** ticket (re-mint on open and on copy — embeds current addr) + Copy; roster (color dot, name, "you" marker) from presence; connectivity line ("Only you — share the invite" / "Offline — reconnecting…"); `[Leave session]` (danger variant → §13.4(a) confirm).
   * Join tab: paste `Input` with inline parse validation (shows doc title from the ticket pre-connect), `[Join]`, progress states from `JoinStage`, inline error on failure.
   * **No role switch anywhere. No "End for everyone" anywhere.** (Decided: everyone writes; sessions end by attrition.)
4. **Status bar** (`render_status.rs`): the §13.1 pill for the active tab; click opens the share dialog. Follow the existing zoom/status element patterns.
5. **Tab badge** (`document_panel.rs` render): small colored dot + peer count on attached tabs; grey variant when `Offline`.
6. **Notifications** (gpui-component `notification.rs`): peer joined ("Maya joined"), peer left, "Left session — this copy is now local", view-rebuilt (debug), incompatible-version (once per peer).
7. **Prompts**: §13.4 copy verbatim; use the existing `window.prompt` flow (`documents.rs:700` pattern) so styling matches.

---

## 18. Failure-mode catalog (each row: explicit handling + a test or QA-script item)

| # | Scenario | Required behavior |
|---|---|---|
| F1 | Garbage/truncated ticket | Inline parse error in join dialog; never dials |
| F2 | Inviter unreachable at join | 15 s subscribe timeout → JoinFailed with actionable copy |
| F3 | Inviter reachable but snapshot pull fails | fallback to neighbors (§10.3); all candidates fail → JoinFailed |
| F4 | Ticket from an old, dead session | subscribe succeeds but no neighbors ever → 15 s timeout (same as F2; copy mentions "session may be over") |
| F5 | Protocol version skew | join: fast clear error; in-session gossip: ignore + one-time notification (§11) |
| F6 | Lineage violation (wrong doc on right topic) | §12.2: join-time meta check fails ⇒ JoinFailed; digest mismatch ⇒ ignore peer; never merge |
| F7 | Peer vanishes (crash/kill) | presence entry ages out ≤30 s → roster update + notification; overlay self-heals |
| F8 | Transient network loss | `Attached{Offline}` (§13.2): edits accumulate, auto-reconnect, digest-pull catch-up; pill visible; zero data loss |
| F9 | Long offline (hours) | same path; reconnect merge interleaves per CRDT semantics; resolution = own-ops undo + manual edit (§8) |
| F10 | All peers leave while I'm attached | `Attached{Online, peers_present: 0}` → "Only you"; wait (drop-in works) or Leave — user's call; no auto-detach |
| F11 | Gossip receiver `Lagged` | treat as gap: immediate digest + pull (§10.4) |
| F12 | Oversized-update blob pull fails / outbox expired | harmless: next anti-entropy round delivers via `Updates` (§10.5) |
| F13 | Projection drift (applier bug) | local self-check → self-heal rebuild + structured log (§7.2); selection clamped |
| F14 | Remote patch during drag/IME | deferral queue (§7.3) |
| F15 | Asset fetch fails everywhere | placeholder persists; text sync unaffected; retry on next reference (§16) |
| F16 | Self-join (own ticket) | detected at parse (`inviter.endpoint_id == own`) → friendly error |
| F17 | Share invoked on an already-attached tab | dialog shows the attached view (re-mint invite); no nested sessions (registry keyed by panel Uuid) |
| F18 | Close tab / quit while attached | §13.4(b)/(c) transactional prompts; Cancel anywhere fully aborts |
| F19 | Crash while attached, pathless tab | session recovery file (§15.6) → recovered as plain local doc on next launch |
| F20 | Two peers edit the same table cell concurrently | LWW on the block payload — one peer's table edit wins (§5.3); QA item to observe, documented limitation |
| F21 | Clock skew between peers | presence timestamps are loro-internal lamport-ish (EphemeralStore) — verify it doesn't use wall-clock comparisons across peers (§23-V7); doc sync needs no wall clock at all |

---

## 19. Performance budgets

* **Keystroke → local paint: zero added frame cost.** LocalApplier runs in the observe callback after paint scheduling; loro insert ≈ 1–5 µs; publish is a channel send. Assert no network I/O and no `export` on the keystroke path beyond loro's incremental local-update bytes.
* Remote keystroke → repaint: ≤ 1 frame after import; per-paragraph regeneration only for touched paragraphs (existing `layout_invalidation_hint` scoping).
* Join on a 5 MB doc: export/import off-main-thread; the only main-thread cost is `replace_document_from_collaboration` (≈ cost of opening the file today).
* Anti-entropy: digests are O(peers in vv) bytes every 10 s to ≤5 neighbors — noise. Self-check projection is O(doc) — that's why it's idle-gated at 30 s.
* Memory: LoroDoc history grows for the session's lifetime and is dropped at detach; never persisted. The blob outbox is capped (64 entries / 16 MiB).

---

## 20. Decisions already made — DO NOT relitigate

1. **Gossip mesh of symmetric peers** (iroh-gossip), not star, not hand-rolled full mesh. Chosen with full knowledge of the trade (anti-entropy + direct side-protocol are the price; host-absence liveness and no-special-peer semantics are the prize).
2. **No roles. Everyone writes.** `CollaborationRole` stays in the editor unused; no Viewer UI, no enforcement, no per-peer permissions.
3. **No starter.** Creating a session = minting secret + seeding content; zero ongoing distinction. All UX branches on `document_path.is_some()`, never on who started.
4. **Sessions end by attrition.** No end-session action, no ended flag, no tombstone. "Alone" is a first-class, indefinitely-valid state.
5. **Membership ⊥ connectivity.** Involuntary disconnect keeps you attached (edits accumulate; auto-reconnect; anti-entropy resync). Only explicit Leave (or its close/quit prompt equivalent) detaches; the doc then freezes at the witnessed state as a plain local document.
6. **Join always creates a fresh tab/doc from the session snapshot.** No merge-local-file-into-session flow exists. **Save As while attached does not detach.**
7. **Process exit = leave (v1)**, guarded by the §13.4 transactional prompt cascade; session resume across restarts is v1.1 (§13.6), designed-for but not built.
8. **CRDT layout:** per-paragraph `LoroText` in a `LoroMovableList` (§5.2); tables/equations/images as atomic LWW payloads matching the editor's own `ReplaceBlock` granularity (§5.3); 4 mark keys with `ExpandType::None`.
9. **Wire = loro update bytes + EphemeralStore bytes.** No operation parsing, no transforms, no custom merge logic anywhere. `WireCanonicalOperation` stays unused.
10. **SessionId = TopicId = the bearer secret**; lineage guarded by §12.2's three checks; topics never reused.
11. **Anyone-can-invite, re-minted tickets** carrying the minter's current address.
12. **Pull-only anti-entropy** (10 s jittered digests via `broadcast_neighbors`); oversized updates via blob-availability notice + direct pull, **no chunk-reassembly protocol**.
13. **Integrity is local-first**: Document-vs-own-CRDT self-check with local rebuild; cross-peer state comparison only as an env-gated QA tool. No arbitration, because projections are derived state.
14. **Undo while attached = loro UndoManager** (own ops only); native stacks cleared at attach/detach; merges are not undoable units — own-ops undo + manual editing is the documented resolution story.
15. **Assets out-of-band, any-peer serving, pull-based** (§16); bytes never in the CRDT.
16. **Theme/zoom/invisibility are local-only.** Flow (`.fl0`) docs out of scope.
17. **Threading:** CRDT on the GPUI main thread inside the session entity; one lazy tokio thread owns endpoint+gossip+direct server; `async-channel` is the only cross-runtime primitive.
18. New core logic in **`flowstate-collab`** (GPUI-free); GPUI glue in `crates/flowstate/src/collab/`; the editor crate stays loro/iroh-free; dependency arrow `flowstate-collab → gpui-flowtext` only.

Deferred (tracked, not designed here beyond noted sketches): session resume (§13.6), selection-range rendering for remote peers, asset `content_hash` dedup, cell-level table CRDT, kick/ban (remedy: rotate session), ticket QR codes, >16 peers, version-history restore.

---

## 21. Testing strategy

### 21.1 Unit / golden (in `flowstate-collab`, all GPUI-free)
* `tests/translation.rs`: for **every** `CanonicalOperation` variant: build a `Document` fixture via `document_from_input`, mutate it through the editor's real `edit_ops` functions, run LocalApplier, project the loro doc back, assert byte-equality with the mutated `Document`. Every text fixture includes `"aé🌍\u{2028}x"`.
* Projection round-trip property test (§7.1).
* `schema.rs` offset helpers: exhaustive positions over the multibyte fixture.
* `tests/anti_entropy.rs`: digest/gap/pull logic against a fake transport (in-memory channel pairs): peer drops offline N ops, digests resume, exactly one pull issued, convergence; `Lagged` path; in-flight pull dedup.
* gpui-flowtext: §6.2 capture regression test; `collab_apply` patch tests (apply around a live selection, IME-deferral predicate, selection remap on multibyte deltas).

### 21.2 Convergence fuzz — the M2 gate; nothing integrates until this is green
`tests/convergence.rs` (proptest): N ∈ {2, 3} simulated peers, each a `(Document, LoroDoc, DocBinding)` triple. Random per-peer op programs (weighted: 70% text insert/delete, 10% split/join, 10% styles, 10% block ops) applied through the editor's real edit ops + LocalApplier; update bytes exchanged through a virtual network applying random delay, reorder, duplication, and per-peer offline windows (buffer then flush — this directly simulates §13.2 drop-out/drop-in); RemoteApplier applies inbound diffs to each peer's `Document`. After quiescence + full exchange: **all projections byte-identical** and equal to a fresh projection of any peer's LoroDoc. Shrunk failures print the op program.

### 21.3 Swarm loopback (`tests/swarm_loopback.rs`)
Real iroh endpoints + real gossip on localhost: `RelayMode::Disabled`, no discovery, addresses registered manually. Three peers: A creates (populate + subscribe), B and C join via A's ticket (full §13.3 sequence). Assert: snapshot fetch, live update propagation A→B→C, presence roster on all three, **A leaves → B and C continue editing and converge** (the no-starter property, asserted in code), B kills its gossip subscription + keeps editing + resubscribes → digest pull converges (§13.2 simulation), blob path (force a >2 KiB update). Mark `#[ignore]` if CI blocks UDP; must run locally before M4 merge.

### 21.4 Manual QA script (commit as `helpers/docs/collab_qa.md`)
Two-three app instances (`cargo run` ×N, different cwd). Cover all 21 failure rows of §18 plus: simultaneous same-paragraph typing; Enter-split race (observe the §5.2 anomaly, confirm convergence); same-table-cell race (F20, observe LWW); image paste on each peer; undo tug-of-war; leave-and-rejoin via fresh ticket; offline-laptop-lid 5 min then resync; close-tab prompt cancellation at both steps; 30-minute soak with autosave on a path-backed peer.

---

## 22. Milestones & work breakdown (for the orchestrating agent)

Dependency graph: **M0 → (M1 ∥ M2) → M3 → M4 → M5 → M6.** Every milestone ends with `cargo clippy` clean and its tests green. Where a milestone says "verify §23-Vn first," the subagent's first action is reading those docs.rs pages and correcting names in its working copy of this plan.

**M0 — Scaffolding (single agent, small).** `cargo new crates/flowstate-collab --lib`; §4 dependency commands; empty modules compiling with `lib.rs` docs; `ids.rs` (SessionId/BlobId/palette); `PROTOCOL_VERSION`/`DIRECT_ALPN` consts; CI stays green.

**M1 — Networking (1 agent).** `net/{runtime,swarm,direct,anti_entropy,blobs}.rs`, `proto_gossip.rs`, `proto_direct.rs`, `ticket.rs`. Verify §23-V1/V2 first. Deliverables: `tests/anti_entropy.rs` green; a temporary `examples/echo_swarm.rs` proving 3-peer subscribe/broadcast/direct-pull on localhost (folded into §21.3 later). No loro yet — payloads are opaque bytes at this layer by design.

**M2 — Document sync core (1–2 agents; the hard one).** `schema.rs`, `binding.rs`, `projection.rs`, `local_apply.rs`, `remote_apply.rs`, `self_check.rs`, plus the gpui-flowtext capture change (§15.1–3, §15.8, §15.10) it needs for fixtures. Verify §23-V3/V4/V6 first. Order of attack: text ops → structure ops (split/join/span) → block ops → fuzz hardening. Deliverables: §21.1 goldens + **§21.2 convergence fuzz green**.

**M3 — Editor integration (1 agent, after M2).** Remaining §15 items (collab_apply.rs, undo redirect, deferral, selection remap, recovery setter, EventEmitter, placeholder), §8 UndoManager wiring. Verify §23-V5/V8 first. Deliverable: editor-level patch/undo/capture tests green (`cx.new`-based entity tests; check how existing `workspace/workspace/tests.rs` constructs entities and follow that pattern).

**M4 — Session orchestration (1 agent).** `collab/mod.rs` (CollabManager global, NetEvent pump, registry), `collab/session.rs` (§13 state machine *exactly*, §13.5 attach/detach lists, presence, assets, self-heal, buffered-import join), workspace hooks (§13.4 prompts into `documents.rs` close/quit paths, joiner tab creation). Deliverables: §21.3 swarm loopback green end-to-end; manual two-instance session works including leave/offline/rejoin.

**M5 — UI (1 agent, can overlap late M4).** §17 complete: commands, menus, share dialog, status pill, tab badge, notifications, prompt copy.

**M6 — Hardening (1 agent).** Full §18 sweep with the §21.4 QA script; perf sanity against §19; `helpers/docs/collaboration.md` (user how-to + maintainer architecture notes); final clippy/test pass across the workspace.

Feature acceptance = §21.4 passes on Linux with three instances, including one run forced through relays (`RelayMode` forced, or two machines on different networks), exercising drop-out/drop-in and the A-leaves-B-C-continue property.

---

## 23. API verification appendix — re-verify these exact names before coding

The architecture depends on none of these spellings; implementers verify rather than guess. Everything *not* listed here was verified on 2026-06-12.

* **V1 (iroh 0.98, M1):** endpoint builder discovery/preset wiring — docs show `Builder::new(preset)` / `.preset()` / `address_lookup()` and `presets::N0`; relays default on (`RelayMode::Default`) but **address lookup is off by default**. Confirm: the preset enabling n0 DNS lookup + pkarr publish; how to await first reachable addr for ticket minting (`endpoint.addr()` + online/watch API); the method to register a remote peer's `EndpointAddr` before dialing (`add_endpoint_addr` or similar). docs.rs/iroh/0.98.2.
* **V2 (iroh-gossip 0.100, M1):** `Gossip::builder()` construction + `max_message_size` config (default is small — confirm the number; our inline limit must stay under it); `iroh_gossip::ALPN` const path; Router registration pattern (`accept(ALPN, gossip)`); `subscribe(TopicId, Vec<EndpointId>)` return type and split (`GossipTopic` → sender/receiver); event enum (`Received{content, delivered_from, scope}`, `NeighborUp`, `NeighborDown`, `Lagged`); `broadcast` vs `broadcast_neighbors`; whether subscribe requires the bootstrap peers' addresses pre-registered on the endpoint (it does not dial without addresses or discovery).
* **V3 (loro 1.13, M2):** `LoroText` UTF-8 surface: confirmed `mark_utf8`, `len_utf8`, `convert_pos(index, PosType, PosType)`; check for `insert_utf8`/`delete_utf8`/`splice_utf8` and `unmark` utf8 variant (else convert via `convert_pos`); `update(text, UpdateOptions)` option fields; `to_delta() -> Vec<TextDelta>` attribute map shape.
* **V4 (loro 1.13, M2):** diff subscription: `doc.subscribe_root` vs `doc.subscribe(&ContainerID)`; `DiffEvent` shape (container path, `Diff::{Text, List, Map}`); the trigger discriminator (`EventTriggerKind::{Local, Import, Checkout}` or current names) used to filter LocalApplier echoes; `subscribe_local_update` callback signature and per-commit firing; `commit_with`/commit-options origin string API; `oplog_vv().encode()`/decode + VV comparison API; `ExportMode::updates(&vv)` exact constructor; pending-import status type returned by `import`.
* **V5 (loro 1.13, M3):** `UndoManager`: constructor, merge-interval + max-steps config, `set_on_push`/`set_on_pop` cursor callback types; `Cursor` encode/decode; `doc.get_cursor_pos` return shape.
* **V6 (loro 1.13, M2):** `LoroMovableList::{insert_container, delete, mov}` exact names/signatures; `LoroMap::{insert, insert_container, get}`; `LoroValue::Binary` for `"data"`; **`LoroDoc::clone` semantics** (shared handle vs deep copy — determines §10.3's serve-side doc access; `fork()` is the deep copy).
* **V7 (loro 1.13, M4):** `awareness::EphemeralStore`: constructor timeout param; set/get/delete-own-key API (the §9 leave broadcast needs an explicit removal or equivalent); `encode`/`encode_all`/`apply`; both subscription kinds (local-updates-bytes vs merged-events); `remove_outdated`; whether peer timestamps are wall-clock-compared across peers (F21).
* **V8 (gpui 0.2 / workspace, M3–M4):** `EventEmitter` impl pattern for an existing entity type; `Global` trait + `cx.set_global` for `CollabManager`; `window.prompt` multi-step sequencing from a detached `cx.spawn` (the §13.4 transactional flow — follow the `close_document_panel` pattern at `documents.rs:670`); how existing entity tests construct editors (`workspace/workspace/tests.rs`) for M3's tests.

---

*End of plan. First action for the orchestrator: run M0, then dispatch M1 and M2 in parallel.*
