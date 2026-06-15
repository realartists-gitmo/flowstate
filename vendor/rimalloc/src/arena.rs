//! Arenas (port of `arena.c`): large OS reservations (1GiB+, growing
//! geometrically) divided into segment-sized blocks tracked by atomic
//! bitmaps. Segments are claimed/released with no syscalls; freed blocks
//! are purged (`MADV_FREE`) lazily after a delay, keeping RSS bounded.

use core::ptr;
use core::sync::atomic::{AtomicBool, AtomicI64, AtomicPtr, AtomicUsize, Ordering};

use crate::bitmap::{
    FIELD_BITS, claim_run_field, claim_single_field, clear_committed_run, decommit_run, field_mask,
    prepare_run, purge_field, release_run,
};
use crate::constants::*;
use crate::options;
use crate::os::{self, OsMem};

/// One arena block backs exactly one normal segment.
pub const ARENA_BLOCK_SIZE: usize = SEGMENT_SIZE;
const MAX_ARENAS: usize = 64;
/// Up to 16 GiB per arena (512 blocks, 8 bitmap fields).
const MAX_BLOCK_FIELDS: usize = 8;
const MAX_ARENA_BLOCKS: usize = MAX_BLOCK_FIELDS * FIELD_BITS;

/// Arena identifier: 0 is "none" (any arena); real ids are `index + 1`.
pub type ArenaId = usize;
pub const ARENA_ID_NONE: ArenaId = 0;

pub struct Arena {
    id: ArenaId,
    exclusive: bool,
    numa_node: i32,
    start: *mut u8,
    block_count: usize,
    #[allow(dead_code)] // owns the reservation for the process lifetime
    mem: OsMem,
    purge_expire: AtomicI64,
    claim_lock: AtomicBool, // serializes multi-block claims only
    blocks_inuse: [AtomicUsize; MAX_BLOCK_FIELDS],
    blocks_dirty: [AtomicUsize; MAX_BLOCK_FIELDS], // ever used (not zero)
    blocks_committed: [AtomicUsize; MAX_BLOCK_FIELDS],
    blocks_purge: [AtomicUsize; MAX_BLOCK_FIELDS], // scheduled for purge
}

// SAFETY: all mutable state is atomic.
unsafe impl Sync for Arena {}
// SAFETY: Arena owns its reservation; the raw pointers are plain data.
unsafe impl Send for Arena {}

static ARENAS: [AtomicPtr<Arena>; MAX_ARENAS] =
    [const { AtomicPtr::new(ptr::null_mut()) }; MAX_ARENAS];
static ARENA_COUNT: AtomicUsize = AtomicUsize::new(0);
/// Earliest pending purge across all arenas (0 = none): lock-free gate so
/// the allocation path never scans bitmaps needlessly.
static PURGE_EXPIRE: AtomicI64 = AtomicI64::new(0);
static RESERVE_LOCK: AtomicBool = AtomicBool::new(false);

impl Arena {
    #[inline]
    fn block_ptr(&self, index: usize) -> *mut u8 {
        // SAFETY: index < block_count, within the reservation.
        unsafe { self.start.add(index * ARENA_BLOCK_SIZE) }
    }

    /// Try to claim `count` contiguous blocks. Single blocks are lock-free;
    /// multi-block claims serialize on a per-arena spinlock (rare: only
    /// huge segments).
    fn try_claim(&self, count: usize) -> Option<usize> {
        let fields = self.block_count.div_ceil(FIELD_BITS);
        if count == 1 {
            for (i, field) in self.blocks_inuse[..fields].iter().enumerate() {
                let limit = (self.block_count - i * FIELD_BITS).min(FIELD_BITS);
                if let Some(bit) = claim_single_field(field, limit) {
                    return Some(i * FIELD_BITS + bit);
                }
            }
            return None;
        }
        if count > FIELD_BITS {
            return None; // >2GiB objects fall back to the OS
        }
        // multi-block: spinlock + scan within fields
        crate::sync::spin_acquire(&self.claim_lock);
        let mut found = None;
        for (i, field) in self.blocks_inuse[..fields].iter().enumerate() {
            let limit = (self.block_count - i * FIELD_BITS).min(FIELD_BITS);
            if let Some(bit) = claim_run_field(field, limit, count) {
                found = Some(i * FIELD_BITS + bit);
                break;
            }
        }
        crate::sync::spin_release(&self.claim_lock);
        found
    }

