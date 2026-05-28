# Rust Runtime Performance Playbook

Source basis: Yuri Gribov and Zakhar Akimov, "Performance of Rust language", May 2026, `https://github.com/yugr/rust-slides/blob/main/EN.pdf`, plus the integrated runtime-focused Rust performance book chapters staged in `new book for implement/`. Use this as an exhaustive runtime optimization checklist.

Build and distribution settings are out of scope. Use the human-provided build and run command unchanged.

## Mandatory Reporting Contract

Every optimization report MUST include an `Indexed Optimization Summary` that enumerates every vector in the index below. For each ID, record the status (`implemented`, `investigated-no-change`, `not-applicable`, or `blocked/deferred`), the evidence you observed, and the reason for applying or rejecting it. Do not omit any vector.

## Optimization Vector Index

Use these stable IDs for investigation notes and final summaries.

### Measurement and Full-Codebase Runtime Strategy

- **M01 Baseline workload**: exact command/input/toolchain/hardware/runtime-resource baseline using the supplied build/run configuration.
- **M02 Runtime configuration sanity**: feature flags, runtime inputs, environment variables, allocator behavior observed in profiles, CPU/OS constraints, and representative workload shape.
- **M03 Profiler attribution**: hot functions, allocation sites, cache misses, branch misses, IO/syscall hot spots, synchronization contention, and generated-code symptoms.
- **M04 Full vector sweep**: profiles prioritize the order of attack; the optimization pass still covers every runtime vector in this index.
- **M05 Algorithm/data structure**: asymptotic, representation, and access-pattern improvements.
- **M06 Reduce call frequency**: eliminate repeated calls, duplicate traversals, redundant conversions, and unnecessary layers around runtime work.
- **M07 Lazy/on-demand work**: skip expensive computation unless the result is needed.
- **M08 Common-case fast path**: specialize high-frequency simple cases, especially 0/1/2 element collections.
- **M09 Compact representation**: encode repetitive/common values compactly with fallback storage for uncommon values.
- **M10 Locality cache**: small cache in front of high-locality lookups.
- **M11 Profiling comments**: explain non-obvious optimized structure with measured frequencies or profile evidence.

### Allocation and Ownership

- **A01 Allocation profiling**: identify allocation rate, size, lifetime, and access frequency with DHAT/dhat-rs/heap profilers.
- **A02 Boxed cold/large fields**: box large enum/struct fields when reducing hot type size outweighs allocation cost.
- **A03 Rc/Arc economics**: use sharing to reduce copies; remove refcounted heap allocation from values that are not shared.
- **A04 Vec preallocation**: `with_capacity`, `reserve`, `reserve_exact` using measured length distributions.
- **A05 Short vectors**: `SmallVec` for many short vectors; `ArrayVec` when a hard maximum length is known.
- **A06 Boxed slices**: `Vec::into_boxed_slice`/collect to `Box<[T]>` for immutable vectors to remove capacity word and slack.
- **A07 ThinVec**: `thin-vec` for frequently empty vectors inside frequently instantiated types or enum variants.
- **A08 Reuse collections**: workhorse `Vec`/`String`/maps with `clear`, output parameters, or struct-owned scratch space.
- **A09 String capacity and small strings**: `String::with_capacity`, `smallstr`, or `smartstring` for measured small-string workloads.
- **A10 Avoid formatting allocation**: avoid `format!` when literals, `write!`, `format_args!`, or lazy formatting suffice.
- **A11 Clone control**: remove hot unnecessary clones; use `clone_from` to reuse existing allocations.
- **A12 Borrow instead of own**: replace hot `to_owned`/`to_string` with borrowed storage when lifetimes are correct.
- **A13 Cow**: `Cow` for mixed borrowed/owned or mostly-read clone-on-write data.
- **A14 Hash table capacity**: preallocate `HashMap`/`HashSet` like vectors when growth is predictable.
- **A15 Line-reading allocations**: replace `BufRead::lines` with reusable `String` plus `read_line` when a `&str` loop body works.
- **A16 Allocation regression tests**: dhat-rs heap tests or equivalent to prevent accidental allocation growth.

