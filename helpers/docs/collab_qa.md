# Flowstate Collaboration QA

Manual QA for P2P `.db8` collaboration. Run with two or three app instances using separate working directories. Use fresh documents unless a scenario says otherwise.

## Setup

1. Start instance A and open or create a `.db8` document.
2. Use `Share / Collaborate...` to start a session and copy an invite.
3. Start instance B, use `Join Collaboration Session...`, paste the invite, and verify a new shared tab opens.
4. For three-peer cases, repeat join from instance C using a fresh invite from either A or B.

## Core Sync

1. Type in A and verify B/C update live.
2. Type in B and verify A/C update live.
3. Paste multi-paragraph rich text and verify convergence.
4. Exercise multibyte text with the fixture `a\u{00e9}\u{1f30d}\u{2028}x`.
5. Apply paragraph styles, run styles, highlights, underline, and strikethrough from different peers.
6. Insert, move, replace, and delete image/equation/table blocks.
7. Paste an image on A; verify B/C show a loading placeholder then the image.
8. Paste a different image on B; verify A/C can serve it after B has it.

## Presence

1. Move the caret with keyboard arrows in A; verify B sees the remote caret move.
2. Select text in B; verify A sees the caret position update.
3. Leave C or kill it; verify presence ages out within 30 seconds.
4. Reopen the share dialog and verify participant count/status remains sensible.

## Undo And Conflicts

1. Type several edits on A, then undo and redo; verify only A's edits are undone/redone and all peers converge.
2. Interleave A and B edits in the same paragraph, then undo on A and B independently.
3. Simultaneously edit the same table cell from A and B; verify the block converges with last-writer-wins behavior.
4. Simultaneously split a paragraph on A while B types later in the paragraph; verify no data loss and convergence.

## Network And Recovery

1. Disconnect B's network or block traffic, keep editing on B, then reconnect; verify anti-entropy catches up.
2. Let A leave while B and C remain; verify B and C continue editing and converging.
3. Force a large paste above the gossip inline limit; verify blob pull or later anti-entropy convergence.
4. Quit or crash a pathless joined tab; verify a recovery file exists under the temp `flowstate-collab-recovery` directory and opens as a local document on recovery.
5. Test a stale/dead invite; verify join fails with actionable copy.

## Prompts And Leave

1. Click `Leave session`; cancel, then leave. Verify the tab remains open as a local document.
2. Close an attached clean path-backed tab; verify leave prompt, then close.
3. Close an attached dirty path-backed tab; verify leave prompt, save prompt, and cancel at each step keeps the session attached.
4. Close a pathless attached tab; verify leave prompt, `Save As...` / `Don't Save` / `Cancel` behavior.
5. Quit with multiple attached tabs; verify the combined leave prompt, then per-tab save prompts. Cancel anywhere must abort quit.

## Compatibility And Error Cases

1. Paste a malformed invite; verify inline parse error and no network dial.
2. Use an unsupported-version invite if available; verify clear rejection.
3. Try joining a session already open in the same app; verify it is rejected.
4. Kill the inviter before join; verify join fails without modifying existing documents.
5. Confirm saved files are plain `.db8` snapshots and do not contain CRDT history.

## Failure Checklist

| Row | Scenario | Expected Result |
|---|---|---|
| F1 | Garbage or truncated ticket | Inline parse error; no network dial. |
| F2 | Inviter unreachable at join | Join times out with actionable copy. |
| F3 | Inviter reachable but snapshot pull fails | Fallback candidates are tried; all failures end in `JoinFailed`. |
| F4 | Old dead-session ticket | Join times out and copy indicates the session may be over. |
| F5 | Protocol version skew | Join rejects fast or in-session gossip is ignored with a one-time notice. |
| F6 | Lineage violation | Join meta check fails or digest mismatch is ignored; no merge. |
| F7 | Peer crash or kill | Presence ages out within 30 seconds and roster updates. |
| F8 | Transient network loss | Offline pill appears; edits accumulate and resync after reconnect. |
| F9 | Long offline window | Reconnect merges local and remote edits without data loss. |
| F10 | All other peers leave | Session remains attached as `Only you`; no auto-detach. |
| F11 | Gossip receiver lag | Digest pull runs and peers converge. |
| F12 | Oversized blob pull fails | Later anti-entropy recovers the missing update. |
| F13 | Projection drift | Self-check rebuilds from projection and clamps selection. |
| F14 | Remote patch during drag or IME | Patch is deferred, then applied safely. |
| F15 | Asset fetch fails everywhere | Placeholder persists; text sync remains unaffected. |
| F16 | Self-join with own ticket | Friendly error; no dial. |
| F17 | Share on already-attached tab | Attached view opens and can mint a fresh invite. |
| F18 | Close tab or quit while attached | Transactional prompts appear; Cancel aborts the close or quit. |
| F19 | Crash with attached pathless tab | Recovery file opens as a local document on next launch. |
| F20 | Same table cell edited concurrently | Cell converges with documented last-writer-wins behavior. |
| F21 | Peer clock skew | Presence remains sensible; document sync does not depend on wall clock. |

## Additional Soak Checklist

1. Simultaneously type in the same paragraph from two peers.
2. Race Enter-split on one peer against typing later in the same paragraph on another peer.
3. Race edits in the same table cell and observe last-writer-wins convergence.
4. Paste an image on each peer and verify asset placeholders resolve.
5. Undo and redo interleaved edits from multiple peers.
6. Leave and rejoin using a fresh ticket.
7. Close a laptop lid or disconnect a peer for 5 minutes, then resync.
8. Cancel close-tab prompts at each step and verify the session stays attached.
9. Run a 30-minute soak with autosave enabled on a path-backed peer.
