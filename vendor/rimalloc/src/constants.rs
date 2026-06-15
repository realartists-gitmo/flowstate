//! Core size constants, mirroring `mimalloc/types.h` (64-bit values).

pub const PTR_SIZE: usize = size_of::<usize>();
pub const PTR_SHIFT: usize = PTR_SIZE.trailing_zeros() as usize;

/// Minimal alignment necessary (`MI_MAX_ALIGN_SIZE`), 16 for SSE et al.
pub const MAX_ALIGN_SIZE: usize = 16;

pub const SEGMENT_SLICE_SHIFT: usize = 13 + PTR_SHIFT; // 64 KiB on 64-bit
pub const SEGMENT_SHIFT: usize = if PTR_SIZE > 4 {
    9 + SEGMENT_SLICE_SHIFT // 32 MiB
} else {
    7 + SEGMENT_SLICE_SHIFT // 4 MiB on 32-bit
};
pub const SMALL_PAGE_SHIFT: usize = SEGMENT_SLICE_SHIFT; // 64 KiB
pub const MEDIUM_PAGE_SHIFT: usize = 3 + SMALL_PAGE_SHIFT; // 512 KiB

pub const SEGMENT_SIZE: usize = 1 << SEGMENT_SHIFT;
pub const SEGMENT_ALIGN: usize = SEGMENT_SIZE;
pub const SEGMENT_MASK: usize = SEGMENT_ALIGN - 1;
pub const SEGMENT_SLICE_SIZE: usize = 1 << SEGMENT_SLICE_SHIFT;
pub const SLICES_PER_SEGMENT: usize = SEGMENT_SIZE / SEGMENT_SLICE_SIZE; // 512

pub const SMALL_PAGE_SIZE: usize = 1 << SMALL_PAGE_SHIFT;
pub const MEDIUM_PAGE_SIZE: usize = 1 << MEDIUM_PAGE_SHIFT;

pub const SMALL_OBJ_SIZE_MAX: usize = SMALL_PAGE_SIZE / 8; // 8 KiB
pub const MEDIUM_OBJ_SIZE_MAX: usize = MEDIUM_PAGE_SIZE / 8; // 64 KiB
pub const MEDIUM_OBJ_WSIZE_MAX: usize = MEDIUM_OBJ_SIZE_MAX / PTR_SIZE;
pub const LARGE_OBJ_SIZE_MAX: usize = SEGMENT_SIZE / 2; // 16 MiB

/// `MI_SMALL_WSIZE_MAX`: max word-size served by the direct small-alloc path.
pub const SMALL_WSIZE_MAX: usize = 128;
pub const SMALL_SIZE_MAX: usize = SMALL_WSIZE_MAX * PTR_SIZE; // 1 KiB

/// Blocks up to this size are guaranteed block-size aligned.
pub const MAX_ALIGN_GUARANTEE: usize = MEDIUM_OBJ_SIZE_MAX;
/// Alignments above this go into dedicated huge segments.
pub const BLOCK_ALIGNMENT_MAX: usize = SEGMENT_SIZE >> 1;
/// Maximum slice count for which interior pointers can find their page.
pub const MAX_SLICE_OFFSET_COUNT: usize = (BLOCK_ALIGNMENT_MAX / SEGMENT_SLICE_SIZE) - 1;

/// `MI_MAX_ALLOC_SIZE`: largest request the allocator accepts. On 64-bit the
/// cap keeps a segment's slice count within 32 bits (mimalloc issue #877),
/// well below `isize::MAX`, so `good_size`'s page round-up cannot overflow.
pub const MAX_ALLOC_SIZE: usize = SEGMENT_SLICE_SIZE * (u32::MAX as usize - 1);

// Commit mask: one bit per commit chunk (= one slice) of a segment.
pub const MINIMAL_COMMIT_SIZE: usize = SEGMENT_SLICE_SIZE;
pub const COMMIT_SIZE: usize = SEGMENT_SLICE_SIZE; // 64 KiB
pub const COMMIT_MASK_BITS: usize = SEGMENT_SIZE / COMMIT_SIZE;
pub const COMMIT_MASK_FIELD_BITS: usize = usize::BITS as usize;
pub const COMMIT_MASK_FIELD_COUNT: usize = COMMIT_MASK_BITS / COMMIT_MASK_FIELD_BITS;
const _: () = assert!(COMMIT_MASK_BITS == COMMIT_MASK_FIELD_COUNT * COMMIT_MASK_FIELD_BITS);

/// `MI_SEGMENT_BIN_MAX == mi_segment_bin(MI_SLICES_PER_SEGMENT)`.
pub const SEGMENT_BIN_MAX: usize = 35;

/// Number of exponential size bins (`MI_BIN_HUGE`), plus the full-queue bin.
pub const BIN_HUGE: usize = 73;
pub const BIN_FULL: usize = BIN_HUGE + 1;

/// `MI_PAGES_DIRECT`: entries in the heap's direct small-page array.
pub const PAGES_DIRECT: usize = SMALL_WSIZE_MAX + PADDING_WSIZE + 1;

/// Padding (`MI_PADDING`) is disabled in release parity builds.
pub const PADDING_SIZE: usize = 0;
pub const PADDING_WSIZE: usize = 0;

/// Convert a byte size to a machine-word count, rounding up.
#[inline(always)]
pub const fn wsize_from_size(size: usize) -> usize {
    size.div_ceil(PTR_SIZE)
}

#[inline(always)]
pub const fn align_up(sz: usize, alignment: usize) -> usize {
    debug_assert!(alignment != 0);
    let mask = alignment - 1;
    if alignment & mask == 0 {
        (sz + mask) & !mask
    } else {
        ((sz + mask) / alignment) * alignment
    }
}

#[inline(always)]
pub const fn align_down(sz: usize, alignment: usize) -> usize {
    debug_assert!(alignment != 0);
    let mask = alignment - 1;
    if alignment & mask == 0 {
        sz & !mask
    } else {
        (sz / alignment) * alignment
    }
}

#[inline(always)]
pub const fn is_power_of_two(x: usize) -> bool {
    x != 0 && x & (x - 1) == 0
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `MAX_ALLOC_SIZE` matches C v2.3.2's 64-bit value (issue #877) and stays
    /// below `isize::MAX` so `good_size`'s page round-up cannot overflow.
    #[test]
    fn max_alloc_size_matches_c() {
        assert_eq!(MAX_ALLOC_SIZE, SEGMENT_SLICE_SIZE * (u32::MAX as usize - 1));
        assert!(MAX_ALLOC_SIZE < isize::MAX as usize);
    }
}