### Bounds Checks, Aliasing, and Iteration Shape

- **L01 Slice signatures**: runtime workers take `&[T]`/`&mut [T]`, not `&Vec<T>`/`&mut Vec<T>`.
- **L02 Common safe ranges**: compute shared lengths, reslice, assert preconditions, or access farthest fixed index once.
- **L03 Iterator/internal iteration**: replace indexing loops with `iter`, `iter_mut`, `zip`, `for_each`, `fold`, or `find` when it removes checks or exposes lengths.
- **L04 Exact chunking**: prefer `chunks_exact`/`chunks_exact_mut` and handle remainders explicitly.
- **L05 Simple index expressions**: replace complex affine indexing with offsets, zipped iterators, chunks, or enumerated iterators.
- **L06 Unsafe unchecked access**: `get_unchecked`, raw pointers, `assert_unchecked`, or `unreachable_unchecked` with local tested invariants.
- **L07 Noalias helper boundaries**: small helper functions expose separate slice/reference arguments and constants.
- **L08 Raw-pointer caution**: raw pointers can lose Rust reference aliasing metadata and fail to optimize.
- **L09 Avoid collect round trips**: return `impl Iterator` or consume iterators directly instead of collecting and then immediately iterating.
- **L10 Extend existing collections**: use `extend` rather than `collect::<Vec<_>>()` plus `append`.
- **L11 Iterator length hints**: implement `size_hint` or `ExactSizeIterator::len` where possible.
- **L12 Chain/filter shape**: replace hot `chain` with a single iterator where possible; use `filter_map` over `filter().map()` when equivalent.
- **L13 Copied small values**: `iter().copied()` can generate better code for small `Copy` items.
- **L14 Vec removal methods**: `swap_remove` for unordered O(1) deletion; `retain` for bulk removal.
- **L15 Zero-filled vectors**: prefer `vec![0; n]` for zero-filled vectors.
- **L16 Clippy ptr_arg**: heed `ptr_arg` and similar lints that make APIs slice-oriented.

### Type Layout, Copies, and Cache Behavior

- **T01 Measure type layout**: `size_of`, `-Zprint-type-sizes`, `top-type-sizes`, DHAT copy profiling.
- **T02 Large copy threshold**: types larger than 128 bytes are copied with `memcpy` in relevant generated code; shrink copied runtime types.
- **T03 Repr discipline**: remove `#[repr(C)]` from internal runtime types unless ABI/layout stability requires it.
- **T04 Smaller enum variants**: box rare outsized variants or fields when enum size dominates.
- **T05 Smaller stored integers**: store indices/counts as `u32`/`u16`/`u8` when ranges prove safe; cast at use points.
- **T06 Boxed-slice layout**: use `Box<[T]>` to store pointer+len instead of `Vec` pointer+len+capacity.
- **T07 ThinVec layout**: one-word vector handle for frequently empty fields.
- **T08 Niche-aware types**: `NonZero*`, references, `NonNull<T>`, and `Option<T>` can encode compactly.
- **T09 Hot/cold field placement**: group hot fields and split cold fields when cache/memory evidence points to layout pressure.
- **T10 Static size assertions**: protect runtime-critical type sizes with cfg-gated `static_assertions`.

### Hashing and Maps

- **H01 Faster hashers**: `rustc-hash`/Fx, `fnv`, or `ahash` for trusted-key workloads where HashDoS is not a concern.
- **H02 Compare hashers**: benchmark multiple hashers on representative keys and keep the fastest correct choice.
- **H03 No-hash integer keys**: `nohash-hasher` for already-random or identity-safe integer-like keys.
- **H04 Byte-wise hashing**: `zerocopy`/`bytemuck` byte hashing for padding-free types when derive-field hashing is hot.
- **H05 Disallow wrong map types**: Clippy `disallowed_types` after a project-standard map/hasher switch.

### IO, Text, and Bytes

