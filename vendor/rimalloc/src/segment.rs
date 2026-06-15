//! Segments (port of `segment.c`): 32MiB aligned chunks divided into 64KiB
//! slices. Pages are spans of slices; free spans are kept in per-thread span
//! queues binned by slice count. The segment header (including the `slices`
//! metadata array) occupies the first slices ("info slices").

use core::cell::Cell;
use core::ptr;
use core::sync::atomic::{AtomicUsize, Ordering};

use crate::constants::*;
use crate::heap::Heap;
use crate::options;
use crate::os::{self, OsMem};
use crate::page::{Page, Slice};

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SegmentKind {
    Normal, // SEGMENT_SIZE with pages inside
    Huge,   // one huge page, variable size
}

/// `mi_commit_mask_t`: one bit per commit chunk (= slice).
pub struct CommitMask {
    mask: [Cell<usize>; COMMIT_MASK_FIELD_COUNT],
}

impl CommitMask {
    #[allow(clippy::declare_interior_mutable_const)] // template value, never read in place
    pub const EMPTY: CommitMask = CommitMask {
        mask: [const { Cell::new(0) }; COMMIT_MASK_FIELD_COUNT],
    };

    pub fn clear_all(&self) {
        for f in &self.mask {
            f.set(0);
        }
    }
    pub fn set_full(&self) {
        for f in &self.mask {
            f.set(!0);
        }
    }
    pub fn is_empty(&self) -> bool {
        self.mask.iter().all(|f| f.get() == 0)
    }
    pub fn is_full(&self) -> bool {
        self.mask.iter().all(|f| f.get() == !0)
    }
    /// Are all bits of `other` set in `self`?
    pub fn all_set(&self, other: &CommitMask) -> bool {
        self.mask
            .iter()
            .zip(&other.mask)
            .all(|(a, b)| a.get() & b.get() == b.get())
    }
    pub fn any_set(&self, other: &CommitMask) -> bool {
        self.mask
            .iter()
            .zip(&other.mask)
            .any(|(a, b)| a.get() & b.get() != 0)
    }
    pub fn set(&self, other: &CommitMask) {
        for (a, b) in self.mask.iter().zip(&other.mask) {
            a.set(a.get() | b.get());
        }
    }
    pub fn clear(&self, other: &CommitMask) {
        for (a, b) in self.mask.iter().zip(&other.mask) {
            a.set(a.get() & !b.get());
        }
    }
    pub fn intersect(&self, other: &CommitMask) -> CommitMask {
        let r = CommitMask::EMPTY;
        for ((a, b), c) in self.mask.iter().zip(&other.mask).zip(&r.mask) {
            c.set(a.get() & b.get());
        }
        r
    }
    pub fn copy_from(&self, other: &CommitMask) {
        for (a, b) in self.mask.iter().zip(&other.mask) {
            a.set(b.get());
        }
    }
    /// `mi_commit_mask_create`: set `count` bits starting at `bitidx`.
    pub fn create(bitidx: usize, bitcount: usize) -> CommitMask {
        debug_assert!(bitidx + bitcount <= COMMIT_MASK_BITS);
        let cm = CommitMask::EMPTY;
        if bitcount == COMMIT_MASK_BITS {
            cm.set_full();
        } else if bitcount > 0 {
            let mut field = bitidx / COMMIT_MASK_FIELD_BITS;
            let mut ofs = bitidx % COMMIT_MASK_FIELD_BITS;
            let mut left = bitcount;
            while left > 0 {
                let avail = COMMIT_MASK_FIELD_BITS - ofs;
                let count = left.min(avail);
                let mask = if count == COMMIT_MASK_FIELD_BITS {
                    !0
                } else {
                    ((1usize << count) - 1) << ofs
                };
                cm.mask[field].set(cm.mask[field].get() | mask);
                left -= count;
                ofs = 0;
                field += 1;
            }
        }
        cm
    }
    /// Raw field words (for the Lean conformance bridge).
    pub fn to_words(&self) -> [usize; COMMIT_MASK_FIELD_COUNT] {
        core::array::from_fn(|i| self.mask[i].get())
    }

    /// Iterate maximal runs of set bits as `(bit_index, bit_count)`.
    pub fn for_each_run(&self, mut f: impl FnMut(usize, usize)) {
        let mut idx = 0;
        while idx < COMMIT_MASK_BITS {
            // find next set bit
            while idx < COMMIT_MASK_BITS
                && self.mask[idx / COMMIT_MASK_FIELD_BITS].get()
                    & (1 << (idx % COMMIT_MASK_FIELD_BITS))
                    == 0
            {
                idx += 1;
            }
            let start = idx;
            while idx < COMMIT_MASK_BITS
                && self.mask[idx / COMMIT_MASK_FIELD_BITS].get()
                    & (1 << (idx % COMMIT_MASK_FIELD_BITS))
                    != 0
            {
                idx += 1;
            }
            if idx > start {
                f(start, idx - start);
            }
        }
    }
}

/// Where a segment's memory came from (`mi_memkind_t`).
#[derive(Clone, Copy, Debug)]
pub enum MemSource {
    Os,
    Arena(crate::arena::ArenaRef),
}

/// Provenance of segment memory (`mi_memid_t`).
#[derive(Clone, Copy, Debug)]
pub struct MemId {
    pub mem: OsMem,
    pub source: MemSource,
    pub is_pinned: bool,
    pub initially_committed: bool,
    pub initially_zero: bool,
}

/// `mi_segment_t`. The struct is followed (within the same allocation) by
/// nothing: `slices` is a fixed array as in C.
#[repr(C)]
pub struct Segment {
    pub memid: Cell<MemId>,
    pub allow_decommit: Cell<bool>,
    pub allow_purge: Cell<bool>,
    pub segment_size: Cell<usize>,

    pub purge_expire: Cell<i64>,
    pub purge_mask: CommitMask,
    pub commit_mask: CommitMask,

    pub next: Cell<*mut Segment>, // intrusive: abandoned list
    pub was_reclaimed: Cell<bool>,
    pub dont_free: Cell<bool>,
    pub free_is_zero: Cell<bool>,

    pub abandoned: Cell<usize>, // abandoned pages (<= used)
    pub abandoned_visits: Cell<usize>,
    pub used: Cell<usize>, // pages in use
    pub cookie: Cell<usize>,

    pub abandoned_os_next: Cell<*mut Segment>,
    pub abandoned_os_prev: Cell<*mut Segment>,
    pub abandoned_os_listed: Cell<bool>,

    pub segment_slices: Cell<usize>,
    pub segment_info_slices: Cell<usize>,

    pub kind: Cell<SegmentKind>,
    pub slice_entries: Cell<usize>,
    pub thread_id: AtomicUsize,

    pub slices: [Slice; SLICES_PER_SEGMENT + 1],
}

/// The block-area start offset within a page (pure core of
/// `_mi_segment_page_start_from_slice`): nudge the start off OS-page/cache
/// boundaries while keeping it `MAX_ALIGN_SIZE`-aligned and, for blocks up
/// to `MAX_ALIGN_GUARANTEE`, block-size aligned. Modeled and verified in
/// `verify/RimallocVerify/PageStart.lean`; the live function is pinned to
/// the proven contract in `tests/lean_conformance.rs`.
pub(crate) fn page_start_offset(block_size: usize, pstart_addr: usize, psize: usize) -> usize {
    let mut start_offset = 0;
    if block_size > 0 && block_size <= MAX_ALIGN_GUARANTEE {
        let adjust = block_size - (pstart_addr % block_size);
        if adjust < block_size && psize >= block_size + adjust {
            start_offset += adjust;
        }
    }
    if block_size >= PTR_SIZE {
        if block_size <= 64 {
            start_offset += 3 * block_size;
        } else if block_size <= 512 {
            start_offset += block_size;
        }
    }
    align_up(start_offset, MAX_ALIGN_SIZE)
}

/// `_mi_ptr_segment`: the segment containing `p` (which must point into one).
///
/// # Safety
/// `p` must point into a live segment (one byte past the start is ok).
#[inline(always)]
pub unsafe fn ptr_segment<'a>(p: *const u8) -> &'a Segment {
    debug_assert!(!p.is_null());
    // SAFETY: per contract; align down to the SEGMENT_SIZE boundary
    // (one byte before p, for blocks aligned at N*SEGMENT_SIZE). The
    // segment mapping's provenance was exposed at allocation.
    unsafe { &*ptr::with_exposed_provenance((p.addr() - 1) & !SEGMENT_MASK) }
}