    /// Post-claim bookkeeping: ensure committed, compute zero/commit state.
    fn prepare(&self, index: usize, count: usize) -> (bool, bool) {
        let (fi, bit) = (index / FIELD_BITS, index % FIELD_BITS);
        let is_zero = prepare_run(
            &self.blocks_dirty[fi],
            &self.blocks_committed[fi],
            &self.blocks_purge[fi],
            bit,
            count,
            || {
                // SAFETY: the claimed range is exclusively ours.
                unsafe { os::commit(self.block_ptr(index), count * ARENA_BLOCK_SIZE) };
            },
        );
        (is_zero, true)
    }

    fn release(&self, index: usize, count: usize) {
        let (fi, bit) = (index / FIELD_BITS, index % FIELD_BITS);
        // schedule the physical pages for release before making the blocks
        // claimable again
        let expire = options::clock_now() + options::purge_delay().max(1);
        self.purge_expire.store(expire, Ordering::Relaxed);
        let cur = PURGE_EXPIRE.load(Ordering::Relaxed);
        if cur == 0 || expire < cur {
            PURGE_EXPIRE.store(expire, Ordering::Relaxed);
        }
        let prev = release_run(&self.blocks_inuse[fi], &self.blocks_purge[fi], bit, count);
        debug_assert!(prev & field_mask(bit, count) == field_mask(bit, count));
    }

    /// Purge scheduled blocks that are still free (best effort; a racing
    /// claim simply refaults).
    fn purge_now(&self) {
        let fields = self.block_count.div_ceil(FIELD_BITS);
        let decommits = options::purge_decommits() != 0;
        for fi in 0..fields {
            purge_field(
                &self.blocks_purge[fi],
                &self.blocks_inuse[fi],
                |bit, run_len| {
                    let index = fi * FIELD_BITS + bit;
                    if decommits {
                        // Take the run out of circulation while decommitting:
                        // a concurrent claim that saw the committed bits set
                        // must not touch memory mid-decommit. Skip runs that
                        // were (re)claimed since the purgeable snapshot.
                        decommit_run(
                            &self.blocks_inuse[fi],
                            &self.blocks_committed[fi],
                            bit,
                            run_len,
                            || {
                                // SAFETY: free blocks, exclusively held by us.
                                unsafe {
                                    os::decommit(self.block_ptr(index), run_len * ARENA_BLOCK_SIZE);
                                }
                            },
                        );
                    } else {
                        // SAFETY: free blocks; contents may be discarded.
                        unsafe {
                            os::reset(self.block_ptr(index), run_len * ARENA_BLOCK_SIZE);
                        }
                    }
                },
            );
        }
        self.purge_expire.store(0, Ordering::Relaxed);
    }
}

/// Reserve a fresh arena. Size grows geometrically with the arena count.
fn arena_reserve() -> Option<&'static Arena> {
    crate::sync::spin_locked(&RESERVE_LOCK, arena_reserve_locked)
}

fn arena_reserve_locked() -> Option<&'static Arena> {
    // MIMALLOC_ARENA_RESERVE (KiB) sets the base reservation; 0 disables
    // arena reservation entirely (allocation falls back to the OS and the
    // segment cache).
    let reserve = crate::options::arena_reserve().max(0) as usize * 1024;
    if reserve == 0 {
        return None;
    }
    let n = ARENA_COUNT.load(Ordering::Acquire);
    // base reservation, doubling every 2 arenas, capped at MAX_ARENA_BLOCKS.
    let base_blocks = if cfg!(miri) {
        2 // miri: small host allocs
    } else {
        reserve.div_ceil(ARENA_BLOCK_SIZE).max(1)
    };
    let blocks = (base_blocks << (n / 2)).min(MAX_ARENA_BLOCKS);
    let size = blocks * ARENA_BLOCK_SIZE;
    // Reserve uncommitted; blocks are committed on first claim.
    let commit = cfg!(miri);
    let (start, mem) = OsMem::alloc_aligned(size, SEGMENT_ALIGN, commit)?;
    add_arena_locked(start, blocks, mem, false, -1, mem.is_zero)
}

