//! Text region segments (T.88 6.4, 7.4.3).
//!
//! A text region is the placement list a symbol dictionary exists to serve: a
//! sequence of (symbol, position) pairs, coded as small integers because every
//! position is expressed as a delta from the last one.
//!
//! The region is walked in horizontal **strips** of SBSTRIPS rows. `T` is the
//! coordinate across the strips and `S` the coordinate along them, and
//! TRANSPOSED swaps which axis each of the two indexes: with it clear `S` runs
//! across the page and `T` down it, with it set the other way about. Within a
//! strip the running coordinate CURS is advanced by the gap `IDS` plus
//! SBDSOFFSET — the offset applies to the gap between instances, never to the
//! coordinate itself — and an OOB from `IADS` closes the strip.
//!
//! The part of 6.4.5 worth stating plainly is the one that keeps those gaps
//! small. CURS is advanced past the symbol *before* the draw for the two
//! corners that name the far edge along the strip, and *after* it for the two
//! that name the near edge. The two conditions are exact complements, so the
//! invariant either way is that CURS finishes on the symbol's far edge and the
//! next `IDS` is the gap from there. Getting the split wrong does not fail: it
//! makes text drift by one pixel per symbol along every line.
//!
//! Huffman-coded text regions (SBHUFF) and instance refinement (REFINE) are
//! refused by name rather than approximated.

use super::arith_int::{decode_iaid, decode_int, IaidCtx, IntCtxSet};
use super::bitmap::{Bitmap, CombOp};
use super::budget::Budget;
use super::mq::MqDecoder;
use super::reader::Reader;
use super::segment::{parse_region_info, RegionInfo};
use super::Jbig2Error;

/// The most symbol instances one text region may place.
///
/// T.88 gives SBNUMINSTANCES a 32-bit field and no ceiling. Four million
/// placements is far past any page a scanner produces — a dense A4 page of
/// small type holds a few thousand — and the cap is checked before the count
/// drives a loop. The work each placement then costs is charged separately,
/// against the stream's budget, so this is a sanity bound rather than the thing
/// that makes the region affordable.
pub(crate) const MAX_INSTANCES: u32 = 1 << 22;

/// What placing one symbol instance costs beyond the pixels it composites,
/// in the units [`Budget`] counts.
///
/// A placement is four or five arithmetic integer decodes and a symbol ID
/// whatever the symbol's size, so it is never free — and a symbol with no rows
/// composites nothing at all, which would otherwise let SBNUMINSTANCES buy
/// arithmetic decoding the budget never sees. The figure is not an exact
/// accounting of those decisions; it is a fixed price that ties the number of
/// placements a stream may make to the one allowance the stream has.
pub(crate) const INSTANCE_COST: u64 = 64;

/// Which corner of a symbol its coded coordinate names (T.88 7.4.3.1.1,
/// REFCORNER).
///
/// The discriminants are the field's own encoding, which is why the ordering
/// looks arbitrary: the bit pattern counts up through BOTTOMLEFT, TOPLEFT,
/// BOTTOMRIGHT, TOPRIGHT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefCorner {
    /// The symbol's bottom-left pixel lies at the coded coordinate.
    BottomLeft,
    /// The symbol's top-left pixel lies at the coded coordinate.
    TopLeft,
    /// The symbol's bottom-right pixel lies at the coded coordinate.
    BottomRight,
    /// The symbol's top-right pixel lies at the coded coordinate.
    TopRight,
}

impl RefCorner {
    /// Decodes the two-bit REFCORNER field of T.88 7.4.3.1.1.
    ///
    /// All four values are defined, so there is nothing to reject; bits above
    /// the low two belong to other fields and are masked away.
    pub(crate) fn from_bits(bits: u8) -> RefCorner {
        match bits & 0x3 {
            0 => RefCorner::BottomLeft,
            1 => RefCorner::TopLeft,
            2 => RefCorner::BottomRight,
            _ => RefCorner::TopRight,
        }
    }
}

/// SBSYMCODELEN: the number of bits an arithmetic text region spends on a
/// symbol ID (T.88 7.4.3.2, Table 31).
///
/// It is the width of the largest id, `ceil(log2(SBNUMSYMS))`, which is 0 when
/// there is exactly one symbol — a region with a single symbol codes no id bits
/// at all. Computed from `leading_zeros` rather than a logarithm so that no
/// rounding decision is delegated to floating point.
pub(crate) fn sym_code_len(num_syms: u32) -> u32 {
    if num_syms > 1 {
        u32::BITS - (num_syms - 1).leading_zeros()
    } else {
        0
    }
}

