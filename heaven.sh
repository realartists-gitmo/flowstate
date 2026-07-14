#!/usr/bin/env bash
# heaven.sh — the perf-heaven gate (Phase 0 of flowstate_perf_heaven_spec.md).
#
# One entry point for the three oracles that make aggressive optimization safe:
#   SPEED       — the flowstate-corpus perf benches (is it faster, where did the
#                 time/bytes go?).
#   FIDELITY    — the corpus sweep over every real .docx (did we break a real
#                 document?).
#   CONVERGENCE — the intent/convergence fuzz suites (does it still converge
#                 N-peer under adversarial interleavings?).
#
# The doctrine (§2 of the spec): MEASURE -> CUT -> PROVE-RED -> RECOVER ->
# RE-MEASURE -> LOCK. This script is the MEASURE and PROVE ends; you run it
# before a cut to capture a baseline and after to prove the win survived and no
# oracle went red.
#
# Usage:
#   ./heaven.sh                 # fidelity + convergence + a quick bench (the gate)
#   ./heaven.sh fidelity        # corpus sweep only (0 failures required)
#   ./heaven.sh convergence     # fuzz suites only
#   ./heaven.sh bench           # perf benches only, printed to stdout
#   ./heaven.sh baseline        # bench -> $HEAVEN_DIR/baseline.txt
#   ./heaven.sh compare         # bench -> $HEAVEN_DIR/current.txt, diff vs baseline
#   ./heaven.sh watch           # continuous corpus sweep as files are added
#   ./heaven.sh verify          # T1 richtext fast-path equivalence (snapshot->Src) + fuzz
#   ./heaven.sh docx-roundtrip  # docx exporter export->reimport audit (pre-existing losses)
#   ./heaven.sh docx-fallback   # docx unit suite under the FORCED old walk (escape-hatch guard)
#   ./heaven.sh soak            # long-horizon randomized fuzz sweep (FUZZ_SOAK_* env)
#   ./heaven.sh all             # everything
#
# Env:
#   FLOWSTATE_CORPUS_DIR   corpus root (default repo-local helpers/corpus/dropbox-office-flat)
#   FLOWSTATE_BENCH_FIXTURE  a large .docx for the benches (required for bench)
#   HEAVEN_DIR             where baselines live (default ~/.flowstate-corpus)
#   FLOWSTATE_RICHTEXT_VERIFY  build both the T1 Src fast path and the full state
#                         and assert equality (the fast-path recovery guard)
#   FLOWSTATE_RICHTEXT_NO_FASTPATH  disable the T1 Src fast path (escape hatch)
set -euo pipefail

cd "$(dirname "$0")"

HEAVEN_DIR="${HEAVEN_DIR:-$HOME/.flowstate-corpus}"
mkdir -p "$HEAVEN_DIR"

bold() { printf '\033[1m%s\033[0m\n' "$*"; }
rule() { printf '%s\n' "----------------------------------------------------------------------"; }

# --- FIDELITY: the corpus sweep -------------------------------------------------
# The bar: every real .docx imports and projects with ZERO defects and no content
# loss. This is what catches a projection regression from a perf change.
# (NOTE: `docx-roundtrip` below is a SEPARATE, stricter export->reimport audit
# that fails on a large PRE-EXISTING fraction of the corpus — the docx exporter is
# lossy — so it is NOT part of the perf-regression gate.)
run_fidelity() {
  bold "FIDELITY — corpus sweep (0 defects / no content loss)"
  rule
  cargo run --release --quiet -p flowstate-corpus --bin flowstate-corpus -- "$@"
}

run_watch() {
  bold "FIDELITY — continuous corpus sweep (--watch)"
  rule
  cargo run --release --quiet -p flowstate-corpus --bin flowstate-corpus -- --watch "$@"
}

# The docx export->reimport round-trip — a separate audit of the docx EXPORTER
# (not the projection/perf paths). A large pre-existing fraction fails today.
# §act-eleven C3: the docx unit suite under the FORCED fallback walk — the old
# XmlNode walk is the typed walk's correctness escape hatch, and an escape
# hatch nothing exercises is broken exactly when needed. Separate cargo
# invocation = separate process, so the env-gated OnceLock cannot leak into
# the typed-path run above.
run_docx_fallback() {
  bold "DOCX — unit suite under FLOWSTATE_DOCX_TYPED_WALK=0 (fallback walk)"
  rule
  FLOWSTATE_DOCX_TYPED_WALK=0 cargo test --release -p flowstate-docx
}