/// `_mi_ptr_page`: the page containing `p`.
///
/// # Safety
/// `p` must point into the block area of a live page.
#[inline(always)]
pub unsafe fn ptr_page<'a>(p: *const u8) -> &'a Page {
    // SAFETY: per contract.
    let segment = unsafe { ptr_segment(p) };
    segment.page_of(p)
}

impl Segment {
    /// `_mi_arena_memid_is_suitable` on this segment's memory.
    pub fn memid_suitable(&self, req: crate::arena::ArenaId) -> bool {
        match self.memid.get().source {
            MemSource::Arena(aref) => crate::arena::ref_is_suitable(aref, req),
            MemSource::Os => req == crate::arena::ARENA_ID_NONE,
        }
    }

    /// Base address of the segment mapping, carrying provenance over the
    /// whole (exposed) mapping rather than just the header struct.
    #[inline(always)]
    pub fn as_ptr(&self) -> *mut u8 {
        ptr::with_exposed_provenance_mut(ptr::from_ref(self).addr())
    }

    #[inline(always)]
    pub fn size(&self) -> usize {
        self.segment_slices.get() * SEGMENT_SLICE_SIZE
    }

    #[inline]
    pub fn info_size(&self) -> usize {
        self.segment_info_slices.get() * SEGMENT_SLICE_SIZE
    }

    #[inline(always)]
    pub fn is_abandoned(&self) -> bool {
        self.thread_id.load(Ordering::Relaxed) == 0
    }

    #[inline]
    pub fn slice_index(&self, slice: &Slice) -> usize {
        let index =
            (ptr::from_ref(slice).addr() - self.slices.as_ptr().addr()) / size_of::<Slice>();
        debug_assert!(index < self.slice_entries.get());
        index
    }

    /// `mi_slice_start`: address of the slice's memory.
    #[inline]
    pub fn slice_start(&self, slice: &Slice) -> *mut u8 {
        // SAFETY: slices map 1:1 onto the segment's memory.
        unsafe {
            self.as_ptr()
                .add(self.slice_index(slice) * SEGMENT_SLICE_SIZE)
        }
    }

    /// One past the last usable slice entry.
    #[inline]
    pub fn slices_end(&self) -> *const Slice {
        // SAFETY: slice_entries <= SLICES_PER_SEGMENT+1.
        unsafe { self.slices.as_ptr().add(self.slice_entries.get()) }
    }

    /// `_mi_segment_page_of`: the page holding `p` (interior pointers ok).
    #[inline(always)]
    pub fn page_of(&self, p: *const u8) -> &Page {
        let diff = p.addr() - self.as_ptr().addr();
        debug_assert!(diff > 0 && diff <= SEGMENT_SIZE);
        let idx = diff >> SEGMENT_SLICE_SHIFT;
        debug_assert!(idx <= self.slice_entries.get());
        let slice = &self.slices[idx];
        let first = slice.slice_first();
        debug_assert!(first.slice_offset.get() == 0);
        first
    }

    /// `_mi_segment_page_start_from_slice`: start of a page's block area.
    /// The start is offset to avoid OS-page/cache alignment effects while
    /// staying block-size aligned for blocks up to `MAX_ALIGN_GUARANTEE`.
    fn page_start_from_slice(&self, slice: &Slice, block_size: usize) -> (*mut u8, usize) {
        let idx = self.slice_index(slice);
        let psize = slice.slice_count.get() as usize * SEGMENT_SLICE_SIZE;
        let pstart_addr = self.as_ptr().addr() + idx * SEGMENT_SLICE_SIZE;
        let start_offset = page_start_offset(block_size, pstart_addr, psize);
        // SAFETY: start_offset < psize, within the segment.
        let pstart = unsafe { self.as_ptr().add(idx * SEGMENT_SLICE_SIZE + start_offset) };
        (pstart, psize - start_offset)
    }

    /// `_mi_segment_page_start`
    pub fn page_start(&self, page: &Page) -> (*mut u8, usize) {
        self.page_start_from_slice(page, page.block_size.get())
    }

    // ---- commit / purge ----

    /// `mi_segment_commit_mask`
    fn commit_range_mask(
        &self,
        conservative: bool,
        p: *mut u8,
        size: usize,
    ) -> (*mut u8, usize, CommitMask) {
        debug_assert!(self.kind.get() != SegmentKind::Huge);
        if size == 0 || size > SEGMENT_SIZE {
            return (p, 0, CommitMask::EMPTY);
        }
        let segstart = self.info_size();
        let segsize = self.size();
        if p.addr() >= self.as_ptr().addr() + segsize {
            return (p, 0, CommitMask::EMPTY);
        }
        let pstart = p.addr() - self.as_ptr().addr();
        debug_assert!(pstart + size <= segsize);

        let (mut start, mut end) = if conservative {
            (
                align_up(pstart, COMMIT_SIZE),
                align_down(pstart + size, COMMIT_SIZE),
            )
        } else {
            (
                align_down(pstart, MINIMAL_COMMIT_SIZE),
                align_up(pstart + size, MINIMAL_COMMIT_SIZE),
            )
        };
        if pstart >= segstart && start < segstart {
            start = segstart;
        }
        end = end.min(segsize);

        let full_size = end.saturating_sub(start);
        if full_size == 0 {
            return (p, 0, CommitMask::EMPTY);
        }
        let bitidx = start / COMMIT_SIZE;
        let bitcount = full_size / COMMIT_SIZE;
        (
            self.as_ptr().with_addr(self.as_ptr().addr() + start),
            full_size,
            CommitMask::create(bitidx, bitcount),
        )
    }

    /// `mi_segment_commit`
    fn commit(&self, p: *mut u8, size: usize) -> bool {
        let (start, full_size, mask) = self.commit_range_mask(false, p, size);
        if mask.is_empty() || full_size == 0 {
            return true;
        }
        if !self.commit_mask.all_set(&mask) {
            // SAFETY: range lies within this segment's mapping.
            unsafe { os::commit(start, full_size) };
            self.commit_mask.set(&mask);
        }
        if self.purge_mask.any_set(&mask) {
            self.purge_expire
                .set(options::clock_now() + options::purge_delay());
        }
        self.purge_mask.clear(&mask);
        true
    }

    /// `mi_segment_ensure_committed`
    pub fn ensure_committed(&self, p: *mut u8, size: usize) -> bool {
        if self.commit_mask.is_full() && self.purge_mask.is_empty() {
            return true;
        }
        debug_assert!(self.kind.get() != SegmentKind::Huge);
        let ok = self.commit(p, size);
        debug_assert!(
            !ok || {
                let (_, fs, mask) = self.commit_range_mask(false, p, size);
                fs == 0 || self.commit_mask.all_set(&mask)
            },
            "ensure_committed left range uncovered"
        );
        ok
    }

    /// `mi_segment_purge`
    fn purge(&self, p: *mut u8, size: usize) {
        if !self.allow_purge.get() {
            return;
        }
        let (start, full_size, mask) = self.commit_range_mask(true, p, size);
        if mask.is_empty() || full_size == 0 {
            return;
        }
        if self.commit_mask.any_set(&mask) {
            debug_assert!(self.allow_decommit.get());
            if options::purge_decommits() != 0 {
                // SAFETY: purged ranges hold no live blocks (callers
                // guarantee); decommitted ranges recommit on next use.
                unsafe { os::decommit(start, full_size) };
                self.commit_mask.clear(&mask);
            } else {
                // SAFETY: purged ranges hold no live blocks (callers guarantee).
                unsafe { os::reset(start, full_size) };
            }
        }
        self.purge_mask.clear(&mask);
    }

    /// `mi_segment_schedule_purge`
    fn schedule_purge(&self, p: *mut u8, size: usize) {
        if !self.allow_purge.get() {
            return;
        }
        if options::purge_delay() == 0 {
            self.purge(p, size);
            return;
        }
        let (_, full_size, mask) = self.commit_range_mask(true, p, size);
        if mask.is_empty() || full_size == 0 {
            return;
        }
        let cmask = self.commit_mask.intersect(&mask); // only purge committed parts
        self.purge_mask.set(&cmask);
        let now = options::clock_now();
        let expire = self.purge_expire.get();
        if expire == 0 {
            self.purge_expire.set(now + options::purge_delay());
        } else if expire <= now {
            if expire + options::purge_extend_delay() <= now {
                self.try_purge(true);
            } else {
                self.purge_expire.set(now + options::purge_extend_delay());
            }
        } else {
            self.purge_expire
                .set(expire + options::purge_extend_delay());
        }
    }