/// Decodes a text region segment's data (T.88 7.4.3), returning where the
/// region goes and the pixels that go there.
///
/// `symbols` are the symbols exported by the referred-to dictionary segments,
/// concatenated in the order the referred-to list gives them (SBSYMS). Their
/// count is what sizes the symbol ID code, so an empty list is refused: it
/// would leave every id in the stream unanswerable.
///
/// `budget` is the embedded stream's remaining allowance of decoding work, the
/// same one the page's other regions draw on. The region is charged from the
/// dimensions its header declares before its bitmap is allocated, and each
/// placement is charged before it composites, so neither the declared size nor
/// the declared instance count can buy work the allowance never sees.
pub(crate) fn decode_text_region(
    data: &[u8],
    symbols: &[&Bitmap],
    budget: &mut Budget,
) -> Result<(RegionInfo, Bitmap), Jbig2Error> {
    let mut r = Reader::new(data);
    let info = parse_region_info(&mut r)?;
    let params = parse_params(&mut r)?;
    if symbols.is_empty() {
        return Err(Jbig2Error::Malformed("text region with no symbols"));
    }
    let num_syms = u32::try_from(symbols.len())
        .map_err(|_| Jbig2Error::Malformed("symbol count exceeds the limit"))?;

    budget.charge_region(info.width, info.height)?;
    let mut region = Bitmap::filled(info.width, info.height, params.def_pixel)?;

    Walk {
        values: Arithmetic {
            dec: MqDecoder::new(r.rest()),
            ints: IntCtxSet::new(),
            iaid: IaidCtx::new(sym_code_len(num_syms)),
        },
        region: &mut region,
        symbols,
        params: &params,
        budget,
    }
    .run()?;
    Ok((info, region))
}

/// The fields of a text region segment that precede its coded data
/// (T.88 7.4.3.1).
struct TextParams {
    /// SBSTRIPS, the number of rows one strip spans: `1 << LOGSBSTRIPS`, so 1,
    /// 2, 4 or 8.
    strips: i32,
    /// REFCORNER, which corner of a symbol its coordinate names.
    corner: RefCorner,
    /// TRANSPOSED, which swaps the axes S and T index.
    transposed: bool,
    /// SBCOMBOP, how a symbol's pixels combine with what is already there.
    comb_op: CombOp,
    /// SBDEFPIXEL, the value the region is filled with before any placement.
    def_pixel: u8,
    /// SBDSOFFSET, added to every gap after the first instance of a strip.
    ds_offset: i32,
    /// SBNUMINSTANCES, the number of placements the region carries.
    instances: u32,
}

/// Parses the text region segment flags and the instance count that follows
/// them (T.88 7.4.3.1.1 and 7.4.3.1.4).
///
/// The two coding modes this build does not implement are refused before any
/// further byte is read, because the layout after the flags depends on them: a
/// Huffman region carries a flags word of its own here and a refining one
/// carries the SBRAT pixels, so reading past either would leave the cursor in
/// the wrong field and turn an unsupported stream into a plausible wrong
/// answer.
///
/// Bit 15, SBRTEMPLATE, selects the template refinement uses; with REFINE
/// refused above it selects nothing, so it is not examined.
fn parse_params(r: &mut Reader<'_>) -> Result<TextParams, Jbig2Error> {
    let flags = r.u16()?;
    if flags & 0x0001 != 0 {
        return Err(Jbig2Error::Unimplemented("Huffman-coded text region"));
    }
    if flags & 0x0002 != 0 {
        return Err(Jbig2Error::Unimplemented("text region symbol refinement"));
    }
    let strips = 1i32 << ((flags >> 2) & 0x3);
    let corner = RefCorner::from_bits(((flags >> 4) & 0x3) as u8);
    let transposed = flags & 0x0040 != 0;
    // Two bits here rather than the three a region information field carries,
    // so REPLACE is unreachable — 7.4.3.1.1 does not offer it.
    let comb_op = CombOp::from_bits(((flags >> 7) & 0x3) as u8)?;
    let def_pixel = u8::from(flags & 0x0200 != 0);
    // SBDSOFFSET is five bits of two's complement, so it sign-extends from bit
    // 14 of the flags word — bit 4 of the extracted field — and not from bit
    // 15, which belongs to SBRTEMPLATE.
    let raw = i32::from((flags >> 10) & 0x1F);
    let ds_offset = if raw > 15 { raw - 32 } else { raw };

    let instances = r.u32()?;
    if instances > MAX_INSTANCES {
        return Err(Jbig2Error::Malformed("instance count exceeds the limit"));
    }
    Ok(TextParams {
        strips,
        corner,
        transposed,
        comb_op,
        def_pixel,
        ds_offset,
        instances,
    })
}

