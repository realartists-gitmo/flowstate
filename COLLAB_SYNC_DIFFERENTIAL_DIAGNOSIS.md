# Collaboration Sync Differential Diagnosis

## Primary Diagnosis (Confirmed)

**Issue:** Host-originated updates have `source_session_id = None`, causing them to be filtered out by the update processor.

**Location:**
- `flowstate-sync/src/lib.rs:1169` - `HostedCollaborationPublisher::publish_update`
- `flowstate-sync/src/lib.rs:1402` - `HostedCollaboration::publish_update`
- Both call `LiveUpdate::wire(None, WireMessage::Update {...})`

**Filter Logic:**
- `flowstate/src/workspace/workspace/documents.rs:2046` - Only processes updates where `update.source_session_id.is_some()`

**Evidence:**
- Host logs show `host_ignored_local_or_unscoped_update` with `source_session=None`
- Client logs show successful `client_publish_granular_source_mutations` but host never acknowledges
- `publish_presence` (line 1120) correctly uses `Some(self.config.session_id)` and works

**Fix:** Use `Some(self.config.session_id)` instead of `None` in both `publish_update` methods.

---

## Alternative Diagnoses (If Primary Is Incorrect)

### 1. Session ID Mismatch or Corruption

**Hypothesis:** The session IDs are being compared incorrectly or are corrupted during transmission.

**Check:**
- Verify `self.config.session_id` is properly initialized in both `HostedCollaborationPublisher` and `HostedCollaboration`
- Check if session IDs are being serialized/deserialized correctly in wire protocol
- Look for any session ID transformations in the network layer

**Locations to investigate:**
- `FlowstateSyncConfig::new` - session ID generation
- Wire message serialization in protocol layer
- Session registration in `LiveUpdateHub`

### 2. Filtering Logic Too Restrictive

**Hypothesis:** The filter at line 2046 should allow `None` for local/host updates but handle them differently.

**Check:**
- Review git history to see if this filter was recently added or changed
- Check if there's a separate code path for local vs. remote updates
- Look for comments explaining why `source_session_id.is_some()` is required

**Locations to investigate:**
- `documents.rs:2040-2110` - Complete update handling logic
- Git blame on line 2046 to understand intent

### 3. Update Queue Coalescing Bug

**Hypothesis:** Updates are being merged/coalesced incorrectly, losing session context.

**Check:**
- Review `coalesce_pending_collaboration_update` function
- Check if session IDs are preserved during queue merging
- Look at `merge_outbound_collaboration_update` logic

**Locations to investigate:**
- `documents.rs:2793-2812` - `coalesce_pending_collaboration_update`
- `documents.rs:2816-2850` - `merge_outbound_collaboration_update`

### 4. LiveUpdateHub Filtering

**Hypothesis:** `LiveUpdateHub` is filtering out or dropping updates with certain session IDs.

**Check:**
- Review `LiveUpdateHub::publish` implementation
- Check if there's peer filtering logic
- Look for broadcast channel capacity issues

**Locations to investigate:**
- `flowstate-sync/src/lib.rs` - `LiveUpdateHub` implementation
- Broadcast receiver logic in update loops

### 5. Race Condition in Update Application

**Hypothesis:** Host's local document state is being modified before the update is published, causing a stale/empty update.

**Check:**
- Review the order of operations in `publish_granular_source_mutations_to_host`
- Check if `apply_granular_source_mutations` is modifying state prematurely
- Look for locks being held too long

**Locations to investigate:**
- `documents.rs:3031-3044` - `publish_granular_source_mutations_to_host`
- Document state lock patterns

### 6. Loro CRDT Index Error Propagation

**Hypothesis:** The client-side Loro error (`Index out of bound`) is causing broader sync failure.

**Evidence from logs:**
- `client_local_update_error] Loro error: Index out of bound. The given pos is 18446744073709551615`
- This error appears when processing large mutation batches

**Check:**
- Review how large mutation batches are chunked
- Check if Loro state is properly initialized before applying mutations
- Look for off-by-one errors in position calculations

**Locations to investigate:**
- Granular mutation application code
- `db8_source_mutations_to_granular` conversion logic
- Loro document initialization

### 7. Application-Level Filtering

**Hypothesis:** The `UpdateApplication` parameter is being used to filter updates inappropriately.

**Check:**
- Review how `UpdateApplication` affects update processing
- Check if certain applications are being ignored
- Look for application-specific filtering logic

**Locations to investigate:**
- All uses of `update_application_label`
- Application matching in update handlers

### 8. Permission/Role Violation

**Hypothesis:** The host's role or permissions are being checked incorrectly, causing updates to be rejected.

**Check:**
- Review `ensure!(self.config.role_request.can_write(), ...)` checks
- Verify host is properly configured as `Role::Owner`
- Check if role is being validated during update processing

**Locations to investigate:**
- `publish_update` role checks
- `RolePolicy::owner_only` configuration

### 9. Document State Lock Poisoning

**Hypothesis:** Document locks are being poisoned or held indefinitely, preventing updates.

**Check:**
- Look for panic conditions that could poison locks
- Review lock acquisition patterns
- Check for deadlocks or long-held locks

**Locations to investigate:**
- All `document.lock()` calls
- Error handling around lock acquisition

### 10. Network Layer Dropping Messages

**Hypothesis:** Updates are being published but lost in the network/broadcast layer.

**Check:**
- Review broadcast channel capacity
- Check for lagged receiver detection
- Look at network buffer sizes

**Locations to investigate:**
- `LiveUpdateHub` broadcast channel configuration
- Receiver loop error handling
- Channel capacity constants

### 11. Granular Mutation Conversion Bug

**Hypothesis:** The conversion from Db8 mutations to granular mutations loses critical information.

**Check:**
- Review `db8_source_mutations_to_granular` implementation
- Check if all mutation types are properly converted
- Verify metadata is preserved

**Locations to investigate:**
- Mutation conversion functions
- `GranularSourceMutation` vs `Db8CollabSourceMutation` differences

### 12. Frontier/Version Vector Mismatch

**Hypothesis:** Document frontiers are out of sync, causing updates to be rejected as stale.

**Check:**
- Review frontier comparison logic
- Check if frontier is properly updated after applying mutations
- Look for frontier serialization issues

**Locations to investigate:**
- `last_known_frontier` handling in presence messages
- Frontier validation in update processing

---

## Testing Strategy After Fix

1. **Unit Test:** Verify `publish_update` creates LiveUpdate with correct session_id
2. **Integration Test:** Host typing should appear in host's own UI
3. **Bidirectional Test:** Both host and client edits should sync both ways
4. **Large Edit Test:** Paste large content blocks to test mutation batching
5. **Rapid Edit Test:** Type quickly to test queue coalescing
6. **Stress Test:** Multiple clients with concurrent edits

---

## Why This Started Recently

**Hypothesis:** The filtering logic at `documents.rs:2046` was likely added or modified in a recent change to fix a different issue (possibly echo prevention or infinite update loops), but it inadvertently broke host local updates.

**Investigation:** Run `git blame` on line 2046 to see when this condition was added and review the associated commit message/PR.
