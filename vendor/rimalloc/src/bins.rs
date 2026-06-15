//! Size-class ("bin") mapping, mirroring `page-queue.c`.
//!
//! Bins are spaced exponentially in 12.5% increments: sizes of at most 8
//! words get an exact bin; above that the top three bits of the word size
//! select the bin. `MAX_ALIGN_SIZE == 16 == 2*PTR_SIZE` gives the `ALIGN2W`
//! variant: small sizes round up to double-word multiples.

use crate::constants::*;

/// Index of a heap page queue: `1..=BIN_HUGE` are size classes, `BIN_FULL`
/// is the queue of full pages. Constructed only through [`Bin::from_size`]
/// or the named constants, so indexing `[_; BIN_FULL + 1]` never re-checks.
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Debug)]
pub struct Bin(u8);

impl Bin {
    pub const HUGE: Bin = Bin(BIN_HUGE as u8);
    pub const FULL: Bin = Bin(BIN_FULL as u8);

    /// `mi_bin`: the bin for a given byte size (huge if too large).
    #[inline]
    pub fn from_size(size: usize) -> Bin {
        let mut wsize = wsize_from_size(size);
        let bin = if wsize <= 8 {
            // ALIGN2W: round to double-word sizes.
            if wsize <= 1 { 1 } else { (wsize + 1) & !1 }
        } else if wsize > MEDIUM_OBJ_WSIZE_MAX {
            BIN_HUGE
        } else {
            wsize -= 1;
            // Use the top 3 bits of the word size; adjust by 3 because the
            // first 8 sizes get an exact bin.
            let b = (usize::BITS - 1 - wsize.leading_zeros()) as usize;
            let bin = ((b << 2) + ((wsize >> (b - 2)) & 0x03)) - 3;
            debug_assert!(bin > 0 && bin < BIN_HUGE);
            bin
        };
        Bin(bin as u8)
    }

    #[inline(always)]
    pub const fn index(self) -> usize {
        self.0 as usize
    }

    /// Largest block size (in bytes) served by this bin (`_mi_bin_size`).
    #[inline]
    pub const fn block_size(self) -> usize {
        BIN_WSIZE[self.0 as usize] * PTR_SIZE
    }
}

/// Largest word size per bin — the inverse of [`Bin::from_size`], matching
/// `MI_PAGE_QUEUES_EMPTY` in `init.c`. Entry 0 is a sentinel; the last two
/// entries are the huge and full queue markers.
pub const BIN_WSIZE: [usize; BIN_FULL + 1] = {
    let mut t = [0usize; BIN_FULL + 1];
    t[0] = 1;
    let mut bin = 1;
    while bin <= 8 {
        t[bin] = bin;
        bin += 1;
    }
    while bin < BIN_HUGE {
        let b = (bin + 3) >> 2;
        let m = (bin + 3) & 3;
        t[bin] = (1 << b) + ((m + 1) << (b - 2));
        bin += 1;
    }
    t[BIN_HUGE] = MEDIUM_OBJ_WSIZE_MAX + 1; // huge queue
    t[BIN_FULL] = MEDIUM_OBJ_WSIZE_MAX + 2; // full queue
    t
};

/// `mi_good_size`: the actual usable size class an allocation rounds to.
#[inline]
pub fn good_size(size: usize) -> usize {
    if size <= MEDIUM_OBJ_SIZE_MAX {
        Bin::from_size(size + PADDING_SIZE).block_size()
    } else if size <= MAX_ALLOC_SIZE {
        align_up(size + PADDING_SIZE, crate::os::page_size())
    } else {
        size
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn table_matches_reference() {
        // Spot-check against the table in init.c.
        let expect: &[(usize, usize)] = &[
            (1, 1),
            (8, 8),
            (9, 10),
            (16, 32),
            (17, 40),
            (24, 128),
            (32, 512),
            (40, 2048),
            (48, 8192),
            (56, 32768),
            (64, 131072),
            (72, 524288),
        ];
        for &(bin, wsize) in expect {
            assert_eq!(BIN_WSIZE[bin], wsize, "bin {bin}");
        }
    }

    #[test]
    fn bin_is_inverse_of_table() {
        for bin in 1..BIN_HUGE {
            let wsize = BIN_WSIZE[bin];
            if wsize > MEDIUM_OBJ_WSIZE_MAX {
                continue;
            }
            let chosen = Bin::from_size(wsize * PTR_SIZE);
            // ALIGN2W: odd word sizes <= 8 round up to the next even bin.
            let expect = if wsize <= 8 && wsize > 1 {
                (bin + 1) & !1
            } else {
                bin
            };
            assert_eq!(chosen.index(), expect, "max of bin {bin}");
            // The chosen bin must fit the size...
            assert!(chosen.block_size() >= wsize * PTR_SIZE);
            // ...and one word more must spill into a later bin.
            assert!(Bin::from_size((wsize + 1) * PTR_SIZE).index() > expect.min(bin));
        }
        assert_eq!(Bin::from_size(MEDIUM_OBJ_SIZE_MAX + 1), Bin::HUGE);
        assert_eq!(Bin::from_size(0).index(), 1);
    }
}