/// Where a text region's coded values come from (T.88 6.4.6 to 6.4.10).
///
/// The walk of 6.4.5 is one procedure over six reads, and each of those six
/// clauses defines the read twice over — once for the arithmetic variant of the
/// format and once for the Huffman one. Naming them here is what keeps the
/// placement arithmetic, which is the same either way and is the part that goes
/// subtly wrong, separate from whatever is feeding it.
///
/// `Ok(None)` is OOB. Only 6.4.8's is meaningful — it is what closes a strip —
/// but every read is given the same shape so that the walk, rather than the
/// source, decides what an unexpected one means.
trait Values {
    /// 6.4.6, before the multiplication by SBSTRIPS the caller applies. Serves
    /// both the initial STRIPT of 6.4.5 step 2 and each strip's delta.
    fn delta_t(&mut self) -> Result<Option<i32>, Jbig2Error>;
    /// 6.4.7: the S coordinate of a strip's first instance, as a delta on
    /// FIRSTS.
    fn first_s(&mut self) -> Result<Option<i32>, Jbig2Error>;
    /// 6.4.8: the gap to a later instance of the strip. OOB closes the strip.
    fn delta_s(&mut self) -> Result<Option<i32>, Jbig2Error>;
    /// 6.4.9: an instance's T coordinate within its strip.
    fn curt(&mut self) -> Result<Option<i32>, Jbig2Error>;
    /// 6.4.10: an instance's symbol ID.
    fn symbol_id(&mut self) -> Result<u32, Jbig2Error>;
}

/// The arithmetic value source: the integer procedures of Annex A, all drawing
/// on one decoder and each adapting its own contexts across the whole region.
struct Arithmetic<'d> {
    /// The one arithmetic decoder every coded value of the region comes from.
    dec: MqDecoder<'d>,
    /// The integer procedures of Annex A, adapting across the whole region.
    ints: IntCtxSet,
    /// The symbol ID procedure of A.3, sized by SBSYMCODELEN.
    iaid: IaidCtx,
}

impl Values for Arithmetic<'_> {
    fn delta_t(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(&mut self.dec, &mut self.ints.iadt))
    }

    fn first_s(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(&mut self.dec, &mut self.ints.iafs))
    }

    fn delta_s(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(&mut self.dec, &mut self.ints.iads))
    }

    fn curt(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(&mut self.dec, &mut self.ints.iait))
    }

    fn symbol_id(&mut self) -> Result<u32, Jbig2Error> {
        Ok(decode_iaid(&mut self.dec, &mut self.iaid))
    }
}

/// The state a text region's strip walk carries (T.88 6.4.5).
///
/// The value source is owned rather than borrowed because it is the walk's
/// alone: everything the region reads is read through it, in order, and nothing
/// else in the segment shares the cursor it sits on.
struct Walk<'a, V> {
    /// Where the coded values come from.
    values: V,
    /// SBREGBITMAP, the region being painted.
    region: &'a mut Bitmap,
    /// SBSYMS, the symbols the coded ids index.
    symbols: &'a [&'a Bitmap],
    /// The parameters the segment header fixed.
    params: &'a TextParams,
    /// The embedded stream's remaining allowance of decoding work.
    budget: &'a mut Budget,
}