    /// `mi_segment_try_purge`
    pub fn try_purge(&self, force: bool) {
        if !self.allow_purge.get() || self.purge_expire.get() == 0 || self.purge_mask.is_empty() {
            return;
        }
        if !force && options::clock_now() < self.purge_expire.get() {
            return;
        }
        let mask = CommitMask::EMPTY;
        mask.copy_from(&self.purge_mask);
        self.purge_expire.set(0);
        self.purge_mask.clear_all();
        mask.for_each_run(|idx, count| {
            // SAFETY: bit ranges map within the segment.
            let p = unsafe { self.as_ptr().add(idx * COMMIT_SIZE) };
            self.purge(p, count * COMMIT_SIZE);
        });
    }
}

// ---------------------------------------------------------------------------
// Span queues (free slice spans, per thread)
// ---------------------------------------------------------------------------

/// `mi_span_queue_t`
pub struct SpanQueue {
    pub first: Cell<*mut Slice>,
    pub last: Cell<*mut Slice>,
    pub slice_count: usize,
}

/// Max slice count per span bin (`MI_SEGMENT_SPAN_QUEUES_EMPTY`).
pub const SPAN_QUEUE_SIZES: [usize; SEGMENT_BIN_MAX + 1] = [
    1, 1, 2, 3, 4, 5, 6, 7, 10, 12, 14, 16, 20, 24, 28, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160,
    192, 224, 256, 320, 384, 448, 512, 640, 768, 896, 1024,
];

/// `mi_slice_bin`: bin for a span of `slice_count` slices.
#[inline]
fn slice_bin(slice_count: usize) -> usize {
    debug_assert!(slice_count * SEGMENT_SLICE_SIZE <= SEGMENT_SIZE);
    if slice_count <= 1 {
        return slice_count;
    }
    let n = slice_count - 1;
    let s = (usize::BITS - 1 - n.leading_zeros()) as usize; // bsr, n > 0
    let bin = if s <= 2 {
        n + 1
    } else {
        ((s << 2) | ((n >> (s - 2)) & 0x03)) - 4
    };
    debug_assert!(bin <= SEGMENT_BIN_MAX);
    bin
}

/// Per-thread segment data (`mi_segments_tld_t`).
pub struct SegmentsTld {
    pub spans: [SpanQueue; SEGMENT_BIN_MAX + 1],
    pub count: Cell<usize>,
    pub peak_count: Cell<usize>,
    pub current_size: Cell<usize>,
    pub peak_size: Cell<usize>,
    pub reclaim_count: Cell<usize>,
}

impl SegmentsTld {
    pub fn new() -> SegmentsTld {
        SegmentsTld {
            spans: core::array::from_fn(|i| SpanQueue {
                first: Cell::new(ptr::null_mut()),
                last: Cell::new(ptr::null_mut()),
                slice_count: SPAN_QUEUE_SIZES[i],
            }),
            count: Cell::new(0),
            peak_count: Cell::new(0),
            current_size: Cell::new(0),
            peak_size: Cell::new(0),
            reclaim_count: Cell::new(0),
        }
    }

    /// `mi_span_queue_for`
    fn span_queue_for(&self, slice_count: usize) -> &SpanQueue {
        let sq = &self.spans[slice_bin(slice_count)];
        debug_assert!(sq.slice_count >= slice_count);
        sq
    }

    fn track_size(&self, delta: isize) {
        // `committed` is tracked at whole-segment granularity here, the one
        // place segments enter and leave. Sub-segment lazy commit and
        // decommit-purge are not reflected.
        //
        // A *globally exact* committed counter remains a rats' nest: freed OS
        // segments flow through the global segment cache and the deferred-free
        // list as bare `OsMem` records that have already shed the segment's
        // `commit_mask`, and `cache_push`/`defer_free`/`cache_purge_expired`
        // recommit, reset, and munmap whole regions in contexts that cannot
        // cheaply recover the precise committed-byte delta (mprotect/mmap give
        // no prior-state feedback). Tracking the true delta there would mean
        // threading committed-byte counts back through the cache/defer layer —
        // exactly the cross-layer state duplication that produced the original
        // underflow. The coarse, whole-segment counter is instead exactly
        // balanced by construction: every increase is paired with an equal
        // decrease at the same granularity, so it can never underflow, in
        // either reset or decommit purge mode (asserted in
        // `tests/committed_stat.rs`).
        if delta >= 0 {
            crate::stats::STATS.segments.increase(1);
            crate::stats::STATS.reserved.increase(delta as usize);
            crate::stats::STATS.committed.increase(delta as usize);
        } else {
            crate::stats::STATS.segments.decrease(1);
            crate::stats::STATS.reserved.decrease((-delta) as usize);
            crate::stats::STATS.committed.decrease((-delta) as usize);
        }
        let count = self.count.get();
        self.count.set(count.wrapping_add_signed(delta.signum()));
        self.peak_count
            .set(self.peak_count.get().max(self.count.get()));
        let size = self.current_size.get().wrapping_add_signed(delta);
        self.current_size.set(size);
        self.peak_size.set(self.peak_size.get().max(size));
    }
}

impl SpanQueue {
    /// `mi_span_queue_push`: also marks the slice free (`block_size = 0`).
    fn push(&self, slice: &Slice) {
        debug_assert!(slice.prev.get().is_null() && slice.next.get().is_null());
        slice.prev.set(ptr::null_mut());
        slice.next.set(self.first.get());
        self.first.set(ptr::from_ref(slice).cast_mut());
        // SAFETY: queue nodes are live slices.
        match unsafe { slice.next.get().as_ref() } {
            Some(next) => next.prev.set(ptr::from_ref(slice).cast_mut()),
            None => self.last.set(ptr::from_ref(slice).cast_mut()),
        }
        slice.block_size.set(0); // free
    }

    /// `mi_span_queue_delete`: also marks the slice used (`block_size = 1`).
    fn delete(&self, slice: &Slice) {
        debug_assert!(slice.block_size.get() == 0 && slice.slice_count.get() > 0);
        // SAFETY: prev/next are live queue nodes.
        unsafe {
            if let Some(prev) = slice.prev.get().as_ref() {
                prev.next.set(slice.next.get());
            }
            if ptr::from_ref(slice) == self.first.get() {
                self.first.set(slice.next.get());
            }
            if let Some(next) = slice.next.get().as_ref() {
                next.prev.set(slice.prev.get());
            }
            if ptr::from_ref(slice) == self.last.get() {
                self.last.set(slice.prev.get());
            }
        }
        slice.prev.set(ptr::null_mut());
        slice.next.set(ptr::null_mut());
        slice.block_size.set(1); // no longer free
    }
}

// ---------------------------------------------------------------------------
// Span free / coalesce / allocate
// ---------------------------------------------------------------------------

impl Segment {
    /// `mi_segment_span_free`: register `[slice_index, +slice_count)` as a
    /// free span (queue it unless huge/abandoned), optionally purging.
    fn span_free(
        &self,
        slice_index: usize,
        slice_count: usize,
        allow_purge: bool,
        tld: &SegmentsTld,
    ) {
        debug_assert!(slice_index < self.slice_entries.get());
        let queue = (self.kind.get() != SegmentKind::Huge && !self.is_abandoned())
            .then(|| tld.span_queue_for(slice_count));
        let slice_count = slice_count.max(1);
        let slice = &self.slices[slice_index];
        slice.slice_count.set(slice_count as u32);
        slice.slice_offset.set(0);
        if slice_count > 1 {
            let last_index = (slice_index + slice_count - 1).min(self.slice_entries.get());
            let last = &self.slices[last_index];
            last.slice_count.set(0);
            last.slice_offset
                .set((size_of::<Slice>() * (last_index - slice_index)) as u32);
            last.block_size.set(0);
        }
        if allow_purge {
            self.schedule_purge(self.slice_start(slice), slice_count * SEGMENT_SLICE_SIZE);
        }
        match queue {
            Some(sq) => sq.push(slice),
            None => slice.block_size.set(0), // mark free anyway
        }
    }

    /// `mi_segment_span_remove_from_queue`
    fn span_remove_from_queue(&self, slice: &Slice, tld: &SegmentsTld) {
        debug_assert!(self.kind.get() != SegmentKind::Huge);
        tld.span_queue_for(slice.slice_count.get() as usize)
            .delete(slice);
    }

