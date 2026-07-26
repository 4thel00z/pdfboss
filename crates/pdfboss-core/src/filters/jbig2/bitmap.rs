//! The bilevel bitmap every JBIG2 region decodes into, and the composition
//! operators that place one bitmap onto another.
//!
//! T.88 6.2 decodes a generic region into a bitmap of individually addressable
//! pixels; 7.4.1 gives every region segment a location and an external
//! combination operator saying how it merges with what is already on the page.
//! Both operations live here.

use super::Jbig2Error;

/// The largest bitmap this decoder will allocate, in pixels.
///
/// Storage is one byte per pixel, so this caps a single region at 128 MiB. A
/// 600 dpi A4 page is about 35 million pixels; the cap leaves comfortable room
/// above that while refusing the multi-gigabyte allocation a hostile 32-bit
/// width and height would otherwise request.
///
/// This bounds memory, and only memory. It is emphatically not a bound on how
/// long a region takes to decode, and reading it as one is a mistake: the
/// product it tests collapses to zero when either dimension is zero, so a
/// region no pixels wide passes this cap for *any* height, allocates nothing,
/// and still costs the decoder a pass over every row it declared. Bounding the
/// work is the job of the budget in the [`super::budget`] module, which charges
/// per row as well as per pixel and spans the whole stream rather than one
/// region.
pub(crate) const MAX_PIXELS: u64 = 1 << 27;

/// The external combination operator of a region segment (T.88 7.4.1).
///
/// The three-bit field in the region segment information flags selects one of
/// these; values 5 to 7 are reserved.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum CombOp {
    /// Set a destination pixel wherever either bitmap has one.
    Or,
    /// Keep a destination pixel only where both bitmaps have one.
    And,
    /// Set a destination pixel where exactly one bitmap has one.
    Xor,
    /// Set a destination pixel where both or neither bitmap has one.
    Xnor,
    /// Overwrite the destination with the source, zeros included.
    Replace,
}

impl CombOp {
    /// Decodes the three-bit operator field of T.88 7.4.1.
    ///
    /// Only the low three bits are meaningful; the caller masks the flags byte
    /// before calling. Values 5 to 7 are reserved and rejected rather than
    /// silently treated as OR, because a stream using them is not a stream this
    /// decoder understands.
    pub(crate) fn from_bits(bits: u8) -> Result<CombOp, Jbig2Error> {
        match bits {
            0 => Ok(CombOp::Or),
            1 => Ok(CombOp::And),
            2 => Ok(CombOp::Xor),
            3 => Ok(CombOp::Xnor),
            4 => Ok(CombOp::Replace),
            _ => Err(Jbig2Error::Malformed("reserved combination operator")),
        }
    }
}

/// A bilevel bitmap: one byte per pixel, valued 0 or 1, in row-major order.
///
/// A byte per pixel rather than a bit costs eight times the memory and buys a
/// context-formation inner loop with no shifting or masking in it, which is
/// where a generic region spends nearly all of its time. [`MAX_PIXELS`] is what
/// keeps the memory bounded.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Bitmap {
    width: u32,
    height: u32,
    /// `width * height` bytes, each 0 or 1. The 0-or-1 invariant is
    /// established by every writer in this module and relied upon by
    /// [`CombOp::Xnor`], whose `1 - (a ^ b)` would underflow otherwise.
    data: Vec<u8>,
}

impl Bitmap {
    /// A zero-filled bitmap of the given size.
    ///
    /// Fails with [`Jbig2Error::TooLarge`] when the pixel count exceeds
    /// [`MAX_PIXELS`]. A zero width or height is legal and yields an empty
    /// bitmap: T.88 places no lower bound on a region's dimensions, and
    /// refusing one here would reject a legal stream. What stops a caller
    /// looping over the rows of such a bitmap is the work budget, not this.
    pub(crate) fn new(width: u32, height: u32) -> Result<Bitmap, Jbig2Error> {
        Bitmap::filled(width, height, 0)
    }