- **I01 Stream locking**: manually lock stdout/stderr/stdin for repeated operations.
- **I02 Buffering**: `BufReader`/`BufWriter` for many small file, socket, stdin, stdout, or stderr operations.
- **I03 Explicit flush**: call `flush` when buffered-output errors must be observed.
- **I04 Raw bytes**: use `read_until`, byte slices, `bstr`, or byte-line crates when UTF-8 is unnecessary.
- **I05 Reusable line buffer**: same as A15, especially for input-heavy programs.

### Standard Library and Synchronization Choices

- **S01 Lazy Option/Result defaults**: `ok_or_else`, `map_or_else`, `unwrap_or_else`, `or_else` when fallback construction is expensive.
- **S02 Rc/Arc make_mut**: clone-on-write mutation through `Rc::make_mut`/`Arc::make_mut`.
- **S03 parking_lot**: `parking_lot` sync primitives when measured faster/smaller and semantics fit.
- **S04 Wrapper coalescing**: combine values under one `RefCell`, `Mutex`, `Arc<Mutex<_>>`, etc. when accessed together.
- **S05 Thread parallelism**: Rayon, Crossbeam, and atomics/locks when workload and architecture support parallel speedup.
- **S06 Data parallelism/SIMD**: explicit SIMD or architecture intrinsics for vectorizable loops.

### Inlining, Outlining, Codegen, and Machine Code

- **C01 Assembly/IR inspection**: inspect generated code for runtime questions.
- **C02 Inline small/single-call functions**: `#[inline]`/`#[inline(always)]` when it removes call overhead or enables optimization.
- **C03 Inlining verification**: verify inlining effects; keep wins and revert losses from instruction-cache pressure or inhibited nearby inlining.
- **C04 Split hot/cold call sites**: always-inlined hot wrapper plus never-inlined cold wrapper for large multi-callsite functions.
- **C05 Outline cold paths**: move rare work into `#[cold]` functions.
- **C06 Visibility tightening**: keep runtime helpers private or crate-local unless public API requires export.
- **C07 Target-specific code**: `target_feature`, `core::arch`, `std::simd`/portable SIMD require semantic and portability approval.
- **C08 Cachegrind inline evidence**: use event counts on function first/last lines to infer inlining in Cachegrind.

### Arithmetic and Floating Point

- **R01 Range/overflow structure**: exclusive ranges and overflow-aware loop bounds that do not inhibit vectorization.
- **R02 Explicit arithmetic semantics**: `wrapping_*`, `checked_*`, `saturating_*`, `overflowing_*`, or `Wrapping<T>`.
- **R03 NonZero divisors**: encode nonzero invariants to remove divide-by-zero checks.
- **R04 Cast correctness**: `try_from`/`try_into` for lossy casts unless truncation is intended.
- **R05 Overflow-check symptoms**: identify overflow checks in generated code and restructure arithmetic where semantics allow.
- **R06 Unsafe arithmetic**: `unchecked_*` requires airtight invariants and tests.
- **F01 IEEE default**: keep exact IEEE behavior unless numerical contract permits otherwise.
- **F02 Local fast math/SIMD**: nightly fast intrinsics, algebraic intrinsics, SIMD, or math crates for measured hotspots.
- **F03 Fast-math exclusions**: never rewrite NaN/Inf/signed-zero/rounding-sensitive or Kahan-style code casually.

### Logging, Assertions, and Linting

- **G01 Disabled logging work**: avoid preparing messages/data when logging/tracing/debugging is inactive.
- **G02 Hot assertions**: use `debug_assert!` instead of `assert!` after proving the assertion is not required for safety or correctness in normal runtime behavior.
- **G03 Clippy performance lints**: run and heed relevant Clippy perf suggestions.
- **G04 Clippy guardrails**: `disallowed_types` for forbidden stdlib replacements.
- **G05 Lint-driven API simplification**: non-perf lints such as `ptr_arg` can unlock optimizer-friendly code.

## Measurement Discipline