impl<V: Values> Walk<'_, V> {
    /// Walks the strips, compositing every symbol instance the region declares
    /// (T.88 6.4.5).
    ///
    /// Both loops end on something the coded data cannot extend. Every pass of
    /// the inner loop either places an instance or reads the OOB that closes
    /// the strip, and an exhausted arithmetic decoder reads as OOB (T.88
    /// E.3.4); every pass of the outer loop enters the inner one with at least
    /// one placement still owed, and the inner loop's first pass always takes
    /// it. So the total number of passes is bounded by SBNUMINSTANCES, which
    /// the segment header fixed before any of this was read.
    fn run(&mut self) -> Result<(), Jbig2Error> {
        let strips = i64::from(self.params.strips);
        // 6.4.5 step 2: the leading strip offset is negated, so a region whose
        // first strip starts above its own top edge says so with a positive
        // value here.
        let initial = self.values.delta_t()?.ok_or(Jbig2Error::Malformed(
            "unexpected OOB decoding the leading strip offset",
        ))?;
        let mut strip_t = i64::from(initial).saturating_mul(strips).saturating_neg();
        // FIRSTS runs across the whole region rather than resetting per strip:
        // each strip's first instance is a delta on the previous strip's.
        let mut first_s: i64 = 0;
        let mut placed: u32 = 0;

        while placed < self.params.instances {
            let delta = self.values.delta_t()?.ok_or(Jbig2Error::Malformed(
                "unexpected OOB decoding a strip offset",
            ))?;
            // The delta counts strips, not rows (6.4.5 step 3(b)).
            strip_t = strip_t.saturating_add(i64::from(delta).saturating_mul(strips));

            // 6.4.5 step 3(c)(i): a strip's first instance gives its S
            // coordinate as a delta on FIRSTS.
            let dfs = self.values.first_s()?.ok_or(Jbig2Error::Malformed(
                "unexpected OOB decoding a first S coordinate",
            ))?;
            first_s = first_s.saturating_add(i64::from(dfs));
            let mut curs = first_s;

            loop {
                if placed >= self.params.instances {
                    break;
                }
                curs = self.place_one(curs, strip_t)?;
                placed += 1;

                // Every later instance of the strip gives the gap from the far
                // edge of the one just placed, offset by SBDSOFFSET; an OOB
                // closes the strip.
                let Some(ids) = self.values.delta_s()? else {
                    break;
                };
                curs = curs
                    .saturating_add(i64::from(ids))
                    .saturating_add(i64::from(self.params.ds_offset));
            }
        }
        Ok(())
    }

    /// Decodes and composites one symbol instance, returning the value CURS
    /// takes after it (T.88 6.4.5 step 3(c)).
    fn place_one(&mut self, curs: i64, strip_t: i64) -> Result<i64, Jbig2Error> {
        // 6.4.5 step 3(c)(iii): a one-row strip has nowhere to offset within,
        // and 6.4.9 codes no IAIT value for it.
        let curt = if self.params.strips == 1 {
            0
        } else {
            self.values.curt()?.ok_or(Jbig2Error::Malformed(
                "unexpected OOB decoding a T coordinate",
            ))?
        };
        let ti = strip_t.saturating_add(i64::from(curt));

        let id = self.values.symbol_id()?;
        // The code length is the bit width of the largest id, so a symbol count
        // that is not a power of two leaves ids the code can express and the
        // list cannot answer. Refusing those keeps the lookup in bounds.
        let symbol = *self
            .symbols
            .get(id as usize)
            .ok_or(Jbig2Error::Malformed("symbol id out of range"))?;

        self.budget.charge(INSTANCE_COST)?;
        self.budget.charge_region(symbol.width(), symbol.height())?;

        // 6.4.5 steps 3(c)(vi) and (x): CURS always finishes on the symbol's
        // far edge along the strip. Which end of the symbol that is depends on
        // the corner, so the advance happens either before the draw or after it
        // — never both, never neither. The two conditions are complements,
        // which is why one boolean drives them.
        let w = i64::from(symbol.width());
        let h = i64::from(symbol.height());
        let extent = if self.params.transposed { h } else { w } - 1;
        let advance_first = if self.params.transposed {
            matches!(
                self.params.corner,
                RefCorner::BottomLeft | RefCorner::BottomRight
            )
        } else {
            matches!(
                self.params.corner,
                RefCorner::TopRight | RefCorner::BottomRight
            )
        };

        let si = if advance_first {
            curs.saturating_add(extent)
        } else {
            curs
        };
        let (x, y) = top_left(si, ti, w, h, self.params.transposed, self.params.corner);
        self.region.combine(
            symbol,
            clamp_offset(x),
            clamp_offset(y),
            self.params.comb_op,
        );
        Ok(if advance_first {
            si
        } else {
            si.saturating_add(extent)
        })
    }
}

