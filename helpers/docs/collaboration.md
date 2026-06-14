# Flowstate Collaboration

Flowstate collaboration lets multiple app instances edit the same `.db8` document over a peer-to-peer session. Saved files remain normal `.db8` files; collaboration history is session state, not file state.

## User How-To

1. Open the document you want to share.
2. Choose `Share / Collaborate...`.
3. Select `Start session`, then copy the invite.
4. Send the invite to another Flowstate user.
5. The other user chooses `Join Collaboration Session...`, pastes the invite, and joins into a new tab titled with `(shared)`.
6. Use the Share dialog to copy fresh invites, inspect participants, or leave the session.

Status labels:

| Status | Meaning |
|---|---|
| Online | Connected and publishing local edits. |
| Only you | Session is still active, but no other peers are currently present. |
| Offline | Local edits continue and sync should resume after reconnection. |
| Fetching | Joining peer is downloading the current document snapshot. |
| Building | Snapshot download finished and the local shared document is being built. |

Leaving a session keeps the current tab as a local document. Closing or quitting while attached should prompt before leaving. Pathless attached tabs write recovery snapshots under the temp `flowstate-collab-recovery` directory if the app exits unexpectedly.

## Maintainer Notes

Collaboration is split between the GPUI app crate and the GPUI-free collaboration crate:

| Area | Responsibility |
|---|---|
| `crates/flowstate/src/collab` | Session lifecycle, UI-facing phases, presence, asset pulls, direct request serving, and editor patch application. |
| `crates/flowstate-collab` | Loro schema/projection, local and remote appliers, anti-entropy, tickets, gossip, direct pulls, and convergence tests. |
| `crates/gpui-flowtext` | Document model, editor canonical operations, block assets, persistence, and collab patch application. |

Data flow:

1. Local editor mutations emit canonical operations.
2. `LocalApplier` writes those operations into the session `LoroDoc`.
3. Local Loro updates publish over gossip when small, or by blob/direct pull when large.
4. Remote update imports produce `CollabPatch` values via `RemoteApplier`.
5. App sessions apply patches immediately unless the editor is in a deferred state such as drag or IME composition.
6. Digest anti-entropy compares version vectors and pulls missing updates when gossip lags.
7. Image assets travel separately by asset ID and stable content hash; missing assets render placeholders until fetched.

Operational notes:

1. Invite tickets contain the session ID, title, protocol version, and inviter endpoint address.
2. Joins first subscribe to the session, then direct-pull a snapshot with byte progress, then build the editor document.
3. Lineage checks reject mismatched session metadata before applying snapshots or digest pulls.
4. Presence uses ephemeral state and should not drive document correctness.
5. Saved `.db8` files intentionally exclude CRDT session history.

Verification references:

1. `helpers/docs/collab_qa.md` has the manual QA matrix.
2. `cargo test -p flowstate-collab` covers GPUI-free sync behavior.
3. `cargo test -p flowstate-collab --test convergence` runs the convergence fuzz gate.
