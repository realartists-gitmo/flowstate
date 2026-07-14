//! Shared library surface for the `flowstate-corpus` perf/fidelity harness bins.
//!
//! The default binary (`main.rs`, the corpus sweep) and the perf-bench bins
//! (`collab_bench`, `hotpath_bench`, `perf_probe`, `contention_bench`) share the
//! synthetic-fixture builders here so equation/object shapes the real corpus
//! lacks can be driven without a `.docx` (§perf-heaven T7).

pub mod fixtures;
