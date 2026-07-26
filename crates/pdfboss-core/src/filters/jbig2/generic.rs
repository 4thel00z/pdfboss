//! Generic region decoding (ITU-T T.88 6.2).
//!
//! A generic region is a rectangle of pixels decoded one at a time, each
//! against an adaptive arithmetic context formed from the pixels already
//! decoded around it. Four templates (6.2.5.3, figures 4 to 7) select which
//! neighbours take part; each template reserves one to four *adaptive* slots
//! whose offsets the segment header carries, so an encoder can point them at
//! whatever correlates best with the image.
//!
//! Everything here is the general path: it reads each template pixel through
//! the bounds-checked accessor, so any AT offset a stream declares is honoured
//! and none of them can read outside the bitmap.

use super::bitmap::Bitmap;
use super::mq::{MqContexts, MqDecoder};
use super::reader::Reader;
use super::Jbig2Error;

/// Number of arithmetic contexts a generic region addresses.
///
/// The widest template (0) forms a 16-bit context, so the array has to cover
/// every 16-bit value. The narrower templates simply leave the upper part of
/// it untouched, which costs 128 KiB and saves the caller from sizing the
/// array per template — and the symbol dictionary, which shares one array
/// across symbols coded with a single template, never needs to.
pub(crate) const GB_CONTEXT_LEN: usize = 1 << 16;

/// The nominal AT pixel offsets, as `(dx, dy)` per slot, indexed by template
/// (T.88 6.2.5.3).
///
/// Templates 1 to 3 define only A1; their remaining slots repeat template 0's
/// so that every [`GenericParams`] holds four well-defined offsets, and
/// [`context_at`] never reads a slot the template does not use anyway.
pub(crate) const NOMINAL_AT: [[(i8, i8); 4]; 4] = [
    [(3, -1), (-3, -1), (2, -2), (-2, -2)],
    [(3, -1), (-3, -1), (2, -2), (-2, -2)],
    [(2, -1), (-3, -1), (2, -2), (-2, -2)],
    [(2, -1), (-3, -1), (2, -2), (-2, -2)],
];

/// The sentinel contexts the typical-prediction decision is coded against,
/// indexed by template (T.88 6.2.5.7).
///
/// They are fixed values in the same bit numbering as the templates, not
/// derived from any pixel, and they are chosen to be contexts a real
/// neighbourhood is unlikely to produce often, so the TPGDON decisions adapt
/// largely independently of the pixel decisions.
pub(crate) const TPGD_CONTEXT: [u16; 4] = [0x9B25, 0x0795, 0x00E5, 0x0195];

/// The highest template number T.88 defines.
const MAX_TEMPLATE: u8 = 3;

/// The parameters of a generic region decoding procedure that come from the
/// segment header (T.88 6.2.5.1, 7.4.6.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct GenericParams {
    /// GBTEMPLATE, 0 to 3. Selects the pixel neighbourhood.
    pub(crate) template: u8,
    /// The AT pixel offsets A1 to A4, as `(dx, dy)` relative to the pixel
    /// being decoded. Templates 1 to 3 use only A1.
    pub(crate) at: [(i8, i8); 4],
    /// TPGDON: whether each row is preceded by a typical-prediction decision.
    pub(crate) tpgdon: bool,
}

impl GenericParams {
    /// The parameters an encoder gets by leaving the AT pixels where T.88
    /// 6.2.5.3 puts them, with typical prediction off.
    ///
    /// A template above 3 does not exist; it clamps rather than failing, so
    /// that this cannot become a panic on a path reached from stream data.
    pub(crate) fn nominal(template: u8) -> GenericParams {
        GenericParams {
            template,
            at: NOMINAL_AT[usize::from(template.min(MAX_TEMPLATE))],
            tpgdon: false,
        }
    }
}

