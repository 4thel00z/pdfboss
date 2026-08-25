//! Pattern dictionary and halftone region segments (T.88 6.6, 6.7, 7.4.4,
//! 7.4.5), with the gray-scale image decoding they rest on (Annex C).
//!
//! A halftone-coded bitmap is tone painted with ink: the encoder cuts the
//! image into a grid of cells, quantises each cell to one of a small set of
//! fixed-size patterns — typically dithered dots of increasing darkness — and
//! codes only the per-cell pattern index. The patterns come from a *pattern
//! dictionary* segment, which stores them concatenated left to right in one
//! collective bitmap, coded as a single generic region (6.7.5); the region
//! segment then codes the index array as a *gray-scale image* (Annex C),
//! GSBPP bitplanes each coded as a generic region and Gray-coded so that a
//! plane's true value is its coded value XORed with the next more significant
//! plane.
//!
//! The grid is not a plain raster. Its origin (HGX, HGY) and vector
//! (HRX, HRY) are fixed-point values in 1/256ths of a pixel, and cell
//! (ng, mg) lands at the shifted-down-by-8 pair (HGX + mg·HRY + ng·HRX,
//! HGY + mg·HRX − ng·HRY) — a grid that may be tilted, and may hang off the
//! region on any side. Cells that fall wholly outside can be flagged in a
//! skip mask (6.6.5.1) so their gray-scale pixels are never coded at all.
//!
//! Cost follows the module's one rule: every count a loop runs over is
//! charged against the stream's [`Budget`] before the loop is entered. The
//! collective bitmap and each bitplane are charged by the generic region
//! decoder from their declared dimensions; the patterns are charged per head
//! at [`PATTERN_COST`] because they are kept for the rest of the segment walk;
//! and the rendering pass is charged cells × pattern area, because neither
//! factor is bounded by the region it draws into.

use super::bitmap::{Bitmap, CombOp};
use super::budget::Budget;
use super::generic::{
    decode_generic_region, decode_mmr_region, decode_mmr_region_consumed, GenericParams,
    GB_CONTEXT_LEN,
};
use super::mq::{MqContexts, MqDecoder};
use super::reader::Reader;
use super::segment::{parse_region_info, RegionInfo};
use super::Jbig2Error;

/// What one pattern costs beyond the pixels it is made of, in the units
/// [`Budget`] counts.
///
/// The same price, for the same reason, as a symbol's
/// [`SYMBOL_COST`](super::symbol_dict::SYMBOL_COST): a decoded pattern is
/// *kept* — the page walk holds every dictionary's patterns until the last
/// segment is read — and the count of them comes from a four-byte GRAYMAX
/// field nothing else bounds. Charging per head before anything is decoded is
/// what stops seven bytes of segment data from asking for four billion
/// bitmaps.
pub(crate) const PATTERN_COST: u64 = 512;