/// `mi_arena_add`: publish a new arena (assumes RESERVE_LOCK is held).
fn add_arena_locked(
    start: *mut u8,
    blocks: usize,
    mem: OsMem,
    exclusive: bool,
    numa_node: i32,
    is_zero: bool,
) -> Option<&'static Arena> {
    let n = ARENA_COUNT.load(Ordering::Acquire);
    if n >= MAX_ARENAS || blocks == 0 {
        return None;
    }
    let header_size = align_up(size_of::<Arena>(), os::page_size());
    let (hp, hmem) = OsMem::alloc_aligned(header_size, os::page_size(), true)?;
    let arena_ptr = hp.cast::<Arena>();
    // SAFETY: fresh committed memory sized for an Arena.
    unsafe {
        arena_ptr.write(Arena {
            id: n + 1,
            exclusive,
            numa_node,
            start,
            block_count: blocks.min(MAX_ARENA_BLOCKS),
            mem,
            purge_expire: AtomicI64::new(0),
            claim_lock: AtomicBool::new(false),
            blocks_inuse: [const { AtomicUsize::new(0) }; MAX_BLOCK_FIELDS],
            blocks_dirty: [const { AtomicUsize::new(0) }; MAX_BLOCK_FIELDS],
            blocks_committed: [const { AtomicUsize::new(0) }; MAX_BLOCK_FIELDS],
            blocks_purge: [const { AtomicUsize::new(0) }; MAX_BLOCK_FIELDS],
        });
        if !is_zero {
            (*arena_ptr).blocks_dirty.iter().for_each(|f| {
                f.store(!0, Ordering::Relaxed);
            });
        }
        if mem.is_committed {
            (*arena_ptr).blocks_committed.iter().for_each(|f| {
                f.store(!0, Ordering::Relaxed);
            });
        }
    }
    let _ = hmem; // header lives for the process lifetime
    ARENAS[n].store(arena_ptr, Ordering::Release);
    ARENA_COUNT.store(n + 1, Ordering::Release);
    // SAFETY: just initialized; arenas are never freed.
    Some(unsafe { &*arena_ptr })
}

/// `mi_arena_id_is_suitable`
#[inline]
fn id_is_suitable(id: ArenaId, exclusive: bool, req: ArenaId) -> bool {
    (!exclusive && req == ARENA_ID_NONE) || id == req
}

/// `_mi_arena_memid_is_suitable` for an [`ArenaRef`].
pub fn ref_is_suitable(aref: ArenaRef, req: ArenaId) -> bool {
    // SAFETY: arenas are immortal.
    let arena = unsafe { &*aref.arena };
    id_is_suitable(arena.id, arena.exclusive, req)
}

/// The current NUMA node (`_mi_os_numa_node`); single-node on macOS.
pub fn numa_node() -> i32 {
    0
}

fn arenas() -> impl Iterator<Item = &'static Arena> {
    let n = ARENA_COUNT.load(Ordering::Acquire);
    ARENAS[..n].iter().filter_map(|slot| {
        // SAFETY: published arenas are initialized and immortal.
        unsafe { slot.load(Ordering::Acquire).as_ref() }
    })
}

/// Provenance of an arena allocation (stored in the segment's `MemId`).
#[derive(Clone, Copy, Debug)]
pub struct ArenaRef {
    arena: *const Arena,
    index: u32,
    count: u32,
}

/// Allocate `count` contiguous arena blocks (SEGMENT_ALIGN aligned) from an
/// arena suitable for `req` (preferring the local NUMA node).
/// Returns `(ptr, is_zero, ArenaRef)`.
pub fn alloc(count: usize, req: ArenaId) -> Option<(*mut u8, bool, ArenaRef)> {
    try_purge(false);
    let node = numa_node();
    loop {
        // pass 1: NUMA-local (or unbound) suitable arenas; pass 2: any suitable
        for numa_strict in [true, false] {
            for arena in arenas() {
                if !id_is_suitable(arena.id, arena.exclusive, req)
                    || (numa_strict && arena.numa_node >= 0 && arena.numa_node != node)
                {
                    continue;
                }
                if let Some(index) = arena.try_claim(count) {
                    let (is_zero, _committed) = arena.prepare(index, count);
                    let aref = ArenaRef {
                        arena: ptr::from_ref(arena),
                        index: index as u32,
                        count: count as u32,
                    };
                    return Some((arena.block_ptr(index), is_zero, aref));
                }
            }
        }
        if req != ARENA_ID_NONE {
            return None; // a specific arena was requested and it is full
        }
        arena_reserve()?;
    }
}

