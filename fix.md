# Flowstate Collaboration — Remediation Plan (`fix.md`)

Closes the gaps found auditing the `fable-collab` branch against `plan.md`. The
implementation is ~85% faithful and builds/clippy-clean; this doc enumerates the
deltas needed to reach the plan's intent. Each task lists **files**, **what to do**,
and an **acceptance check**. Audience: an Opus-tier implementer. Keep `plan.md` as
the source of truth for behavior; section refs (e.g. §13.2) point there.

Conventions (from repo `AGENTS.md` / `plan.md`): 2-space indent, files <1000 LOC,
prefer `gpui-component` widgets, `cargo clippy` clean per task, never bare
`unwrap()` on `get`, every `#[allow]`/`unsafe` needs a `reason`. Don't relitigate
§20 decisions.

---

## Execution plan (parallelism)

Five tasks split across crate/file boundaries run in **two waves**. Within a wave,
spawn one subagent per task — they touch disjoint files so they won't collide.

```
WAVE 1 (3 agents, fully parallel — different crates/files):
  ├─ Agent E  "editor"     → crates/gpui-flowtext/**           (Tasks E1,E2)
  ├─ Agent D  "docsync"    → flowstate-collab/{schema,local_apply,remote_apply,
  │                          self_check,patch_apply(new)} + tests/{convergence,
  │                          remote_apply}                      (Tasks D1,D2,D3)
  └─ Agent N  "net"        → flowstate-collab/net/**, ticket.rs, runtime.rs,
                             tests/swarm_loopback(new)          (Tasks N1,N2,N3)

WAVE 2 (2 agents, parallel — app crate, partitioned by file):
  ├─ Agent S  "session"    → flowstate/src/collab/{mod,session,session_io,
  │                          session_timers,asset_transfer}.rs, collab.rs,
  │                          flowstate-collab/net/mod.rs        (Tasks S1..S6)
  └─ Agent U  "ui"         → flowstate/src/collab/{share_dialog,status,
                             presence_view,notify(new)}.rs, render_status.rs
                                                                (Tasks U1..U4)

WAVE 3 (1 agent, sequential — after 1+2 merge):
  └─ Verification + docs + LOC splits                           (Tasks V1,V2)
```

**Coordination points (only two):**
1. **Notifications contract.** Agent S emits notices; Agent U renders them. Land
   the shared event enum first: Agent S adds `pub enum SessionNotice {...}` +
   `cx.emit(...)` in `session.rs`; Agent U subscribes and renders via
   `notify.rs`. Define the enum (below in S5) before either starts so both compile.
2. **`NetEvent::IncompatibleVersion`.** Agent N adds the variant + emits it
   (N2); Agent S routes it in the `mod.rs` event pump; Agent U renders it. Agree
   the variant name up front.

Severity: **P0** = correctness / explicit plan gate · **P1** = robustness/UX ·
**P2** = fidelity/cleanup.

---

## WAVE 1

### Agent D — Document-sync test gate (the big one)

#### D1 (P0) — Convergence proptest fuzz + RemoteApplier coverage
`plan.md` §21.2 calls this "the M2 gate; nothing integrates until this is green."
Current `tests/convergence.rs` is a single hardcoded 2-peer `#[test]` that never
exercises `RemoteApplier`. Replace it with the real fuzz.

- **New file `flowstate-collab/src/patch_apply.rs`** (GPUI-free, ≤400 LOC): add
  `pub fn apply_patches(document: &mut Document, binding: &mut DocBinding, doc: &LoroDoc, patches: &[CollabPatch]) -> Result<()>`.
  Mutate the `Document` from each `CollabPatch` using the **exported `edit_ops`
  primitives** (`insert_text_at`, `mutate_runs_in_range`, span replace, block
  splice — already re-exported from `gpui-flowtext`, see existing imports in
  `tests/convergence.rs`). This is the headless twin of the editor's
  `apply_collab_patches`; it lets tests close the loop without GPUI. Register in
  `lib.rs`.
  - For `ParagraphText` regenerate the paragraph from `new`; for structural
    patches splice `blocks`/`paragraphs`/`ids` using the **patch-provided**
    `block_id`/`paragraph_id` (do not invent ids); `AssetArrived` is a no-op here.
