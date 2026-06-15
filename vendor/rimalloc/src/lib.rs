#![cfg_attr(feature = "nightly", feature(thread_local))]
//! # rimalloc
//!
//! A pure-Rust port of [mimalloc](https://github.com/microsoft/mimalloc)
//! (v2.3.2): a compact general-purpose allocator with per-thread heaps,
//! sharded free lists, and lock-free cross-thread frees.
//!
//! ```
//! use rimalloc::Rimalloc;
//!
//! // #[global_allocator]
//! // static ALLOC: Rimalloc = Rimalloc;
//! let p = rimalloc::malloc(42);
//! assert!(rimalloc::usable_size(p) >= 42);
//! rimalloc::free(p);
//! ```

#![allow(clippy::missing_safety_doc)]

mod alloc;
mod api;
mod arena;
mod bins;
pub mod bitmap;
mod constants;
mod free;
mod heap;
mod heap_ops;
pub mod hooks;
mod init;
pub mod options;
mod os;
mod page;
mod page_ops;
pub mod protocol;
mod segment;
mod stats;
mod sync;
mod visit;

pub use api::*;
pub use bins::good_size as mi_good_size;
pub use heap::Heap;
pub use visit::{HeapArea, heap_visit_blocks};

// Errno constants: POSIX values are universal.
#[cfg(not(windows))]
pub use libc::{EFAULT, EINVAL, ENOMEM};
#[cfg(windows)]
pub const EFAULT: i32 = 14;
#[cfg(windows)]
pub const EINVAL: i32 = 22;
#[cfg(windows)]
pub const ENOMEM: i32 = 12;

/// `_mi_error_message`: route to the registered output/error hooks
/// (stderr by default) without allocating.
pub(crate) fn error(msg: &str) {
    error_code(msg, EFAULT)
}

pub(crate) fn error_code(msg: &str, code: i32) {
    hooks::output("rimalloc: error: ");
    hooks::output(msg);
    hooks::output("\n");
    hooks::error(code);
}

/// Bridge for the Lean conformance tests (`tests/lean_conformance.rs`):
/// exposes the exact functions the verified model mirrors, so the live
/// implementation can be pinned to the model over its whole domain.
pub mod lean_model {
    /// `Bin::from_size` on a word count (what `Bins.lean::binFromWsize` models).
    pub fn bin_from_wsize(wsize: usize) -> usize {
        crate::bins::Bin::from_size(wsize * crate::constants::PTR_SIZE).index()
    }

    /// `BIN_WSIZE` table entry (what `Bins.lean::binWsize` models).
    pub fn bin_wsize(bin: usize) -> usize {
        crate::bins::BIN_WSIZE[bin]
    }

    /// `CommitMask::create` as raw words (what `CommitMask.lean::createLoop`
    /// models).
    pub fn commit_mask_create_words(idx: usize, count: usize) -> [usize; 8] {
        let cm = crate::segment::CommitMask::create(idx, count);
        cm.to_words()
    }

    /// Page block-area start offset (what `PageStart.lean::startOffset`
    /// models).
    pub fn page_start_offset(block_size: usize, pstart_addr: usize, psize: usize) -> usize {
        crate::segment::page_start_offset(block_size, pstart_addr, psize)
    }
}
