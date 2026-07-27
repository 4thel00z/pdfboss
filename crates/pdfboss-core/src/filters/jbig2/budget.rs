//! The decoding work budget shared by every region in one embedded stream.
//!
//! A JBIG2 region's cost is set by the dimensions it declares, not by how many
//! bytes it carries. The arithmetic decoder's marker convention (T.88 E.3.4)
//! synthesises data indefinitely once the coded bytes run out, so a region need
//! not supply the bits it claims: a segment header of a few dozen bytes can ask
//! for as many pixel decisions as its width and height fields can express.
//!
//! Capping the *bitmap* a region allocates does not cap that work, for two
//! reasons.
//!
//! A region no pixels wide allocates nothing whatever its height, because the
//! product that the allocation cap tests is zero — yet the decoding procedure
//! still makes a pass over every row it declares. The work is in the row count,
//! and the allocation cap cannot see the row count.
//!
//! And a cap on one region says nothing about a stream of them. Annex D.3 puts
//! no limit on how many segments an embedded stream holds, so per-region
//! ceilings alone leave the total growing linearly with segment count at a
//! ruinous ratio to the bytes that buy it.
//!
//! Decoding is not the only thing that costs, either. A symbol dictionary may
//! re-export the symbols it was handed instead of coding any of its own
//! (6.5.10), which copies a bitmap without decoding a pixel of it, and the
//! symbols a dictionary exports are held for the rest of the segment walk
//! rather than composited and dropped. So what is charged here is not "pixels
//! decoded" but "bitmaps brought into existence", whichever way they arrived.
//!
//! Hence one budget per stream, charged from the declared dimensions *before*
//! the decoding loop is entered. The charge is structural — it reads the
//! header, never the coded data — so a region that cannot be afforded is
//! refused without a pixel being decoded, and the total work an embedded stream
//! can provoke is a fixed constant no matter what the stream says or how long
//! it is.

use super::Jbig2Error;

/// The decoding work one embedded stream may perform, in units of one pixel
/// decision.
///
/// This is twice the largest bitmap a single region may allocate, so a page
/// assembled from several regions — a striped page, or one where a second
/// region overpaints part of the first — still fits comfortably, while a stream
/// that simply repeats maximum-size regions is stopped after the second one.
/// The absolute figure matters less than that it is a constant: whatever the
/// stream declares, and however many segments it holds, the decoder's cost is
/// bounded by this number.
pub(crate) const MAX_WORK: u64 = 2 * super::bitmap::MAX_PIXELS;

/// What entering a row of a region costs on top of that row's pixels.
///
/// A row is not free even when it has no pixels in it: the context windows are
/// primed from the two reference rows, and with typical prediction on (6.2.5.7)
/// a coding decision is made per row as well. Charging a fixed amount per row
/// is what stops a region of zero or negligible width from buying an unbounded
/// number of rows. The figure is an upper bound on that per-row setup measured
/// in pixel decisions, so a row can never cost more than it is charged.
pub(crate) const ROW_COST: u64 = 8;

/// A running allowance of decoding work, spent by the region decoders.
///
/// One of these covers a whole embedded stream, globals included, so that the
/// cost of a stream is bounded whatever mixture of segments it holds.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Budget {
    /// Work not yet spent. Never goes negative: a charge that would overdraw is
    /// refused and leaves the remainder untouched.
    remaining: u64,
}

impl Budget {
    /// A budget of [`MAX_WORK`], which is what one embedded stream gets.
    pub(crate) fn new() -> Budget {
        Budget {
            remaining: MAX_WORK,
        }
    }

    /// A budget of a stated size, for tests that need exhaustion to be cheap to
    /// reach.
    #[cfg(test)]
    pub(crate) fn with_limit(limit: u64) -> Budget {
        Budget { remaining: limit }
    }