    /// A bitmap of the given size with every pixel set to `value`.
    ///
    /// Any non-zero `value` stores as 1. The page information segment's default
    /// pixel value (T.88 7.4.8) is what needs this.
    pub(crate) fn filled(width: u32, height: u32, value: u8) -> Result<Bitmap, Jbig2Error> {
        // The product is computed in u64 so it cannot wrap: two u32 dimensions
        // multiply to at most 2^64 - 2^33 + 1, and the cap is checked before a
        // single byte is reserved.
        let pixels = u64::from(width) * u64::from(height);
        if pixels > MAX_PIXELS {
            return Err(Jbig2Error::TooLarge { width, height });
        }
        // Also guards a 32-bit host, where the cap alone would not be enough.
        let len = usize::try_from(pixels).map_err(|_| Jbig2Error::TooLarge { width, height })?;
        Ok(Bitmap {
            width,
            height,
            data: vec![u8::from(value != 0); len],
        })
    }

    /// The bitmap's width in pixels.
    pub(crate) fn width(&self) -> u32 {
        self.width
    }

    /// The bitmap's height in pixels.
    pub(crate) fn height(&self) -> u32 {
        self.height
    }

    /// The pixel at `(x, y)`, or 0 outside the bitmap.
    ///
    /// Out-of-range reads are the normal case: the generic region templates
    /// reach up to four pixels left and two rows up, so every pixel in the
    /// first rows and columns reads outside. T.88 6.2.5.2 defines those as 0.
    /// The coordinates are `i64` for the same reason — the neighbourhood of
    /// pixel (0, 0) has negative coordinates in it.
    pub(crate) fn get(&self, x: i64, y: i64) -> u8 {
        if x < 0 || y < 0 || x >= i64::from(self.width) || y >= i64::from(self.height) {
            return 0;
        }
        let idx = y as usize * self.width as usize + x as usize;
        self.data.get(idx).copied().unwrap_or(0)
    }

    /// Stores `value` at `(x, y)`, or does nothing if that is outside the
    /// bitmap.
    ///
    /// Any non-zero `value` stores as 1, which is what keeps the 0-or-1
    /// invariant the composition operators depend on.
    pub(crate) fn set(&mut self, x: u32, y: u32, value: u8) {
        if x >= self.width || y >= self.height {
            return;
        }
        let idx = y as usize * self.width as usize + x as usize;
        if let Some(slot) = self.data.get_mut(idx) {
            *slot = u8::from(value != 0);
        }
    }

    /// Row `y` as a slice of one byte per pixel, or an empty slice outside the
    /// bitmap.
    #[allow(dead_code)] // Read by the halftone grid walk, which lands later.
    pub(crate) fn row(&self, y: u32) -> &[u8] {
        if y >= self.height {
            return &[];
        }
        let stride = self.width as usize;
        let start = y as usize * stride;
        self.data.get(start..start + stride).unwrap_or(&[])
    }

    /// Copies row `y - 1` over row `y`.
    ///
    /// This is the typical-prediction path of T.88 6.2.5.7: when LTP is 1 the
    /// row is a repeat of the one above it and no pixels are decoded. Row 0 has
    /// nothing above it, so `y == 0` is a no-op and the row stays as it was
    /// initialised — which is the all-zero row the procedure expects.
    pub(crate) fn duplicate_row(&mut self, y: u32) {
        if y == 0 || y >= self.height {
            return;
        }
        let stride = self.width as usize;
        let src = (y as usize - 1) * stride;
        let dst = y as usize * stride;
        if stride == 0 || dst + stride > self.data.len() {
            return;
        }
        self.data.copy_within(src..src + stride, dst);
    }