/// Where a symbol's top-left pixel lands, given that its REFCORNER lies at
/// `(s, t)` (T.88 6.4.5 step 3(c)(viii)).
///
/// TRANSPOSED does not rotate the symbol; it swaps which axis each of the two
/// coordinates indexes. So the untransposed cases read `(s, t)` as
/// `(column, row)` and the transposed ones read `(t, s)`, while the corner
/// adjustments stay attached to the symbol's own width and height.
fn top_left(s: i64, t: i64, w: i64, h: i64, transposed: bool, corner: RefCorner) -> (i64, i64) {
    let from_right = |v: i64| v.saturating_sub(w).saturating_add(1);
    let from_bottom = |v: i64| v.saturating_sub(h).saturating_add(1);
    if transposed {
        match corner {
            RefCorner::TopLeft => (t, s),
            RefCorner::TopRight => (from_right(t), s),
            RefCorner::BottomLeft => (t, from_bottom(s)),
            RefCorner::BottomRight => (from_right(t), from_bottom(s)),
        }
    } else {
        match corner {
            RefCorner::TopLeft => (s, t),
            RefCorner::TopRight => (from_right(s), t),
            RefCorner::BottomLeft => (s, from_bottom(t)),
            RefCorner::BottomRight => (from_right(s), from_bottom(t)),
        }
    }
}