    /// `mi_segment_span_free_coalesce`: free a span, merging with free
    /// neighbors. Returns the (possibly extended) span head.
    pub fn span_free_coalesce<'a>(&'a self, slice: &'a Slice, tld: &SegmentsTld) -> &'a Slice {
        debug_assert!(slice.slice_count.get() > 0 && slice.slice_offset.get() == 0);
        if self.kind.get() == SegmentKind::Huge {
            slice.block_size.set(0);
            return slice;
        }
        let is_abandoned = self.is_abandoned();
        let mut slice_count = slice.slice_count.get() as usize;
        let idx = self.slice_index(slice);
        let mut head_idx = idx;
        let next_idx = idx + slice_count;
        if next_idx < self.slice_entries.get() {
            let next = &self.slices[next_idx];
            if next.block_size.get() == 0 {
                slice_count += next.slice_count.get() as usize;
                if !is_abandoned {
                    self.span_remove_from_queue(next, tld);
                }
            }
        }
        if idx > 0 {
            let prev = self.slices[idx - 1].slice_first();
            if prev.block_size.get() == 0 {
                slice_count += prev.slice_count.get() as usize;
                let prev_idx = self.slice_index(prev);
                slice.slice_count.set(0);
                slice
                    .slice_offset
                    .set((size_of::<Slice>() * (idx - prev_idx)) as u32);
                if !is_abandoned {
                    self.span_remove_from_queue(prev, tld);
                }
                head_idx = prev_idx;
            }
        }
        self.span_free(head_idx, slice_count, true, tld);
        &self.slices[head_idx]
    }

    /// `mi_segment_span_allocate`: turn a free span into a page.
    /// Returns `None` if committing failed.
    fn span_allocate(&self, slice_index: usize, slice_count: usize) -> Option<&Page> {
        let slice = &self.slices[slice_index];
        debug_assert!(slice.block_size.get() <= 1);
        let (start, _) = self.page_start_from_slice(slice, 0);
        if !self.ensure_committed(start, slice_count * SEGMENT_SLICE_SIZE) {
            return None;
        }
        slice.slice_offset.set(0);
        slice.slice_count.set(slice_count as u32);
        let bsize = slice_count * SEGMENT_SLICE_SIZE;
        slice.block_size.set(bsize);

        // Back-offsets for interior pointer lookup: first
        // MAX_SLICE_OFFSET_COUNT entries and the last one (for coalescing).
        let mut extra = (slice_count - 1).min(MAX_SLICE_OFFSET_COUNT);
        if slice_index + extra >= self.slice_entries.get() {
            extra = self.slice_entries.get() - slice_index - 1;
        }
        for i in 1..=extra {
            let s = &self.slices[slice_index + i];
            s.slice_offset.set((size_of::<Slice>() * i) as u32);
            s.slice_count.set(0);
            s.block_size.set(1);
        }
        let last_index = (slice_index + slice_count - 1).min(self.slice_entries.get());
        if last_index > slice_index {
            let last = &self.slices[last_index];
            last.slice_offset
                .set((size_of::<Slice>() * (last_index - slice_index)) as u32);
            last.slice_count.set(0);
            last.block_size.set(1);
        }

        let page = slice;
        page.is_committed.set(true);
        page.is_zero_init.set(self.free_is_zero.get());
        page.is_huge.set(self.kind.get() == SegmentKind::Huge);
        self.used.set(self.used.get() + 1);
        Some(page)
    }

    /// `mi_segment_slice_split`: split a used span, freeing the tail.
    fn slice_split(&self, slice: &Slice, slice_count: usize, tld: &SegmentsTld) {
        debug_assert!(slice.slice_count.get() as usize >= slice_count);
        debug_assert!(slice.block_size.get() > 0);
        if slice.slice_count.get() as usize <= slice_count {
            return;
        }
        debug_assert!(self.kind.get() != SegmentKind::Huge);
        let next_index = self.slice_index(slice) + slice_count;
        let next_count = slice.slice_count.get() as usize - slice_count;
        self.span_free(next_index, next_count, false, tld);
        slice.slice_count.set(slice_count as u32);
    }
}