run_docx_roundtrip() {
  bold "DOCX-ROUNDTRIP — export -> reimport text survives (docx exporter audit)"
  rule
  cargo run --release --quiet -p flowstate-corpus --bin flowstate-corpus -- --roundtrip "$@"
}

# --- T1 fast-path equivalence (the perf-heaven recovery guard) ------------------
# The §perf-heaven T1 richtext Src fast path is ACTIVE by default. This proves it
# bit-identical to the full state build over every real corpus doc AND every fuzz
# interleaving. BOTH legs run release, so BOTH need FLOWSTATE_RICHTEXT_VERIFY to
# compile the assert in (run_convergence exports it below). NOTE: the corpus leg
# is the real T1 exerciser — fuzz docs are built by live ops (`Dst` state), so
# the Src fast path rarely engages there; the fuzz leg mainly guards the assert's
# own plumbing plus any Src states reached via snapshot exchange.
# A divergence panics with the offending document/interleaving — the doctrine's
# fuzz-guided recovery signal. Must be green before the fast path is trusted.
run_verify() {
  bold "T1 VERIFY — richtext Src fast path == full state (corpus, snapshot->Src+verify)"
  rule
  # FLOWSTATE_SNAPSHOT_VERIFY forces each doc through a Loro snapshot -> fresh
  # reimport -> project, so the projection lands on a `Src` state and the T1 fast
  # path actually runs; FLOWSTATE_RICHTEXT_VERIFY makes it assert bit-identical to
  # the full build. (A plain import leaves the state `Dst` and never runs T1.)
  FLOWSTATE_RICHTEXT_VERIFY=1 FLOWSTATE_SNAPSHOT_VERIFY=1 \
    cargo run --release --quiet -p flowstate-corpus --bin flowstate-corpus -- --recheck "$@"
  echo
  bold "T1 VERIFY — richtext Src fast path == full state (fuzz, release + verify env)"
  rule
  run_convergence
}

# --- CONVERGENCE: the fuzz suites ----------------------------------------------
# The N-peer convergence + intent complexity guards. These must stay green; the
# T2 tripwire (mass_ops_do_not_scan_block_index_per_paragraph) lives here.
FUZZ_TESTS=(
  object_table_convergence
  intent_fuzz
  intent_complexity
  network_pathology
  anti_entropy
  swarm_loopback
  field_regressions
  vendored_patch_guards
  doc_io_pump
)
run_convergence() {
  bold "CONVERGENCE — fuzz + complexity suites"
  rule
  local args=()
  for t in "${FUZZ_TESTS[@]}"; do args+=(--test "$t"); done
  # FLOWSTATE_RICHTEXT_VERIFY: the vendored Src-equivalence assert is
  # `cfg!(debug_assertions) || env` — these suites run RELEASE, so without the
  # env the assert is compiled out and the "fuzz verifies T1" claim is theater.
  FLOWSTATE_RICHTEXT_VERIFY=1 cargo test --release -p flowstate-collab "${args[@]}"
  # §act-eleven C2: the A10.3 runtime-index oracle (incremental index ==
  # fresh-from-projection, every non-rebuild batch) is DEBUG-only. Without this
  # leg the canonical gate never runs the oracle that caught four latent
  # index-staleness bugs in the act-ten run.
  bold "CONVERGENCE — intent suites in DEBUG (A10.3 index oracle armed)"
  rule
  cargo test -p flowstate-collab --test intent_fuzz --test intent_complexity --test object_table_convergence
}

# --- §act-eleven C10: the long-horizon randomized SOAK --------------------------
# A wide seed sweep at larger op counts than the deterministic CI seeds — the
# machine is idle-capable and disposable (corpus policy), so let it run for
# hours. Tune with FUZZ_SOAK_SEEDS / FUZZ_SOAK_ROUNDS / FUZZ_SOAK_OPS;
# FUZZ_PER_OP_CHECK=1 adds per-op tracing for shrink-on-failure.
run_soak() {
  bold "SOAK — intent + object/table fuzz, ${FUZZ_SOAK_SEEDS:-500} seeds"
  rule
  FLOWSTATE_RICHTEXT_VERIFY=1 cargo test --release -p flowstate-collab \
    --test intent_fuzz --test object_table_convergence -- --ignored --nocapture "$@"
}