/// Forms the arithmetic coding context for the pixel at `(x, y)`
/// (T.88 6.2.5.7, figures 4 through 7).
///
/// Bits run most-significant first, top template row to bottom, left to right
/// within each row, with the AT pixels in the slots the figures assign them:
/// slot 0 is A1, slot 3 is A4. Reads outside the bitmap yield 0, which is what
/// 6.2.5.2 requires of the region's surroundings.
///
/// An undefined template yields 0. That is unreachable through
/// [`parse_generic_flags`], which can only produce 0 to 3, but the pixel loop
/// runs on attacker-supplied data and a panic here would be worth more to an
/// attacker than a wrong context is.
pub(crate) fn context_at(bm: &Bitmap, x: u32, y: u32, params: &GenericParams) -> u16 {
    let x = i64::from(x);
    let y = i64::from(y);
    let at = |slot: usize| -> u16 {
        let (dx, dy) = params.at[slot];
        u16::from(bm.get(x + i64::from(dx), y + i64::from(dy)))
    };
    let px = |dx: i64, dy: i64| -> u16 { u16::from(bm.get(x + dx, y + dy)) };

    match params.template {
        0 => {
            (at(3) << 15)
                | (px(-1, -2) << 14)
                | (px(0, -2) << 13)
                | (px(1, -2) << 12)
                | (at(2) << 11)
                | (at(1) << 10)
                | (px(-2, -1) << 9)
                | (px(-1, -1) << 8)
                | (px(0, -1) << 7)
                | (px(1, -1) << 6)
                | (px(2, -1) << 5)
                | (at(0) << 4)
                | (px(-4, 0) << 3)
                | (px(-3, 0) << 2)
                | (px(-2, 0) << 1)
                | px(-1, 0)
        }
        1 => {
            (px(-1, -2) << 12)
                | (px(0, -2) << 11)
                | (px(1, -2) << 10)
                | (px(2, -2) << 9)
                | (px(-2, -1) << 8)
                | (px(-1, -1) << 7)
                | (px(0, -1) << 6)
                | (px(1, -1) << 5)
                | (px(2, -1) << 4)
                | (at(0) << 3)
                | (px(-3, 0) << 2)
                | (px(-2, 0) << 1)
                | px(-1, 0)
        }
        2 => {
            (px(-1, -2) << 9)
                | (px(0, -2) << 8)
                | (px(1, -2) << 7)
                | (px(-2, -1) << 6)
                | (px(-1, -1) << 5)
                | (px(0, -1) << 4)
                | (px(1, -1) << 3)
                | (at(0) << 2)
                | (px(-2, 0) << 1)
                | px(-1, 0)
        }
        3 => {
            (px(-3, -1) << 9)
                | (px(-2, -1) << 8)
                | (px(-1, -1) << 7)
                | (px(0, -1) << 6)
                | (px(1, -1) << 5)
                | (at(0) << 4)
                | (px(-4, 0) << 3)
                | (px(-3, 0) << 2)
                | (px(-2, 0) << 1)
                | px(-1, 0)
        }
        _ => 0,
    }
}

/// Decodes a generic region into a fresh bitmap (T.88 6.2.5.7).
///
/// `cx` is the shared GB context array, of [`GB_CONTEXT_LEN`] entries. Its
/// state persists across calls by design: a symbol dictionary decodes every
/// symbol in a height class through one array, so the caller owns it rather
/// than this function allocating a fresh one per region.
///
/// `skip`, when given, marks pixels the caller already knows are 0 — a
/// halftone region skips the grid cells that fall outside the page. Those
/// pixels are stored as 0 and consume no coded bits at all, which is what
/// keeps the encoder and decoder in step.
///
/// The loop is bounded by `width` and `height` alone, both of which come from
/// the segment header and are validated by the allocation. Nothing the coded
/// data can say changes how many pixels are decoded, so no input can make this
/// run long: it is exactly `width * height` decisions, and that product is
/// capped before the bitmap is allocated.
pub(crate) fn decode_generic_region(
    dec: &mut MqDecoder,
    cx: &mut MqContexts,
    width: u32,
    height: u32,
    params: &GenericParams,
    skip: Option<&Bitmap>,
) -> Result<Bitmap, Jbig2Error> {
    let mut bm = Bitmap::new(width, height)?;
    let mut ltp = 0u8;
    for y in 0..height {
        if params.tpgdon {
            // The typical-prediction decision toggles LTP; while LTP is 1 each
            // row is a copy of the one above and carries no coded pixels.
            let slot = usize::from(TPGD_CONTEXT[usize::from(params.template.min(MAX_TEMPLATE))]);
            ltp ^= dec.decode(cx.get_mut(slot));
            if ltp == 1 {
                bm.duplicate_row(y);
                continue;
            }
        }
        for x in 0..width {
            if skip.is_some_and(|s| s.get(i64::from(x), i64::from(y)) == 1) {
                bm.set(x, y, 0);
                continue;
            }
            let ctx = usize::from(context_at(&bm, x, y, params));
            let pixel = dec.decode(cx.get_mut(ctx));
            bm.set(x, y, pixel);
        }
    }
    Ok(bm)
}

