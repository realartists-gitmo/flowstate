---
name: rust-performance-optimization
description: Diagnose and aggressively optimize Rust runtime performance from evidence. Use when Codex needs to speed up Rust code, investigate regressions, review runtime hot paths and full-codebase optimization vectors, reduce allocations or bounds checks, improve cache/layout behavior, interpret benchmark/profiling output, or choose safe versus unsafe runtime optimization tactics.
---

# Rust Performance Optimization

Use this skill to optimize Rust runtime performance aggressively and exhaustively from evidence. Preserve correctness first; optimize every applicable runtime vector; verify with benchmarks, tests, assembly, profiler output, allocation profiles, or type-size evidence before claiming wins.

Build and distribution settings are out of scope. Use the human-provided build and run command unchanged.

## Mandatory Indexed Optimization Summary

Every optimization engagement MUST end with an `Indexed Optimization Summary`.

- Use the stable vector IDs in `references/rust-performance-playbook.md#optimization-vector-index`.
- Enumerate every vector ID, including vectors that were not changed.
- For each vector, record: status (`implemented`, `investigated-no-change`, `not-applicable`, or `blocked/deferred`), evidence observed, and why it was or was not applied.
- Do not omit any vector. A terse `not-applicable: no HashMap/HashSet usage in affected code` is acceptable; silence is not.
- For diagnosis-only tasks, use the same index and mark actionable candidates separately from rejected vectors.

## Workflow

1. Establish the runtime target.
   - Identify the user-visible workload, benchmark, regression, full-codebase sweep, or function family.
   - Record the command, input size, toolchain, feature flags, CPU/OS constraints, and current timing or resource use. Do not alter build or distribution settings.
   - Prefer existing project benchmarks. If missing, add the smallest benchmark or scenario that exercises the target behavior.

2. Collect runtime facts.
   - Run `scripts/rust_perf_audit.sh <repo>` to summarize benchmark targets and runtime optimization-sensitive code.
   - Use profiler output when available: `perf`, `samply`, `flamegraph`, `heaptrack`, DHAT/dhat-rs, Cachegrind, `cargo instruments`, platform profilers, or project-specific tracing.
   - Measure allocations, type sizes, generated code, cache behavior, branch behavior, IO/syscall counts, and synchronization costs when they guide a runtime optimization.
   - Treat hot paths as prioritization leads, not boundaries. Continue through the full runtime vector index.

3. Sweep every runtime lane from the playbook.
   - Read `references/rust-performance-playbook.md` and use its vector index as the coverage checklist.
   - Cover each section explicitly:
     - Algorithm/data-structure changes, cache behavior, branch prediction, lazy work, special-casing common inputs, compact representations, and local caches.
     - Heap allocations: `Vec`, `String`, `HashMap`/`HashSet`, `Box`, `Rc`/`Arc`, `clone`, `to_owned`, `Cow`, workhorse collections, line reading, and allocation regression tests.
     - Bounds checks, aliasing, iterator shape, chunking, `collect`/`extend`, `size_hint`, slice APIs, and safe range proofs.
     - Type layout and copying: enum variants, smaller integers, boxed slices, `ThinVec`, niches, `repr(C)`, 128-byte copy threshold, and static size assertions.
     - Standard-library runtime paths: hashing, IO buffering/locking, UTF-8 vs bytes, `Option`/`Result` eager defaults, `Vec` removal/retention, zero-filled vectors, wrapper types, and synchronization types.
     - Inlining, outlining, function visibility, generated machine code, SIMD/intrinsics, arithmetic semantics, and floating-point semantics.
     - Logging/debugging overhead, hot assertions, Clippy performance lints, and disallowed-type guardrails.

4. Inspect generated code when it can expose a runtime win.
   - Use `cargo asm`, `cargo-show-asm`, `cargo llvm-ir`, `cargo rustc -- --emit=asm`, `-Zprint-type-sizes`, `top-type-sizes`, or Compiler Explorer-style minimal examples.
   - Look for repeated bounds checks, panic paths inside loops, missed vectorization, redundant `memset`/`memcpy`, unnecessary allocation, missed inlining, large copies, and calls through slow stdlib paths.

5. Apply aggressive correct runtime optimizations.
   - Exhaust safe, idiomatic rewrites before unsafe code: better algorithms, slices over containers, iterator `zip`/internal iteration, reslicing, common safe ranges, `BufReader`/`BufWriter`, preallocation, collection reuse, `MaybeUninit` APIs, layout-conscious types, and better stdlib methods.
   - Use crates when they are the right runtime tool and fit project constraints: e.g. `smallvec`, `arrayvec`, `thin-vec`, `smartstring`, `rustc-hash`, `fnv`, `ahash`, `nohash-hasher`, `parking_lot`, `rayon`, `crossbeam`, `bstr`, or type-size/static assertion crates.
   - Use unsafe for isolated runtime-critical paths when safe code cannot express the needed invariant. Document the invariant locally and test it.
   - Avoid benchmark gaming: do not remove work, reduce input fidelity, or optimize only the benchmark harness.

6. Verify.
   - Run correctness tests first.
   - Rerun the same benchmark enough times to see noise. Use `hyperfine`, Criterion, `cargo bench`, profiler deltas, DHAT allocation deltas, or project-specific benchmarking. Compare against the baseline command.
   - Report absolute numbers, percent change, confidence/noise caveats, and files/functions changed.
   - Include the mandatory `Indexed Optimization Summary` covering every vector ID.

## Rust-Specific Reminders

- Use the build and run command supplied by the project or human; build and distribution settings are outside this skill.
- Treat compiler-version effects as real. Bounds-check elimination, vectorization, copy elision, inlining, and stdlib implementations can change non-monotonically across Rust/LLVM versions.
- Iterators can remove bounds checks and expose lengths, but long chains, `chain`, external iteration, debug builds, or aliasing through captured references can regress; inspect and benchmark.
- Unsafe can block alias analysis via raw pointers or introduce UB; verify generated code and benchmark.
- Crate replacements require workload verification. Faster hashers, `parking_lot`, small-vector/string crates, SIMD, and parallelism must be benchmarked on representative workloads.

## Bundled Resources

- `references/rust-performance-playbook.md`: comprehensive runtime optimization vector index, tactics, caveats, and reporting requirements.
- `scripts/rust_perf_audit.sh`: non-mutating repository audit for benchmarks and suspicious runtime optimization-sensitive patterns.
