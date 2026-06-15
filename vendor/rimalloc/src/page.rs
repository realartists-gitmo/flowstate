//! The mimalloc page (`mi_page_t`): a 64KiB–512KiB region serving blocks of
//! one size class, with three free lists:
//! - `free`: blocks `malloc` pops from (owner thread only),
//! - `local_free`: blocks freed by the owner, migrated to `free` on exhaustion,
//! - `xthread_free`: lock-free stack of blocks freed by other threads.
//!
//! Invariants (see `types.h`):
//!   `used - |thread_free|` = live blocks;
//!   `used + |free| + |local_free| = capacity <= reserved`.
//!
//! All fields are `Cell`/atomics and accessed through `&Page`, never `&mut`:
//! page metadata lives inside segments and is reachable from other threads
//! (via `xthread_free`), so exclusive references would be unsound.

use core::cell::Cell;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::constants::*;
use crate::heap::Heap;
use crate::segment::Segment;

/// A free block: the first word is the (unencoded) next pointer.
#[repr(transparent)]
pub struct Block {
    pub next: Cell<*mut Block>,
}

impl Block {
    /// View a user pointer as a block pointer (a plain cast; any
    /// dereference remains the caller's obligation).
    #[inline(always)]
    pub fn at(p: *mut u8) -> *mut Block {
        p.cast()
    }

    /// Set the next pointer of a block that is *entering* a free list.
    /// Must not go through `Cell::set` (= `replace`), which reads the old
    /// value: the block's memory may be uninitialized or hold partial user
    /// data, and reading it as a pointer is UB.
    ///
    /// # Safety
    /// `this` must point to block-sized memory owned by the allocator.
    #[inline(always)]
    pub unsafe fn write_next(this: *mut Block, next: *mut Block) {
        // SAFETY: per contract; plain write, no read of the old value.
        unsafe {
            this.write(Block {
                next: Cell::new(next),
            })
        }
    }
}

/// `mi_delayed_t`: the bottom two bits of `xthread_free`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
#[repr(u8)]
pub enum Delayed {
    /// Push on the owning heap's delayed list.
    UseDelayedFree = 0,
    /// Transient: another thread is accessing the owning heap.
    Freeing = 1,
    /// Push on the page-local thread free queue.
    NoDelayedFree = 2,
    /// Sticky: page is abandoned (no owning heap); reset on reclaim.
    NeverDelayedFree = 3,
}

impl Delayed {
    #[inline(always)]
    pub fn from_bits(bits: usize) -> Delayed {
        match bits & 3 {
            0 => Delayed::UseDelayedFree,
            1 => Delayed::Freeing,
            2 => Delayed::NoDelayedFree,
            _ => Delayed::NeverDelayedFree,
        }
    }
}

/// `mi_thread_free_t`: a block pointer with [`Delayed`] flags in the low
/// bits and the list length in the top 16 (see `protocol::TF_COUNT_SHIFT`).
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ThreadFree(pub usize);

impl ThreadFree {
    #[inline(always)]
    pub fn block(self) -> *mut Block {
        ptr::with_exposed_provenance_mut(self.0 & crate::protocol::TF_PTR_MASK)
    }
    #[inline(always)]
    pub fn count(self) -> usize {
        self.0 >> crate::protocol::TF_COUNT_SHIFT
    }
    #[inline(always)]
    pub fn delayed(self) -> Delayed {
        Delayed::from_bits(self.0)
    }
}