/// `mi_reserve_os_memory_ex`: reserve fresh OS memory as an arena.
pub fn reserve_os_memory(size: usize, commit: bool, exclusive: bool) -> Option<ArenaId> {
    let blocks = size.div_ceil(ARENA_BLOCK_SIZE);
    let (start, mem) = OsMem::alloc_aligned(blocks * ARENA_BLOCK_SIZE, SEGMENT_ALIGN, commit)?;
    locked_add(start, blocks, mem, exclusive, -1, mem.is_zero)
}

/// `mi_manage_os_memory_ex`: adopt externally-provided memory as an arena.
/// The memory is never freed by rimalloc.
///
/// # Safety
/// `start..start+size` must be valid, otherwise-unused memory that outlives
/// the process's use of this allocator; `is_committed`/`is_zero` must be
/// accurate.
pub unsafe fn manage_os_memory(
    start: *mut u8,
    size: usize,
    is_committed: bool,
    is_zero: bool,
    numa_node: i32,
    exclusive: bool,
) -> Option<ArenaId> {
    // align inward to whole blocks
    let astart = align_up(start.addr(), SEGMENT_ALIGN);
    let aend = align_down(start.addr() + size, ARENA_BLOCK_SIZE.max(SEGMENT_ALIGN));
    if aend <= astart {
        return None;
    }
    let blocks = (aend - astart) / ARENA_BLOCK_SIZE;
    let astart_ptr = start.with_addr(astart);
    let mem = OsMem {
        base: astart_ptr,
        size: blocks * ARENA_BLOCK_SIZE,
        align: SEGMENT_ALIGN,
        is_committed,
        is_zero,
    };
    astart_ptr.expose_provenance(); // segment lookups reconstruct by address
    locked_add(astart_ptr, blocks, mem, exclusive, numa_node, is_zero)
}

fn locked_add(
    start: *mut u8,
    blocks: usize,
    mem: OsMem,
    exclusive: bool,
    numa_node: i32,
    is_zero: bool,
) -> Option<ArenaId> {
    crate::sync::spin_locked(&RESERVE_LOCK, || {
        add_arena_locked(start, blocks, mem, exclusive, numa_node, is_zero).map(|a| a.id)
    })
}

/// Address range check (`mi_is_in_heap_region` support).
pub fn contains(p: *const u8) -> bool {
    arenas().any(|a| {
        p.addr() >= a.start.addr() && p.addr() < a.start.addr() + a.block_count * ARENA_BLOCK_SIZE
    })
}

/// Return blocks to their arena.
pub fn free(aref: ArenaRef, all_committed: bool) {
    // SAFETY: arenas are immortal.
    let arena = unsafe { &*aref.arena };
    if !all_committed {
        // The dying segment decommitted parts of this range (the arena
        // tracks commitment at block granularity): mark the whole range
        // uncommitted so the next claim recommits it (C `_mi_arena_free`),
        // preventing a re-claim from handing out PROT_NONE memory.
        let (fi, bit) = (
            aref.index as usize / FIELD_BITS,
            aref.index as usize % FIELD_BITS,
        );
        clear_committed_run(&arena.blocks_committed[fi], bit, aref.count as usize);
    }
    arena.release(aref.index as usize, aref.count as usize);
}

/// Execute pending purges if due (cheap lock-free check on the fast path).
pub fn try_purge(force: bool) {
    let expire = PURGE_EXPIRE.load(Ordering::Relaxed);
    if expire == 0 {
        return;
    }
    let now = options::clock_now();
    if !force && now < expire {
        return;
    }
    PURGE_EXPIRE.store(0, Ordering::Relaxed);
    for arena in arenas() {
        let e = arena.purge_expire.load(Ordering::Relaxed);
        if e != 0 && (force || now >= e) {
            arena.purge_now();
        }
    }
}

/// fork() support: hold the reservation lock and every arena's claim lock
/// across the fork so the child's bitmaps are consistent.
#[cfg(not(windows))]
pub(crate) fn lock_for_fork() {
    crate::sync::spin_acquire(&RESERVE_LOCK);
    for arena in arenas() {
        crate::sync::spin_acquire(&arena.claim_lock);
    }
}

#[cfg(not(windows))]
pub(crate) fn unlock_for_fork() {
    for arena in arenas() {
        crate::sync::spin_release(&arena.claim_lock);
    }
    crate::sync::spin_release(&RESERVE_LOCK);
}