/// Decodes a pattern dictionary segment (T.88 7.4.4, 6.7) into the patterns
/// it exports, HDPATS[0] through HDPATS[GRAYMAX].
///
/// The collective bitmap is one generic region, (GRAYMAX + 1) × HDPW wide and
/// HDPH tall, holding every pattern concatenated left to right (6.7.5 step 1);
/// with HDMMR set it is one facsimile stream instead, whose byte count is the
/// rest of the segment. Either way it is then cut back into patterns on
/// HDPW-column boundaries (step 4).
///
/// The arithmetic variant's parameters are fixed by Table 27 rather than
/// carried in the segment: TPGDON 0, no skip, and AT1 at (−HDPW, 0) — the
/// corresponding pixel of the pattern one cell to the left, which is the
/// neighbour most like this one in a dictionary of graduated tones. AT2 to
/// AT4 sit at their template 0 nominal positions.
pub(crate) fn decode_pattern_dict(
    data: &[u8],
    budget: &mut Budget,
) -> Result<Vec<Bitmap>, Jbig2Error> {
    let mut r = Reader::new(data);
    // 7.4.4.1.1: HDMMR in bit 0, HDTEMPLATE in bits 1 to 2, the rest reserved.
    let flags = r.u8()?;
    if flags & 0xF8 != 0 {
        return Err(Jbig2Error::Malformed(
            "reserved pattern dictionary flag bits",
        ));
    }
    let mmr = flags & 1 != 0;
    let template = (flags >> 1) & 3;
    if mmr && template != 0 {
        return Err(Jbig2Error::Malformed(
            "MMR pattern dictionary with a template",
        ));
    }
    // 7.4.4.1.2 and 7.4.4.1.3: one byte each, and zero is forbidden for both.
    let hdpw = r.u8()?;
    let hdph = r.u8()?;
    if hdpw == 0 {
        return Err(Jbig2Error::Malformed("pattern width of zero"));
    }
    if hdph == 0 {
        return Err(Jbig2Error::Malformed("pattern height of zero"));
    }
    let graymax = r.u32()?;

    // The patterns are kept for the rest of the segment walk, so they are
    // charged per head before anything is decoded — see PATTERN_COST. Once
    // this charge has succeeded the count is at most MAX_WORK / PATTERN_COST,
    // so the collective width below fits a u32 with room to spare; a width
    // that does not is one this refusal would have caught.
    let count = u64::from(graymax) + 1;
    budget.charge(count.saturating_mul(PATTERN_COST))?;
    let width = u32::try_from(count * u64::from(hdpw)).map_err(|_| Jbig2Error::WorkLimit)?;

    let collective = if mmr {
        decode_mmr_region(r.rest(), budget, width, u32::from(hdph))?
    } else {
        // Table 27, hand-checked against the page render: GBTEMPLATE is
        // HDTEMPLATE, TPGDON 0, USESKIP 0, A1 = (−HDPW, 0), A2 = (−3, −1),
        // A3 = (2, −2), A4 = (−2, −2).
        let params = GenericParams {
            template,
            at: [(-i16::from(hdpw), 0), (-3, -1), (2, -2), (-2, -2)],
            tpgdon: false,
        };
        let mut dec = MqDecoder::new(r.rest());
        let mut cx = MqContexts::new(GB_CONTEXT_LEN);
        decode_generic_region(
            &mut dec,
            &mut cx,
            budget,
            width,
            u32::from(hdph),
            &params,
            None,
        )?
    };

    // 6.7.5 step 4: pattern GRAY is columns HDPW × GRAY through
    // HDPW × (GRAY + 1) − 1 of the collective bitmap.
    let hdpw = u32::from(hdpw);
    (0..=graymax)
        .map(|gray| collective.columns(gray * hdpw, hdpw))
        .collect()
}

/// The halftone grid of T.88 6.6.5: origin and vector, in 1/256ths of a
/// pixel. The origin is signed; the vector is not, which restricts it to one
/// quadrant — any other orientation is expressed by moving the origin.
struct Grid {
    x: i32,
    y: i32,
    rx: u16,
    ry: u16,
}

impl Grid {
    /// Where grid cell (ng, mg)'s pattern goes (6.6.5.1 step 1 a) i),
    /// 6.6.5.2 step 1 a) i)):
    ///
    /// x = (HGX + mg × HRY + ng × HRX) >> 8
    /// y = (HGY + mg × HRX − ng × HRY) >> 8
    ///
    /// The products run to 2^48, so the sum is formed in i64 where nothing
    /// wraps, and the shift is arithmetic — a negative origin keeps flooring
    /// toward the top left, exactly as the spec's signed shift does.
    fn cell(&self, ng: u32, mg: u32) -> (i64, i64) {
        let ng = i64::from(ng);
        let mg = i64::from(mg);
        let x = (i64::from(self.x) + mg * i64::from(self.ry) + ng * i64::from(self.rx)) >> 8;
        let y = (i64::from(self.y) + mg * i64::from(self.rx) - ng * i64::from(self.ry)) >> 8;
        (x, y)
    }
}

/// Computes HSKIP (T.88 6.6.5.1): a cell is skipped when the pattern drawn
/// there could not touch the region — off it entirely on any side. `region`
/// is (HBW, HBH) and `pattern` is (HPW, HPH), width first in both.
fn skip_mask(
    grid: &Grid,
    gw: u32,
    gh: u32,
    region: (u32, u32),
    pattern: (u32, u32),
    budget: &mut Budget,
) -> Result<Bitmap, Jbig2Error> {
    let (region_width, region_height) = region;
    let (hpw, hph) = pattern;
    budget.charge_region(gw, gh)?;
    let mut skip = Bitmap::new(gw, gh)?;
    for mg in 0..gh {
        for ng in 0..gw {
            let (x, y) = grid.cell(ng, mg);
            // 6.6.5.1 step 1 a) ii): (x + HPW ≤ 0) OR (x ≥ HBW) OR
            // (y + HPH ≤ 0) OR (y ≥ HBH).
            let outside = x + i64::from(hpw) <= 0
                || x >= i64::from(region_width)
                || y + i64::from(hph) <= 0
                || y >= i64::from(region_height);
            skip.set(ng, mg, u8::from(outside));
        }
    }
    Ok(skip)
}