/// Reads the generic region segment flags byte and the AT pixel offsets that
/// follow it (T.88 7.4.6.2), returning `(MMR, parameters)`.
///
/// Bit 0 is MMR, bits 1 to 2 are GBTEMPLATE, bit 3 is TPGDON, and bits 4 to 7
/// are reserved: a stream setting them is not one this decoder understands, so
/// it is refused rather than silently masked off.
///
/// An MMR-coded region carries no AT bytes, and neither do the slots a
/// template does not use — those keep their nominal offsets, so the returned
/// parameters always describe a complete neighbourhood whatever the header
/// said.
pub(crate) fn parse_generic_flags(r: &mut Reader<'_>) -> Result<(bool, GenericParams), Jbig2Error> {
    let flags = r.u8()?;
    if flags & 0xF0 != 0 {
        return Err(Jbig2Error::Malformed("reserved generic region flag bits"));
    }
    let mmr = flags & 0x01 != 0;
    let template = (flags >> 1) & 0x03;
    let tpgdon = flags & 0x08 != 0;

    let mut params = GenericParams::nominal(template);
    params.tpgdon = tpgdon;

    // 7.4.6.2: eight AT bytes for template 0, two for the rest, none at all
    // when the region is MMR-coded.
    let at_pairs = if mmr {
        0
    } else if template == 0 {
        4
    } else {
        1
    };
    for slot in params.at.iter_mut().take(at_pairs) {
        let dx = r.i8()?;
        let dy = r.i8()?;
        *slot = (dx, dy);
    }
    Ok((mmr, params))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::jbig2::mq::{encoder::MqEncoder, MqContext};

    /// The 8x4 subject bitmap the hand-computed context vectors are taken
    /// against.
    fn subject() -> Bitmap {
        let rows = ["10110010", "01101001", "11001100", "00101011"];
        let mut bm = Bitmap::new(8, 4).expect("8x4");
        for (y, row) in rows.iter().enumerate() {
            for (x, ch) in row.bytes().enumerate() {
                bm.set(x as u32, y as u32, u8::from(ch == b'1'));
            }
        }
        bm
    }

    #[test]
    fn nominal_contexts_match_the_hand_computed_values() {
        let bm = subject();
        let cases: [(u8, u32, u32, u16); 6] = [
            (0, 4, 3, 0xA4C2),
            (1, 4, 3, 0x0862),
            (2, 4, 3, 0x011A),
            (3, 4, 3, 0x0262),
            (0, 1, 1, 0x0160),
            (0, 0, 0, 0x0000),
        ];
        for (template, x, y, want) in cases {
            let params = GenericParams::nominal(template);
            assert_eq!(
                context_at(&bm, x, y, &params),
                want,
                "template {template} at ({x}, {y})",
            );
        }
    }

    /// Moving A1 off its nominal position must change exactly its own bit.
    /// Nominal A1 for template 0 is (+3, -1) = (7, 2) = 0; moving it to
    /// (-2, 0) = (2, 3) = 1 sets bit 4 and nothing else.
    #[test]
    fn a_relocated_at_pixel_changes_only_its_own_bit() {
        let bm = subject();
        let mut params = GenericParams::nominal(0);
        params.at[0] = (-2, 0);
        assert_eq!(context_at(&bm, 4, 3, &params), 0xA4D2);
    }

    /// Every AT pixel occupies a distinct bit. Relocating each in turn onto a
    /// known-1 pixel, from a bitmap that is otherwise all zeros, must light
    /// exactly the bit the template assigns it.
    ///
    /// The target is three rows up, at (4, 0) seen from (4, 3). Template 0's
    /// fixed pixels only reach rows y-1 and y-2, so no fixed slot can also
    /// read it — which is what makes `ctx == 1 << bit` an exact assertion
    /// rather than one contaminated by a neighbouring bit.
    #[test]
    fn each_at_pixel_owns_its_documented_bit() {
        let mut bm = Bitmap::new(8, 4).expect("8x4");
        bm.set(4, 0, 1); // the pixel every relocated AT will point at
        let expected_bit = [4u32, 10, 11, 15]; // A1, A2, A3, A4 for template 0
        for (slot, bit) in expected_bit.iter().enumerate() {
            let mut params = GenericParams::nominal(0);
            params.at[slot] = (0, -3);
            let ctx = context_at(&bm, 4, 3, &params);
            assert_eq!(ctx, 1 << bit, "AT slot {} must own bit {bit}", slot + 1);
        }
    }

    /// Contexts must never exceed the template's width, or they would index
    /// outside a correctly-sized context array.
    #[test]
    fn contexts_stay_within_the_template_width() {
        let bm = Bitmap::filled(16, 16, 1).expect("16x16");
        let widths = [16u32, 13, 10, 10];
        for template in 0..4u8 {
            let params = GenericParams::nominal(template);
            for y in 0..16 {
                for x in 0..16 {
                    let ctx = u32::from(context_at(&bm, x, y, &params));
                    assert!(
                        ctx < (1 << widths[template as usize]),
                        "template {template} at ({x}, {y}) gave {ctx:#x}",
                    );
                }
            }
        }
    }

    #[test]
    fn tpgd_contexts_are_the_published_values() {
        assert_eq!(TPGD_CONTEXT, [0x9B25, 0x0795, 0x00E5, 0x0195]);
    }

    #[test]
    fn nominal_at_table_matches_the_standard() {
        assert_eq!(NOMINAL_AT[0], [(3, -1), (-3, -1), (2, -2), (-2, -2)]);
        assert_eq!(NOMINAL_AT[1][0], (3, -1));
        assert_eq!(NOMINAL_AT[2][0], (2, -1));
        assert_eq!(NOMINAL_AT[3][0], (2, -1));
    }

    /// Encodes a bitmap the way the decoder will read it.
    ///
    /// The context formation is shared with the decoder deliberately: the
    /// vectors above already pin that against the standard, so what these
    /// round trips are left to prove is the decode *loop* — row order, the
    /// typical-prediction toggle, the skip mask, and the region bounds.
    fn encode(bm: &Bitmap, params: &GenericParams, skip: Option<&Bitmap>) -> Vec<u8> {
        let mut enc = MqEncoder::new();
        let mut cx = vec![MqContext::default(); GB_CONTEXT_LEN];
        let mut ltp = 0u8;
        for y in 0..bm.height() {
            if params.tpgdon {
                // Typical prediction is only worth signalling when this row
                // repeats the one above; encode the LTP toggle accordingly.
                let repeats = y > 0 && bm.row(y) == bm.row(y - 1);
                let want = u8::from(repeats);
                let bit = ltp ^ want;
                let slot = TPGD_CONTEXT[params.template as usize] as usize;
                enc.encode(&mut cx[slot], bit);
                ltp = want;
                if ltp == 1 {
                    continue;
                }
            }
            for x in 0..bm.width() {
                if skip.is_some_and(|s| s.get(i64::from(x), i64::from(y)) == 1) {
                    continue;
                }
                let ctx = context_at(bm, x, y, params) as usize;
                enc.encode(&mut cx[ctx], bm.get(i64::from(x), i64::from(y)));
            }
        }
        enc.finish()
    }

    fn round_trip(bm: &Bitmap, params: &GenericParams) -> Bitmap {
        let coded = encode(bm, params, None);
        let mut dec = MqDecoder::new(&coded);
        let mut cx = MqContexts::new(GB_CONTEXT_LEN);
        decode_generic_region(&mut dec, &mut cx, bm.width(), bm.height(), params, None)
            .expect("decode")
    }

    fn pseudo_random_bitmap(width: u32, height: u32, seed: u32) -> Bitmap {
        let mut state = seed | 1;
        let mut bm = Bitmap::new(width, height).expect("bitmap");
        for y in 0..height {
            for x in 0..width {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                bm.set(x, y, u8::from((state >> 24) & 1 == 1));
            }
        }
        bm
    }

    #[test]
    fn round_trips_every_template() {
        let bm = pseudo_random_bitmap(37, 23, 0x1234);
        for template in 0..4u8 {
            let params = GenericParams::nominal(template);
            let out = round_trip(&bm, &params);
            for y in 0..bm.height() {
                assert_eq!(out.row(y), bm.row(y), "template {template}, row {y}");
            }
        }
    }

    #[test]
    fn round_trips_with_relocated_at_pixels() {
        let bm = pseudo_random_bitmap(29, 19, 0x99);
        let mut params = GenericParams::nominal(0);
        params.at = [(-2, 0), (0, -2), (5, -1), (-5, -1)];
        let out = round_trip(&bm, &params);
        for y in 0..bm.height() {
            assert_eq!(out.row(y), bm.row(y), "row {y}");
        }
    }

    /// Typical prediction: a bitmap with long stretches of repeated rows is
    /// exactly what TPGDON exists for, and the repeated rows must come back
    /// identical.
    #[test]
    fn round_trips_with_typical_prediction() {
        let seed = pseudo_random_bitmap(31, 4, 0x77);
        let mut bm = Bitmap::new(31, 20).expect("31x20");
        for y in 0..20u32 {
            // Rows 4..12 all repeat row 3.
            let src = if (4..12).contains(&y) { 3 } else { y % 4 };
            for x in 0..31 {
                bm.set(x, y, seed.get(i64::from(x), i64::from(src)));
            }
        }
        for template in 0..4u8 {
            let mut params = GenericParams::nominal(template);
            params.tpgdon = true;
            let out = round_trip(&bm, &params);
            for y in 0..bm.height() {
                assert_eq!(out.row(y), bm.row(y), "template {template}, row {y}");
            }
        }
    }

    /// Skipped pixels are forced to 0 and consume no coded bits (6.2.5.7).
    #[test]
    fn skipped_pixels_are_zero_and_uncoded() {
        let bm = pseudo_random_bitmap(24, 12, 0x5A5A);
        let mut skip = Bitmap::new(24, 12).expect("24x12");
        for y in 0..12u32 {
            for x in 0..24u32 {
                skip.set(x, y, u8::from(x % 3 == 0));
            }
        }
        // The source must already be 0 wherever it is skipped, or the encoder
        // and decoder would disagree about the pixel's value. Sweep the full
        // 24 columns, not just the first 12 — a partial sweep leaves live
        // pixels under the skip mask and the round trip fails at x = 12.
        let mut source = bm;
        for y in 0..12u32 {
            for x in 0..24u32 {
                if skip.get(i64::from(x), i64::from(y)) == 1 {
                    source.set(x, y, 0);
                }
            }
        }
        let params = GenericParams::nominal(0);
        let coded = encode(&source, &params, Some(&skip));
        let mut dec = MqDecoder::new(&coded);
        let mut cx = MqContexts::new(GB_CONTEXT_LEN);
        let out =
            decode_generic_region(&mut dec, &mut cx, 24, 12, &params, Some(&skip)).expect("decode");
        for y in 0..12u32 {
            assert_eq!(out.row(y), source.row(y), "row {y}");
        }
    }

    #[test]
    fn a_zero_sized_region_decodes_to_an_empty_bitmap() {
        let params = GenericParams::nominal(0);
        let mut dec = MqDecoder::new(&[]);
        let mut cx = MqContexts::new(GB_CONTEXT_LEN);
        let out = decode_generic_region(&mut dec, &mut cx, 0, 0, &params, None).expect("decode");
        assert_eq!((out.width(), out.height()), (0, 0));
    }

    /// Garbage in must not panic, hang, or allocate unboundedly — it may
    /// produce nonsense pixels, and that is fine.
    #[test]
    fn arbitrary_bytes_decode_without_panicking() {
        let mut state: u32 = 0xDEAD_BEEF;
        for template in 0..4u8 {
            for tpgdon in [false, true] {
                let data: Vec<u8> = (0..256)
                    .map(|_| {
                        state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                        (state >> 24) as u8
                    })
                    .collect();
                let mut params = GenericParams::nominal(template);
                params.tpgdon = tpgdon;
                let mut dec = MqDecoder::new(&data);
                let mut cx = MqContexts::new(GB_CONTEXT_LEN);
                let out = decode_generic_region(&mut dec, &mut cx, 64, 64, &params, None)
                    .expect("decode must not fail on garbage, only produce garbage");
                assert_eq!((out.width(), out.height()), (64, 64));
            }
        }
    }

    #[test]
    fn an_oversized_region_is_refused() {
        let params = GenericParams::nominal(0);
        let mut dec = MqDecoder::new(&[]);
        let mut cx = MqContexts::new(GB_CONTEXT_LEN);
        assert!(
            decode_generic_region(&mut dec, &mut cx, u32::MAX, u32::MAX, &params, None).is_err()
        );
    }

    #[test]
    fn parses_generic_flags_and_nominal_at_bytes() {
        // MMR 0, template 0, TPGDON 0, then eight nominal AT bytes.
        let bytes = [0x00u8, 3, 0xFF, 0xFD, 0xFF, 2, 0xFE, 0xFE, 0xFE];
        let mut r = Reader::new(&bytes);
        let (mmr, params) = parse_generic_flags(&mut r).expect("flags");
        assert!(!mmr);
        assert_eq!(params.template, 0);
        assert!(!params.tpgdon);
        assert_eq!(params.at, [(3, -1), (-3, -1), (2, -2), (-2, -2)]);
        assert!(r.is_empty());
    }

    #[test]
    fn parses_generic_flags_for_the_narrow_templates() {
        for template in 1..4u8 {
            let flags = (template << 1) | 0b1000; // TPGDON set
            let bytes = [flags, 2, 0xFF];
            let mut r = Reader::new(&bytes);
            let (mmr, params) = parse_generic_flags(&mut r).expect("flags");
            assert!(!mmr);
            assert_eq!(params.template, template);
            assert!(params.tpgdon);
            assert_eq!(params.at[0], (2, -1));
            assert!(r.is_empty());
        }
    }

    #[test]
    fn mmr_consumes_no_at_bytes() {
        let bytes = [0x01u8];
        let mut r = Reader::new(&bytes);
        let (mmr, params) = parse_generic_flags(&mut r).expect("flags");
        assert!(mmr);
        assert_eq!(params.template, 0);
        assert!(r.is_empty());
    }

    #[test]
    fn reserved_generic_flag_bits_are_rejected() {
        let mut r = Reader::new(&[0xF0u8]);
        assert_eq!(
            parse_generic_flags(&mut r),
            Err(Jbig2Error::Malformed("reserved generic region flag bits")),
        );
    }

    /// A flags byte promising AT pairs the segment does not carry is a
    /// truncation, not a panic.
    #[test]
    fn truncated_at_bytes_are_reported() {
        for bytes in [vec![0x00u8], vec![0x00, 3, 0xFF], vec![0x02u8, 2]] {
            let mut r = Reader::new(&bytes);
            assert_eq!(parse_generic_flags(&mut r), Err(Jbig2Error::Truncated));
        }
    }
}