    /// Composites `src` onto this bitmap with its top-left corner at `(x, y)`,
    /// clipped to this bitmap's extent.
    ///
    /// This is the general region composition of T.88 6.1 and 7.4.1. A region
    /// may legitimately be placed so that it hangs off any edge of the page, so
    /// the overlap is computed once, up front, in `i64` — an offset of
    /// `i32::MIN` plus a `u32` width has no room to wrap there — and the inner
    /// loop then runs over pixels already known to be inside both bitmaps.
    pub(crate) fn combine(&mut self, src: &Bitmap, x: i32, y: i32, op: CombOp) {
        let off_x = i64::from(x);
        let off_y = i64::from(y);
        let x0 = off_x.max(0);
        let y0 = off_y.max(0);
        let x1 = (off_x + i64::from(src.width)).min(i64::from(self.width));
        let y1 = (off_y + i64::from(src.height)).min(i64::from(self.height));
        if x0 >= x1 || y0 >= y1 {
            return;
        }

        let dst_stride = self.width as usize;
        let src_stride = src.width as usize;
        for dst_y in y0..y1 {
            let src_y = dst_y - off_y;
            let dst_base = dst_y as usize * dst_stride;
            let src_base = src_y as usize * src_stride;
            for dst_x in x0..x1 {
                let src_x = dst_x - off_x;
                let dst_idx = dst_base + dst_x as usize;
                let src_idx = src_base + src_x as usize;
                let (Some(&src_pixel), Some(dst_pixel)) =
                    (src.data.get(src_idx), self.data.get_mut(dst_idx))
                else {
                    continue;
                };
                *dst_pixel = match op {
                    CombOp::Or => *dst_pixel | src_pixel,
                    CombOp::And => *dst_pixel & src_pixel,
                    CombOp::Xor => *dst_pixel ^ src_pixel,
                    CombOp::Xnor => 1 - (*dst_pixel ^ src_pixel),
                    CombOp::Replace => src_pixel,
                };
            }
        }
    }