- Attribute runtime cost to workloads, functions, data structures, allocation sites, IO sites, synchronization points, and generated-code patterns before and after editing.
- Use standard project benchmarks (`cargo bench`, Criterion), profiler output, allocation profiles, type-size evidence, IO counters, and generated-code inspection.
- Stabilize measurements: fixed inputs, warmed caches, consistent CPU power profile, repeated runs, same toolchain/features, same supplied build/run configuration.
- Profiles determine first targets. The pass remains full-codebase and vector-complete.
- Verify correctness before and after every optimization.
- Different profilers expose different dimensions; use multiple tools when attribution is incomplete.
- When a function is costly, both reduce its own cost and eliminate avoidable calls to it.
- Accumulate small wins across the codebase; runtime improvements compound.
- Explain non-obvious optimized structure with comments that cite measured facts, e.g. `99% of inputs have <= 1 item`.

## General Runtime Tactics

- Apply algorithm, data-structure, representation, memory-layout, and low-level rewrites wherever they preserve behavior and improve runtime.
- Minimize cache misses and branch mispredictions where profiles or data layout show pressure.
- Remove obvious slowdowns, then continue into structural and generated-code wins.
- Avoid computing values until needed; use lazy/on-demand computation for fallbacks or diagnostics that are inactive in measured workloads.
- Check common simple cases before complex general handling, especially collections with 0, 1, or 2 elements when those dominate.
- For repetitive data, use compact encodings for common values plus fallback tables for unusual values when memory/cache profiles justify it.
- Measure case frequencies and order cases by runtime frequency when branch prediction or instruction-cache pressure is relevant.
- Place small caches in front of lookup-heavy data structures when locality is high and invalidation is simple.

## Runtime Tooling Checklist

Useful commands and tools:

- `cargo bench`
- Criterion benchmarks
- `hyperfine --warmup 3 '<command>'`
- `cargo flamegraph --bench <bench>`
- `perf record --call-graph=dwarf <command>` then `perf report`
- `samply`, `heaptrack`, DHAT/dhat-rs, Cachegrind, `cargo instruments`, and project-specific tracing where available.
- `cargo asm <path::to::fn>`, `cargo show-asm`, `cargo llvm-ir <path::to::fn>`, `cargo rustc -- --emit=asm`.
- `-Zprint-type-sizes` or `top-type-sizes` for type-size inspection.
- Compiler Explorer for small generated-code experiments.

Inspect:

- Runtime feature flags and inputs.
- Allocation counts, allocation sizes, allocation lifetimes, and retained capacities.
- Type sizes, alignment, enum variant sizes, field padding, and hot copy sites.
- Bounds checks, panic paths, missed vectorization, `memset`/`memcpy`, and call boundaries in generated code.
- IO buffering/locking and syscall frequency.
- Hashing, synchronization, logging, assertions, and UTF-8 validation in runtime traces.

## Heap Allocations and Ownership

### Allocation facts and profiling

- Heap allocation/deallocation involves allocator data structures, synchronization, and sometimes syscalls. Small allocations still carry allocator overhead.
- If `malloc`, `free`, allocator frames, or allocator locks are hot, reduce allocation rate, allocation size, and lifetime pressure in code.
- DHAT identifies allocation sites, rates, sizes, lifetimes, and access rates. In rustc experience, reducing allocation rate by about 10 allocations per million instructions produced measurable improvements.
- Use dhat-rs heap usage tests to prevent allocation regressions when allocation count or size is part of the runtime contract.

### Box, Rc, and Arc

- `Box<T>` puts one `T` on the heap. Use it to shrink containing types, especially rare large enum variants.
- `Rc<T>`/`Arc<T>` store a heap value plus reference counts. Use sharing to reduce copies; remove refcounting where values are not actually shared.
- `Rc::clone`/`Arc::clone` do not allocate; they increment counts. Inner mutation can use `Rc::make_mut`/`Arc::make_mut` for clone-on-write.

### Vec growth and capacity