/// The parameters of a gray-scale image decoding procedure
/// (T.88 Annex C, Table C.1).
struct GrayParams<'a> {
    /// GSMMR: whether the bitplanes are facsimile streams.
    mmr: bool,
    /// GSTEMPLATE, meaningful only when `mmr` is false.
    template: u8,
    /// GSW and GSH: the dimensions of every bitplane.
    width: u32,
    height: u32,
    /// GSBPP: how many bitplanes there are, at least one.
    bpp: u32,
    /// GSKIP: pixels no bit is coded for, stored as 0 (GSUSESKIP folded in —
    /// absent means no skipping).
    skip: Option<&'a Bitmap>,
}

/// Decodes the gray-scale image of T.88 Annex C, returning the *true* —
/// un-Gray-coded — bitplanes, most significant first.
///
/// C.5 decodes plane GSBPP − 1 first and then works down, XORing each coded
/// plane with the true plane above it; both variants share state across
/// planes exactly as a symbol dictionary shares it across symbols. In the
/// arithmetic variant that state is the decoder and the GB context array,
/// reset per segment (7.4.5.2 step 3), not per plane. In the MMR variant it
/// is the position in the byte stream: the planes sit end to end, each closed
/// by an end-of-facsimile block because nothing else says where it stops
/// (6.2.6), each starting on the next byte boundary.
///
/// Every plane's parameters are Table C.4's, which — hand-checked against the
/// page render — are exactly the template's nominal AT offsets with TPGDON
/// off: A1 = (3, −1) for templates 0 and 1 and (2, −1) for 2 and 3, A2 to A4
/// at (−3, −1), (2, −2), (−2, −2).
fn decode_gray_planes(
    data: &[u8],
    params: &GrayParams<'_>,
    budget: &mut Budget,
) -> Result<Vec<Bitmap>, Jbig2Error> {
    let mut planes: Vec<Bitmap> = Vec::with_capacity(params.bpp as usize);
    if params.mmr {
        let mut offset = 0usize;
        for _ in 0..params.bpp {
            let rest = data.get(offset..).ok_or(Jbig2Error::Truncated)?;
            let (mut plane, consumed) =
                decode_mmr_region_consumed(rest, budget, params.width, params.height)?;
            offset = offset.saturating_add(consumed);
            if let Some(above) = planes.last() {
                plane.combine(above, 0, 0, CombOp::Xor);
            }
            planes.push(plane);
        }
        return Ok(planes);
    }

    let generic = GenericParams::nominal(params.template);
    let mut dec = MqDecoder::new(data);
    let mut cx = MqContexts::new(GB_CONTEXT_LEN);
    for _ in 0..params.bpp {
        let mut plane = decode_generic_region(
            &mut dec,
            &mut cx,
            budget,
            params.width,
            params.height,
            &generic,
            params.skip,
        )?;
        // C.5 step 3 b): the coded plane XORed with the true plane above it
        // is this plane's true value. The most significant plane has nothing
        // above it and is its own truth (step 1).
        if let Some(above) = planes.last() {
            plane.combine(above, 0, 0, CombOp::Xor);
        }
        planes.push(plane);
    }
    Ok(planes)
}

/// GSVALS[x, y] (T.88 C.5 step 4): the planes' bits assembled most
/// significant first, as [`decode_gray_planes`] returns them.
fn gray_value(planes: &[Bitmap], x: u32, y: u32) -> u32 {
    planes.iter().fold(0u32, |value, plane| {
        (value << 1) | u32::from(plane.get(i64::from(x), i64::from(y)))
    })
}