/// `mi_segments_page_find_and_allocate`: best-fit search in the span queues.
fn page_find_and_allocate<'a>(
    slice_count: usize,
    req_arena: crate::arena::ArenaId,
    tld: &SegmentsTld,
) -> Option<&'a Page> {
    debug_assert!(slice_count * SEGMENT_SLICE_SIZE <= LARGE_OBJ_SIZE_MAX);
    let start_bin = slice_bin(slice_count);
    let slice_count = slice_count.max(1);
    for sq in &tld.spans[start_bin..] {
        let mut p = sq.first.get();
        // SAFETY: queue nodes are live slices in live segments.
        while let Some(slice) = unsafe { p.as_ref() } {
            if slice.slice_count.get() as usize >= slice_count
                && slice.segment().memid_suitable(req_arena)
            {
                let segment = slice.segment();
                sq.delete(slice);
                if slice.slice_count.get() as usize > slice_count {
                    segment.slice_split(slice, slice_count, tld);
                }
                debug_assert!(
                    slice.slice_count.get() as usize == slice_count && slice.block_size.get() > 0
                );
                let index = segment.slice_index(slice);
                return match segment.span_allocate(index, slice_count) {
                    Some(page) => Some(page),
                    None => {
                        // commit failed; restore the slice
                        segment.span_free_coalesce(slice, tld);
                        None
                    }
                };
            }
            p = slice.next.get();
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Segment allocation
// ---------------------------------------------------------------------------

/// `mi_segment_calculate_slices`
fn calculate_slices(mut required: usize) -> (usize, usize) {
    let page_size = os::page_size();
    let mut isize_ = align_up(size_of::<Segment>(), page_size);
    let mut guardsize = 0;
    if cfg!(feature = "secure") {
        // a protected page between the segment info and the page data,
        // and one at the end of the segment
        guardsize = page_size;
        if required > 0 {
            required = align_up(required, SEGMENT_SLICE_SIZE) + page_size;
        }
    }
    isize_ = align_up(isize_ + guardsize, SEGMENT_SLICE_SIZE);
    let info_slices = isize_ / SEGMENT_SLICE_SIZE;
    let segment_size = if required == 0 {
        SEGMENT_SIZE
    } else {
        align_up(required + isize_ + guardsize, SEGMENT_SLICE_SIZE)
    };
    (segment_size / SEGMENT_SLICE_SIZE, info_slices)
}

/// `mi_segment_os_alloc` (direct OS allocation; arenas come later).
fn segment_os_alloc<'a>(
    page_alignment: usize,
    mut segment_slices: usize,
    info_slices: usize,
    commit: bool,
    required: usize,
    req_arena: crate::arena::ArenaId,
    tld: &SegmentsTld,
) -> Option<(&'a Segment, usize)> {
    let mut alignment = SEGMENT_ALIGN;
    let mut align_offset = 0;
    if page_alignment > 0 {
        debug_assert!(page_alignment >= SEGMENT_ALIGN);
        alignment = page_alignment;
        let info_size = info_slices * SEGMENT_SLICE_SIZE;
        align_offset = align_up(info_size, SEGMENT_ALIGN);
        let extra = align_offset - info_size;
        (segment_slices, _) = calculate_slices(required + extra);
    }
    let segment_size = segment_slices * SEGMENT_SLICE_SIZE;
    // Prefer the arenas for standard-aligned allocations (syscall-free
    // reuse, batched purging); fall back to the OS segment cache / mmap.
    // Heaps bound to a specific arena always try it, rounding odd huge
    // sizes up to whole blocks.
    if align_offset == 0
        && alignment == SEGMENT_ALIGN
        && (segment_size.is_multiple_of(crate::arena::ARENA_BLOCK_SIZE)
            || req_arena != crate::arena::ARENA_ID_NONE)
        && let Some((p, is_zero, aref)) = crate::arena::alloc(
            segment_size.div_ceil(crate::arena::ARENA_BLOCK_SIZE),
            req_arena,
        )
    {
        let mem = OsMem {
            base: p,
            size: segment_size,
            align: alignment,
            is_committed: true,
            is_zero,
        };
        if !is_zero {
            // SAFETY: the header is zeroed below, before the reference is used.
            unsafe { ptr::write_bytes(p, 0, size_of::<Segment>()) };
        }
        // SAFETY: aligned, committed, header zeroed/zero.
        let segment: &Segment = unsafe { &*p.cast() };
        segment.commit_mask.set_full();
        segment.memid.set(MemId {
            mem,
            source: MemSource::Arena(aref),
            is_pinned: false,
            initially_committed: true,
            initially_zero: is_zero,
        });
        segment.allow_decommit.set(true);
        segment.allow_purge.set(options::purge_delay() >= 0);
        segment.segment_size.set(segment_size);
        segment.purge_expire.set(0);
        segment.purge_mask.clear_all();
        segment.free_is_zero.set(is_zero);
        tld.track_size(segment_size as isize);
        return Some((segment, segment_slices));
    }
    // A heap bound to a specific arena must never fall back to the OS
    // (mirrors `_mi_arena_alloc_aligned`, which fails with ENOMEM when an
    // arena id is requested): pages of an OS segment are not
    // `memid_suitable` for such a heap, so `segments_page_alloc` would loop
    // forever allocating segments it can never use — and a huge page would
    // silently escape the reservation.
    if req_arena != crate::arena::ARENA_ID_NONE {
        return None;
    }
    // Allocate so that `base + align_offset` is `alignment`-aligned.
    let (p, mem) = if align_offset == 0 {
        if segment_size == SEGMENT_SIZE
            && alignment == SEGMENT_ALIGN
            && let Some(cached) = os::cache_pop()
        {
            cached
        } else {
            OsMem::alloc_aligned(segment_size, alignment, commit)?
        }
    } else {
        let (p, mem) = OsMem::alloc_aligned(segment_size + alignment, alignment, commit)?;
        // SAFETY: offset stays inside the over-allocated mapping.
        (unsafe { p.add(alignment - align_offset) }, mem)
    };
    let commit_needed = (info_slices * SEGMENT_SLICE_SIZE).div_ceil(COMMIT_SIZE);
    if !mem.is_committed {
        // SAFETY: committing the info prefix of our fresh mapping.
        unsafe { os::commit(p, commit_needed * COMMIT_SIZE) };
    }
    // Zero the whole header + slices metadata before creating the &Segment:
    // reused regions hold stale or partially-uninitialized bytes, `Cell::set`
    // reads the old value, and writing through the raw pointer *after*
    // taking the reference would invalidate it. The all-zero pattern is
    // valid for every field. (Fresh OS memory is already zero.)
    if !mem.is_zero {
        // SAFETY: the info area is committed and covers the Segment struct.
        unsafe { ptr::write_bytes(p, 0, size_of::<Segment>()) };
    }
    // SAFETY: header initialized just above; slice metadata is initialized
    // by `segment_alloc` before use.
    let segment: &Segment = unsafe { &*p.cast() };
    if mem.is_committed {
        segment.commit_mask.set_full();
    } else {
        segment
            .commit_mask
            .copy_from(&CommitMask::create(0, commit_needed));
    }
    segment.memid.set(MemId {
        mem,
        source: MemSource::Os,
        is_pinned: false,
        initially_committed: mem.is_committed,
        initially_zero: mem.is_zero,
    });
    segment.allow_decommit.set(true);
    segment.allow_purge.set(options::purge_delay() >= 0);
    segment.segment_size.set(segment_size);
    segment.purge_expire.set(0);
    segment.purge_mask.clear_all();
    segment.free_is_zero.set(mem.is_zero);
    tld.track_size(segment_size as isize);
    Some((segment, segment_slices))
}

/// `mi_segment_alloc`: allocate a fresh segment; if `required > 0` it is a
/// huge segment and the huge page is returned too.
fn segment_alloc<'a>(
    required: usize,
    page_alignment: usize,
    req_arena: crate::arena::ArenaId,
    tld: &SegmentsTld,
) -> Option<(&'a Segment, Option<&'a Page>)> {
    let (segment_slices, info_slices) = calculate_slices(required);
    let eager_delay = crate::init::current_thread_count() > 1
        && tld.peak_count.get() < options::eager_commit_delay() as usize;
    let eager = !eager_delay && options::eager_commit() == 1;
    let commit = eager || required > 0;

    let (segment, segment_slices) = segment_os_alloc(
        page_alignment,
        segment_slices,
        info_slices,
        commit,
        required,
        req_arena,
        tld,
    )?;

    let mut slice_entries = segment_slices.min(SLICES_PER_SEGMENT);
    let mut guard_slices = 0;
    if cfg!(feature = "secure") {
        // Guard page between the segment metadata and the page data, and
        // one at the very end of the segment; a trailing slice is
        // sacrificed so the guard never overlaps usable block area.
        let ps = os::page_size();
        let info_size = info_slices * SEGMENT_SLICE_SIZE;
        let seg_size = segment_slices * SEGMENT_SLICE_SIZE; // true size (huge > 32MiB)
        // SAFETY: both pages are inside our mapping; the end page is
        // committed first if the segment was reserved lazily.
        unsafe {
            os::protect(segment.as_ptr().add(info_size - ps), ps);
            let end = segment.as_ptr().add(seg_size - ps);
            if !commit {
                os::commit(end, ps);
            }
            os::protect(end, ps);
        }
        if slice_entries == segment_slices {
            slice_entries -= 1; // don't use the last slice
        }
        guard_slices = 1;
    }
    segment.segment_slices.set(segment_slices);
    segment.segment_info_slices.set(info_slices);
    segment
        .thread_id
        .store(crate::init::thread_id(), Ordering::Release);
    segment
        .cookie
        .set(segment.as_ptr().addr() ^ 0xa5a5_5a5a_1234_5678);
    segment.slice_entries.set(slice_entries);
    segment.kind.set(if required == 0 {
        SegmentKind::Normal
    } else {
        SegmentKind::Huge
    });

    // Reserve the first slices for the segment info.
    let page0 = segment.span_allocate(0, info_slices)?;
    debug_assert!(ptr::eq(page0, &segment.slices[0]));
    segment.used.set(0); // info slices don't count toward usage

    if segment.kind.get() == SegmentKind::Normal {
        segment.span_free(info_slices, slice_entries - info_slices, false, tld);
        Some((segment, None))
    } else {
        let huge =
            segment.span_allocate(info_slices, segment_slices - info_slices - guard_slices)?;
        Some((segment, Some(huge)))
    }
}

/// `mi_segment_os_free`
fn segment_os_free(segment: &Segment, tld: &SegmentsTld) {
    segment.thread_id.store(0, Ordering::Relaxed);
    tld.track_size(-(segment.size() as isize));
    if segment.was_reclaimed.get() {
        tld.reclaim_count.set(tld.reclaim_count.get() - 1);
        segment.was_reclaimed.set(false);
    }
    if cfg!(feature = "secure") {
        // Remove the guard pages so the memory can be recycled.
        let ps = os::page_size();
        // SAFETY: the same ranges protected in segment_alloc.
        unsafe {
            os::unprotect(segment.as_ptr().add(segment.info_size() - ps), ps);
            os::unprotect(segment.as_ptr().add(segment.size() - ps), ps);
        }
    }
    let memid = segment.memid.get();
    match memid.source {
        MemSource::Arena(aref) => {
            // Returning blocks only flips bitmap bits — no dealloc happens
            // while references into the segment are still on the stack.
            crate::arena::free(aref, segment.commit_mask.is_full());
        }
        MemSource::Os => {
            // Cache standard segments for syscall-free reuse (the mapping
            // stays committed: stat-neutral until eviction); otherwise
            // defer the unmap since references into the segment may still
            // be on the call stack.
            let mem = memid.mem;
            let standard = mem.size == SEGMENT_SIZE && mem.base.addr() & SEGMENT_MASK == 0;
            if !os::cache_push(mem, standard) {
                os::defer_free(mem);
            }
        }
    }
}

/// `mi_segment_free`
fn segment_free(segment: &Segment, tld: &SegmentsTld) {
    debug_assert!(segment.used.get() == 0);
    if segment.dont_free.get() {
        return;
    }
    // Remove remaining free spans from the queues.
    let mut p = segment.slices.as_ptr();
    while p < segment.slices_end() {
        // SAFETY: span heads partition the slices array.
        let slice = unsafe { &*p };
        debug_assert!(slice.slice_count.get() > 0 && slice.slice_offset.get() == 0);
        if slice.block_size.get() == 0 && segment.kind.get() != SegmentKind::Huge {
            segment.span_remove_from_queue(slice, tld);
        }
        // SAFETY: stays within slices array bounds.
        p = unsafe { p.add(slice.slice_count.get() as usize) };
    }
    segment_os_free(segment, tld);
}

// ---------------------------------------------------------------------------
// Page free / abandonment
// ---------------------------------------------------------------------------

impl Segment {
    /// `mi_segment_page_clear`: release a fully-free page back to the segment.
    fn page_clear<'a>(&'a self, page: &'a Page, tld: &SegmentsTld) -> &'a Slice {
        debug_assert!(page.all_free());
        debug_assert!(self.used.get() > 0);

        // Zero the page metadata (but keep segment fields and the heap tag).
        page.is_zero_init.set(false);
        let heap_tag = page.heap_tag.get();
        page.capacity.set(0);
        page.reserved.set(0);
        page.set_in_full(false);
        page.set_has_aligned(false);
        page.free_is_zero.set(false);
        page.retire_expire.set(0);
        page.free.set(ptr::null_mut());
        page.local_free.set(ptr::null_mut());
        page.used.set(0);
        page.block_size_shift.set(0);
        page.block_size.set(1);
        page.block_recip.set(0);
        page.heap_tag.set(heap_tag);
        page.page_start.set(ptr::null_mut());
        page.xthread_free.store(0, Ordering::Relaxed);
        page.xheap.store(0, Ordering::Relaxed);
        page.next.set(ptr::null_mut());
        page.prev.set(ptr::null_mut());

        crate::stats::STATS.pages.decrease(1);
        let slice = self.span_free_coalesce(page, tld);
        self.used.set(self.used.get() - 1);
        self.free_is_zero.set(false);
        slice
    }