- `Vec<T>` is pointer, length, capacity. Elements are heap-allocated when capacity and element size are nonzero.
- Empty `Vec::new`, `Vec::default`, and `vec![]` allocate nothing.
- Growth strategy is unspecified; currently it quasi-doubles and jumps 0 -> 4 -> 8 -> 16 -> ... for common cases. Reallocations become less frequent but slack capacity grows.
- Measure length distributions at allocation sites and choose the representation from the distribution.
- Use `Vec::with_capacity`, `reserve`, or `reserve_exact` when minimum/exact/maximum lengths are known. Preallocating 20 items avoids repeated growth through 4/8/16/32.
- `shrink_to_fit` reduces wasted capacity and can reallocate; keep it when retained memory pressure improves runtime.
- `HashMap`/`HashSet` have analogous capacity APIs and contiguous backing allocations.

### Short vectors and immutable vectors

- `SmallVec<[T; N]>` stores up to `N` elements inline, then falls back to heap. Use it when many vectors fit inline; benchmark operation overhead and copy cost.
- Replace `vec![]` literals with `smallvec![]` where switching to `SmallVec`.
- `ArrayVec<T, N>` removes heap fallback when the maximum length is known exactly; benchmark and keep wins.
- `Vec::into_boxed_slice` converts future-immutable vectors to pointer+len and drops capacity slack; account for any conversion reallocation. Collecting directly into `Box<[T]>` avoids reallocation when iterator length is known.
- `slice::into_vec` converts a boxed slice back to `Vec` without clone or reallocation.
- `ThinVec<T>` stores length/capacity in the allocation and has a one-word handle. Use for frequently empty vectors in frequently instantiated types or enum variants.

### Strings and formatting

- `String` has `Vec<u8>`-like allocation behavior; use `String::with_capacity` when output length is predictable.
- `smallstr::SmallString` is small-vector-like for strings.
- `smartstring::SmartString` avoids heap allocation for strings under three machine words; on 64-bit platforms this includes ASCII strings up to 23 bytes.
- `format!` always returns a `String` and allocates. Prefer string literals, `write!` into an existing buffer, `format_args!`, or `lazy_format`.

### Cloning, owning, and borrowing

- Cloning non-empty heap-backed values such as `Vec` or `String` allocates; `Rc`/`Arc` clone increments counts without allocation.
- `clone_from` reuses existing allocations, e.g. cloning one vector into another with sufficient capacity.
- Remove hot unnecessary clones after proving ownership/lifetime semantics.
- `to_owned`, `to_string`, and related calls allocate borrowed data into owned data. Replace with borrowed storage when lifetimes are correct.
- `Cow<'a, T>` holds borrowed or owned data without forcing allocation for borrowed cases and can clone on write via `to_mut`. Use for mixed literal/formatted strings, `&[T]`/`Vec<T>`, and `&Path`/`PathBuf`.

### Reusing collections and buffers

- Modify caller-provided collections instead of returning repeated short-lived collections.
- Keep workhorse collections outside repeated loops and `clear` them between iterations; capacity is retained. Comment the measured reason.
- Struct-owned scratch collections amortize allocations across repeated method calls.
- For line-oriented input, `BufRead::lines` allocates one `String` per line. A reusable `String` with `read_line` reduces this to the reallocations required by line-length growth if the loop can consume `&str`.

## Bounds Checks, Aliasing, and Iteration

### Bounds-check symptoms

- Panic/bounds-check calls in loops.
- Missed vectorization around indexed slice or `Vec` access.
- Repeated checks where one precondition proves a whole loop safe.

### Safe bounds-check tactics

- Use slices rather than containers in runtime workers to simplify alias analysis and expose lengths.
- Compute a common safe range before iterating over multiple containers: `let n = x.len().min(y.len());`.
- Reslice once before a loop: `let xs = &xs[..n];` then iterate/index `xs`.
- Build offset slices in two steps to avoid overflow-obscured proofs: prefer `&v[i..][..n]` over `&v[i..i + n]`.
- Add precondition asserts that replace many checks with one, such as `assert_eq!(coefficients.len(), 64);`.
- Access the farthest fixed index first or destructure a known-size prefix with slice patterns.
- Replace complex affine index expressions with simple offsets, chunks, zipped iterators, or enumerated iterators.
- Use the Bounds Check Cookbook when safe rewrites are not obvious.