- **Rewrite `tests/convergence.rs`** as a `proptest!`:
  - Peer = `(Document, LoroDoc, DocBinding)` + a diff-event subscription that
    collects `DiffEvent`s.
  - Generate random per-peer op programs, weighted **70% text ins/del, 10%
    split/join, 10% styles, 10% block ops**, run through the real `edit_ops` +
    `LocalApplier`. N ∈ {2, 3}.
  - Virtual network exchanging `subscribe_local_update` bytes with random
    **delay, reorder, duplication, and per-peer offline windows** (buffer →
    flush). On import, drain collected `DiffEvent`s → `RemoteApplier::apply_event`
    → `apply_patches` into that peer's `Document`.
  - After quiescence + full exchange: assert **all peers' `Document`s
    byte-identical** (texts, run vectors, styles, block kinds, payload bytes) AND
    equal to a fresh `projection::document_from_loro` of any peer. Shrunk failures
    should print the op program.
  - Every text fixture must include `"aé🌍\u{2028}x"`.
- **Acceptance:** `cargo test -p flowstate-collab` green with the new proptest
  (≥256 cases); failure shrinks print the program.

#### D2 (P0) — RemoteApplier golden tests + `ReplaceBlock{None}`
`tests/translation.rs` covers LocalApplier only; the remote path is untested.
- **New `tests/remote_apply.rs`**: for each `CollabPatch`-producing diff (text,
  style, data/rev, list insert/delete/move), drive a second `LoroDoc` by importing
  peer-1 updates, run `RemoteApplier::apply_event`, assert the produced
  `Vec<CollabPatch>` shape + (via `apply_patches`) byte-equality with the source
  `Document`.
- Add the missing `ReplaceBlock { block: None }` LocalApplier path test
  (`local_apply.rs` `single_changed_object_row`) to `tests/translation.rs`.
- **Acceptance:** new tests green; every `CanonicalOperation` and every
  `CollabPatch` variant now has coverage.

#### D3 (P2) — Offset/self-check discipline
- **`self_check.rs`**: reorder `projection_hash` to match §7.2 literal order
  (styles → run `(len,styles)` → text bytes → kinds → payload). Harmless but spec.
- **`schema.rs` + `local_apply.rs`**: add the §5.5 debug assertion
  `debug_assert_eq!(text.len_utf8(), paragraph_text_len(&document.paragraphs[ix]))`
  after each paragraph mutation in the LocalApplier (both directions).
- **Acceptance:** tests still green in debug; assertion present at mutation sites.

---

### Agent N — Networking robustness

#### N1 (P2) — Await first reachable address before minting tickets
§10.1 / §23-V1. Tickets currently embed `endpoint.addr()` immediately
(`net/runtime.rs:156-158,177-179`), so a cold install can embed an unreachable
address.
- In `net/runtime.rs`, after `bind()`, await the endpoint's first reachable
  address (watcher/online API — verify exact name on docs.rs for iroh 0.98 via the
  `docs` subagent). Gate `CreateSession` / `MintTicketAddr` replies on it (short
  timeout, then best-effort).
- **Acceptance:** minted `EndpointAddr` contains a relay URL and/or direct addrs
  in a from-cold run.

#### N2 (P1) — Surface protocol-version skew
§11 / F5. Today a version mismatch is only `eprintln!` in `swarm.rs:149`.
- Add `NetEvent::IncompatibleVersion { session, peer }` (in
  `flowstate-collab/src/net/mod.rs`). Emit it from the gossip decode-error path in
  `swarm.rs` when the failure is a version mismatch (distinguish from generic
  decode error). Keep the message-ignore behavior.
- (Agent S routes it; Agent U renders a one-time-per-peer toast.)
- **Acceptance:** decoding a wrong-`PROTOCOL_VERSION` frame emits the event once.