    /// Packs to one bit per pixel, MSB first, each row padded to a whole
    /// number of bytes — the layout PDF expects for `/BitsPerComponent 1`
    /// sample data.
    ///
    /// Polarity is preserved as JBIG2 defines it: a set bit is a foreground
    /// pixel. That is deliberate and must stay that way. JBIG2 calls a 1 pixel
    /// *foreground*, while `/DeviceGray` at one bit per component calls a 0
    /// sample *black*, so something has to reconcile the two — but that
    /// something is the `JBIG2Decode` arm at the filter boundary, where the
    /// decision can be made once and checked against real files. A bitmap does
    /// not know whether it is destined for a page, a symbol dictionary, or a
    /// refinement reference, so inverting here would corrupt every use that is
    /// not the last one.
    #[allow(dead_code)] // Called by the `JBIG2Decode` filter arm, wired later.
    pub(crate) fn pack_rows(&self) -> Vec<u8> {
        let stride = self.width.div_ceil(8) as usize;
        // A bitmap with no columns packs to no bytes, whatever its height, and
        // returning here is what stops that height from costing a pass per row.
        // It can be any `u32`: the allocation cap tests `width * height`, which
        // is zero for every height once the width is, so a zero-width bitmap of
        // four billion rows is a bitmap this type will hand out. Height alone
        // is bounded only when there is at least one column, and then by the
        // cap — `data` holds `width * height` bytes.
        if stride == 0 {
            return Vec::new();
        }
        let mut out = vec![0u8; stride * self.height as usize];
        for y in 0..self.height {
            let base = y as usize * stride;
            for (x, &pixel) in self.row(y).iter().enumerate() {
                if pixel != 0 {
                    if let Some(byte) = out.get_mut(base + x / 8) {
                        *byte |= 0x80 >> (x % 8);
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn from_rows(rows: &[&str]) -> Bitmap {
        let height = rows.len() as u32;
        let width = rows[0].len() as u32;
        let mut bm = Bitmap::new(width, height).expect("small bitmap");
        for (y, row) in rows.iter().enumerate() {
            for (x, ch) in row.bytes().enumerate() {
                bm.set(x as u32, y as u32, u8::from(ch == b'1'));
            }
        }
        bm
    }

    #[test]
    fn new_is_zero_filled_and_filled_is_not() {
        let zero = Bitmap::new(3, 2).expect("3x2");
        assert_eq!(zero.row(0), &[0, 0, 0]);
        let ones = Bitmap::filled(3, 2, 1).expect("3x2");
        assert_eq!(ones.row(1), &[1, 1, 1]);
    }

    /// The template reads x-4 and y-2, so out-of-range access is the common
    /// case, not the exceptional one. It must read 0, never panic.
    #[test]
    fn get_outside_bounds_reads_zero() {
        let bm = from_rows(&["11", "11"]);
        assert_eq!(bm.get(0, 0), 1);
        for (x, y) in [
            (-1, 0),
            (0, -1),
            (-4, -2),
            (2, 0),
            (0, 2),
            (i64::MIN, i64::MAX),
        ] {
            assert_eq!(bm.get(x, y), 0, "({x}, {y}) must read 0");
        }
    }

    #[test]
    fn set_outside_bounds_is_a_no_op() {
        let mut bm = Bitmap::new(2, 2).expect("2x2");
        bm.set(9, 9, 1);
        assert_eq!(bm.row(0), &[0, 0]);
        assert_eq!(bm.row(1), &[0, 0]);
    }

    #[test]
    fn row_outside_bounds_is_empty() {
        let bm = Bitmap::new(2, 2).expect("2x2");
        assert!(bm.row(2).is_empty());
    }

    #[test]
    fn duplicate_row_copies_the_row_above() {
        let mut bm = from_rows(&["101", "000"]);
        bm.duplicate_row(1);
        assert_eq!(bm.row(1), &[1, 0, 1]);
    }

    #[test]
    fn duplicate_row_zero_is_a_no_op() {
        let mut bm = from_rows(&["101", "010"]);
        bm.duplicate_row(0);
        assert_eq!(bm.row(0), &[1, 0, 1]);
    }

    #[test]
    fn combine_applies_every_operator() {
        // dst row: 1 1 0 0 ; src row: 1 0 1 0
        let cases: [(CombOp, [u8; 4]); 5] = [
            (CombOp::Or, [1, 1, 1, 0]),
            (CombOp::And, [1, 0, 0, 0]),
            (CombOp::Xor, [0, 1, 1, 0]),
            (CombOp::Xnor, [1, 0, 0, 1]),
            (CombOp::Replace, [1, 0, 1, 0]),
        ];
        for (op, want) in cases {
            let mut dst = from_rows(&["1100"]);
            let src = from_rows(&["1010"]);
            dst.combine(&src, 0, 0, op);
            assert_eq!(dst.row(0), &want, "{op:?}");
        }
    }

    #[test]
    fn combine_clips_at_every_edge() {
        let src = from_rows(&["11", "11"]);
        // Straddling the top-left corner: only src(1,1) lands, at dst(0,0).
        let mut dst = Bitmap::new(3, 3).expect("3x3");
        dst.combine(&src, -1, -1, CombOp::Or);
        assert_eq!(dst.row(0), &[1, 0, 0]);
        assert_eq!(dst.row(1), &[0, 0, 0]);

        // Straddling the bottom-right corner: only src(0,0) lands, at dst(2,2).
        let mut dst = Bitmap::new(3, 3).expect("3x3");
        dst.combine(&src, 2, 2, CombOp::Or);
        assert_eq!(dst.row(2), &[0, 0, 1]);

        // Entirely outside: nothing changes, nothing panics.
        let mut dst = Bitmap::new(3, 3).expect("3x3");
        dst.combine(&src, 50, 50, CombOp::Replace);
        dst.combine(&src, -50, -50, CombOp::Replace);
        assert_eq!(dst.row(1), &[0, 0, 0]);
    }

    /// Offsets at the extremes of `i32` must clip like any other, without the
    /// offset arithmetic wrapping on the way.
    #[test]
    fn combine_survives_extreme_offsets() {
        let src = from_rows(&["11", "11"]);
        let mut dst = Bitmap::new(3, 3).expect("3x3");
        dst.combine(&src, i32::MIN, i32::MIN, CombOp::Or);
        dst.combine(&src, i32::MAX, i32::MAX, CombOp::Or);
        dst.combine(&src, i32::MIN, i32::MAX, CombOp::Replace);
        assert_eq!(dst.row(0), &[0, 0, 0]);
        assert_eq!(dst.row(2), &[0, 0, 0]);
    }

    /// REPLACE must overwrite with zeros too, not just set ones.
    #[test]
    fn combine_replace_clears_pixels() {
        let mut dst = from_rows(&["1111"]);
        let src = from_rows(&["0000"]);
        dst.combine(&src, 0, 0, CombOp::Replace);
        assert_eq!(dst.row(0), &[0, 0, 0, 0]);
    }

    /// Packed output is MSB-first with each row padded to a byte boundary,
    /// matching PDF image sample layout.
    #[test]
    fn pack_rows_is_msb_first_and_row_padded() {
        let bm = from_rows(&["100000001", "010000000"]);
        // 9 pixels per row -> 2 bytes per row.
        assert_eq!(
            bm.pack_rows(),
            vec![0b1000_0000, 0b1000_0000, 0b0100_0000, 0b0000_0000]
        );
    }

    #[test]
    fn pack_rows_of_an_exact_byte_width() {
        let bm = from_rows(&["10110010"]);
        assert_eq!(bm.pack_rows(), vec![0b1011_0010]);
    }

    #[test]
    fn comb_op_rejects_reserved_values() {
        for bits in 0..=4u8 {
            assert!(CombOp::from_bits(bits).is_ok(), "{bits} must be valid");
        }
        for bits in 5..=7u8 {
            assert_eq!(
                CombOp::from_bits(bits),
                Err(Jbig2Error::Malformed("reserved combination operator")),
            );
        }
    }

    /// A hostile width x height must be refused before allocating, and the
    /// multiply must not wrap.
    #[test]
    fn oversized_bitmaps_are_refused_without_allocating() {
        assert_eq!(
            Bitmap::new(u32::MAX, u32::MAX),
            Err(Jbig2Error::TooLarge {
                width: u32::MAX,
                height: u32::MAX
            }),
        );
        assert!(Bitmap::new(0xFFFF, 0xFFFF).is_err());
        assert!(Bitmap::filled(0xFFFF, 0xFFFF, 1).is_err());
        // Just under the cap still works.
        assert!(Bitmap::new(4096, 4096).is_ok());
    }

    /// A zero-dimension region is legal and yields an empty bitmap.
    #[test]
    fn zero_dimensions_are_allowed() {
        let bm = Bitmap::new(0, 5).expect("0x5");
        assert_eq!((bm.width(), bm.height()), (0, 5));
        assert!(bm.pack_rows().is_empty());
        let bm = Bitmap::new(5, 0).expect("5x0");
        assert!(bm.pack_rows().is_empty());
    }

    /// The cap tests a product, so a zero dimension slips past it at any
    /// height at all. That is the right answer for an allocation — there is
    /// nothing to allocate — and it is precisely why this cap cannot double as
    /// a bound on how much work such a bitmap costs.
    ///
    /// Every operation on one has to reach its answer without walking those
    /// rows. `/Width 0` and `/Height 4294967295` is a pair of numbers an image
    /// dictionary is free to contain, so this is not a hypothetical shape.
    #[test]
    fn a_zero_width_bitmap_of_any_height_costs_nothing_to_use() {
        let mut bm = Bitmap::new(0, u32::MAX).expect("legal, and empty");
        assert_eq!((bm.width(), bm.height()), (0, u32::MAX));
        assert!(bm.row(0).is_empty());
        assert!(bm.row(u32::MAX - 1).is_empty());
        assert!(bm.pack_rows().is_empty());
        bm.duplicate_row(u32::MAX - 1);
        bm.set(0, u32::MAX - 1, 1);
        assert_eq!(bm.get(0, i64::from(u32::MAX) - 1), 0);

        // Compositing in either direction has to clip away without walking the
        // rows either, and the empty bitmap must not disturb a real one.
        let src = from_rows(&["11", "11"]);
        bm.combine(&src, 0, 0, CombOp::Or);
        let mut dst = from_rows(&["00", "00"]);
        dst.combine(&bm, 0, 0, CombOp::Or);
        assert_eq!(dst.row(0), &[0, 0]);

        let tall = Bitmap::new(0, u32::MAX).expect("legal, and empty");
        let mut wide = Bitmap::new(4, 4).expect("4x4");
        wide.combine(&tall, 0, 0, CombOp::Replace);
        assert_eq!(wide.row(3), &[0, 0, 0, 0]);
    }
}