/// A mimalloc page. When unallocated, the same struct doubles as a slice
/// span header (`mi_slice_t`): `slice_count`/`slice_offset` describe the
/// span and `block_size` is 1 if used, 0 if free.
///
/// The metadata is split into three cache lines (128 bytes on Apple
/// Silicon) so concurrent cross-thread frees neither false-share with the
/// owner's malloc fast path nor with *each other*:
/// - line 0 is init-time-constant and read-shared (span lookup fields and
///   the unalign constants every foreign free needs) — it stays valid in
///   every core's cache;
/// - line 1 holds only the contended atomics (`xthread_free`, `xheap`)
///   that foreign frees CAS, so that traffic invalidates nothing else;
/// - line 2 has the owner's per-allocation fields (`free`, `local_free`,
///   `used`, queue links).
#[repr(C, align(128))]
pub struct Page {
    // ---- line 0: init-constant, read by everyone ----
    // "Owned" by the segment:
    pub slice_count: Cell<u32>,  // slices in this page (0 if not a page)
    pub slice_offset: Cell<u32>, // byte distance to the first slice of the span
    pub is_committed: Cell<bool>,
    pub is_zero_init: Cell<bool>,
    pub is_huge: Cell<bool>,
    pub heap_tag: Cell<u8>,
    pub block_size_shift: Cell<u8>, // nonzero iff block_size == 1<<shift
    pub block_size: Cell<usize>,
    pub page_start: Cell<*mut u8>,
    /// Lemire fastmod reciprocal of `block_size` (`u64::MAX / b + 1`), or 0
    /// when unusable; lets `page_ptr_unalign` avoid a hardware divide.
    pub block_recip: Cell<u64>,

    _pad0: [u8; 88],

    // ---- line 1: the atomics foreign frees write ----
    pub xthread_free: AtomicUsize,
    pub xheap: AtomicUsize,

    _pad1: [u8; 112],

    // ---- line 2: owner-thread hot fields ----
    pub free: Cell<*mut Block>,
    pub local_free: Cell<*mut Block>,
    pub used: Cell<u16>,
    pub capacity: Cell<u16>, // blocks committed
    pub reserved: Cell<u16>, // blocks reserved in memory
    /// Bit 0: page is in the full queue; bit 1: contains aligned (interior)
    /// blocks. One byte so the free fast path tests both with one load.
    flags: Cell<u8>,
    pub free_is_zero: Cell<bool>,
    pub retire_expire: Cell<u8>,

    pub next: Cell<*mut Page>,
    pub prev: Cell<*mut Page>,

    /// Random keys encoding the free lists (`secure` feature; `MI_ENCODE_FREELIST`).
    pub keys: [Cell<usize>; 2],
}

// SAFETY: pages are reachable from other threads, but those only touch the
// atomic fields (`xthread_free`, `xheap`) and the constant-while-shared
// fields (`page_start`, `block_size`, `block_size_shift`), mirroring
// mimalloc's discipline.
unsafe impl Sync for Page {}

/// A slice span header is the same struct viewed differently.
pub type Slice = Page;

/// Free-list pointer encoding (`mi_ptr_encode`/`mi_ptr_decode`): with the
/// `secure` feature, next pointers are stored as
/// `rotl(ptr ^ k1, k0 % BITS) + k0` with per-page (or per-heap) random
/// keys, detecting corrupted lists and making overwrites useless to an
/// attacker. `null` is substituted by the list owner's address so an
/// encoded null is not a key oracle.
#[inline(always)]
pub fn ptr_encode(null_sub: usize, p: *mut Block, keys: &[Cell<usize>; 2]) -> *mut Block {
    if cfg!(feature = "secure") {
        let x = if p.is_null() { null_sub } else { p.addr() };
        let (k0, k1) = (keys[0].get(), keys[1].get());
        ptr::with_exposed_provenance_mut(
            (x ^ k1)
                .rotate_left((k0 % usize::BITS as usize) as u32)
                .wrapping_add(k0),
        )
    } else {
        p
    }
}

#[inline(always)]
pub fn ptr_decode(null_sub: usize, enc: *mut Block, keys: &[Cell<usize>; 2]) -> *mut Block {
    if cfg!(feature = "secure") {
        let (k0, k1) = (keys[0].get(), keys[1].get());
        let x = enc
            .addr()
            .wrapping_sub(k0)
            .rotate_right((k0 % usize::BITS as usize) as u32)
            ^ k1;
        if x == null_sub {
            ptr::null_mut()
        } else {
            ptr::with_exposed_provenance_mut(x)
        }
    } else {
        enc
    }
}

