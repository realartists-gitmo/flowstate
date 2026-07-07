# Raw-authority CI guard

`check_raw_authority.sh` enforces invariant 6 of `flowstate_loro_first_spec.md` (§13.2):
after the Loro-first cutover, production code must never reference the condemned
raw projection-space authority surfaces (semantic-command batching, projection
replay/rebase, pending-edit flushing, stale-projection reconciliation), and the
local write path (`crates/flowstate-collab/src/local_write/`) must never grow a
frontier-validity concept (`base_frontier` there is banned; the legitimate
`ProjectionPatchBatch.base_frontier` patch-stream metadata elsewhere is not flagged).

## Run it

    tools/check_raw_authority.sh

It scans `crates/*/src` (excluding `vendor/`, `/tests/` paths, and `*tests.rs`
files), prints every offending `file:line`, and exits non-zero on any hit.

## When it fires

Never allowlist or suppress a hit. The fix is always to express the mutation
through the `LocalDocHandle` intent API so the CRDT document remains the sole
authority, then delete the raw-authority reference.

Note: this guard is wired into CI only after the stage-4 cutover completes;
until then, hits are expected while the migration is in progress.
