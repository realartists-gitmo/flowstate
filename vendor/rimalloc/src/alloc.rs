//! Allocation fast paths (port of `alloc.c`).

use core::ptr::{self, NonNull};

use crate::constants::*;
use crate::heap::{Heap, heap_get_free_small_page};
use crate::page::Block;
use crate::page::Page;
use crate::page_ops::malloc_generic;

/// `_mi_page_malloc_zero`: pop a block from the page free list, or fall
/// into the generic path. ~7 instructions in the fast case.
#[inline(always)]
pub fn page_malloc_zero(heap: &Heap, page: &Page, size: usize, zero: bool) -> Option<NonNull<u8>> {
    debug_assert!(page.block_size.get() == 0 || page.block_size() >= size);
    let block = page.free.get();
    if block.is_null() {
        return malloc_generic(heap, size, zero, 0);
    }
    // SAFETY: free-list nodes are live blocks of this page.
    page.free.set(unsafe { page.block_next(block) });
    page.used.set(page.used.get() + 1);
    debug_assert!(
        page.block_size() < MAX_ALIGN_SIZE || block.addr().is_multiple_of(MAX_ALIGN_SIZE)
    );

    if zero {
        debug_assert!(page.block_size.get() != 0);
        if page.free_is_zero.get() {
            // SAFETY: the block is exclusively ours; clear the list word.
            unsafe { Block::write_next(block, ptr::null_mut()) };
        } else {
            // SAFETY: the block is block_size bytes, exclusively ours now.
            unsafe { ptr::write_bytes(block.cast::<u8>(), 0, page.block_size()) };
        }
    } else if cfg!(feature = "debug-fill") && !page.is_huge.get() {
        // MI_DEBUG_UNINIT: stamp fresh blocks to surface uninitialized reads.
        // SAFETY: the block is exclusively ours.
        unsafe { ptr::write_bytes(block.cast::<u8>(), 0xD0, page.usable_block_size()) };
    }
    NonNull::new(block.cast())
}

/// `mi_heap_malloc_small_zero`
#[inline(always)]
pub fn heap_malloc_small_zero(heap: &Heap, size: usize, zero: bool) -> Option<NonNull<u8>> {
    debug_assert!(size <= SMALL_SIZE_MAX);
    // SAFETY: pages_free_direct entries are live pages or the static empty page.
    let page = unsafe { &*heap_get_free_small_page(heap, size + PADDING_SIZE) };
    page_malloc_zero(heap, page, size + PADDING_SIZE, zero)
}

/// `_mi_heap_malloc_zero_ex`: the main entry point.
#[inline(always)]
pub fn heap_malloc_zero(heap: &Heap, size: usize, zero: bool) -> Option<NonNull<u8>> {
    if size <= SMALL_SIZE_MAX {
        heap_malloc_small_zero(heap, size, zero)
    } else {
        malloc_generic(heap, size + PADDING_SIZE, zero, 0)
    }
}