#### N3 (P2) — Swarm loopback integration test
§21.3 / M1 deliverable — currently absent.
- **New `tests/swarm_loopback.rs`** (`#[ignore]` if CI blocks UDP): real iroh
  endpoints + real gossip on localhost, `RelayMode::Disabled`, addresses
  registered manually. 3 peers: A creates, B & C join via A's ticket; assert
  snapshot fetch, A→B→C update propagation, presence roster on all three, **A
  leaves → B & C keep editing & converge** (the no-starter property), a forced
  >2 KiB blob path, and a kill-subscription→resubscribe→digest-pull heal.
- **Acceptance:** `cargo test -p flowstate-collab -- --ignored swarm_loopback`
  passes locally.

---

### Agent E — Editor fidelity (`gpui-flowtext`)

#### E1 (P1) — Emit `SelectionChanged` on all selection-mutation paths
§15.7. Presence carets are driven off `EditorEvent::SelectionChanged`, but it's
not emitted on mouse paths, so mouse-only caret moves don't update presence.
- Add `emit_selection_changed(cx)` (mirror existing usage in `caret_movement.rs`)
  to the selection-mutating paths that currently lack it:
  `mouse.rs` (click/drag set-selection at `:38,153,163,196,233,254,274`),
  `hit_testing.rs` caret placement, `select_all` (`commands.rs:308`), search-result
  nav (`search_highlights.rs:32`), outline `scroll_to_paragraph` (`commands.rs:162`).
- Guard against spurious emits (only when the selection actually changed).
- **Acceptance:** clicking to move the caret fires `SelectionChanged` (add a small
  editor test or assert via existing test harness in `rich_text/tests`).

#### E2 (P1) — IME deferral + table/equation offset remap
§7.3 / §7-item-4.
- **IME:** `editor/platform.rs` exposes no marked-text state. Add a query
  `pub fn ime_composition_active(&self) -> bool` (track marked-text begin/end in
  the platform input handler), and add it to the `||` chain in
  `collab_apply.rs:23` `collab_apply_deferred`.
- **Offset remap:** in `collab_apply.rs` `ParagraphText` handling, also remap
  `table_cell_*` / `equation_source_*` selection offsets through `delta_utf8`
  (same retain/insert/delete walk used for `self.selection`), not just the main
  selection.
- **Acceptance:** remote patch during an active IME composition is queued (not
  applied); a remote edit to a paragraph while a table cell is being edited remaps
  the cell caret instead of corrupting it.

---

## WAVE 2 (app crate)

### Agent S — Session lifecycle robustness

#### S1 (P0) — Self-join detection
§13.3 step 1 / F16. `join_session` (`collab/mod.rs:94-129`) lacks the own-invite
guard.
- After parsing the ticket, if `ticket.inviter.id == <own endpoint id>` (get it
  via the runtime/`CollabManager`), fail fast with friendly copy ("That's your own
  invite — open the Share dialog instead.").
- **Acceptance:** pasting one's own ticket shows the friendly error, never dials.

#### S2 (P1) — Connectivity detection per §13.2
`session_timers.rs:52-69` uses OR (`quiet_rounds>=2 || !endpoint_online`) and has
no probe dial; plan requires **all of**: zero neighbors >5 s **AND** 2 quiet
digest rounds **AND** (`EndpointOnline(false)` OR probe-dial-to-last-peer-fails).
- Change the boolean to the three-way AND. Add a probe-dial attempt to the most
  recent known peer as the third conjunct's OR-branch (best-effort; reuse
  `pull_with_fallback`-style dial or a lightweight `NetCommand`).
- In the offline recovery loop (`run_recovery_if_due`), **retain and re-register
  last-presence-seen peer addresses** (not just the original inviter) and **await
  first `NeighborUp` (10 s)** before flipping back to `Online`.
- **Acceptance:** "alone but networked" stays `Online, peers_present==0`; a real
  network drop flips to `Offline` only when all three conditions hold; reconnect
  restores `Online` after a `NeighborUp`.