### Unsafe bounds-check tactics

- `get_unchecked`, `get_unchecked_mut`, raw pointers, `unreachable_unchecked`, and `assert_unchecked` are available for locally proven invariants.
- Unsafe indexing requires a local, tested invariant; never rely on distant assumptions.
- Replacing checks with `min`, masking, or padding changes error behavior unless clamping/wrapping is semantically correct.

### Aliasing tactics

- Prefer signatures like `fn kernel(dst: &mut [T], src: &[T])` for runtime loops.
- Split container management from element processing: prepare lengths/capacity outside, then call a slice-based worker.
- Use small helper functions to expose noalias function arguments when a large function hides references inside locals or struct fields.
- Keep mutable and shared borrows structurally separate.
- Prefer references; use raw pointers after proving references cannot express the runtime invariant. Rust references give LLVM noalias information at function boundaries; raw pointers lack that metadata.

### Iterator tactics

- Avoid `collect` when the collection is immediately iterated again; return `impl Iterator<Item = T>` when ergonomic and lifetimes allow.
- Use `extend` to add an iterator to an existing collection instead of collecting into a temporary vector and appending.
- Implement `Iterator::size_hint` or `ExactSizeIterator::len` for custom iterators; `collect`/`extend` can allocate less.
- Replace `chain` in runtime paths when a single iterator or explicit branch is faster.
- Use `filter_map` instead of `filter().map()` when equivalent.
- Use `chunks_exact`/`chunks_exact_mut` when chunk size divides length; otherwise combine with remainder handling. The same applies to reverse chunks and mutable variants.
- For small `Copy` items such as integers, `iter().copied()` can generate better code than references. Confirm with machine code when this is the intended win.
- Inspect and benchmark iterator rewrites; keep codegen wins and revert regressions.

## Standard Library Types and Methods

- Read stdlib docs for runtime types; many performance-relevant methods exist.
- `vec![0; n]` is the best way to create a zero-filled vector and can use OS assistance. Avoid unsafe or manual alternatives unless they prove faster.
- `Vec::remove` is O(n) and shifts elements; `Vec::swap_remove` is O(1) when order does not matter.
- `Vec::retain` removes multiple elements efficiently. Similar `retain` methods exist for `String`, `HashSet`, and `HashMap`.
- `Option::ok_or`, `map_or`, `unwrap_or` and `Result::or`, `map_or`, `unwrap_or` eagerly compute fallback values. Use the `_else` variants when fallback construction is expensive or allocates.
- `Rc::make_mut` and `Arc::make_mut` provide clone-on-write mutation.
- `parking_lot` provides alternative `Mutex`, `RwLock`, `Condvar`, and `Once` implementations. APIs and semantics differ from std; benchmark and keep the faster correct primitive.
- If a project standardizes on replacement types, use Clippy `disallowed_types` to prevent accidental std equivalents.

## Type Layout, Copies, and Cache Behavior

- Shrinking frequently instantiated or frequently copied types reduces memory use, memory traffic, cache pressure, and allocator pressure.
- Types larger than 128 bytes are copied with `memcpy` in relevant generated code. If `memcpy` appears in profiles, use DHAT copy profiling to identify hot copy sites and involved types. Shrinking to 128 bytes or less can remove those calls.
- `std::mem::size_of` gives size; `-Zprint-type-sizes` gives size, alignment, field order, enum discriminant, variant sizes, and padding. Nightly is required.
- Rust automatically reorders `repr(Rust)` struct and enum fields to minimize size. Remove unnecessary `repr(C)` from internal runtime types.
- Box one or more fields in rare outsized enum variants to shrink the enum, accepting allocation and pattern-matching ergonomics cost.
- Store indices/counts as smaller integers (`u32`, `u16`, `u8`) when ranges prove safe; convert to `usize` at use points.
- Use `Box<[T]>` or `ThinVec<T>` to shrink vector-containing runtime types when mutation/capacity semantics allow.
- Use niche-aware types where meaningful: `NonZero*`, references, `NonNull<T>`, and `Option<T>` can avoid extra discriminants.
- Group hot fields and split cold fields when cache profiles or memory layout evidence justify it.
- Protect important sizes with cfg-gated static assertions, commonly on `x86_64` to avoid cross-platform false failures.

