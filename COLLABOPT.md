# Collaboration Transport Fragility Under Load

## Summary

The error `COLLAB_RETRYABLE_TRANSPORT_FAILURE: failed to write Flowstate frame length: connection lost: closed by peer: 0` occurs because the remote peer closes the QUIC connection when it encounters an error processing a received update.

**The chain:**
1. Local edit generates mutations → applied to local LoroDoc → Loro update bytes produced
2. Update sent to remote via QUIC transport
3. Remote calls `import_update_checked()` on the bytes → fails (e.g. mutation conflicts, schema mismatch, CRDT divergence)
4. The `?` in `serve_live_stream` at `lib.rs:2909` propagates this error up → the host's connection handler for this peer terminates → connection closed with `error_code = 0`
5. Local side's **next** write attempt fails with `closed by peer: 0` because the remote already dropped the connection

**Why insertion vs deletion matters (pre-Fix-A):** The `canonical_operations_for_content_replacement` bug emitted `SplitParagraph` → `InsertParagraph` for paragraphs that already existed on the remote. These mutations are valid locally (local LoroDoc accepts them) but the remote's LoroDoc rejects them because the container already exists. Deletions don't trigger this because they never generate `SplitParagraph` for existing containers. **Fix A (already applied)** should resolve this specific mutation cause.

**But the deeper problem remains:** The transport + session layer collapses under minimal load because a single failed mutation on the remote tears down the entire connection. Even with Fix A, other transient errors (CRDT frontier divergence, projection failures, concurrent edit conflicts) will produce the same pattern. The system needs resilience at the session layer — not just mutation correctness.

---

## Options

### Option 1: Graceful update rejection (keep connection alive)

On the remote host, when `import_update_checked` fails, catch the error, log it, send an error response to the sender, but **continue the connection loop** instead of letting `?` terminate it.

**Proposed change in `serve_live_stream` (lib.rs:2904-2910):**
Wrap the import in a match/result handler. On failure, send a `WireMessage::Error` back and `continue` the loop.

**Pro:**
- Eliminates the disconnection cascade entirely
- Peer can self-correct (e.g. request resync) without disrupting the session
- Simple, localized change (~10 lines)
- Other peers are unaffected by one peer's bad update

**Con:**
- Bad updates accumulate in the CRDT if the sender retries them blindly — need a way for the sender to know it must resync
- Error message to sender adds one extra write per rejected update
- Doesn't help if the error is from the connection itself (not the update processing)

**Rating: 9/10**

---

### Option 2: Outbound write queue with automatic retry

Instead of writing directly to the transport and failing immediately on disconnect, queue outbound updates internally and retry them after reconnection.

Currently the client loop at `documents.rs:538-598` writes to transport and on failure:
- Retries once immediately
- On second failure: sets `force_reconnect = true` and breaks the loop
- The pending local update is **lost** — after reconnection, the user must re-perform the edit

**Proposed change:** Buffer the update bytes before writing. On transport failure, don't discard — hold in a reconnection buffer. After reconnection, drain the buffer into the fresh transport.

**Pro:**
- No edit loss under transient failures
- Completely decouples edit generation from transport health
- Handles any transport failure, not just mutation errors

**Con:**
- Stateful buffer management adds complexity
- Buffer could grow unbounded under sustained failure
- Buffered updates might be based on a stale CRDT frontier — replaying them after resync could conflict with the fresh snapshot
- Need to track frontiers per buffered update

**Rating: 7/10**

---

### Option 3: Rate-limit outbound writes at the transport layer

Add a write throttle so edits are batched into fewer, larger transport frames, reducing the number of round-trips and thus the window for connection drops.

**Proposed change:** In `collect_outbound_update_batch` or at the `publish_granular_source_mutations` level, introduce a short debounce timer (e.g. 50-100ms) that coalesces rapid edits into a single transport write.

**Pro:**
- Fewer writes = fewer chances for transport failure
- Smaller CRDT updates per write (batched ops are more efficient)
- Simple to implement

**Con:**
- Adds latency to collaboration propagation
- User sees a delay before their edits appear on peers
- Doesn't solve the root cause (remote killing connection on error)
- Masking the symptoms rather than fixing them

**Rating: 4/10**

---

### Option 4: Bidi stream per message (separate streams for each update)

Instead of multiplexing all updates over a single QUIC bi-directional stream, open a new stream per message. QUIC streams are independent — a failure on one doesn't affect others.

**Pro:**
- A bad update rejected by the remote only closes its own stream, not the connection
- Other in-flight updates on different streams proceed unaffected
- Natural back-pressure via stream flow control

**Con:**
- Significant refactor — the entire `write_wire_message`/`read_wire_message` framing is built around a single stream
- Stream overhead: QUIC stream creation has per-stream state
- Still doesn't prevent the remote from eventually closing the connection if too many streams fail
- Stream lifecycle management adds complexity

**Rating: 5/10**

---

### Option 5: Resync-on-error with local edit replay

When the remote rejects an update, instead of the peer disconnecting and the user losing their edit, automatically:
1. Request a fresh snapshot from the remote
2. Hold the failed edit in memory
3. After snapshot is applied, re-attempt the edit against the new document state
4. Only surface an error to the user if the replay also fails

**Pro:**
- Fully transparent to the user — no edit loss, no visible disconnection
- Handles any failure mode, not just transport errors
- Self-healing: recovers CRDT frontier alignment automatically

**Con:**
- Complex to implement correctly (edit replay, conflict detection, ordering)
- The replayed edit might produce different results if the document state changed
- Could cause infinite replay loops if the edit genuinely cannot be applied

**Rating: 6/10**

---

### Option 6: In-order delivery with back-pressure signalling

Add explicit sequencing and ACK tracking so the sender doesn't pipeline writes beyond the remote's processing capacity. If the remote is busy, back-pressure propagates to the local edit pipeline, naturally slowing down edit generation.

**Proposed change:**
- Tag each `WireMessage::Update` with a monotonic sequence number
- Remote includes the last-processed sequence number in the Ack
- Sender pauses writes when in-flight count exceeds a configurable window

**Pro:**
- Prevents overwhelming the remote under rapid edits
- Works at the correct layer (session, not transport)
- No edit loss — edits queue locally until remote is ready

**Con:**
- Adds round-trip latency per window of edits
- Sequence tracking adds protocol complexity
- Doesn't address the remote killing the connection on error (only prevents overload)

**Rating: 8/10** (but complementary to Option 1, not a replacement)

---

## Recommended approach

**Option 1 (9/10) + Option 2 (7/10) compose well:**

1. **(Option 1)** Make the remote host resilient: catch `import_update_checked` errors in `serve_live_stream` and send an error response back to the sender instead of closing the connection. This prevents the cascade where one bad edit disconnects the peer.

2. **(Option 2)** Buffer outbound updates on the sender side so a transient disconnect doesn't lose edits. After reconnection, drain the buffer into the fresh transport. Track the frontier at buffer time so stale updates can be discarded after resync.

3. **(Option 1 + 2 together)** The combined behavior: a bad update gets rejected (peer stays connected), the sender receives the rejection and requests a snapshot, the snapshot arrives and resets the local CRDT state, and any buffered post-snapshot edits replay cleanly.

Fix A (already applied) removes the primary source of invalid mutations, but the session layer should tolerate edge cases regardless.