    /// Debug validator (port of `mi_segment_is_valid`), asserting the
    /// Lean-proven span invariants (`Spans.lean`) on a live segment:
    /// span heads partition the slice array exactly (`norm_total`), all
    /// spans are non-empty (`norm_wf`), no two adjacent free spans exist
    /// (`norm_coalesced`), and back-offsets let interior pointers and
    /// coalescing find span heads.
    #[cfg(debug_assertions)]
    pub fn assert_valid(&self) {
        if self.kind.get() != SegmentKind::Normal {
            return; // huge layouts intentionally exceed `slice_entries`
        }
        let entries = self.slice_entries.get();
        let mut idx = 0;
        let mut total = 0;
        let mut prev_free = false;
        while idx < entries {
            let head = &self.slices[idx];
            let count = head.slice_count.get() as usize;
            debug_assert!(count > 0, "empty span at {idx}");
            debug_assert!(
                head.slice_offset.get() == 0,
                "span head with offset at {idx}"
            );
            let is_free = head.block_size.get() == 0;
            debug_assert!(
                !(prev_free && is_free),
                "adjacent free spans at {idx} (coalescing incomplete)"
            );
            prev_free = is_free;
            // back-offsets: interior entries point at the head
            let last = (idx + count - 1).min(entries - 1);
            if is_free {
                if last > idx {
                    let l = &self.slices[last];
                    debug_assert!(
                        l.slice_offset.get() as usize == size_of::<Slice>() * (last - idx),
                        "free-span tail back-offset wrong at {last}"
                    );
                }
            } else {
                let extra = (count - 1)
                    .min(MAX_SLICE_OFFSET_COUNT)
                    .min(entries - idx - 1);
                for i in 1..=extra {
                    let e = &self.slices[idx + i];
                    debug_assert!(
                        e.slice_offset.get() as usize == size_of::<Slice>() * i
                            && e.slice_count.get() == 0,
                        "used-span back-offset wrong at {}",
                        idx + i
                    );
                }
            }
            total += count;
            idx += count;
        }
        debug_assert!(
            total == entries,
            "span partition broken: covers {total} of {entries} slices"
        );
    }

    /// `_mi_segment_page_free`
    pub fn page_free(&self, page: &Page, _force: bool, tld: &SegmentsTld) {
        #[cfg(debug_assertions)]
        self.assert_valid();
        self.page_clear(page, tld);
        if self.used.get() == 0 {
            segment_free(self, tld);
        } else if self.used.get() == self.abandoned.get() {
            self.abandon(tld);
        } else {
            self.try_purge(false);
        }
    }

    /// `mi_segment_abandon`: all pages are abandoned; move the segment to
    /// the global abandoned list.
    fn abandon(&self, tld: &SegmentsTld) {
        debug_assert!(self.used.get() == self.abandoned.get() && self.used.get() > 0);
        debug_assert!(self.abandoned_visits.get() == 0);

        // Remove the free spans from the span queues (they stay marked free).
        let mut p = self.slices.as_ptr();
        while p < self.slices_end() {
            // SAFETY: span heads partition the slices array.
            let slice = unsafe { &*p };
            if slice.block_size.get() == 0 && self.kind.get() != SegmentKind::Huge {
                self.span_remove_from_queue(slice, tld);
                slice.block_size.set(0); // keep it free
            }
            // SAFETY: stays in bounds.
            p = unsafe { p.add(slice.slice_count.get() as usize) };
        }

        self.try_purge(options::abandoned_page_purge() == 1);

        crate::stats::STATS
            .segments_abandoned
            .fetch_add(1, core::sync::atomic::Ordering::Relaxed);
        tld.track_size(-(self.size() as isize));
        self.thread_id.store(0, Ordering::Release);
        self.abandoned_visits.set(1);
        if self.was_reclaimed.get() {
            tld.reclaim_count.set(tld.reclaim_count.get() - 1);
            self.was_reclaimed.set(false);
        }
        abandoned::mark(self);
    }

    /// `_mi_segment_page_abandon`
    pub fn page_abandon(&self, page: &Page, tld: &SegmentsTld) {
        debug_assert!(page.heap().is_null());
        self.abandoned.set(self.abandoned.get() + 1);
        debug_assert!(self.abandoned.get() <= self.used.get());
        if self.used.get() == self.abandoned.get() {
            self.abandon(tld);
        }
        let _ = page;
    }
}

/// Global registry of abandoned segments (stand-in for the arena bitmaps +
/// subprocess OS-list of mimalloc; all our segments are OS-allocated).
pub mod abandoned {
    use super::Segment;
    use core::ptr;
    use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    struct List {
        lock: AtomicBool,
        first: core::cell::Cell<*mut Segment>,
        last: core::cell::Cell<*mut Segment>,
    }
    // SAFETY: all access happens under `lock`.
    unsafe impl Sync for List {}

    pub static RECLAIM_TRIES: AtomicUsize = AtomicUsize::new(0);
    pub static RECLAIM_HITS: AtomicUsize = AtomicUsize::new(0);

    static LIST: List = List {
        lock: AtomicBool::new(false),
        first: core::cell::Cell::new(ptr::null_mut()),
        last: core::cell::Cell::new(ptr::null_mut()),
    };
    static COUNT: AtomicUsize = AtomicUsize::new(0);

    pub fn count() -> usize {
        COUNT.load(Ordering::Relaxed)
    }

    #[cfg(not(windows))]
    pub(crate) fn lock_for_fork() {
        crate::sync::spin_acquire(&LIST.lock);
    }
    #[cfg(not(windows))]
    pub(crate) fn unlock_for_fork() {
        crate::sync::spin_release(&LIST.lock);
    }

    fn locked<R>(f: impl FnOnce() -> R) -> R {
        crate::sync::spin_locked(&LIST.lock, f)
    }

    /// `_mi_arena_segment_mark_abandoned` (OS-list flavor): push at the end.
    pub fn mark(segment: &Segment) {
        locked(|| {
            link_last(segment);
        });
        COUNT.fetch_add(1, Ordering::Relaxed);
    }

    /// Append `segment` at the tail. Lock must be held.
    fn link_last(segment: &Segment) {
        segment.abandoned_os_next.set(ptr::null_mut());
        segment.abandoned_os_prev.set(LIST.last.get());
        segment.abandoned_os_listed.set(true);
        // SAFETY: list nodes are live abandoned segments.
        match unsafe { LIST.last.get().as_ref() } {
            Some(last) => last
                .abandoned_os_next
                .set(ptr::from_ref(segment).cast_mut()),
            None => LIST.first.set(ptr::from_ref(segment).cast_mut()),
        }
        LIST.last.set(ptr::from_ref(segment).cast_mut());
    }

    fn unlink(segment: &Segment) {
        // SAFETY: list nodes are live abandoned segments.
        unsafe {
            match segment.abandoned_os_prev.get().as_ref() {
                Some(prev) => prev.abandoned_os_next.set(segment.abandoned_os_next.get()),
                None => LIST.first.set(segment.abandoned_os_next.get()),
            }
            match segment.abandoned_os_next.get().as_ref() {
                Some(next) => next.abandoned_os_prev.set(segment.abandoned_os_prev.get()),
                None => LIST.last.set(segment.abandoned_os_prev.get()),
            }
        }
        segment.abandoned_os_next.set(ptr::null_mut());
        segment.abandoned_os_prev.set(ptr::null_mut());
        segment.abandoned_os_listed.set(false);
        COUNT.fetch_sub(1, Ordering::Relaxed);
    }

    /// `_mi_arena_segment_clear_abandoned`: atomically un-abandon a specific
    /// segment. Returns false if some other thread claimed it first.
    pub fn clear(segment: &Segment) -> bool {
        locked(|| {
            let on_list = segment.abandoned_os_listed.get();
            if on_list {
                unlink(segment);
            }
            on_list
        })
    }

    /// Pop the first abandoned segment, if any.
    pub fn pop<'a>() -> Option<&'a Segment> {
        if COUNT.load(Ordering::Relaxed) == 0 {
            return None;
        }
        locked(|| {
            // SAFETY: list nodes are live abandoned segments.
            let segment = unsafe { LIST.first.get().as_ref() }?;
            unlink(segment);
            Some(segment)
        })
    }