## Hashing and Hash Tables

- Default `HashMap`/`HashSet` hashing is high quality and DoS-resistant but relatively slow, especially for short keys such as integers.
- Use faster hashers for trusted key sources where HashDoS is not a concern.
- `rustc-hash` (`FxHashMap`/`FxHashSet`) is very fast and low quality; it has performed best in rustc, especially for integer keys. The older `fxhash` crate is less maintained.
- `fnv` is higher quality than Fx and benchmarks slower in many integer-heavy workloads.
- `ahash` can exploit AES instructions; benchmark it against Fx and fnv on representative keys.
- Benchmark multiple alternatives. rustc has seen fnv -> fx speedups up to 6%, fx -> ahash slowdowns of 1-4%, and fx -> default slowdowns from 4-84%.
- `nohash-hasher` is useful when integer-like keys are already random or identity hashing preserves distribution.
- Deriving `Hash` hashes each field separately. For padding-free types, byte-wise hashing via `zerocopy`, `bytemuck`, or techniques described by `derive_hash_fast` can be faster, depending on type layout and hasher.
- After changing standard map types project-wide, add Clippy `disallowed_types` guardrails.

## IO, Text, and Bytes

- `print!` and `println!` lock stdout on every call. For repeated output, manually lock stdout and use `write!`/`writeln!`. Apply similarly to stdin/stderr.
- File IO is unbuffered by default. For many small reads/writes to files, sockets, or stdio, use `BufReader`/`BufWriter`.
- Explicitly call `flush` when buffered output errors must be reported; drop-time flush errors are ignored.
- Buffering stdout can combine with manual locking for many writes.
- `BufRead::lines` is convenient but allocates a `String` per line; use `read_line` with a reusable `String`.
- `String` input validates UTF-8. If the workload is byte-oriented or ASCII-only and does not need UTF-8 semantics, use `BufRead::read_until`, byte slices, `bstr`, or byte-oriented line readers.

## Wrapper Types, Synchronization, and Parallelism

- Wrapper types such as `RefCell`, `Mutex`, `RwLock`, and `Arc<Mutex<_>>` impose access overhead.
- If multiple wrapped values are accessed together, a single wrapper around a tuple/struct can reduce locking/refcell overhead and simplify consistency.
- Preserve independent locking granularity when concurrent access patterns require it; otherwise coalesce wrappers.
- For thread parallelism, start with Rayon and Crossbeam; use atomics/locks carefully and rely on resources such as *Rust Atomics and Locks* for design.
- Parallelism can produce large wins and can add scheduling, synchronization, false sharing, and determinism costs. Benchmark realistic workloads and keep profitable decompositions.
- Fine-grained data parallelism/SIMD helps vectorizable loops; verify current stable/nightly/crate options and generated code.

## Inlining, Outlining, Visibility, and Machine Code

- Uninlined function entry/exit can be a runtime cost. Inlining can remove call overhead and enable constant propagation, bounds-check removal, vectorization, or other optimizations.
- Inline attributes:
  - no attribute: compiler decides based on size, generics, crate boundary, and callsite.
  - `#[inline]`: suggests inlining and makes cross-crate IR available.
  - `#[inline(always)]`: strongest inlining request; verify emitted code.
  - `#[inline(never)]`: strong request not to inline.