impl Page {
    #[allow(clippy::declare_interior_mutable_const)] // template value, never read in place
    pub const EMPTY: Page = Page {
        slice_count: Cell::new(0),
        slice_offset: Cell::new(0),
        is_committed: Cell::new(false),
        is_zero_init: Cell::new(false),
        is_huge: Cell::new(false),
        capacity: Cell::new(0),
        reserved: Cell::new(0),
        flags: Cell::new(0),
        free_is_zero: Cell::new(false),
        retire_expire: Cell::new(0),
        free: Cell::new(ptr::null_mut()),
        local_free: Cell::new(ptr::null_mut()),
        used: Cell::new(0),
        block_size_shift: Cell::new(0),
        heap_tag: Cell::new(0),
        block_size: Cell::new(0),
        page_start: Cell::new(ptr::null_mut()),
        block_recip: Cell::new(0),
        xthread_free: AtomicUsize::new(0),
        xheap: AtomicUsize::new(0),
        _pad0: [0; 88],
        _pad1: [0; 112],
        next: Cell::new(ptr::null_mut()),
        prev: Cell::new(ptr::null_mut()),
        keys: [Cell::new(0), Cell::new(0)],
    };

    /// Set `block_size` together with its fastmod reciprocal (valid for
    /// `0 < b < 2^32`; otherwise `page_ptr_unalign` falls back to `%`).
    pub fn set_block_size(&self, b: usize) {
        debug_assert!(b > 0);
        self.block_size.set(b);
        self.block_recip.set(if (b as u64) < (1 << 32) {
            u64::MAX / b as u64 + 1
        } else {
            0
        });
    }

    /// `_mi_page_segment`: pages live inside their segment, so mask the address.
    /// The segment mapping's provenance is exposed at allocation, so the
    /// masked address reconstructs a valid pointer.
    #[inline(always)]
    pub fn segment(&self) -> &Segment {
        // SAFETY: a live page is embedded in the slices array of its
        // SEGMENT_ALIGN-aligned segment whose provenance was exposed.
        unsafe { &*ptr::with_exposed_provenance(ptr::from_ref(self).addr() & !SEGMENT_MASK) }
    }

    #[inline(always)]
    pub fn start(&self) -> *mut u8 {
        debug_assert!(!self.page_start.get().is_null());
        self.page_start.get()
    }

    /// `mi_page_thread_free`
    #[inline(always)]
    pub fn thread_free(&self) -> ThreadFree {
        ThreadFree(self.xthread_free.load(Ordering::Relaxed))
    }

    /// `mi_page_heap`
    #[inline(always)]
    pub fn heap(&self) -> *mut Heap {
        ptr::with_exposed_provenance_mut(self.xheap.load(Ordering::Relaxed))
    }

    /// `mi_page_set_heap`
    #[inline]
    pub fn set_heap(&self, heap: *mut Heap) {
        debug_assert!(self.thread_free().delayed() != Delayed::Freeing);
        self.xheap
            .store(heap.expose_provenance(), Ordering::Release);
        if !heap.is_null() {
            // SAFETY: heap outlives its pages; tag is plain data.
            self.heap_tag.set(unsafe { (*heap).tag.get() });
        }
    }

    const FLAG_IN_FULL: u8 = 0x01;
    const FLAG_HAS_ALIGNED: u8 = 0x02;

    #[inline(always)]
    pub fn in_full(&self) -> bool {
        self.flags.get() & Self::FLAG_IN_FULL != 0
    }

    #[inline(always)]
    pub fn set_in_full(&self, v: bool) {
        let f = self.flags.get() & !Self::FLAG_IN_FULL;
        self.flags.set(f | if v { Self::FLAG_IN_FULL } else { 0 });
    }