/// Narrows a placement coordinate to the offset [`Bitmap::combine`] takes.
///
/// The coordinates are accumulated in `i64` because the deltas that build them
/// are signed 32-bit values a stream may repeat, so they can leave the range an
/// offset can express. Saturating rather than wrapping is what makes that
/// harmless: a coordinate that far outside the region clips entirely away,
/// which is the right outcome, whereas a wrap would paint it over the opposite
/// corner.
fn clamp_offset(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::jbig2::bitmap::Bitmap;
    use crate::filters::jbig2::budget::{Budget, ROW_COST};
    use crate::filters::jbig2::testing::{
        expect_at, glyph, text_segment, text_segment_with_curt, two_symbols, Op, Shape,
    };
    use crate::filters::jbig2::Jbig2Error;

    /// Decodes with the allowance a real embedded stream gets.
    fn decode(data: &[u8], symbols: &[&Bitmap]) -> Result<(RegionInfo, Bitmap), Jbig2Error> {
        decode_text_region(data, symbols, &mut Budget::new())
    }

    /// Three instances across two strips, with the coordinates walked by hand
    /// from 6.4.5.
    ///
    /// STRIPT starts at `-0`, the first strip's `IADT` of 2 puts it at row 2,
    /// and FIRSTS of 1 puts the first symbol's left edge at column 1. CURS then
    /// ends on that symbol's far edge, column 3, so the gap of 2 lands the
    /// second symbol at column 5 rather than at column 3. The second strip's
    /// `IADT` of 5 puts it at row 7, and its `IAFS` of -1 carries FIRSTS back
    /// from 1 to 0 — FIRSTS runs across strips, unlike CURS.
    #[test]
    fn places_instances_across_strips() {
        let syms = two_symbols();
        let refs: Vec<&Bitmap> = syms.iter().collect();
        let data = text_segment(
            (20, 16),
            Shape::default(),
            3,
            2,
            0,
            &[
                Op::Strip(2),
                Op::First(1, 0),
                Op::Next(2, 1),
                Op::EndStrip,
                Op::Strip(5),
                Op::First(-1, 0),
                Op::EndStrip,
            ],
        );
        let (info, region) = decode(&data, &refs).expect("text region");
        assert_eq!((info.width, info.height), (20, 16));
        expect_at(&region, &syms[0], 1, 2);
        expect_at(&region, &syms[1], 5, 2);
        expect_at(&region, &syms[0], 0, 7);
        // Nothing was painted outside those three placements.
        assert_eq!(region.get(19, 15), 0);
    }

    /// All eight TRANSPOSED by REFCORNER combinations, against coordinates
    /// derived by hand from 6.4.5 steps 3(c)(vi), (viii) and (x).
    ///
    /// One instance of a 3 by 2 symbol, FIRSTS = 4 and STRIPT = 5. Where two
    /// rows agree it is not a coincidence: the advance of step (vi) and the
    /// corner offset of step (viii) are designed to cancel, so a symbol
    /// occupies the same cells whichever end of itself it is placed by.
    #[test]
    fn honours_every_refcorner_and_transposition() {
        let symbol = glyph(&["101", "010"]);
        let syms = [&symbol];
        // (transposed, corner, expected x, expected y)
        let cases: [(bool, u8, i64, i64); 8] = [
            (false, 1, 4, 5), // TOPLEFT
            (false, 3, 4, 5), // TOPRIGHT
            (false, 0, 4, 4), // BOTTOMLEFT
            (false, 2, 4, 4), // BOTTOMRIGHT
            (true, 1, 5, 4),  // TOPLEFT
            (true, 3, 3, 4),  // TOPRIGHT
            (true, 0, 5, 4),  // BOTTOMLEFT
            (true, 2, 3, 4),  // BOTTOMRIGHT
        ];
        for (transposed, corner, x, y) in cases {
            let shape = Shape {
                corner,
                transposed,
                ..Shape::default()
            };
            let data = text_segment(
                (16, 16),
                shape,
                1,
                1,
                0,
                &[Op::Strip(5), Op::First(4, 0), Op::EndStrip],
            );
            let (_, region) = decode(&data, &syms).expect("text region");
            expect_at(&region, &symbol, x, y);
        }
    }

    /// The invariant behind the pre/post split of steps 3(c)(vi) and (ix):
    /// after any placement CURS sits on the symbol's far edge along the strip,
    /// in all eight combinations.
    ///
    /// A single placement cannot see this, because the corner offset of step
    /// (viii) hides where CURS ended. A second instance can: `IDS` is measured
    /// from the first symbol's far edge, so the distance between the two
    /// placements is `extent + IDS` exactly when the invariant holds. Get it
    /// wrong in one direction and every line drifts wider as it runs; wrong in
    /// the other and the symbols pile up. Neither fails outright, which is why
    /// the gap is asserted rather than the first placement alone.
    #[test]
    fn curs_ends_on_the_symbol_far_edge_for_every_corner() {
        let symbol = glyph(&["101", "010"]);
        let syms = [&symbol];
        // transposed, corner, then the two placements as x, y and x, y.
        let cases: [(bool, u8, i64, i64, i64, i64); 8] = [
            (false, 1, 4, 3, 8, 3), // TOPLEFT
            (false, 3, 4, 3, 8, 3), // TOPRIGHT
            (false, 0, 4, 2, 8, 2), // BOTTOMLEFT
            (false, 2, 4, 2, 8, 2), // BOTTOMRIGHT
            (true, 1, 3, 4, 3, 7),  // TOPLEFT
            (true, 3, 1, 4, 1, 7),  // TOPRIGHT
            (true, 0, 3, 4, 3, 7),  // BOTTOMLEFT
            (true, 2, 1, 4, 1, 7),  // BOTTOMRIGHT
        ];
        for (transposed, corner, first_x, first_y, second_x, second_y) in cases {
            let shape = Shape {
                corner,
                transposed,
                ..Shape::default()
            };
            let data = text_segment(
                (24, 24),
                shape,
                2,
                1,
                0,
                &[Op::Strip(3), Op::First(4, 0), Op::Next(2, 0), Op::EndStrip],
            );
            let (_, region) = decode(&data, &syms).expect("text region");
            expect_at(&region, &symbol, first_x, first_y);
            expect_at(&region, &symbol, second_x, second_y);
        }
    }

    /// SBDSOFFSET is a five-bit signed field applied to every gap after the
    /// first instance of a strip, not to the coordinate itself.
    #[test]
    fn applies_the_signed_ds_offset() {
        let symbol = glyph(&["11", "11"]);
        let syms = [&symbol];
        for offset in [-16i32, -1, 0, 1, 15] {
            let shape = Shape {
                dsoffset: offset,
                ..Shape::default()
            };
            let data = text_segment(
                (48, 8),
                shape,
                2,
                1,
                0,
                &[Op::Strip(0), Op::First(4, 0), Op::Next(20, 0), Op::EndStrip],
            );
            let (_, region) = decode(&data, &syms).expect("text region");
            // The first instance sits at S = 4 and leaves CURS on its far edge,
            // 4 + 2 - 1 = 5, so the second sits at 5 + 20 + offset. The gap is
            // 20 rather than something smaller so that even an offset of -16
            // leaves the second symbol on the region instead of clipping away
            // and passing vacuously.
            expect_at(&region, &symbol, 4, 0);
            expect_at(&region, &symbol, i64::from(25 + offset), 0);
        }
    }

    /// With SBSTRIPS greater than one each instance carries its own T offset
    /// within the strip, decoded through `IAIT`.
    #[test]
    fn decodes_the_within_strip_t_offset() {
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        let shape = Shape {
            log_strips: 2, // SBSTRIPS = 4
            ..Shape::default()
        };
        // STRIPT starts at -0 * 4 and the strip's delta of 1 puts it at row 4.
        // The strip holds two instances, at CURT 0 and CURT 3 within it.
        let data =
            text_segment_with_curt((8, 16), shape, 2, 1, 0, &[(1, &[(2, 0, 0), (4, 3, 0)][..])]);
        let (_, region) = decode(&data, &syms).expect("text region");
        assert_eq!(region.get(2, 4), 1, "first at S = 2, T = 4 + 0");
        // A 1 by 1 symbol leaves CURS where it started, so the gap of 4 puts
        // the second instance at S = 6.
        assert_eq!(region.get(6, 7), 1, "second at S = 6, T = 4 + 3");
    }

    /// The delta on STRIPT is counted in strips, so with SBSTRIPS at 4 a delta
    /// of 1 moves four rows down rather than one.
    #[test]
    fn the_strip_delta_is_scaled_by_the_strip_height() {
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        let shape = Shape {
            log_strips: 2, // SBSTRIPS = 4
            ..Shape::default()
        };
        let data = text_segment_with_curt(
            (8, 32),
            shape,
            2,
            1,
            0,
            &[(1, &[(0, 0, 0)][..]), (3, &[(1, 0, 0)][..])],
        );
        let (_, region) = decode(&data, &syms).expect("text region");
        assert_eq!(region.get(0, 4), 1, "first strip at row 1 * 4");
        assert_eq!(region.get(1, 16), 1, "second strip at row (1 + 3) * 4");
    }

    /// Step 2 negates the leading `IADT`, so a positive value starts the
    /// region above its own top edge.
    #[test]
    fn the_leading_strip_offset_is_negated() {
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        // STRIPT = -3, then + 5 = 2.
        let data = text_segment(
            (8, 8),
            Shape::default(),
            1,
            1,
            3,
            &[Op::Strip(5), Op::First(0, 0), Op::EndStrip],
        );
        let (_, region) = decode(&data, &syms).expect("text region");
        assert_eq!(region.get(0, 2), 1, "STRIPT = -3 + 5");
    }

    #[test]
    fn honours_the_default_pixel_value() {
        let symbol = glyph(&["0"]);
        let syms = [&symbol];
        let shape = Shape {
            defpixel: true,
            combop: 3, // XNOR, so a 0 symbol pixel over a 1 ground gives 0
            ..Shape::default()
        };
        let data = text_segment(
            (4, 4),
            shape,
            1,
            1,
            0,
            &[Op::Strip(0), Op::First(0, 0), Op::EndStrip],
        );
        let (_, region) = decode(&data, &syms).expect("text region");
        assert_eq!(region.get(3, 3), 1, "untouched cells keep SBDEFPIXEL");
        assert_eq!(region.get(0, 0), 0, "XNOR of 1 and 0 is 0");
    }

    #[test]
    fn sym_code_len_is_the_bit_width_of_the_largest_id() {
        assert_eq!(sym_code_len(1), 0);
        assert_eq!(sym_code_len(2), 1);
        assert_eq!(sym_code_len(3), 2);
        assert_eq!(sym_code_len(4), 2);
        assert_eq!(sym_code_len(5), 3);
        assert_eq!(sym_code_len(256), 8);
        assert_eq!(sym_code_len(257), 9);
    }

    /// A region with no symbols at all is degenerate rather than illegal for
    /// this helper, and must not underflow on `SBNUMSYMS - 1`.
    #[test]
    fn sym_code_len_of_no_symbols_is_zero() {
        assert_eq!(sym_code_len(0), 0);
    }

    #[test]
    fn huffman_and_refinement_report_themselves() {
        for (bit, want) in [
            (0x0001u16, "Huffman-coded text region"),
            (0x0002, "text region symbol refinement"),
        ] {
            let mut data = vec![0u8; 17];
            data.extend_from_slice(&bit.to_be_bytes());
            data.extend_from_slice(&0u32.to_be_bytes());
            assert_eq!(decode(&data, &[]), Err(Jbig2Error::Unimplemented(want)));
        }
    }

    /// A symbol count that is not a power of two leaves ids the code can
    /// express but the symbol list cannot answer, and those must be refused
    /// rather than indexed.
    #[test]
    fn an_out_of_range_symbol_id_is_rejected() {
        let symbols = [glyph(&["1"]), glyph(&["1"]), glyph(&["1"])];
        let refs: Vec<&Bitmap> = symbols.iter().collect();
        // Three symbols need a two-bit code, which can also carry the id 3.
        let data = text_segment(
            (8, 8),
            Shape::default(),
            1,
            3,
            0,
            &[Op::Strip(0), Op::First(0, 3), Op::EndStrip],
        );
        assert_eq!(
            decode(&data, &refs),
            Err(Jbig2Error::Malformed("symbol id out of range")),
        );
    }

    #[test]
    fn a_text_region_with_no_symbols_is_rejected() {
        let mut data = vec![0u8; 17];
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&1u32.to_be_bytes());
        assert_eq!(
            decode(&data, &[]),
            Err(Jbig2Error::Malformed("text region with no symbols")),
        );
    }

    #[test]
    fn an_absurd_instance_count_is_refused() {
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        let mut data = vec![0u8; 17];
        data[3] = 8; // width 8
        data[7] = 8; // height 8
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            decode(&data, &syms),
            Err(Jbig2Error::Malformed("instance count exceeds the limit")),
        );
    }

    /// A stream that never closes its strip is stopped by the declared
    /// instance count rather than placing symbols past it.
    #[test]
    fn a_strip_that_never_ends_stops_at_the_declared_count() {
        let symbol = glyph(&["11", "11"]);
        let syms = [&symbol];
        let data = text_segment(
            (32, 8),
            Shape::default(),
            1, // one instance declared, three coded
            1,
            0,
            &[
                Op::Strip(0),
                Op::First(0, 0),
                Op::Next(4, 0),
                Op::Next(4, 0),
                Op::EndStrip,
            ],
        );
        let (_, region) = decode(&data, &syms).expect("text region");
        expect_at(&region, &symbol, 0, 0);
        assert_eq!(region.get(5, 0), 0, "the second instance was not placed");
    }

    /// A region declaring dimensions far beyond the stream's remaining
    /// allowance is refused from the header, before a pixel is decoded.
    #[test]
    fn an_enormous_region_is_refused_by_the_budget() {
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        let mut data = 8_000u32.to_be_bytes().to_vec();
        data.extend_from_slice(&8_000u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.push(0); // OR
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&1u32.to_be_bytes());
        assert_eq!(
            decode_text_region(&data, &syms, &mut Budget::with_limit(1 << 20)),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// The instances of a region draw on the same allowance the region itself
    /// does, so a small region cannot buy unbounded composition by declaring a
    /// great many placements.
    #[test]
    fn instances_draw_on_the_stream_budget() {
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        let data = text_segment(
            (8, 8),
            Shape::default(),
            2,
            1,
            0,
            &[Op::Strip(0), Op::First(0, 0), Op::Next(2, 0), Op::EndStrip],
        );
        // The 8 by 8 region itself, then a fixed price plus the composited
        // area for each of the two placements of a 1 by 1 symbol.
        let region_cost = 8 * (8 + ROW_COST);
        let instance_cost = INSTANCE_COST + (1 + ROW_COST);
        let total = region_cost + 2 * instance_cost;

        let mut budget = Budget::with_limit(total);
        assert!(decode_text_region(&data, &syms, &mut budget).is_ok());

        let mut budget = Budget::with_limit(total - 1);
        assert_eq!(
            decode_text_region(&data, &syms, &mut budget),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// No byte string, however malformed, may panic, hang or read out of
    /// bounds.
    #[test]
    fn arbitrary_bytes_error_rather_than_panicking() {
        let symbol = glyph(&["1", "1"]);
        let syms = [&symbol];
        let mut state: u32 = 0x7E57_10AD;
        for _ in 0..2_000 {
            let len = (state % 129) as usize;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            let _ = decode_text_region(&data, &syms, &mut Budget::with_limit(1 << 16));
        }
    }

    #[test]
    fn every_truncation_of_a_valid_segment_errors_cleanly() {
        let syms = two_symbols();
        let refs: Vec<&Bitmap> = syms.iter().collect();
        let segment = text_segment(
            (20, 16),
            Shape::default(),
            3,
            2,
            0,
            &[
                Op::Strip(2),
                Op::First(1, 0),
                Op::Next(2, 1),
                Op::EndStrip,
                Op::Strip(5),
                Op::First(-1, 0),
                Op::EndStrip,
            ],
        );
        for cut in 0..segment.len() {
            let _ = decode_text_region(&segment[..cut], &refs, &mut Budget::with_limit(1 << 16));
        }
    }
}