- Inlining is non-transitive. If `f` calls `g` and both must inline at a callsite, mark both.
- Candidates: tiny functions, single-callsite functions, and functions whose inlining unlocks constants, alias facts, or bounds-check elimination.
- Measure after adding inline attributes. Keep wins; revert losses from inhibited nearby inlining or instruction-cache pressure.
- Cachegrind can reveal inlining: inlined function first/last lines lack event counts; uninlined function entry/exit lines show counts.
- For large multi-callsite functions where one callsite is hot, split into an `#[inline(always)]` hot variant and an `#[inline(never)]` cold wrapper that calls it.
- Outline rarely executed code into separate `#[cold]` functions to improve hot-path code generation.
- Keep functions non-`pub` unless API requires export. `pub(crate)`, `pub(super)`, or private runtime helpers give the compiler more localization opportunities.
- Inspect generated machine code for bounds checks, panic paths, vectorization, redundant `memset`/`memcpy`, missed inlining, large copies, and slow stdlib calls.
- `core::arch` exposes architecture-specific intrinsics, many for SIMD. Use target-gated code and preserve portability semantics.

## Arithmetic Semantics

- Inclusive ranges and overflow-sensitive bounds can add branches or inhibit vectorization. Prefer exclusive ranges or restructure when overflow is impossible/proven.
- Generated-code inspection reveals whether integer overflow checks are blocking runtime optimization for the supplied configuration.
- Make intended arithmetic explicit: `wrapping_add`, `checked_add`, `saturating_add`, `overflowing_add`, or `Wrapping<T>`.
- `NonZero*` types encode nonzero divisors and can remove divide-by-zero checks when the invariant is real.
- Use `try_from`/`try_into` for lossy casts where correctness matters. Do not replace with `as` unless truncation/wrapping is intended and tested.
- `unchecked_add`, `to_int_unchecked`, and similar APIs require airtight local invariants and targeted tests.

## Floating Point and Fast-Math-Like Optimizations

- Rust intentionally has no global `-ffast-math` equivalent.
- Global fast math can remove NaN/Inf checks, reorder operations, change rounding, alter signed-zero behavior, and break numerically careful algorithms.
- Keep exact IEEE behavior by default.
- For a measured math hotspot, consider local nightly intrinsics (`f*_fast`, `f*_algebraic`, `algebraic_*`), explicit SIMD, or domain-specific math crates when project policy and numerical contract allow it.
- Do not apply fast-math-like rewrites to Kahan summation, NaN-sensitive code, signed-zero-sensitive code, or documented rounding-sensitive code.

## Logging, Debugging, Assertions, and Linting

- Logging/debugging can be slow directly or by forcing expensive data collection. Ensure disabled logging does not compute messages, allocate strings, clone data, or traverse structures.
- `assert!` runs in normal runtime behavior. `debug_assert!` runs only when debug assertions are enabled. Convert runtime-costly assertions after proving they are not required for safety or correctness in the supplied runtime behavior.
- Clippy is useful for performance. Its performance lint group catches many patterns this playbook does not repeat.
- Clippy performance suggestions make code simpler and more idiomatic; apply them unless they harm semantics.
- Non-performance lints can improve performance; `ptr_arg` changes `&Vec<T>`/`&mut Vec<T>` parameters to slices, improving API flexibility and optimizer visibility.
- Use `clippy.toml` `disallowed-types = [...]` to prevent accidental standard types after choosing faster alternatives such as custom hash maps or `parking_lot` primitives.

## Unsafe Optimization Gate

Unsafe code is acceptable only when all of the following are true:

- The runtime bottleneck or vector is identified.
- Safe alternatives were tried or rejected for a specific reason.
- The invariant is local, explicit, and documented next to the unsafe block.
- Tests or assertions cover boundary conditions that would violate the invariant.
- Generated code or benchmark evidence confirms the unsafe block buys the intended result.
- The change does not merely silence panics or alter error behavior unless that semantic change is explicitly desired.

## Reporting Results

Report:

- Baseline and optimized command.
- Hardware, OS, toolchain, features, supplied runtime configuration, and representative input.
- Correctness checks run.
- Before/after timing, memory, allocation count, type size, profiler share, IO/syscall count, synchronization contention, or generated-code observation as applicable.
- Noise/confidence caveats.
- Files/functions changed.
- Why the change addresses the runtime cost.
- The mandatory `Indexed Optimization Summary`, enumerating every vector ID above with status, evidence, and rationale.