/// A grid coordinate as a composition offset. The grid arithmetic runs to
/// 2^40; clamping parks anything past the offset range far off the region,
/// where composition clips it away, rather than wrapping it back on.
fn grid_offset(v: i64) -> i32 {
    v.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Decodes a halftone region segment (T.88 7.4.5, 6.6) into its own bitmap,
/// returning it alongside the region information field that says where it
/// goes.
///
/// `patterns` is the referred-to pattern dictionary's export, HPATS. The
/// caller resolves the reference because the store of decoded dictionaries
/// lives with the segment walk; everything else — grid, gray-scale image,
/// rendering — is this function's.
///
/// Two operators are in play and they are not the same one: HCOMBOP, from
/// this segment's flags, combines each pattern into the region over the
/// HDEFPIXEL background; the region information field's own operator then
/// composites the finished region onto the page, exactly as for any other
/// region type.
///
/// A gray value with no pattern — possible whenever the pattern count is not
/// a power of two, since the planes can code any HBPP-bit value — selects the
/// last pattern instead. 6.6.5.2 leaves the draw undefined rather than
/// forbidding the value, and the last pattern is the nearest tone to every
/// value past it.
pub(crate) fn decode_halftone_region(
    data: &[u8],
    patterns: &[Bitmap],
    budget: &mut Budget,
) -> Result<(RegionInfo, Bitmap), Jbig2Error> {
    if patterns.is_empty() {
        return Err(Jbig2Error::Malformed("halftone region with no patterns"));
    }
    let mut r = Reader::new(data);
    let info = parse_region_info(&mut r)?;

    // 7.4.5.1.1, laid out in Figure 42: HMMR in bit 0, HTEMPLATE in bits 1 to
    // 2, HENABLESKIP in bit 3, HCOMBOP in bits 4 to 6, HDEFPIXEL in bit 7.
    let flags = r.u8()?;
    let mmr = flags & 1 != 0;
    let template = (flags >> 1) & 3;
    let enable_skip = flags & 8 != 0;
    let op = CombOp::from_bits((flags >> 4) & 7)?;
    let default_pixel = flags >> 7;
    if mmr && template != 0 {
        return Err(Jbig2Error::Malformed("MMR halftone region with a template"));
    }
    if mmr && enable_skip {
        return Err(Jbig2Error::Malformed(
            "MMR halftone region with skip enabled",
        ));
    }

    // 7.4.5.1.2 and 7.4.5.1.3: the grid's dimensions, signed origin and
    // unsigned vector.
    let gw = r.u32()?;
    let gh = r.u32()?;
    let grid = Grid {
        x: r.u32()? as i32,
        y: r.u32()? as i32,
        rx: r.u16()?,
        ry: r.u16()?,
    };

    let hpw = patterns[0].width();
    let hph = patterns[0].height();

    // 6.6.5 step 1: the region starts as HDEFPIXEL everywhere, charged like
    // any other region before it is allocated.
    budget.charge_region(info.width, info.height)?;
    let mut region = Bitmap::filled(info.width, info.height, default_pixel)?;

    // 6.6.5 step 2.
    let skip = if enable_skip {
        Some(skip_mask(
            &grid,
            gw,
            gh,
            (info.width, info.height),
            (hpw, hph),
            budget,
        )?)
    } else {
        None
    };

    // 6.6.5 step 3: HBPP = ⌈log2(HNUMPATS)⌉ — the ceiling, hand-checked
    // against the page render, so three patterns take two planes. One pattern
    // takes none: every cell is index 0 and no plane is coded.
    let bpp = (patterns.len() as u32).next_power_of_two().trailing_zeros();

    // 6.6.5 step 4: the gray-scale image, each plane charged by the generic
    // region decoder from the grid dimensions. A grid with no cells codes
    // nothing and renders nothing, so it is not sent through a decode whose
    // MMR arm would refuse a zero dimension.
    let planes = if bpp == 0 || gw == 0 || gh == 0 {
        Vec::new()
    } else {
        let params = GrayParams {
            mmr,
            template,
            width: gw,
            height: gh,
            bpp,
            skip: skip.as_ref(),
        };
        decode_gray_planes(r.rest(), &params, budget)?
    };

    // 6.6.5 step 5, charged before the loop: the rendering pass touches every
    // pattern pixel of every cell, and neither count is bounded by the region
    // it draws into — a small region under an enormous tilted grid is still
    // an enormous number of clipped draws. The pattern area is at least one
    // pixel, so the per-cell overhead rides on the same charge.
    let cells = u64::from(gw).saturating_mul(u64::from(gh));
    budget.charge(cells.saturating_mul(u64::from(hpw).saturating_mul(u64::from(hph))))?;
    let last = patterns.len() - 1;
    for mg in 0..gh {
        for ng in 0..gw {
            let (x, y) = grid.cell(ng, mg);
            let value = (gray_value(&planes, ng, mg) as usize).min(last);
            region.combine(&patterns[value], grid_offset(x), grid_offset(y), op);
        }
    }
    Ok((info, region))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::ccitt::testing::encode_g4_with_eofb;
    use crate::filters::jbig2::testing::{
        encode_generic_sequence, expect_at, glyph, halftone_segment, pattern_dict_segment,
    };

    /// The grid formulas of 6.6.5.1/6.6.5.2, against cells computed by hand
    /// from the page render of the spec. With HRX = 1024 and HRY = 256 —
    /// four pixels along the primary direction, one across it — and the
    /// origin at (0, 512):
    ///
    ///   (ng, mg) = (0, 0): x = 0 >> 8 = 0,             y = 512 >> 8 = 2
    ///   (1, 0):  x = 1024 >> 8 = 4,                    y = (512 − 256) >> 8 = 1
    ///   (0, 1):  x = 256 >> 8 = 1,                     y = (512 + 1024) >> 8 = 6
    ///   (2, 1):  x = (256 + 2048) >> 8 = 9,            y = (512 + 1024 − 512) >> 8 = 4
    ///
    /// The second cell sits *above* the first — the minus term is what tilts
    /// the grid — so a sign error here cannot pass.
    #[test]
    fn the_grid_formulas_match_the_hand_computed_cells() {
        let grid = Grid {
            x: 0,
            y: 512,
            rx: 1024,
            ry: 256,
        };
        assert_eq!(grid.cell(0, 0), (0, 2));
        assert_eq!(grid.cell(1, 0), (4, 1));
        assert_eq!(grid.cell(0, 1), (1, 6));
        assert_eq!(grid.cell(2, 1), (9, 4));
    }

    /// The shift is arithmetic: a negative origin floors toward the top left.
    /// −1023 is −4 × 256 + 1, so −1023 >> 8 must be −4, not −3.
    #[test]
    fn the_grid_shift_floors_negative_coordinates() {
        let grid = Grid {
            x: -1023,
            y: -1024,
            rx: 0,
            ry: 0,
        };
        assert_eq!(grid.cell(0, 0), (-4, -4));
    }

    /// HSKIP by hand (6.6.5.1): an axis-aligned grid of 4-pixel steps over an
    /// 8 x 8 region, three cells across and two down, starting one full
    /// pattern off the left edge. Cell ng = 0 sits at x = −4, and
    /// x + HPW = 0 ≤ 0 is exactly the boundary the ≤ admits; ng = 2 sits at
    /// x = 4, inside; nothing about mg moves x, and y = 0 and 4 are both
    /// inside. So only the ng = 0 column is skipped.
    #[test]
    fn the_skip_mask_matches_the_hand_computed_cells() {
        let grid = Grid {
            x: -1024,
            y: 0,
            rx: 1024,
            ry: 0,
        };
        let skip = skip_mask(&grid, 3, 2, (8, 8), (4, 4), &mut Budget::new()).expect("mask");
        for mg in 0..2u32 {
            assert_eq!(skip.get(0, i64::from(mg)), 1, "ng 0, mg {mg}");
            assert_eq!(skip.get(1, i64::from(mg)), 0, "ng 1, mg {mg}");
            assert_eq!(skip.get(2, i64::from(mg)), 0, "ng 2, mg {mg}");
        }

        // One step to the right, x = −768 puts x + HPW at 1 > 0: a pattern
        // one pixel on the region is not skippable.
        let grid = Grid {
            x: -768,
            y: 0,
            rx: 1024,
            ry: 0,
        };
        let skip = skip_mask(&grid, 3, 1, (8, 8), (4, 4), &mut Budget::new()).expect("mask");
        assert_eq!(skip.get(0, 0), 0);

        // And past the right edge: x = 8 trips x ≥ HBW exactly.
        let grid = Grid {
            x: 0,
            y: 0,
            rx: 1024,
            ry: 0,
        };
        let skip = skip_mask(&grid, 3, 1, (8, 8), (4, 4), &mut Budget::new()).expect("mask");
        assert_eq!(skip.get(0, 0), 0);
        assert_eq!(skip.get(1, 0), 0);
        assert_eq!(skip.get(2, 0), 1, "x = 8 on an 8-wide region");
    }

    /// Annex C by hand, two planes. The wanted values are
    ///
    ///   GI = | 0 1 |      true plane 1 (bit 1): | 0 0 |   plane 0: | 0 1 |
    ///        | 2 3 |                            | 1 1 |            | 0 1 |
    ///
    /// and the *coded* planes are Gray-coded (C.5: coded MSB is the true MSB,
    /// coded J is true J XOR true J + 1):
    ///
    ///   coded plane 1 = | 0 0 |     coded plane 0 = | 0^0 1^0 | = | 0 1 |
    ///                   | 1 1 |                     | 0^1 1^1 |   | 1 0 |
    ///
    /// Decoding must undo that: plane 1 as coded, plane 0 = coded 0 XOR
    /// plane 1, and the values reassemble as plane0 + 2 × plane1.
    #[test]
    fn two_gray_planes_undo_the_gray_coding() {
        let coded_msb = glyph(&["00", "11"]);
        let coded_lsb = glyph(&["01", "10"]);
        let data =
            encode_generic_sequence(&[&coded_msb, &coded_lsb], &GenericParams::nominal(0), None);
        let params = GrayParams {
            mmr: false,
            template: 0,
            width: 2,
            height: 2,
            bpp: 2,
            skip: None,
        };
        let planes = decode_gray_planes(&data, &params, &mut Budget::new()).expect("planes");
        assert_eq!(planes.len(), 2);
        assert_eq!(planes[0], glyph(&["00", "11"]), "true plane 1");
        assert_eq!(planes[1], glyph(&["01", "01"]), "true plane 0");
        assert_eq!(gray_value(&planes, 0, 0), 0);
        assert_eq!(gray_value(&planes, 1, 0), 1);
        assert_eq!(gray_value(&planes, 0, 1), 2);
        assert_eq!(gray_value(&planes, 1, 1), 3);
    }

    /// Three planes, same discipline. Wanted values
    ///
    ///   GI = | 5 2 |    true planes, bit 2 to bit 0:
    ///        | 7 0 |      | 1 0 |   | 0 1 |   | 1 0 |
    ///                     | 1 0 |   | 1 0 |   | 1 0 |
    ///
    /// Gray-coding by hand: coded 2 = true 2; coded 1 = true 1 XOR true 2 =
    /// (1 1 / 0 0); coded 0 = true 0 XOR true 1 = (1 1 / 0 0).
    #[test]
    fn three_gray_planes_undo_the_gray_coding() {
        let coded = [
            glyph(&["10", "10"]),
            glyph(&["11", "00"]),
            glyph(&["11", "00"]),
        ];
        let data = encode_generic_sequence(
            &[&coded[0], &coded[1], &coded[2]],
            &GenericParams::nominal(0),
            None,
        );
        let params = GrayParams {
            mmr: false,
            template: 0,
            width: 2,
            height: 2,
            bpp: 3,
            skip: None,
        };
        let planes = decode_gray_planes(&data, &params, &mut Budget::new()).expect("planes");
        assert_eq!(gray_value(&planes, 0, 0), 5);
        assert_eq!(gray_value(&planes, 1, 0), 2);
        assert_eq!(gray_value(&planes, 0, 1), 7);
        assert_eq!(gray_value(&planes, 1, 1), 0);
    }

    /// The MMR variant of the same two-plane image. Each coded plane is its
    /// own facsimile stream closed by an end-of-facsimile block, and the next
    /// plane starts on the following byte boundary (6.2.6) — so a decoder
    /// that misjudges where plane 1 ends decodes garbage for plane 0.
    #[test]
    fn mmr_gray_planes_are_delimited_by_the_terminator() {
        let mut data = encode_g4_with_eofb(&glyph(&["00", "11"]));
        data.extend_from_slice(&encode_g4_with_eofb(&glyph(&["01", "10"])));
        let params = GrayParams {
            mmr: true,
            template: 0,
            width: 2,
            height: 2,
            bpp: 2,
            skip: None,
        };
        let planes = decode_gray_planes(&data, &params, &mut Budget::new()).expect("planes");
        assert_eq!(gray_value(&planes, 0, 0), 0);
        assert_eq!(gray_value(&planes, 1, 0), 1);
        assert_eq!(gray_value(&planes, 0, 1), 2);
        assert_eq!(gray_value(&planes, 1, 1), 3);
    }

    /// Three distinct patterns whose columns differ, so a cut one column off
    /// reassembles none of them. They are busy on purpose: the Table 27
    /// contexts read the same position in the *previous* pattern through A1,
    /// and only varied ink drives the arithmetic coder's adaptation far
    /// enough that a decoder forming any other context visibly diverges.
    fn three_patterns() -> Vec<Bitmap> {
        vec![
            glyph(&[
                "10010110", "01101001", "11000011", "00111100", "10100101", "01011010",
            ]),
            glyph(&[
                "01100110", "10011001", "01011010", "10100101", "11001100", "00110011",
            ]),
            glyph(&[
                "11110000", "00001111", "10101010", "01010101", "11011011", "00100100",
            ]),
        ]
    }

    #[test]
    fn a_pattern_dictionary_cuts_its_collective_bitmap() {
        let patterns = three_patterns();
        let dict = pattern_dict_segment(&patterns, false);
        let decoded = decode_pattern_dict(&dict, &mut Budget::new()).expect("dictionary");
        assert_eq!(decoded, patterns);
    }

    #[test]
    fn an_mmr_pattern_dictionary_decodes() {
        let patterns = three_patterns();
        let dict = pattern_dict_segment(&patterns, true);
        let decoded = decode_pattern_dict(&dict, &mut Budget::new()).expect("dictionary");
        assert_eq!(decoded, patterns);
    }

    /// The header fields the standard forbids, each named (7.4.4.1).
    #[test]
    fn malformed_pattern_dictionary_headers_are_refused() {
        // flags, HDPW, HDPH, GRAYMAX.
        let build = |flags: u8, hdpw: u8, hdph: u8| {
            let mut data = vec![flags, hdpw, hdph];
            data.extend_from_slice(&0u32.to_be_bytes());
            data
        };
        for (data, want) in [
            (build(0x08, 4, 4), "reserved pattern dictionary flag bits"),
            (build(0x80, 4, 4), "reserved pattern dictionary flag bits"),
            (build(0x03, 4, 4), "MMR pattern dictionary with a template"),
            (build(0x00, 0, 4), "pattern width of zero"),
            (build(0x00, 4, 0), "pattern height of zero"),
        ] {
            assert_eq!(
                decode_pattern_dict(&data, &mut Budget::new()),
                Err(Jbig2Error::Malformed(want)),
            );
        }
    }

    /// GRAYMAX is four bytes nothing bounds, and the patterns it promises are
    /// kept for the whole walk: seven bytes of header must not buy four
    /// billion bitmaps. The refusal comes from the per-head charge, before
    /// anything is decoded.
    #[test]
    fn a_pattern_dictionary_of_enormous_graymax_is_refused() {
        let mut data = vec![0u8, 1, 1];
        data.extend_from_slice(&u32::MAX.to_be_bytes());
        assert!(data.len() < 16, "the demand is {} bytes", data.len());
        assert_eq!(
            decode_pattern_dict(&data, &mut Budget::new()),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// The flag combinations 7.4.5.1.1 forbids, each named.
    #[test]
    fn malformed_halftone_flags_are_refused() {
        let patterns = vec![glyph(&["1"])];
        for (flags, want) in [
            (0x03u8, "MMR halftone region with a template"),
            (0x09, "MMR halftone region with skip enabled"),
            (0x50, "reserved combination operator"),
        ] {
            let data = halftone_segment((4, 4), (0, 0), 0, flags, (1, 1, 0, 0, 256, 0), &[]);
            assert_eq!(
                decode_halftone_region(&data, &patterns, &mut Budget::new()),
                Err(Jbig2Error::Malformed(want)),
                "flags {flags:#04x}",
            );
        }
    }

    /// A tilted grid, placed by the hand-computed cells of
    /// `the_grid_formulas_match_the_hand_computed_cells`: every cell holds
    /// pattern 1, a solid 2 x 2 block, so the region must show exactly four
    /// blocks at (0, 2), (4, 1), (1, 6) and (5, 5).
    #[test]
    fn a_tilted_grid_places_patterns_where_the_formulas_say() {
        let patterns = vec![glyph(&["00", "00"]), glyph(&["11", "11"])];
        // Two patterns take one plane, and every value is 1.
        let plane = glyph(&["11", "11"]);
        let coded = encode_generic_sequence(&[&plane], &GenericParams::nominal(0), None);
        let data = halftone_segment(
            (8, 8),
            (0, 0),
            0, // composited onto the page with OR
            0, // arithmetic, template 0, no skip, HCOMBOP OR, default pixel 0
            (2, 2, 0, 512, 1024, 256),
            &coded,
        );
        let (info, region) =
            decode_halftone_region(&data, &patterns, &mut Budget::new()).expect("region");
        assert_eq!((info.width, info.height), (8, 8));
        expect_at(&region, &patterns[1], 0, 2);
        expect_at(&region, &patterns[1], 4, 1);
        expect_at(&region, &patterns[1], 1, 6);
        expect_at(&region, &patterns[1], 5, 5);
        // A pixel no cell reaches keeps the default value.
        assert_eq!(region.get(7, 0), 0);
    }

    /// A skipped cell's gray-scale pixel is never coded (6.6.5.1, C.5), so
    /// the coded plane holds bits only for the cells on the region. The
    /// skipped cell comes *first* here — its column starts a full pattern off
    /// the left edge — so a decoder that ignores the mask consumes a decision
    /// that belonged to the next cell and misplaces everything after it.
    #[test]
    fn a_skipped_cell_consumes_no_coded_bits() {
        let checker = glyph(&["1010", "0101", "1010", "0101"]);
        let solid = glyph(&["1111", "1111", "1111", "1111"]);
        let patterns = vec![checker.clone(), solid.clone()];

        // Grid cells at x = −4, 0, 4: HSKIP = 1 0 0 as the hand-computed mask
        // test pins. Values: cell 1 is 1 (solid), cell 2 is 0 (checker); the
        // skipped cell's pixel is stored as 0 and never coded.
        let plane = glyph(&["010"]);
        let skip = glyph(&["100"]);
        let coded = encode_generic_sequence(&[&plane], &GenericParams::nominal(0), Some(&skip));
        let data = halftone_segment(
            (8, 4),
            (0, 0),
            0,
            0x08, // HENABLESKIP
            (3, 1, -1024, 0, 1024, 0),
            &coded,
        );
        let (_, region) =
            decode_halftone_region(&data, &patterns, &mut Budget::new()).expect("region");
        expect_at(&region, &solid, 0, 0);
        expect_at(&region, &checker, 4, 0);
    }

    /// A gray value the dictionary has no pattern for — codable whenever the
    /// pattern count is not a power of two — selects the last pattern rather
    /// than reading out of the dictionary. Three patterns, one cell, coded
    /// value 3: true planes are both 1, so the coded MSB is 1 and the coded
    /// LSB is 1 XOR 1 = 0.
    #[test]
    fn a_gray_value_past_the_dictionary_selects_its_last_pattern() {
        let patterns = three_patterns();
        let msb = glyph(&["1"]);
        let lsb = glyph(&["0"]);
        let coded = encode_generic_sequence(&[&msb, &lsb], &GenericParams::nominal(0), None);
        let data = halftone_segment((8, 6), (0, 0), 0, 0, (1, 1, 0, 0, 2048, 0), &coded);
        let (_, region) =
            decode_halftone_region(&data, &patterns, &mut Budget::new()).expect("region");
        expect_at(&region, &patterns[2], 0, 0);
    }

    /// One pattern is index 0 everywhere: ⌈log2(1)⌉ is no bitplanes at all,
    /// and the segment carries no coded data.
    #[test]
    fn a_single_pattern_dictionary_needs_no_gray_planes() {
        let solid = glyph(&["11", "11"]);
        let patterns = vec![solid.clone()];
        let data = halftone_segment((4, 2), (0, 0), 0, 0, (2, 1, 0, 0, 512, 0), &[]);
        let (_, region) =
            decode_halftone_region(&data, &patterns, &mut Budget::new()).expect("region");
        expect_at(&region, &solid, 0, 0);
        expect_at(&region, &solid, 2, 0);
    }

    /// HDEFPIXEL fills the region before any pattern lands, and HCOMBOP is
    /// the operator the patterns land with: XNOR against a background of ones
    /// writes each pattern verbatim, where OR would have left solid ink.
    #[test]
    fn the_default_pixel_and_comb_op_shape_the_region() {
        let dotted = glyph(&["01", "10"]);
        let patterns = vec![glyph(&["00", "00"]), dotted.clone()];
        let plane = glyph(&["1"]);
        let coded = encode_generic_sequence(&[&plane], &GenericParams::nominal(0), None);
        // HDEFPIXEL 1, HCOMBOP XNOR (3 << 4 | 1 << 7): one cell covering half
        // the region; the other half keeps the default 1.
        let data = halftone_segment((4, 2), (0, 0), 0, 0xB0, (1, 1, 0, 0, 512, 0), &coded);
        let (_, region) =
            decode_halftone_region(&data, &patterns, &mut Budget::new()).expect("region");
        expect_at(&region, &dotted, 0, 0);
        assert_eq!(region.get(2, 0), 1, "the uncovered half keeps the default");
        assert_eq!(region.get(3, 1), 1);
    }

    /// A grid of four billion cells costs more than any stream may spend,
    /// whether or not any plane is coded: with two patterns the refusal is
    /// the first plane's charge, with one pattern — no planes at all — it is
    /// the rendering charge. Either way the segment is a few dozen bytes and
    /// nothing is decoded.
    #[test]
    fn an_enormous_grid_is_refused() {
        for patterns in [vec![glyph(&["1"])], vec![glyph(&["1"]), glyph(&["0"])]] {
            let data = halftone_segment(
                (4, 4),
                (0, 0),
                0,
                0,
                (u32::MAX, u32::MAX, 0, 0, 256, 0),
                &[],
            );
            assert!(data.len() < 64, "the demand is {} bytes", data.len());
            assert_eq!(
                decode_halftone_region(&data, &patterns, &mut Budget::new()),
                Err(Jbig2Error::WorkLimit),
                "{} patterns",
                patterns.len(),
            );
        }
    }

    /// The MMR variant end to end: the four-value hand example rendered onto
    /// an axis-aligned grid, each plane its own terminated facsimile stream.
    #[test]
    fn an_mmr_halftone_region_decodes() {
        let patterns = vec![
            glyph(&["0000", "0000", "0000", "0000"]),
            glyph(&["0110", "0110", "0000", "0000"]),
            glyph(&["0000", "0000", "0110", "0110"]),
            glyph(&["1111", "1111", "1111", "1111"]),
        ];
        // GI = (0 1 / 2 3), Gray-coded by hand exactly as in
        // `two_gray_planes_undo_the_gray_coding`.
        let mut coded = encode_g4_with_eofb(&glyph(&["00", "11"]));
        coded.extend_from_slice(&encode_g4_with_eofb(&glyph(&["01", "10"])));
        let data = halftone_segment(
            (8, 8),
            (0, 0),
            0,
            0x01, // HMMR
            (2, 2, 0, 0, 1024, 0),
            &coded,
        );
        let (_, region) =
            decode_halftone_region(&data, &patterns, &mut Budget::new()).expect("region");
        expect_at(&region, &patterns[0], 0, 0);
        expect_at(&region, &patterns[1], 4, 0);
        expect_at(&region, &patterns[2], 0, 4);
        expect_at(&region, &patterns[3], 4, 4);
    }
}