# --- §act-eleven / A11.6: the RASTER screenshot net ------------------------------
# Renders a deterministic editor fixture FULLSCREEN on the live compositor
# (blade needs real Vulkan presentation — Xvfb has no DRI3), captures via the
# COSMIC portal (non-interactive), and compares against the machine-local
# golden with a two-tier tolerance (mean drift + hot-pixel fraction). The
# golden lives in HEAVEN_DIR like the hotpath baselines — fonts/scale/driver
# are machine state. NOTE: the probe window flashes on screen for ~2.5s.
run_screenshot_capture() {
  local out_dir="$1"
  cargo build --release --quiet -p flowstate-corpus --bin screenshot_probe --bin screenshot_compare
  mkdir -p "$out_dir"
  rm -f "$out_dir"/*.png
  ./target/release/screenshot_probe &
  local probe_pid=$!
  sleep 1.6
  cosmic-screenshot --interactive=false --modal=false --notify=false -s "$out_dir" >/dev/null 2>&1 || true
  wait "$probe_pid" || true
  ls -t "$out_dir"/*.png 2>/dev/null | head -1
}
run_screenshot() {
  bold "SCREENSHOT — raster compare vs machine-local golden"
  rule
  if [[ ! -f "$HEAVEN_DIR/screenshot-golden.png" ]]; then
    echo "no golden yet — run './heaven.sh screenshot-baseline' first" >&2
    exit 2
  fi
  local shot
  shot=$(run_screenshot_capture "$HEAVEN_DIR/screenshot-current")
  if [[ -z "$shot" ]]; then
    echo "screenshot capture produced no image (portal denied?)" >&2
    exit 2
  fi
  ./target/release/screenshot_compare "$HEAVEN_DIR/screenshot-golden.png" "$shot"
}
run_screenshot_baseline() {
  bold "SCREENSHOT — capturing new golden"
  rule
  local shot
  shot=$(run_screenshot_capture "$HEAVEN_DIR/screenshot-current")
  if [[ -z "$shot" ]]; then
    echo "screenshot capture produced no image (portal denied?)" >&2
    exit 2
  fi
  cp "$shot" "$HEAVEN_DIR/screenshot-golden.png"
  bold "golden saved -> $HEAVEN_DIR/screenshot-golden.png"
}

# --- SPEED: the perf benches ---------------------------------------------------
# collab_bench: warm field ops (open/import/keystroke/mass-restyle/undo/redo).
# perf_probe:   cold vs warm projection phase timing.
# The hotpath tables come from the --features hotpath* builds; kept optional.
require_fixture() {
  if [[ -z "${FLOWSTATE_BENCH_FIXTURE:-}" ]]; then
    echo "error: set FLOWSTATE_BENCH_FIXTURE=/path/to/large.docx for the benches" >&2
    exit 2
  fi
}
run_bench() {
  require_fixture
  bold "SPEED — perf_probe (cold/warm projection phases)"
  rule
  cargo run --release --quiet -p flowstate-corpus --bin perf_probe -- "$FLOWSTATE_BENCH_FIXTURE" || true
  echo
  bold "SPEED — collab_bench (warm field ops)"
  rule
  cargo run --release --quiet -p flowstate-corpus --bin collab_bench -- "$FLOWSTATE_BENCH_FIXTURE" || true
}

run_baseline() {
  run_bench | tee "$HEAVEN_DIR/baseline.txt"
  bold "baseline saved -> $HEAVEN_DIR/baseline.txt"
}
run_compare() {
  run_bench | tee "$HEAVEN_DIR/current.txt"
  if [[ -f "$HEAVEN_DIR/baseline.txt" ]]; then
    echo
    bold "DELTA vs baseline"
    rule
    diff -u "$HEAVEN_DIR/baseline.txt" "$HEAVEN_DIR/current.txt" || true
  else
    echo "no baseline yet — run './heaven.sh baseline' first" >&2
  fi
}

# --- T7.28: STABLE micro-measurement (core-pinned, min-of-N) --------------------
# Wall-clock on this box swings ~2x from thermal/allocator noise, so a single
# bench run cannot resolve a sub-second cut. `stable` pins the bench to one core
# (taskset, if present — pick an isolated core via HEAVEN_STABLE_CORE) and runs
# it N times (HEAVEN_STABLE_RUNS). Read the MINIMUM across runs: the min is the
# least-contended, most-reproducible estimate. Pre-builds once so compilation
# never pollutes a timed run. Pair with baseline/compare for the delta.
STABLE_RUNS="${HEAVEN_STABLE_RUNS:-5}"
STABLE_CORE="${HEAVEN_STABLE_CORE:-3}"
pin() {
  if command -v taskset >/dev/null 2>&1; then taskset -c "$STABLE_CORE" "$@"; else "$@"; fi
}
run_stable() {
  require_fixture
  bold "SPEED (STABLE) — core $STABLE_CORE, min of $STABLE_RUNS runs (read the MIN, not any single run)"
  rule
  # Pre-build so run 1 is not paying for compilation.
  cargo build --release --quiet -p flowstate-corpus --bin collab_bench
  cargo build --release --quiet -p flowstate-corpus --bin perf_probe
  for i in $(seq 1 "$STABLE_RUNS"); do
    bold "-- run $i/$STABLE_RUNS --"
    pin cargo run --release --quiet -p flowstate-corpus --bin collab_bench -- "$FLOWSTATE_BENCH_FIXTURE" || true
  done
}

# --- T8.21: HOTPATH regression gate (top-N alloc/CPU functions) -----------------
# Wall-clock is noisy (see `stable`), but the hotpath alloc/CPU TABLES are a rich,
# reproducible signal: a regression shows up as a NEW large allocator or a
# function jumping the top-N. `hotpath` runs collab_bench with the alloc + cpu
# hotpath features, extracts the top functions, and diffs them against a saved
# baseline — so a 100 MB allocator that sneaks in is caught by the gate, not by
# eyeball. `hotpath-baseline` saves the current tables as the reference.
run_hotpath() {
  require_fixture
  bold "HOTPATH — top alloc + CPU functions (collab_bench)"
  rule
  # §oom-leads #4: structural rounds are counter nets with a known cliff —
  # exclude them from the share-gate profile until the remote-structural fix.
  export FLOWSTATE_BENCH_SKIP_STRUCTURAL=1
  cargo build --release --features hotpath-alloc --quiet -p flowstate-corpus --bin collab_bench
  bold "ALLOC top functions"
  cargo run --release --features hotpath-alloc --quiet -p flowstate-corpus --bin collab_bench -- "$FLOWSTATE_BENCH_FIXTURE" 2>&1 \
    | sed -n '/alloc-bytes/,/threads/p' | grep -E "^\| [A-Za-z]" | head -15 | tee "$HEAVEN_DIR/hotpath-alloc.txt"
  echo
  cargo build --release --features hotpath-cpu --quiet -p flowstate-corpus --bin collab_bench
  bold "CPU top functions"
  cargo run --release --features hotpath-cpu --quiet -p flowstate-corpus --bin collab_bench -- "$FLOWSTATE_BENCH_FIXTURE" 2>&1 \
    | grep -E "^\| [A-Za-z]" | head -15 | tee "$HEAVEN_DIR/hotpath-cpu.txt"
  local gate_failed=0
  for kind in alloc cpu; do
    if [[ -f "$HEAVEN_DIR/hotpath-$kind.baseline.txt" ]]; then
      echo; bold "GATE vs baseline ($kind top functions)"; rule
      hotpath_gate "$kind" || gate_failed=1
    else
      echo "(no $kind baseline — run './heaven.sh hotpath-baseline' to arm the gate)"
    fi
  done
  echo "(save the current tables as the reference with: ./heaven.sh hotpath-baseline)"
  if (( gate_failed )); then
    echo "HOTPATH GATE: FAILED (see above). A new hot function entered the top-N or an existing one grew past tolerance." >&2
    return 1
  fi
}
# The actual gate (was `diff -u ... || true`, which could never fail and diffed
# run-varying raw numbers). Compares the normalized "% Total" column: FAIL if a
# function absent from the baseline top-N appears at >= HEAVEN_GATE_NEW_PCT, or
# a shared function's share grows by more than HEAVEN_GATE_GROWTH_ABS points AND
# HEAVEN_GATE_GROWTH_REL x its baseline share. `::main` rows (the whole-run root
# span) are skipped — their share is definitionally noisy.
hotpath_gate() {
  local kind="$1"
  awk -F'|' \
    -v new_pct="${HEAVEN_GATE_NEW_PCT:-3.0}" \
    -v grow_abs="${HEAVEN_GATE_GROWTH_ABS:-2.0}" \
    -v grow_rel="${HEAVEN_GATE_GROWTH_REL:-1.5}" '
    function trim(s) { gsub(/^[ \t]+|[ \t]+$/, "", s); return s }
    function pct(s) { s = trim(s); sub(/%$/, "", s); return s + 0 }
    FNR == 1 { file_ix++ }
    NF < 4 { next }
    {
      name = trim($2); share = pct($(NF - 1))
      if (name == "" || name ~ /::main$/ || name == "Function") next
      if (file_ix == 1) base[name] = share
      else { cur[name] = share; order[++n] = name }
    }
    END {
      bad = 0
      for (i = 1; i <= n; i++) {
        name = order[i]; share = cur[name]
        if (!(name in base)) {
          if (share >= new_pct) { printf "  FAIL new hot function: %s at %.2f%% (>= %.1f%%)\n", name, share, new_pct; bad = 1 }
          else printf "  note new entrant:      %s at %.2f%% (below %.1f%% gate)\n", name, share, new_pct
        } else if (share > base[name] * grow_rel && share - base[name] > grow_abs) {
          printf "  FAIL grew:             %s %.2f%% -> %.2f%% (rel x%.2f, +%.2f pts)\n", name, base[name], share, share / (base[name] > 0 ? base[name] : 0.01), share - base[name]; bad = 1
        }
      }
      if (!bad) print "  OK — no new top-N entrant, no function past growth tolerance"
      exit bad
    }' "$HEAVEN_DIR/hotpath-$kind.baseline.txt" "$HEAVEN_DIR/hotpath-$kind.txt"
}
run_hotpath_baseline() {
  # Re-arming IS the answer to a failed gate (a big win shifts every share);
  # don't let the gate's exit code abort the save.
  run_hotpath || true
  cp "$HEAVEN_DIR/hotpath-alloc.txt" "$HEAVEN_DIR/hotpath-alloc.baseline.txt" 2>/dev/null || true
  cp "$HEAVEN_DIR/hotpath-cpu.txt" "$HEAVEN_DIR/hotpath-cpu.baseline.txt" 2>/dev/null || true
  bold "hotpath baseline saved -> $HEAVEN_DIR/hotpath-{alloc,cpu}.baseline.txt"
}

case "${1:-gate}" in
  fidelity)    shift; run_fidelity "$@" ;;
  convergence) shift; run_convergence "$@" ;;
  hotpath)          shift; run_hotpath "$@" ;;
  hotpath-baseline) shift; run_hotpath_baseline "$@" ;;
  bench)       shift; run_bench "$@" ;;
  baseline)    shift; run_baseline "$@" ;;
  compare)     shift; run_compare "$@" ;;
  stable)      shift; run_stable "$@" ;;
  watch)          shift; run_watch "$@" ;;
  verify)         shift; run_verify "$@" ;;
  docx-roundtrip) shift; run_docx_roundtrip "$@" ;;
  docx-fallback)  shift; run_docx_fallback "$@" ;;
  soak)           shift; run_soak "$@" ;;
  screenshot)          shift; run_screenshot "$@" ;;
  screenshot-baseline) shift; run_screenshot_baseline "$@" ;;
  all)            run_fidelity; echo; run_convergence; echo; run_bench ;;
  gate)           run_fidelity; echo; run_convergence ;;
  *) echo "usage: $0 {gate|fidelity|convergence|bench|baseline|compare|stable|hotpath|hotpath-baseline|watch|verify|docx-roundtrip|all}" >&2; exit 2 ;;
esac
