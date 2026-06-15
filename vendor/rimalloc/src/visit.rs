//! Heap walking (port of `heap.c`'s `mi_heap_visit_blocks`): iterate every
//! page area of a heap and optionally every live block inside it. Live
//! blocks are found by building a bitmap of the free list — `local_free`
//! and `thread_free` are collected first so `used` is exact.

use crate::constants::*;
use crate::heap::Heap;
use crate::page::Page;
use crate::page_ops::page_free_collect;

/// `mi_heap_area_t`: a snapshot of one page's block area.
#[derive(Clone, Copy, Debug)]
pub struct HeapArea {
    pub blocks: *mut u8,        // start of the area
    pub reserved: usize,        // bytes reserved
    pub committed: usize,       // bytes committed
    pub used: usize,            // blocks in use
    pub block_size: usize,      // usable block size
    pub full_block_size: usize, // size including internal padding
    pub heap_tag: u8,
}

impl HeapArea {
    fn init(page: &Page) -> HeapArea {
        let bsize = page.block_size();
        HeapArea {
            blocks: page.start(),
            reserved: page.reserved.get() as usize * bsize,
            committed: page.capacity.get() as usize * bsize,
            used: page.used.get() as usize,
            block_size: page.usable_block_size(),
            full_block_size: bsize,
            heap_tag: page.heap_tag.get(),
        }
    }
}

const MAX_BLOCKS: usize = SMALL_PAGE_SIZE / PTR_SIZE; // 8192
const MAP_WORDS: usize = MAX_BLOCKS / usize::BITS as usize;

/// `_mi_heap_area_visit_blocks`: call `visitor(block, usable_size)` for
/// every live block of `page`; stops early on `false`.
fn area_visit_blocks(page: &Page, visitor: &mut impl FnMut(*mut u8, usize) -> bool) -> bool {
    if page.used.get() == 0 {
        return true;
    }
    let pstart = page.start();
    let bsize = page.block_size();
    let ubsize = page.usable_block_size();

    // optimize: page with one block
    if page.capacity.get() == 1 {
        return visitor(pstart, ubsize);
    }

    // optimize: full page
    if page.used.get() == page.capacity.get() {
        let mut block = pstart;
        for _ in 0..page.capacity.get() {
            if !visitor(block, ubsize) {
                return false;
            }
            // SAFETY: blocks stay within the page area.
            block = unsafe { block.add(bsize) };
        }
        return true;
    }

    // bitmap of free blocks
    let capacity = page.capacity.get() as usize;
    debug_assert!(capacity <= MAX_BLOCKS);
    let mut free_map = [0usize; MAP_WORDS];
    let bmapsize = capacity.div_ceil(usize::BITS as usize);
    if !capacity.is_multiple_of(usize::BITS as usize) {
        // mark left-over bits at the end as free
        free_map[bmapsize - 1] = usize::MAX << (capacity % usize::BITS as usize);
    }
    let mut p = page.free.get();
    while !p.is_null() {
        let offset = p.addr() - pstart.addr();
        debug_assert!(offset.is_multiple_of(bsize));
        let blockidx = offset / bsize;
        free_map[blockidx / usize::BITS as usize] |= 1 << (blockidx % usize::BITS as usize);
        // SAFETY: free-list nodes are live blocks in this page.
        p = unsafe { page.block_next(p) };
    }

    // walk all blocks, skipping the free ones
    let mut block = pstart;
    for &word in &free_map[..bmapsize] {
        if word == 0 {
            // every block in this word is in use
            for _ in 0..usize::BITS {
                if !visitor(block, ubsize) {
                    return false;
                }
                // SAFETY: in-bounds while bits remain.
                block = unsafe { block.add(bsize) };
            }
        } else {
            let mut m = !word;
            while m != 0 {
                let bitidx = m.trailing_zeros() as usize;
                // SAFETY: bit indices map within the page area.
                if !visitor(unsafe { block.add(bitidx * bsize) }, ubsize) {
                    return false;
                }
                m &= m - 1;
            }
            // SAFETY: word-stride within (or one past) the page area.
            block = unsafe { block.add(bsize * usize::BITS as usize) };
        }
    }
    true
}

/// `mi_heap_visit_blocks`: visit every area of `heap`, and every live block
/// within each area when `visit_blocks` is true. The visitor receives the
/// area and, for block visits, `Some((block, usable_size))`; returning
/// `false` stops the walk. Returns `false` if the walk was stopped.
pub fn heap_visit_blocks(
    heap: &Heap,
    visit_blocks: bool,
    visitor: &mut impl FnMut(&HeapArea, Option<(*mut u8, usize)>) -> bool,
) -> bool {
    crate::heap_ops::heap_visit_pages(heap, |_, _, page| {
        // collect so the used count is exact
        page_free_collect(page, true);
        let area = HeapArea::init(page);
        if !visitor(&area, None) {
            return false;
        }
        if visit_blocks {
            area_visit_blocks(page, &mut |block, size| visitor(&area, Some((block, size))))
        } else {
            true
        }
    })
}
