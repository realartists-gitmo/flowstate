#!/usr/bin/env bash
set -euo pipefail

repo="${1:-.}"

if [[ ! -d "$repo" ]]; then
  echo "error: repo path does not exist: $repo" >&2
  exit 2
fi

cd "$repo"

search_rs() {
  local title="$1"
  local pattern="$2"
  echo
  printf '== %s ==\n' "$title"
  if command -v rg >/dev/null 2>&1; then
    rg -n --glob '*.rs' "$pattern" . 2>/dev/null || true
  else
    grep -RInE --include='*.rs' "$pattern" . 2>/dev/null || true
  fi
}

search_any() {
  local title="$1"
  local pattern="$2"
  shift 2
  echo
  printf '== %s ==\n' "$title"
  if command -v rg >/dev/null 2>&1; then
    rg -n "$pattern" "$@" 2>/dev/null || true
  else
    grep -RInE "$pattern" "$@" 2>/dev/null || true
  fi
}

echo "== Rust runtime performance audit =="
printf 'repo: %s\n' "$(pwd)"

if command -v rustc >/dev/null 2>&1; then
  printf 'rustc: %s\n' "$(rustc --version)"
fi
if command -v cargo >/dev/null 2>&1; then
  printf 'cargo: %s\n' "$(cargo --version)"
fi

echo
echo "== Cargo targets =="
if [[ -f Cargo.toml ]] && command -v cargo >/dev/null 2>&1; then
  cargo metadata --no-deps --format-version 1 2>/dev/null \
    | sed 's/},/},\
/g' \
    | grep -E '"name"|"kind"|"edition"|"manifest_path"' \
    || true
else
  echo "Cargo.toml not found or cargo unavailable"
fi

echo
echo "== Benchmarks =="
find benches -maxdepth 2 -type f 2>/dev/null | sort || true
if [[ -f Cargo.toml ]]; then
  grep -n '^\[\[bench\]\]' Cargo.toml 2>/dev/null || true
fi

search_any \
  "Benchmark and profiler artifacts (M01-M03)" \
  'criterion|iai|divan|flamegraph|perf|samply|dhat|heaptrack|cachegrind|hyperfine' \
  Cargo.toml benches tests examples .github scripts

search_rs \
  "Allocation and ownership candidates (A01-A16)" \
  'Vec::with_capacity|Vec::new|String::with_capacity|String::new|HashMap::new|HashSet::new|\.reserve(_exact)?\(|\.shrink_to_fit\(|\.clone\(\)|\.clone_from\(|\.to_owned\(\)|\.to_string\(\)|format!\(|Cow<|SmallVec|ArrayVec|ThinVec|Box<\[|into_boxed_slice|Rc<|Arc<|\.lines\(\)|read_line\('

search_rs \
  "Bounds, aliasing, unsafe, and SIMD candidates (L01-L08/C07/R06/S06)" \
  'get_unchecked|get_unchecked_mut|set_len|MaybeUninit|spare_capacity_mut|unsafe \{|unreachable_unchecked|assert_unchecked|std::arch|core::arch|std::simd|#\[target_feature|wrapping_|checked_|saturating_|overflowing_|NonZero'

search_rs \
  "Iterator and collection-shape candidates (L09-L16)" \
  '\.collect::<Vec|\.collect\(\)|\.extend\(|\.append\(|\.chain\(|\.filter_map\(|\.filter\(|\.map\(|\.chunks\(|\.chunks_exact\(|\.rchunks\(|\.chunks_mut\(|\.iter\(\)\.copied\(\)|for .+ in .*\.\.='

search_rs \
  "Type layout and copy candidates (T01-T10)" \
  'repr\(C\)|size_of|size_of_val|static_assertions|NonNull|NonZero|Box<\[|ThinVec|usize|u64|memcpy|copy_from_slice|clone_from_slice'

search_rs \
  "Hashing and map candidates (H01-H05/A14)" \
  'HashMap|HashSet|BuildHasher|RandomState|rustc_hash|FxHash|fnv|Fnv|ahash|AHash|nohash|ByteHash|derive\(.*Hash|#\[derive\(Hash\)'

search_rs \
  "IO, text, logging, and assertion candidates (I01-I05/G01-G02)" \
  'println!\(|print!\(|eprintln!\(|dbg!\(|assert!\(|debug_assert!\(|tracing::|log::|BufReader|BufWriter|stdout\(\)\.lock|stderr\(\)\.lock|stdin\(\)\.lock|read_until\(|read_to_string\(|flush\('

search_rs \
  "Stdlib, sync, wrapper, and parallelism candidates (S01-S06/G03-G05)" \
  '\.ok_or\(|\.map_or\(|\.unwrap_or\(|\.or\(|ok_or_else|map_or_else|unwrap_or_else|or_else|swap_remove|\.remove\(|\.retain\(|Mutex<|RwLock<|RefCell<|Arc<Mutex|parking_lot|rayon|crossbeam|clippy::disallowed_types'

search_rs \
  "Inlining, outlining, and visibility candidates (C01-C08)" \
  '#\[inline|#\[cold\]|#\[target_feature|pub fn|pub\(crate\) fn|pub\(super\) fn'

echo
printf '%s\n' \
  "== Notes ==" \
  "- Matches are candidates, not proof. Profiles prioritize; the vector sweep remains full-codebase." \
  "- Build and distribution settings are intentionally out of scope." \
  "- Use references/rust-performance-playbook.md for vector definitions." \
  "- Final reports must include the Indexed Optimization Summary covering every vector ID."