    #[inline(always)]
    pub fn has_aligned(&self) -> bool {
        self.flags.get() & Self::FLAG_HAS_ALIGNED != 0
    }

    #[inline(always)]
    pub fn set_has_aligned(&self, v: bool) {
        let f = self.flags.get() & !Self::FLAG_HAS_ALIGNED;
        self.flags
            .set(f | if v { Self::FLAG_HAS_ALIGNED } else { 0 });
    }

    /// True if the local free path must take the generic route (full-queue
    /// bookkeeping or interior aligned pointers): a single byte test.
    #[inline(always)]
    pub fn full_or_aligned(&self) -> bool {
        self.flags.get() != 0
    }

    /// `mi_page_all_free`: needs an up-to-date `used` count.
    #[inline(always)]
    pub fn all_free(&self) -> bool {
        self.used.get() == 0
    }

    /// `mi_page_has_any_available`
    #[inline(always)]
    pub fn has_any_available(&self) -> bool {
        debug_assert!(self.reserved.get() > 0);
        self.used.get() < self.reserved.get() || !self.thread_free().block().is_null()
    }

    /// `mi_page_immediate_available`
    #[inline(always)]
    pub fn immediate_available(&self) -> bool {
        !self.free.get().is_null()
    }

    /// `mi_page_is_mostly_used`: more than 7/8th in use.
    #[inline]
    pub fn is_mostly_used(&self) -> bool {
        let frac = self.reserved.get() / 8;
        self.reserved.get() - self.used.get() <= frac
    }

    #[inline(always)]
    pub fn block_size(&self) -> usize {
        debug_assert!(self.block_size.get() > 0);
        self.block_size.get()
    }

    /// `mi_page_usable_block_size`: without fixed padding.
    #[inline(always)]
    pub fn usable_block_size(&self) -> usize {
        self.block_size() - PADDING_SIZE
    }

    // ---- slice-span view (`mi_slice_t`) ----

    /// `mi_slice_first`: the head of the span this slice belongs to.
    #[inline]
    pub fn slice_first(&self) -> &Slice {
        // SAFETY: slice_offset is the byte distance back to the span head
        // within the same (exposed) segment mapping.
        let first: &Slice = unsafe {
            &*ptr::with_exposed_provenance(
                ptr::from_ref(self).addr() - self.slice_offset.get() as usize,
            )
        };
        debug_assert!(first.slice_offset.get() == 0);
        first
    }

    /// Whether this span is in use as a page (`block_size == 1` marker for
    /// used spans that are not pages is set by the segment code).
    #[inline(always)]
    pub fn slice_is_used(&self) -> bool {
        self.block_size.get() > 0
    }

    /// `mi_block_next`: read a block's next pointer (decoding under `secure`).
    ///
    /// # Safety
    /// `b` must be a live free-list node of this page.
    #[inline(always)]
    pub unsafe fn block_next(&self, b: *mut Block) -> *mut Block {
        // SAFETY: per contract.
        let raw = unsafe { (*b).next.get() };
        ptr_decode(ptr::from_ref(self).addr(), raw, &self.keys)
    }

    /// `mi_block_set_next`: link a block into one of this page's free lists
    /// (write-only: the block's memory may be uninitialized).
    ///
    /// # Safety
    /// `b` must point to block-sized memory of this page owned by the caller.
    #[inline(always)]
    pub unsafe fn block_set_next(&self, b: *mut Block, next: *mut Block) {
        let enc = ptr_encode(ptr::from_ref(self).addr(), next, &self.keys);
        // SAFETY: per contract.
        unsafe { Block::write_next(b, enc) };
    }
}

const _: () = assert!(size_of::<Page>() == 384);
const _: () = assert!(core::mem::offset_of!(Page, xthread_free) == 128);
const _: () = assert!(core::mem::offset_of!(Page, free) == 256);
