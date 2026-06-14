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

In addition to things that are left to implement, there are bugs found in existing implementation - fix these after the above implementations if they still exist. Before fixing  - verify that they actually are bugs (is what is being said true or not, and then fix them.

Verify each finding against current code. Fix only still-valid issues, skip the
rest with a brief reason, keep changes minimal, and validate.

Inline comments:
In `@crates/flowstate-collab/src/ids.rs`:
- Around line 18-20: `SessionId::new` currently uses `rand::random()`, but
session IDs need a stronger entropy source; update the constructor in
`crates/flowstate-collab/src/ids.rs` to generate the ID with `rand::rngs::OsRng`
(or `SysRng`) instead of `rand::random()`, and keep the change localized to the
`SessionId`/ID generation path so all session IDs come from a cryptographically
secure source.

In `@crates/flowstate-collab/src/local_apply.rs`:
- Around line 148-164: In local_apply.rs inside the split/row insertion logic,
validate the new row metadata first before mutating the source LoroText: fetch
the inserted block id and version (the lookups around block_ids.get(insert_ix)
and blocks.get(insert_ix)) before calling delete_utf8 on the original paragraph,
and only truncate text once those checks succeed so a failed split cannot leave
the document partially edited.

In `@crates/flowstate-collab/src/net/blobs.rs`:
- Around line 45-57: In `BlobStore::insert_with_id`, reinserting an existing
`BlobId` currently leaves the old payload in `entries`, and `BlobStore::get`
returns that stale first match. Update `insert_with_id` to remove or overwrite
any existing entry with the same `BlobId` before pushing the new bytes, while
keeping `total_bytes` consistent with the replacement logic.

In `@crates/flowstate-collab/src/net/direct.rs`:
- Around line 138-146: The semaphore permit is being acquired after reading the
request frame, which allows unbounded task spawning and request buffering before
the concurrency limit is checked. Move the
self.permits.clone().try_acquire_owned() check to occur before the read_frame
call in handle_stream() to ensure the concurrency limit gates network work. This
issue applies at two locations: the primary site in handle_stream (lines
138-146) and a sibling location (lines 152-160). Additionally, consider gating
the permit acquisition even earlier, before tokio::spawn invokes handle_stream
in the accept() method, to limit task creation itself.

In `@crates/flowstate-collab/src/net/runtime.rs`:
- Around line 18-40: `start()` leaves `RUNTIME` pointing at dead channels after
`net_main()` shuts down, so future calls reuse a stopped bridge; change
`RuntimeBridge`/`start()` so a failed or exited runtime can be detected and
replaced with a fresh `spawn_runtime()` result instead of returning the cached
pair. Also update the `direct.rs` endpoint cache (the `OnceLock` around the
client endpoint) so it is invalidated or refreshed when the runtime restarts,
otherwise the new bridge will still use a stale endpoint.
- Around line 101-107: Contain per-command failures inside net_main so a bad
join/publish does not exit the runtime or skip responses. In
NetCommand::CreateSession, catch SwarmHandle::spawn errors locally, reply to the
caller with the failure, and keep the loop running; in the publish path around
handle.publish(payload).await, handle the error locally instead of propagating
it. Apply the same error containment at
crates/flowstate-collab/src/net/runtime.rs:101-107,
crates/flowstate-collab/src/net/runtime.rs:116-120, and
crates/flowstate-collab/src/net/runtime.rs:128-130, using the existing net_main
/ replace_swarm / handle.publish flow as the anchor points.

In `@crates/flowstate-collab/src/projection.rs`:
- Around line 169-175: `verify_lineage` in
`crates/flowstate-collab/src/projection.rs` only validates `META_SESSION`, so
add a `META_SCHEMA` check against `SCHEMA_VERSION` before returning success.
Update the `verify_lineage` path to fail fast with an error when the stored
schema is missing or does not match the current version, alongside the existing
session check, so snapshot projection never reaches `document_from_loro` with an
incompatible layout.

In `@crates/flowstate-collab/src/remote_apply.rs`:
- Around line 34-37: In remote_apply.rs, the Diff::Map handling in
remote_apply::apply should emit only one object-replacement patch per map diff
instead of calling apply_map_diff once per updated key; update the logic so
ReplaceObjectBlock is generated once per affected row even when replace_block_at
changes KIND, DATA, and REV together. Use the apply_map_diff path and its
related map-diff dispatch to dedupe by row/target before applying patches, and
ensure the same fix covers the other map-diff site referenced by the comment.

In `@crates/flowstate/src/collab/asset_transfer.rs`:
- Around line 142-147: In record_from_bytes, replace the use of DefaultHasher
with a stable, deterministic wire hash for cross-peer verification so identical
bytes always produce the same content_hash across platforms and Rust versions.
Update the content_hash computation to use a portable algorithm such as SHA-256
or xxHash, and keep the existing byte_len and content_hash validation logic in
AssetRecord unchanged except for comparing against the new stable hash value.

In `@crates/flowstate/src/collab/mod.rs`:
- Around line 119-146: The `session_by_panel` mapping is being inserted on line
137 before two fallible operations execute: the `try_send` call for
RegisterDirectHandler and the `establish_joined_peer` call. If either operation
fails, the function returns an error but leaves the mapping in place, causing
subsequent attach attempts to incorrectly think the panel is already attached.
Move the `self.session_by_panel.insert(panel_id, session_id)` statement to after
the `Self::establish_joined_peer(session, commands, cx)?` call succeeds,
ensuring the mapping is only recorded when the entire attachment operation
completes successfully.
- Around line 53-86: The `start_session_for_panel` function registers a session
at line 75 via `self.register_session(entity.clone(), cx)`, but if either the
`RegisterDirectHandler` command send (line 77) or the `CreateSession` command
send (line 82) fails, the function returns an error without cleaning up the
registered session. This creates a resource leak where the session ID remains
registered but the network layer never received the commands. To fix this, when
either `try_send` call fails, detach the session from its context and unregister
it before returning the error. Reference the correct cleanup pattern used in the
`join_session` method at lines 106-110, which properly calls detach and
unregister before returning errors, and apply the same approach to both error
paths in `start_session_for_panel`.

In `@crates/flowstate/src/collab/presence_view.rs`:
- Around line 33-39: The directory creation in collaboration_recovery_path
currently ignores failures from create_dir_all, so update this function to
handle that error instead of discarding it. Either change
collaboration_recovery_path to return a Result<PathBuf> and propagate the
std::fs::create_dir_all error to callers, or at minimum log/return a failure
when the temp recovery directory cannot be created; keep the rest of the
path-building logic with SessionId and sanitized_recovery_title unchanged.

In `@crates/flowstate/src/collab/session_timers.rs`:
- Around line 238-249: When `rebuild_from_projection` rebuilds the document
after a self-check by calling `replace_document_from_collaboration()`, any
deferred remote patches stored in `session_io.rs` become stale since they
describe edits already present in the rebuilt projection. These stale patches
would then be incorrectly re-applied on the next flush. Clear the
`pending_remote_patches` from `session_io.rs` within the
`rebuild_from_projection` method after the document has been rebuilt to prevent
this re-application of already-synchronized edits.

In `@crates/flowstate/src/collab/session.rs`:
- Around line 131-132: Switch the session request channels in `Session` setup
from unbounded to bounded to apply backpressure and avoid memory growth; update
the `direct_tx/direct_rx` and `undo_tx/undo_rx` creation in `collab/session.rs`
to use `async_channel::bounded` with a justified capacity, and make sure the
downstream direct request pump and undo request pump code paths still handle
send/receive behavior correctly. If keeping unbounded is intentional, document
that decision near the channel setup.

In `@crates/flowstate/src/workspace/workspace/collab.rs`:
- Around line 204-208: In join_collaboration_from_clipboard’s synchronous error
path around crate::collab::join_session, don’t just log and return None; surface
a user-visible error prompt before exiting the branch. Update the error handling
in workspace/collab.rs so the failure is reported through the existing UI/error
notification mechanism used by this flow, while still returning None after the
prompt is shown.

In `@crates/flowstate/src/workspace/workspace/top_bar.rs`:
- Around line 109-143: Update collaboration_top_bar_button in
workspace/top_bar.rs so the “Leave Shared Session” menu item is disabled unless
the active document actually has an attached collaboration session, not just
when has_document is true. Use the same workspace/session state used by
confirm_leave_collaboration_on_active_document to compute the enabled flag, and
pass that into file_menu_item for the “Leave Shared Session” entry so local tabs
don’t show a no-op action.

In `@crates/flowstate/src/workspace/workspace/window.rs`:
- Around line 84-87: `window.rs` is shutting down the global collaboration
runtime too early, which breaks other open windows; change the
`window_handle.update` close path so
`workspace.leave_all_collaboration_sessions(cx)` only affects the current
workspace and `crate::collab::shutdown(cx)` is deferred until the last
workspace/window is gone or behind a shared reference count. Also audit the
collaboration close/leave flow in `collab_prompts.rs` so it does not trigger a
global shutdown from a single workspace path, and route any such teardown
through the same last-window guard.

In `@crates/gpui-flowtext/src/rich_text/editor/block_insertion.rs`:
- Around line 79-80: Update prepare_block_insertion_index and
insert_ordered_block_fragment_after_caret so the placeholder caret paragraph is
only removed when the insertion does not include InputBlock::Paragraph; preserve
the lone empty paragraph for paragraph-block inserts to keep
self.document.paragraphs and self.document.blocks aligned. Use the existing
block-rebuild logic in insert_ordered_block_fragment_after_caret and the caret
cleanup in prepare_block_insertion_index to gate the removal instead of
unconditionally stripping the paragraph.

---

Outside diff comments:
In `@crates/gpui-flowtext/src/rich_text/editor/mouse.rs`:
- Around line 278-304: Build the `ReplaceParagraphSpan` canonical op from the
pre-edit state in `mouse.rs` so it reflects the drag/drop before any mutation is
applied; capture the source-side deleted paragraph span as part of the same
canonical operation, not just the inserted drop range. Update the
`EditRecord`/`mark_document_changed_with_ops` path around `canonical_operations`
so the payload uses the pre-delete document contents and includes both source
and destination paragraph ranges when the drag crosses paragraphs.

In `@crates/gpui-flowtext/src/rich_text/editor/object_selection.rs`:
- Around line 492-500: The block identifier is being resolved after the block
and id vectors have been mutated, causing the lookup to fail and fall back to a
fabricated BlockId(0), which can delete the wrong remote block in collaboration
mode. Capture the block identifier by calling
self.identity_map.block_id(block_ix) BEFORE removing the block from the vectors
using blocks.remove() and remove_block_ids(). Use the captured block id in the
CanonicalOperation for DeleteBlock. If the block id cannot be captured (when the
lookup fails), replace the DeleteBlock operation with a broader operation like
ReplaceDocument instead of using a fake BlockId(0). Apply this same fix to all
locations where blocks are deleted and canonical operations are created.

---

Nitpick comments:
In `@crates/flowstate-collab/src/binding.rs`:
- Around line 148-167: In the rebuild_indexes method, remove the unnecessary
clone operation on line 153 where each BindingRow is being cloned. Since the
index_row method only requires a reference to the BindingRow (as seen in its
signature), pass a direct reference to self.rows[ix] to index_row instead of
creating a clone first. This eliminates wasteful O(n) allocations that occur
every time indexes are rebuilt after insert/remove/move operations.

In `@crates/flowstate-collab/src/net/swarm.rs`:
- Around line 137-162: Replace the eprintln! call in the handle_event function's
error handling branch (where proto_gossip::decode fails) with a structured
logging call using tracing::warn!. This will allow production deployments to
configure log levels and get structured output instead of relying on stderr
output, enabling better observability and log aggregation.

In `@crates/flowstate-collab/src/proto_direct.rs`:
- Around line 31-38: Simplify encode_frame in proto_direct.rs by removing the
redundant u32::try_from(payload.len())? conversion after the MAX_FRAME_LEN
check; since payload.len() is already bounded below u32::MAX, compute the length
directly as a u32 and keep the rest of the frame assembly unchanged.

In `@crates/flowstate-collab/src/proto_gossip.rs`:
- Line 7: Move the DIRECT_ALPN constant out of proto_gossip.rs and into
proto_direct.rs so it lives with the other direct-protocol constants like
MAX_FRAME_LEN and MAX_PAYLOAD_CHUNK_LEN. Update any direct-protocol code that
references DIRECT_ALPN, especially net/direct.rs and net/runtime.rs, to import
it from proto_direct.rs instead of proto_gossip.rs, and leave proto_gossip.rs
containing only gossip-related constants such as PROTOCOL_VERSION and
GOSSIP_INLINE_LIMIT.
- Around line 30-32: The encoded_len function allocates the full message buffer
by calling encode(msg)? just to measure its length, which is inefficient even
though the practical impact is minimal with the current 2KB size bound. Either
implement a size-counting approach that avoids the full buffer allocation (such
as using a custom serializer that only counts bytes without storing them), or if
accepting the allocation as reasonable, add a comment in the function explaining
why the current approach is acceptable given the bounded message size and single
call site.

In `@crates/flowstate-collab/src/ticket.rs`:
- Around line 46-48: The encode_bytes method in the Ticket trait implementation
uses expect() which can panic on serialization failure. First, check the Ticket
trait definition to see if the return type can be changed from Vec<u8> to a
Result type. If the trait signature is flexible, update encode_bytes to return
Result<Vec<u8>, Box<dyn std::error::Error>> and use the ? operator instead of
expect(). If the trait signature is fixed and requires Vec<u8>, then keep the
current approach but add a doc comment above the method explaining why postcard
serialization of SessionTicket cannot fail and the panic is justified as a last
resort safeguard.

In `@crates/flowstate-collab/tests/convergence.rs`:
- Around line 14-55: The test
`two_peers_converge_with_reordered_and_duplicated_update_imports` currently
validates convergence for paragraph text and style operations but does not cover
structural block operations. Add new convergence test functions that exercise
insert, move, replace, and delete operations on structural blocks (such as
images, equations, and tables) between two peers with reordered and duplicated
imports, following the same test pattern used for the existing paragraph-focused
test to ensure these operations also converge correctly.

In `@crates/flowstate/src/collab/session_presence.rs`:
- Around line 9-11: The code currently uses eprintln! macro for logging error
messages when presence updates fail. Replace the eprintln! calls with a
structured logging framework such as tracing or log to enable better
observability, filtering, and production-grade log management. Update all
occurrences where eprintln! is used for the flowstate collab presence update
error message (in the error handling block for presence.apply() and any similar
error logging) to use the appropriate logging macro from your chosen framework
instead, ensuring consistent structured logging throughout the module.

In `@crates/flowstate/src/collab/session.rs`:
- Around line 592-595: The UndoManager setup in session.rs uses magic numbers
for merge_interval and max_undo_steps; extract the 500 and 300 values into named
constants or a session/undo configuration near UndoManager::new,
set_merge_interval, and set_max_undo_steps so the defaults are easy to find and
adjust.

In `@crates/flowstate/src/collab/share_dialog.rs`:
- Around line 146-151: The `window_handle.update()` call that writes to the
clipboard is silently discarding any errors with the `let _ =` pattern. Replace
this pattern by capturing the Result returned from the update call and logging
any failures using an appropriate logger (such as error or warn level). Keep the
operation as best-effort by not propagating or panicking on the error, but
ensure that clipboard write failures are logged to help with debugging
clipboard-related issues.

In `@crates/gpui-flowtext/src/rich_text/editor/lifecycle.rs`:
- Around line 265-268: The method `clear_collab_history` has a misleading name
since it clears both the `undo_stack` and `redo_stack` entirely, affecting all
editor history rather than just collaboration-specific history. Rename this
method to `clear_undo_redo_stacks` or `clear_all_history` to accurately reflect
that it clears all undo and redo history, not just collaboration-related
history. Be sure to update all call sites that reference this method name.

**Recommended order if serializing:** D1 first (it's the gate that surfaces latent
remote-path bugs), then everything else; D1/D2/D3, E1/E2, N1/N2/N3 are safe to run
concurrently in Wave 1.