    /// Spends `work` units, or fails leaving the budget unchanged.
    ///
    /// Failure is [`Jbig2Error::WorkLimit`] rather than a truncation or a
    /// malformation, because the stream may be perfectly well formed and simply
    /// ask for more than this decoder is willing to spend on it.
    pub(crate) fn charge(&mut self, work: u64) -> Result<(), Jbig2Error> {
        match self.remaining.checked_sub(work) {
            Some(left) => {
                self.remaining = left;
                Ok(())
            }
            None => Err(Jbig2Error::WorkLimit),
        }
    }

    /// Spends what decoding a `width` by `height` region costs.
    ///
    /// The charge is `height * (width + ROW_COST)`: one unit per pixel decision
    /// plus the per-row setup, which is the term that makes a region of zero
    /// width cost something. Both steps saturate, so the two 32-bit dimensions
    /// cannot wrap into a small charge — `u32::MAX` in both fields lands just
    /// above `u64::MAX` without it.
    pub(crate) fn charge_region(&mut self, width: u32, height: u32) -> Result<(), Jbig2Error> {
        let per_row = u64::from(width).saturating_add(ROW_COST);
        self.charge(per_row.saturating_mul(u64::from(height)))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn charging_spends_down_and_then_refuses() {
        let mut budget = Budget::with_limit(10);
        assert_eq!(budget.charge(4), Ok(()));
        assert_eq!(budget.charge(6), Ok(()));
        assert_eq!(budget.charge(1), Err(Jbig2Error::WorkLimit));
    }

    /// A refused charge must not spend anything, or a stream could drain the
    /// budget with requests that were all rejected.
    #[test]
    fn a_refused_charge_leaves_the_budget_intact() {
        let mut budget = Budget::with_limit(10);
        assert_eq!(budget.charge(11), Err(Jbig2Error::WorkLimit));
        assert_eq!(budget, Budget::with_limit(10));
        assert_eq!(budget.charge(10), Ok(()));
    }

    /// The defect this type exists for: a region of zero width allocates
    /// nothing, so an allocation cap lets it declare any height at all. It must
    /// still be charged for its rows.
    #[test]
    fn a_zero_width_region_is_charged_for_its_rows() {
        let mut budget = Budget::new();
        assert_eq!(
            budget.charge_region(0, u32::MAX),
            Err(Jbig2Error::WorkLimit),
        );
        assert_eq!(budget, Budget::new(), "nothing was spent");
    }

    /// Neither the row cost nor the multiply may wrap a huge region into an
    /// affordable charge.
    #[test]
    fn extreme_dimensions_saturate_rather_than_wrapping() {
        for (width, height) in [
            (u32::MAX, u32::MAX),
            (u32::MAX, 1),
            (1, u32::MAX),
            (0, u32::MAX),
            (u32::MAX, 2),
        ] {
            let mut budget = Budget::new();
            assert_eq!(
                budget.charge_region(width, height),
                Err(Jbig2Error::WorkLimit),
                "{width} x {height}",
            );
        }
    }

    /// A page-sized region has to remain affordable, or the cap would refuse
    /// real documents. 4096 x 4096 is a little over a 600 dpi A4 page.
    #[test]
    fn a_realistic_page_region_fits() {
        let mut budget = Budget::new();
        assert_eq!(budget.charge_region(4096, 4096), Ok(()));
        assert_eq!(budget.charge_region(4096, 4096), Ok(()));
    }

    /// The largest bitmap a region may allocate is affordable exactly twice,
    /// which pins the relationship between the two caps.
    #[test]
    fn the_largest_allocatable_region_is_affordable_but_not_endlessly() {
        let mut budget = Budget::new();
        // 8192 x 16384 is MAX_PIXELS exactly.
        assert_eq!(budget.charge_region(8192, 16384), Ok(()));
        assert_eq!(
            budget.charge_region(8192, 16384),
            Err(Jbig2Error::WorkLimit),
            "the row cost makes the second one just unaffordable",
        );
    }

    /// A zero-row region costs nothing, so an empty region is never refused.
    #[test]
    fn a_region_with_no_rows_is_free() {
        let mut budget = Budget::with_limit(0);
        assert_eq!(budget.charge_region(u32::MAX, 0), Ok(()));
    }
}
