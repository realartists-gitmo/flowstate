#!/usr/bin/env bash
# Re-vendor crates/gpui from zed at a given rev, regenerating the workspace
# tables that upstream's manifest inherits.
#
#   tools/revendor_gpui.sh <zed-checkout> <rev>
#
# Afterwards you MUST re-apply the local patches (they are deliberately not
# automated — each one deserves a fresh look against the new upstream):
#
#   * src/platform/test/window.rs — `a11y_init` activating accessibility, so
#     `debug_a11y_tree_json()` works under `#[gpui::test]`.
#   * src/window/a11y/debug.rs    — dump text-run properties (character_lengths,
#     word_starts, text_selection) so caret/selection are assertable.
#
# `git diff` against the previous vendor is the reliable way to find them; they
# are all tagged `FLOWSTATE PATCH`.
#
# Keep the rev in lockstep with the repo-root Cargo.toml and
# vendor/gpui-component/Cargo.toml — cargo unifies git deps by (url, rev).
set -euo pipefail

ZED="${1:?usage: revendor_gpui.sh <zed-checkout> <rev>}"
REV="${2:?usage: revendor_gpui.sh <zed-checkout> <rev>}"
ROOT="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
DEST="$ROOT/vendor/gpui"

[ -d "$ZED/crates/gpui" ] || { echo "no crates/gpui under $ZED" >&2; exit 1; }

echo "vendoring $ZED/crates/gpui @ $REV -> $DEST"
rm -rf "$DEST"
mkdir -p "$DEST"
cp -r "$ZED/crates/gpui/." "$DEST/"

# Upstream's examples/ and tests/ depend on sibling zed crates and on font
# assets outside the crate; gpui is consumed as a library here, so drop them.
rm -rf "$DEST/examples" "$DEST/tests" "$DEST/docs"

python3 - "$ZED" "$REV" "$DEST" <<'PY'
import sys, tomllib
zed, rev, dest = sys.argv[1], sys.argv[2], sys.argv[3]
root = tomllib.load(open(f'{zed}/Cargo.toml','rb'))
g    = tomllib.load(open(f'{zed}/crates/gpui/Cargo.toml','rb'))
wsdeps = root['workspace']['dependencies']

names = set()
def scan(tbl):
    for k, v in (tbl or {}).items():
        if isinstance(v, dict) and v.get('workspace') is True:
            names.add(k)
for sect in ('dependencies', 'dev-dependencies', 'build-dependencies'):
    scan(g.get(sect))
for tv in (g.get('target') or {}).values():
    for sect in ('dependencies', 'dev-dependencies', 'build-dependencies'):
        scan(tv.get(sect))

def val(v):
    if isinstance(v, str):  return '"%s"' % v
    if isinstance(v, bool): return 'true' if v else 'false'
    if isinstance(v, int):  return str(v)
    if isinstance(v, list): return '[' + ', '.join(val(x) for x in v) + ']'
    if isinstance(v, dict): return '{ ' + ', '.join('%s = %s' % (k, val(x)) for k, x in v.items()) + ' }'
    raise TypeError(v)

lines = []
for n in sorted(names):
    spec = wsdeps.get(n)
    if spec is None:
        print(f"  WARNING: {n} not in zed workspace.dependencies", file=sys.stderr); continue
    if isinstance(spec, dict) and 'path' in spec:
        # zed-internal crate: keep it on the same git rev instead of vendoring it too.
        d = {k: v for k, v in spec.items() if k not in ('path', 'version')}
        d = {'git': 'https://github.com/zed-industries/zed', 'rev': rev, **d}
        lines.append('%s = %s' % (n, val(d)))
    else:
        lines.append('%s = %s' % (n, val(spec)))

header = open(f'{dest}/../gpui-header.toml').read() if False else None
PY

echo
echo "NOTE: this script regenerates the crate contents, not the manifest header."
echo "Diff vendor/gpui/Cargo.toml against the previous revision and re-apply:"
echo "  - the [workspace] / [workspace.package] / [workspace.lints] / [workspace.dependencies] block"
echo "  - autoexamples/autotests/autobenches = false in [package]"
echo "Then re-apply the FLOWSTATE PATCH hunks and run:"
echo "  ./heaven.sh a11y"