#### S3 (P1) — Join timeouts + budget + `EnsureUp`
§13.3 / F2 / F4.
- Issue `NetCommand::EnsureUp` at the start of the join path.
- After `JoinSession`, **await first `NeighborUp` with a 15 s timeout** →
  `Detached(JoinFailed("couldn't reach anyone in this session…"))`.
- Wrap the snapshot pull in a **30 s overall budget** (distinct from the 10 s
  per-peer `DIRECT_PULL_TIMEOUT`).
- **Acceptance:** ticket for a dead session fails after ~15 s with actionable copy.

#### S4 (P1) — Stream join progress
§13.3 step 4. `FetchingSnapshot { got, total }` is frozen at `0/None`
(`session.rs:274`).
- Thread byte progress from the direct-pull (the `DirectResponseHeader::Ok{total_len}`
  + chunk loop already knows `got`/`total`) up to the session phase so the join
  dialog shows real progress. May need a progress channel on `PullSnapshot`.
- **Acceptance:** join dialog advances `got/total` during a large snapshot fetch.

#### S5 (P1) — "(shared)" tab title + emit `SessionNotice`
- §13.3 step 6: title the joined tab `format!("{} (shared)", meta.title)`
  (`collab.rs:271` currently passes the raw ticket title).
- **Notifications contract:** add to `session.rs`:
  ```rust
  pub enum SessionNotice {
    PeerJoined(String), PeerLeft(String), LeftSession,
    ViewRebuilt, IncompatibleVersion(String),
  }
  impl EventEmitter<SessionNotice> for CollabSession {}
  ```
  Emit `PeerJoined`/`PeerLeft` from the presence-roster diff, `LeftSession` from
  `detach(UserLeft)`, `ViewRebuilt` from the self-heal rebuild
  (`session_timers.rs:228`, replacing the `eprintln!`), `IncompatibleVersion` from
  the routed `NetEvent::IncompatibleVersion` (S6). Replace existing `eprintln!`
  notice sites.
- **Acceptance:** roster changes/self-heal/leave each `cx.emit` a `SessionNotice`.

#### S6 (P1) — Route `NetEvent::IncompatibleVersion`
- In the `mod.rs` NetEvent pump, route the new variant (N2) to the session →
  emit `SessionNotice::IncompatibleVersion`, deduped per peer.

#### S7 (P2) — 50 ms presence debounce
§9 / §13.5.3. `refresh_own_presence` fires synchronously on every
`SelectionChanged` (`session.rs:571-577`).
- Debounce the presence refresh by ~50 ms (coalesce rapid selection changes via a
  timer/`cx.spawn` guarded flag).
- **Acceptance:** rapid caret movement produces ≤1 presence broadcast per ~50 ms.

---

### Agent U — UI & notifications

#### U1 (P1) — Notifications (the §17.6 gap)
Today notices are `eprintln!`/modal prompts; gpui-component's notification API
(`vendor/gpui-component/src/notification.rs`) is unused.
- **New `collab/notify.rs`**: thin helper wrapping the vendored notification
  widget (look up exact push API via the `docs` subagent if unclear).
- Subscribe to `CollabSession`'s `SessionNotice` (from S5) in the workspace render
  path and render toasts: peer joined ("Maya joined"), peer left, "Left session —
  this copy is now local", view-rebuilt (debug only),
  incompatible-version (once per peer).
- Replace the modal `window.prompt` "Left session" feedback (`collab.rs:180-188`)
  with a transient toast.
- **Acceptance:** a peer joining/leaving shows a toast; leaving shows the "now
  local" toast (no modal).

#### U2 (P1) — Share dialog → gpui-component `Dialog`
§17.3 + repo UI rule. The dialog is a hand-rolled `div` overlay
(`share_dialog.rs:366-447`), no avatar, and join progress isn't surfaced.
- Rebuild on gpui-component `Dialog` (+ `Input`, `Button`, clipboard copy button,
  `avatar` for the roster). Preserve all current states: not-attached explainer +
  `[Start session]`; attached view (freshly-minted ticket + Copy + roster +
  connectivity line + `[Leave session]` danger); join tab (paste + inline
  validation showing title + `[Join]` + inline error).