    /// Detach up to `n` segments from the head in a single lock acquisition.
    /// Returns a chain linked through `abandoned_os_next`; every node is off
    /// the list (`clear` on it returns false) and exclusively ours.
    pub fn take<'a>(n: usize) -> Option<&'a Segment> {
        if n == 0 || COUNT.load(Ordering::Relaxed) == 0 {
            return None;
        }
        let (head, taken) = locked(|| {
            let head = LIST.first.get();
            // SAFETY: list nodes are live abandoned segments (lock held).
            let Some(mut tail) = (unsafe { head.as_ref() }) else {
                return (head, 0);
            };
            let mut taken = 1usize;
            tail.abandoned_os_prev.set(ptr::null_mut());
            tail.abandoned_os_listed.set(false);
            while taken < n
                // SAFETY: list nodes are live abandoned segments (lock held).
                && let Some(next) = unsafe { tail.abandoned_os_next.get().as_ref() }
            {
                tail = next;
                tail.abandoned_os_prev.set(ptr::null_mut());
                tail.abandoned_os_listed.set(false);
                taken += 1;
            }
            let rest = tail.abandoned_os_next.get();
            tail.abandoned_os_next.set(ptr::null_mut());
            LIST.first.set(rest);
            // SAFETY: as above.
            match unsafe { rest.as_ref() } {
                Some(r) => r.abandoned_os_prev.set(ptr::null_mut()),
                None => LIST.last.set(ptr::null_mut()),
            }
            (head, taken)
        });
        if taken > 0 {
            COUNT.fetch_sub(taken, Ordering::Relaxed);
        }
        // SAFETY: chain head is a live abandoned segment owned by us.
        unsafe { head.as_ref() }
    }

    /// Re-abandon a chain of `count` segments (linked via `abandoned_os_next`,
    /// as built by the caller) in a single lock acquisition.
    pub fn splice(head: *mut Segment, count: usize) {
        if head.is_null() {
            return;
        }
        locked(|| {
            let mut p = head;
            // SAFETY: chain nodes are live abandoned segments owned by the
            // caller until spliced (lock held).
            while let Some(segment) = unsafe { p.as_ref() } {
                let next = segment.abandoned_os_next.get();
                link_last(segment);
                p = next;
            }
        });
        COUNT.fetch_add(count, Ordering::Relaxed);
    }
}

// ---------------------------------------------------------------------------
// Reclaim
// ---------------------------------------------------------------------------

/// Iterate span heads, skipping the initial info span.
fn slices_start_iterate(segment: &Segment) -> (*const Slice, *const Slice) {
    let slice = segment.slices.as_ptr();
    // SAFETY: first span is the info span; its count is in bounds.
    let start = unsafe { slice.add((*slice).slice_count.get() as usize) };
    (start, segment.slices_end())
}

/// `mi_segment_check_free`: collect frees; report if a usable page/span exists.
fn segment_check_free(
    segment: &Segment,
    slices_needed: usize,
    block_size: usize,
    tld: &SegmentsTld,
) -> bool {
    debug_assert!(segment.is_abandoned());
    let mut has_page = false;
    let (mut p, end) = slices_start_iterate(segment);
    while p < end {
        // SAFETY: span heads partition the slices array.
        let mut slice = unsafe { &*p };
        if slice.slice_is_used() {
            let page = slice;
            crate::page_ops::page_free_collect(page, false);
            if page.all_free() {
                segment.abandoned.set(segment.abandoned.get() - 1);
                slice = segment.page_clear(page, tld);
                if slice.slice_count.get() as usize >= slices_needed {
                    has_page = true;
                }
            } else if page.block_size() == block_size && page.has_any_available() {
                has_page = true;
            }
        } else if slice.slice_count.get() as usize >= slices_needed {
            has_page = true;
        }
        let next = segment.slice_index(slice) + slice.slice_count.get() as usize;
        // SAFETY: span heads stay within the slices array.
        p = unsafe { segment.slices.as_ptr().add(next) };
    }
    has_page
}

/// `mi_segment_reclaim`: take ownership of an abandoned segment.
/// Returns `None` if the segment was freed entirely.
fn segment_reclaim<'a>(
    segment: &'a Segment,
    heap: &Heap,
    requested_block_size: usize,
    mut right_page_reclaimed: Option<&mut bool>,
    tld: &SegmentsTld,
) -> Option<&'a Segment> {
    if let Some(flag) = right_page_reclaimed.as_deref_mut() {
        *flag = false;
    }
    segment
        .thread_id
        .store(crate::init::thread_id(), Ordering::Release);
    segment.abandoned_visits.set(0);
    segment.was_reclaimed.set(true);
    tld.reclaim_count.set(tld.reclaim_count.get() + 1);
    tld.track_size(segment.size() as isize);
    debug_assert!(segment.next.get().is_null());

    let (mut p, end) = slices_start_iterate(segment);
    while p < end {
        // SAFETY: span heads partition the slices array.
        let mut slice = unsafe { &*p };
        if slice.slice_is_used() {
            let page = slice;
            debug_assert!(page.is_committed.get());
            debug_assert!(page.heap().is_null());
            segment.abandoned.set(segment.abandoned.get() - 1);
            page.set_heap(ptr::from_ref(heap).cast_mut());
            crate::page_ops::page_use_delayed_free(
                page,
                crate::page::Delayed::UseDelayedFree,
                true,
            );
            crate::page_ops::page_free_collect(page, false);
            if page.all_free() {
                slice = segment.page_clear(page, tld);
            } else {
                crate::page_ops::page_reclaim(heap, page);
                if requested_block_size == page.block_size()
                    && page.has_any_available()
                    && let Some(flag) = right_page_reclaimed.as_deref_mut()
                {
                    *flag = true;
                }
            }
        } else {
            slice = segment.span_free_coalesce(slice, tld);
        }
        let next = segment.slice_index(slice) + slice.slice_count.get() as usize;
        // SAFETY: span heads stay within the slices array.
        p = unsafe { segment.slices.as_ptr().add(next) };
    }

    debug_assert!(segment.abandoned.get() == 0);
    if segment.used.get() == 0 {
        segment_free(segment, tld);
        None
    } else {
        Some(segment)
    }
}

/// `_mi_segment_attempt_reclaim`: called on a cross-thread free into an
/// abandoned segment.
pub fn attempt_reclaim(heap: &Heap, segment: &Segment) -> bool {
    if !segment.is_abandoned() || !segment.memid_suitable(heap.arena_id.get()) {
        return false;
    }
    let target = options::target_segments_per_thread();
    let tld = heap.segments_tld();
    if target > 0 && target as usize <= tld.count.get() {
        return false;
    }
    if abandoned::clear(segment) {
        segment_reclaim(segment, heap, 0, None, tld).is_some()
    } else {
        false
    }
}

/// `_mi_abandoned_reclaim_all`
pub fn reclaim_all(heap: &Heap, tld: &SegmentsTld) {
    while let Some(segment) = abandoned::pop() {
        segment_reclaim(segment, heap, 0, None, tld);
    }
}

/// `mi_segment_try_reclaim`
fn try_reclaim<'a>(
    heap: &Heap,
    needed_slices: usize,
    block_size: usize,
    reclaimed: &mut bool,
    tld: &SegmentsTld,
) -> Option<&'a Segment> {
    *reclaimed = false;
    let mut max_tries = {
        // ~10% of the abandoned count, clamped to [8, 1024].
        let perc = options::max_segment_reclaim().clamp(0, 100) as usize;
        let total = abandoned::count();
        if perc == 0 || total == 0 {
            return None;
        }
        let relative = (total * perc) / 100;
        let tries = relative.clamp(1, 1024);
        if tries < 8 && total > 8 { 8 } else { tries }
    };
    while max_tries > 0 {
        max_tries -= 1;
        let target = options::target_segments_per_thread().clamp(0, 1024) as usize;
        if target != 0 && tld.count.get() >= target {
            break;
        }
        let Some(segment) = abandoned::pop() else {
            break;
        };
        abandoned::RECLAIM_TRIES.fetch_add(1, Ordering::Relaxed);
        segment
            .abandoned_visits
            .set(segment.abandoned_visits.get() + 1);
        let has_page = segment_check_free(segment, needed_slices, block_size, tld);
        if segment.used.get() == 0 {
            segment_reclaim(segment, heap, 0, None, tld);
        } else if has_page {
            abandoned::RECLAIM_HITS.fetch_add(1, Ordering::Relaxed);
            return segment_reclaim(segment, heap, block_size, Some(reclaimed), tld);
        } else if segment.abandoned_visits.get() > 3 {
            segment_reclaim(segment, heap, 0, None, tld);
        } else {
            max_tries += 1; // not a real try
            segment.try_purge(false);
            abandoned::mark(segment);
        }
    }
    None
}

