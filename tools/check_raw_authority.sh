#!/usr/bin/env bash
#
# check_raw_authority.sh — CI guard for the Loro-first cutover
# (flowstate_loro_first_spec.md §13.2, invariant 6).
#
# WHY: Under the ratified Loro-first architecture, raw projection-space
# authority is unrepresentable by law — all local mutation must flow through
# the LocalDocHandle intent API so the CRDT document is the single source of
# truth. The identifiers banned below are the condemned raw-authority surface:
# semantic-command batching, projection replay/rebase, pending-edit flushing,
# and stale-projection reconciliation. Once the stage-4 cutover completes,
# production code must never reference them again; this guard stops silent
# reintroduction. Additionally, `base_frontier` is banned specifically inside
# crates/flowstate-collab/src/local_write/ — the write path must not grow a
# frontier-validity concept (ProjectionPatchBatch.base_frontier elsewhere is
# legitimate patch-stream ordering metadata and is NOT flagged).
#
# NOTE: While the cutover is in progress this script is expected to report
# hits. It gets wired into CI at cutover completion (after stage 4).

set -u

ROOT="$(cd "$(dirname "$0")/.." && pwd)"

BANNED_IDENTIFIERS='SemanticEditCommand|SemanticCommandBatch|apply_editor_commands|apply_editor_transaction|pending_semantic_edits|take_pending_session_edits|take_pending_runtime_edits|replace_document_projection_replaying_pending|rebuild_visible_from_committed|replay_semantic_command_on_projection|StaleProjectionError|StableEditorSelection|flush_local_edits|flush_document_runtime_edits|semantic_commands_for_span_edit'

fail=0
hits_file="$(mktemp)"
trap 'rm -f "$hits_file"' EXIT

# --- Pass 1: globally banned identifiers in production source -----------------
# Scope: crates/*/src, Rust sources only. Excluded: vendor/ directories, any
# path containing /tests/, and files ending in `tests.rs` (which also covers
# `_tests.rs`).
grep -rnE --include='*.rs' --exclude-dir=vendor "$BANNED_IDENTIFIERS" \
    "$ROOT"/crates/*/src 2>/dev/null \
    | grep -vE '(/tests/|tests\.rs:)' >> "$hits_file" || true

# --- Pass 2: base_frontier banned only inside the local write path ------------
LOCAL_WRITE_DIR="$ROOT/crates/flowstate-collab/src/local_write"
if [ -d "$LOCAL_WRITE_DIR" ]; then
    # commit.rs legitimately constructs ProjectionPatchBatch { base_frontier, .. }
    # (patch-STREAM ordering metadata, spec §7) — everything else in the write
    # path must not grow a frontier-validity concept.
    grep -rnE --include='*.rs' --exclude-dir=vendor 'base_frontier' \
        "$LOCAL_WRITE_DIR" 2>/dev/null \
        | grep -vE '(/tests/|tests\.rs:|/commit\.rs:)' >> "$hits_file" || true
fi

# --- Report --------------------------------------------------------------------
if [ -s "$hits_file" ]; then
    echo "FORBIDDEN raw-authority references found in production source:" >&2
    echo >&2
    cat "$hits_file" >&2
    echo >&2
    count="$(wc -l < "$hits_file" | tr -d ' ')"
    echo "check_raw_authority: FAIL — $count offending line(s)." >&2
    echo "Do not allowlist. Migrate the code to the LocalDocHandle intent API" >&2
    echo "(see flowstate_loro_first_spec.md §13.2, invariant 6, and" >&2
    echo "tools/README-raw-authority-guard.md)." >&2
    fail=1
else
    echo "check_raw_authority: OK — no raw-authority references in production source."
fi

exit "$fail"