- **Surface `JoinStage` progress** inside the join tab (Resolving / Subscribing /
  Fetching got·total / Building), using S4's data.
- Keep: **no role switch, no "End for everyone"** (§20.2/§20.4).
- **Acceptance:** dialog renders via `Dialog`; roster shows avatars; join shows
  staged progress.

#### U3 (P2) — Roster color hash consistency
§9. Rendering hashes the hex key (`presence.rs:157-162`) while `self_color`
hashes raw `EndpointId` bytes — two functions. Pick one (`hash(EndpointId) %
PALETTE.len()` per plan) and use it everywhere; drop the vestigial one.
- **Acceptance:** a peer's dot color matches between roster and caret, derived from
  `EndpointId`.

#### U4 (P2) — LOC split for `share_dialog.rs`
After U2, if `share_dialog.rs` exceeds ~500 LOC, split (e.g. `share_dialog.rs`
view + `share_dialog_state.rs` logic). Keep <1000 hard cap; aim for the §3.1 caps.

---

## WAVE 3 — Verification & docs (sequential, after merge)

#### V1 — Cleanup + final gate
- Split `collab/mod.rs` (currently 409 LOC > §3.1 cap 350) into `mod.rs` +
  e.g. `manager.rs`/`pump.rs` if it didn't shrink during Wave 2.
- (Optional P2) `SessionId::new()` → use `OsRng` explicitly per §12.2.
- (Optional P2) Add `head_container`/`anchor_container` to `PresenceSelection`
  (§9) if container-from-cursor recovery proves fragile; otherwise leave a
  `// reason:` comment that it's intentionally derived.
- Run `cargo clippy` (whole workspace) — must be clean; `cargo test` (workspace,
  GPUI-free + editor) green; `swarm_loopback` green locally.

#### V2 — Docs (§22 M6)
- `helpers/docs/collaboration.md` — user how-to + maintainer architecture notes.
- `helpers/docs/collab_qa.md` — the §21.4 manual QA script (all 21 F-rows +
  same-paragraph typing, Enter-split race, same-cell race, image paste, undo
  tug-of-war, leave/rejoin, offline-lid resync, close-prompt cancellation, 30-min
  soak).

---

## Quick traceability (gap → task)

| Plan ref | Gap | Task |
|---|---|---|
| §21.2 | Convergence fuzz is a 2-peer stub; RemoteApplier untested | D1, D2 |
| §13.3 / F16 | No self-join detection | S1 |
| §13.2 | Offline logic OR-not-AND; no probe dial; addrs not retained | S2 |
| §13.3 / F2,F4 | No 15 s neighbor / 30 s snapshot timeout; no EnsureUp | S3 |
| §13.3 | Join progress frozen 0/None | S4, U2 |
| §13.3 | "(shared)" title missing | S5 |
| §17.6 | Notifications unimplemented (eprintln/modal) | N2, S5, S6, U1 |
| §15.7 | SelectionChanged not emitted on mouse paths | E1 |
| §7.3 / §7 | IME deferral + table/equation remap missing | E2 |
| §17.3 | Custom overlay not gpui-component Dialog; no avatar | U2 |
| §9 | 50 ms debounce missing; dual color hash | S7, U3 |
| §10.1 | No await-first-reachable-addr | N1 |
| §21.3 | swarm_loopback test absent | N3 |
| §7.2 / §5.5 | hash order; len_utf8 assert | D3 |
| §3.1 | LOC caps: mod.rs 409, share_dialog.rs 530 | U4, V1 |

**Recommended order if serializing:** D1 first (it's the gate that surfaces latent
remote-path bugs), then everything else; D1/D2/D3, E1/E2, N1/N2/N3 are safe to run
concurrently in Wave 1.