/// `_mi_abandoned_collect`
pub fn abandoned_collect(heap: &Heap, force: bool, tld: &SegmentsTld) {
    // One pass over the segments abandoned at entry (the C version's arena
    // cursor visits each at most once); re-pushed segments are not revisited.
    // The whole batch is detached in one lock acquisition and survivors are
    // re-abandoned in one more, instead of locking twice per segment.
    let max_tries = if force {
        abandoned::count()
    } else {
        abandoned::count().min(1024)
    };
    let mut p = abandoned::take(max_tries);
    let mut keep_head: *mut Segment = ptr::null_mut();
    let mut keep_tail: *mut Segment = ptr::null_mut();
    let mut keep_count = 0usize;
    while let Some(segment) = p {
        // SAFETY: the chain was detached by `take`; read the link before the
        // segment is potentially freed below.
        p = unsafe { segment.abandoned_os_next.get().as_ref() };
        segment.abandoned_os_next.set(ptr::null_mut());
        segment_check_free(segment, 0, 0, tld);
        if segment.used.get() == 0 {
            segment_reclaim(segment, heap, 0, None, tld);
        } else {
            segment.try_purge(force);
            let sp = ptr::from_ref(segment).cast_mut();
            if keep_tail.is_null() {
                keep_head = sp;
            } else {
                // SAFETY: keep_tail is a live segment owned by this chain.
                unsafe { (*keep_tail).abandoned_os_next.set(sp) };
            }
            keep_tail = sp;
            keep_count += 1;
        }
    }
    abandoned::splice(keep_head, keep_count);
}

// ---------------------------------------------------------------------------
// Page allocation entry points
// ---------------------------------------------------------------------------

/// `mi_segment_force_abandon`: abandon every used page of `segment` so the
/// segment becomes reclaimable by other threads. The pages may be freed in
/// the process; `dont_free` keeps the segment metadata valid throughout.
fn segment_force_abandon(segment: &Segment) {
    debug_assert!(!segment.is_abandoned());
    segment.dont_free.set(true);
    let (start, end) = slices_start_iterate(segment);
    let mut p = start;
    while p < end {
        // SAFETY: span heads partition the slices array.
        let mut slice = unsafe { &*p };
        if slice.slice_is_used() {
            let page = slice.slice_first();
            crate::page_ops::page_free_collect(page, false);
            if segment.used.get() == segment.abandoned.get() + 1 {
                // last in-use page: after abandoning it the segment itself
                // is abandoned and must not be touched again.
                segment.dont_free.set(false);
                crate::page_ops::page_force_abandon(page);
                return;
            }
            crate::page_ops::page_force_abandon(page);
            // the page may have been freed and coalesced backward; restart
            // from the (possibly merged) span head before advancing.
            slice = slice.slice_first();
        }
        let next = segment.slice_index(slice) + slice.slice_count.get() as usize;
        // SAFETY: span heads stay within the slices array.
        p = unsafe { segment.slices.as_ptr().add(next) };
    }
    segment.dont_free.set(false);
}

/// `mi_segments_try_abandon`: when over the per-thread segment target,
/// abandon full-bin segments to increase cross-thread reuse.
fn segments_try_abandon(heap: &Heap, tld: &SegmentsTld) {
    let target = options::target_segments_per_thread();
    if target <= 0 || tld.count.get() < target as usize {
        return;
    }
    let target = target as usize;
    let min_target = if target > 4 { target * 3 / 4 } else { target };
    // Only full-bin pages hold candidate segments (mirrors C's heuristic of
    // not maintaining a per-thread segment list).
    for _ in 0..64 {
        if tld.count.get() < min_target {
            break;
        }
        let mut p = heap.pages[crate::constants::BIN_FULL].first.get();
        // SAFETY: queue nodes are live pages of this heap.
        while let Some(page) = unsafe { p.as_ref() } {
            if page.block_size() <= LARGE_OBJ_SIZE_MAX {
                break;
            }
            p = page.next.get();
        }
        // SAFETY: queue nodes are live pages of this heap.
        let Some(page) = (unsafe { p.as_ref() }) else {
            break;
        };
        segment_force_abandon(page.segment());
    }
}

/// `mi_segment_reclaim_or_alloc`
fn reclaim_or_alloc<'a>(
    heap: &Heap,
    needed_slices: usize,
    block_size: usize,
    tld: &SegmentsTld,
) -> Option<&'a Segment> {
    debug_assert!(block_size <= LARGE_OBJ_SIZE_MAX);
    // Shed excess segments first so other threads can reclaim them.
    segments_try_abandon(heap, tld);
    let mut reclaimed = false;
    let segment = try_reclaim(heap, needed_slices, block_size, &mut reclaimed, tld);
    if reclaimed {
        // The right page is already in the heap's queue; report "no segment"
        // so the caller retries through the page queues.
        return None;
    }
    if let Some(segment) = segment {
        return Some(segment);
    }
    segment_alloc(0, 0, heap.arena_id.get(), tld).map(|(s, _)| s)
}

/// `mi_segments_page_alloc`
fn segments_page_alloc<'a>(
    heap: &Heap,
    required: usize,
    block_size: usize,
    tld: &SegmentsTld,
) -> Option<&'a Page> {
    let page_size = align_up(
        required,
        if required > MEDIUM_PAGE_SIZE {
            MEDIUM_PAGE_SIZE
        } else {
            SEGMENT_SLICE_SIZE
        },
    );
    let slices_needed = page_size / SEGMENT_SLICE_SIZE;
    loop {
        if let Some(page) = page_find_and_allocate(slices_needed, heap.arena_id.get(), tld) {
            page.segment().try_purge(false);
            return Some(page);
        }
        reclaim_or_alloc(heap, slices_needed, block_size, tld)?;
    }
}

/// `mi_segment_huge_page_alloc`
fn huge_page_alloc<'a>(
    size: usize,
    page_alignment: usize,
    req_arena: crate::arena::ArenaId,
    tld: &SegmentsTld,
) -> Option<&'a Page> {
    let (segment, page) = segment_alloc(size, page_alignment, req_arena, tld)?;
    let page = page?;
    debug_assert!(segment.used.get() == 1 && page.block_size() >= size);
    let (start, psize) = segment.page_start(page);
    page.set_block_size(psize);
    debug_assert!(page.is_huge.get());

    // Decommit the unused prefix of very large alignments.
    if page_alignment > 0 && segment.allow_decommit.get() {
        let aligned = align_up(start.addr(), page_alignment);
        debug_assert!(psize - (aligned - start.addr()) >= size);
        // SAFETY: range is inside the page, past the block header word.
        unsafe {
            let decommit_start = start.add(size_of::<usize>()); // keep the free-list word
            let len = aligned - decommit_start.addr();
            if len > 0 {
                os::reset(decommit_start, len);
            }
        }
    }
    Some(page)
}

/// `_mi_segment_huge_page_reset`: a huge block was freed on another thread;
/// release its physical memory while the owner still holds it.
pub fn huge_page_reset(segment: &Segment, page: &Page, block: *mut crate::page::Block) {
    debug_assert!(segment.kind.get() == SegmentKind::Huge);
    debug_assert!(page.used.get() == 1 && page.free.get().is_null());
    if segment.allow_decommit.get() {
        let csize = page.block_size() - size_of::<usize>();
        // SAFETY: the huge block spans the page; skip the first word.
        unsafe {
            os::reset(block.cast::<u8>().add(size_of::<usize>()), csize);
        }
    }
}

/// `_mi_segment_page_alloc`: the entry point from `page.c`.
pub fn segment_page_alloc<'a>(
    heap: &Heap,
    block_size: usize,
    page_alignment: usize,
    tld: &SegmentsTld,
) -> Option<&'a Page> {
    let page = if page_alignment > BLOCK_ALIGNMENT_MAX {
        debug_assert!(is_power_of_two(page_alignment));
        let page_alignment = page_alignment.max(SEGMENT_SIZE);
        huge_page_alloc(block_size, page_alignment, heap.arena_id.get(), tld)?
    } else if block_size <= SMALL_OBJ_SIZE_MAX {
        segments_page_alloc(heap, block_size, block_size, tld)?
    } else if block_size <= MEDIUM_OBJ_SIZE_MAX {
        segments_page_alloc(heap, MEDIUM_PAGE_SIZE, block_size, tld)?
    } else if block_size <= LARGE_OBJ_SIZE_MAX {
        segments_page_alloc(heap, block_size, block_size, tld)?
    } else {
        huge_page_alloc(block_size, page_alignment, heap.arena_id.get(), tld)?
    };
    Some(page)
}
