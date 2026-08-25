//! The generic refinement region decoding procedure (T.88 6.3).
//!
//! A refinement region is coded against a *reference* bitmap it is expected to
//! resemble. Each pixel's context is drawn from both the pixels already decoded
//! in this region and the pixels of the reference around the corresponding
//! location, so a region that differs from its reference in only a few places
//! codes into very little.
//!
//! Two templates exist (6.3.5.3): template 0 gathers thirteen pixels and has
//! two adaptive pixels, template 1 gathers ten and has none.
//!
//! The refinement region segment of 7.4.7 reaches this, and so do the text
//! region symbol refinement of 6.4.11 and the refinement/aggregate symbol
//! coding of a symbol dictionary (6.5.8.2) — every caller the standard
//! defines.

use super::bitmap::Bitmap;
use super::budget::Budget;
#[cfg(test)]
use super::mq::{encoder::MqEncoder, MqContext};
use super::mq::{MqContexts, MqDecoder};
use super::reader::Reader;
use super::Jbig2Error;

/// Contexts the refinement templates address, one per value of the widest
/// (13-bit) template (T.88 6.3.4, CONTEXT).
pub(crate) const GR_CONTEXT_LEN: usize = 1 << 13;

/// The highest template number 6.3.5.3 defines.
const MAX_TEMPLATE: u8 = 1;

/// The nominal locations of the adaptive pixels of template 0 (Figure 12):
/// RA1 in the region being decoded, RA2 in the reference.
pub(crate) const NOMINAL_AT: [(i8, i8); 2] = [(-1, -1), (-1, -1)];

/// One template pixel: an offset, and which bitmap it is read from.
#[derive(Clone, Copy)]
enum Tap {
    /// A pixel of the region being decoded, relative to the current pixel.
    Region(i64, i64),
    /// A pixel of the reference, relative to the current pixel's counterpart
    /// there.
    Reference(i64, i64),
    /// Adaptive pixel `n` of the region being decoded.
    RegionAt(usize),
    /// Adaptive pixel `n` of the reference.
    ReferenceAt(usize),
}

/// Template 0 (Figure 12), gathered in reading order: the region's pixels top
/// to bottom and left to right, then the reference's.
///
/// The adaptive pixels keep their slot in this order wherever they point,
/// which is what 6.3.5.6 step 3 c) ii) requires of the gathering — consistent,
/// and independent of where the AT pixels are.
const TEMPLATE_0: [Tap; 13] = [
    Tap::RegionAt(0),
    Tap::Region(0, -1),
    Tap::Region(1, -1),
    Tap::Region(-1, 0),
    Tap::ReferenceAt(1),
    Tap::Reference(0, -1),
    Tap::Reference(1, -1),
    Tap::Reference(-1, 0),
    Tap::Reference(0, 0),
    Tap::Reference(1, 0),
    Tap::Reference(-1, 1),
    Tap::Reference(0, 1),
    Tap::Reference(1, 1),
];

/// Template 1 (Figure 13), in the same reading order. The reference group is a
/// cross rather than a full square, and there are no adaptive pixels.
const TEMPLATE_1: [Tap; 10] = [
    Tap::Region(-1, -1),
    Tap::Region(0, -1),
    Tap::Region(1, -1),
    Tap::Region(-1, 0),
    Tap::Reference(0, -1),
    Tap::Reference(-1, 0),
    Tap::Reference(0, 0),
    Tap::Reference(1, 0),
    Tap::Reference(0, 1),
    Tap::Reference(1, 1),
];

/// The context that codes the SLTP bit, per template (6.3.5.6 step 3 b),
/// Figures 14 and 15).
///
/// Both figures show every template pixel as 0 except the reference pixel
/// corresponding to the current one, which is 1. Rather than transcribe the
/// two integers, they are computed from the template tables themselves, so a
/// mistake in a template's order cannot leave a stale constant agreeing with
/// it. [`tests::the_sltp_contexts_match_the_figures_that_define_them`] pins the
/// results against the values read off the figures.
fn sltp_context(template: u8) -> u16 {
    let taps = taps_for(template);
    let mut cx = 0u16;
    for tap in taps {
        let bit = matches!(tap, Tap::Reference(0, 0));
        cx = (cx << 1) | u16::from(bit);
    }
    cx
}

/// The template a given identifier selects, clamped rather than indexed out of
/// bounds: the value reaches here from a segment header.
fn taps_for(template: u8) -> &'static [Tap] {
    if template.min(MAX_TEMPLATE) == 0 {
        &TEMPLATE_0
    } else {
        &TEMPLATE_1
    }
}

/// The reference bitmap a refinement is coded against, with its offset
/// (T.88 6.3.2: GRREFERENCE, GRREFERENCEDX and GRREFERENCEDY).
///
/// The three travel together everywhere — a reference without its offset does
/// not locate anything — so they are one parameter.
#[derive(Clone, Copy)]
pub(crate) struct Reference<'a> {
    /// The bitmap the region is refining.
    pub(crate) bitmap: &'a Bitmap,
    /// GRREFERENCEDX: the pixel of `bitmap` matching `(x, y)` is at `x - dx`.
    pub(crate) dx: i32,
    /// GRREFERENCEDY: and at `y - dy`.
    pub(crate) dy: i32,
}

impl<'a> Reference<'a> {
    /// A reference aligned with the region, which is the common case.
    pub(crate) fn aligned(bitmap: &'a Bitmap) -> Reference<'a> {
        Reference {
            bitmap,
            dx: 0,
            dy: 0,
        }
    }
}

/// The parameters of a refinement that come from a segment header
/// (T.88 6.3.2, 7.4.7.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct RefinementParams {
    /// GRTEMPLATE, 0 or 1.
    pub(crate) template: u8,
    /// The adaptive pixels RA1 and RA2, as `(dx, dy)`. Template 1 has none and
    /// ignores these.
    pub(crate) at: [(i8, i8); 2],
    /// TPGRON: whether each row is preceded by a typical-prediction decision.
    pub(crate) tpgron: bool,
}

impl RefinementParams {
    /// The parameters an encoder gets by leaving the AT pixels where 6.3.5.3
    /// puts them, with typical prediction off.
    ///
    /// Only the tests build parameters this way. Every real caller reads them
    /// from a segment header — 7.4.7.3 for a refinement region, and the SDRAT
    /// and SBRAT fields for the two coding procedures that embed refinement.
    #[cfg(test)]
    pub(crate) fn nominal(template: u8) -> RefinementParams {
        RefinementParams {
            template,
            at: NOMINAL_AT,
            tpgron: false,
        }
    }
}

/// Reads a refinement AT pixel field — SBRATX1 to SBRATY2 of T.88 7.4.3.1.3,
/// or SDRATX1 to SDRATY2 of 7.4.2.1.3, one signed byte each — and folds it,
/// with the template bit, into this procedure's parameters.
///
/// Template 1 has no adaptive pixels, so both clauses omit the field for it
/// and the nominal offsets stand in for values nothing will read. TPGRON is 0
/// for every embedded refinement — Tables 12 and 18 both fix it — which is why
/// it is not a parameter here; the refinement region segment of 7.4.7 carries
/// its own flag and parses its own field.
pub(crate) fn parse_refinement_at(
    r: &mut Reader<'_>,
    template_1: bool,
) -> Result<RefinementParams, Jbig2Error> {
    if template_1 {
        return Ok(RefinementParams {
            template: 1,
            at: NOMINAL_AT,
            tpgron: false,
        });
    }
    let mut at = NOMINAL_AT;
    for pixel in &mut at {
        *pixel = (r.u8()? as i8, r.u8()? as i8);
    }
    Ok(RefinementParams {
        template: 0,
        at,
        tpgron: false,
    })
}

/// Decodes a refinement region against `reference` (T.88 6.3.5.6).
///
/// The pixel of the reference corresponding to `(x, y)` here is the one at
/// `(x - dx, y - dy)`. Reads outside either bitmap yield 0, as 6.3.5.2
/// requires.
///
/// `cx` is the shared GR context array of [`GR_CONTEXT_LEN`] entries; it is
/// passed in rather than allocated so that a segment refining many bitmaps
/// keeps one set of adaptive statistics across them.
///
/// The budget is charged from the declared dimensions before any pixel is
/// decoded, for the reason it is everywhere else in this decoder: a region
/// states what it will cost and need not carry the bits to back it up.
pub(crate) fn decode_refinement_region(
    dec: &mut MqDecoder,
    cx: &mut MqContexts,
    budget: &mut Budget,
    width: u32,
    height: u32,
    reference: Reference<'_>,
    params: &RefinementParams,
) -> Result<Bitmap, Jbig2Error> {
    budget.charge_region(width, height)?;
    let mut bm = Bitmap::new(width, height)?;
    let taps = taps_for(params.template);
    let at = params.at;
    let sltp_cx = usize::from(sltp_context(params.template));
    let dx = i64::from(reference.dx);
    let dy = i64::from(reference.dy);
    let reference = reference.bitmap;
    let mut ltp = false;

    for y in 0..height {
        if params.tpgron {
            // 6.3.5.6 step 3 b). The decoded bit toggles LTP; note that this
            // decision is not itself part of any pixel's template.
            ltp ^= dec.decode(cx.get_mut(sltp_cx)) == 1;
        }
        let ry = i64::from(y) - dy;
        for x in 0..width {
            let rx = i64::from(x) - dx;
            if ltp {
                // 6.3.5.6 step 3 d) i): a pixel whose 3x3 reference
                // neighbourhood is uniform takes that common value and costs
                // no decision at all.
                if let Some(value) = uniform_reference(reference, rx, ry) {
                    bm.set(x, y, value);
                    continue;
                }
            }
            let mut context = 0usize;
            for tap in taps {
                let pixel = match *tap {
                    Tap::Region(tx, ty) => bm.get(i64::from(x) + tx, i64::from(y) + ty),
                    Tap::Reference(tx, ty) => reference.get(rx + tx, ry + ty),
                    Tap::RegionAt(n) => {
                        let (ax, ay) = at[n];
                        bm.get(i64::from(x) + i64::from(ax), i64::from(y) + i64::from(ay))
                    }
                    Tap::ReferenceAt(n) => {
                        let (ax, ay) = at[n];
                        reference.get(rx + i64::from(ax), ry + i64::from(ay))
                    }
                };
                context = (context << 1) | usize::from(pixel);
            }
            let pixel = dec.decode(cx.get_mut(context));
            bm.set(x, y, pixel);
        }
    }
    Ok(bm)
}

/// The common value of the 3x3 reference neighbourhood centred at `(x, y)`, or
/// `None` when the nine pixels are not all equal (T.88 6.3.5.6 step 3 d) i),
/// Figure 16).
///
/// Pixels outside the reference read as 0 here exactly as they do in a
/// template, so a neighbourhood straddling the edge is uniform only when the
/// pixels inside it are 0 too.
fn uniform_reference(reference: &Bitmap, x: i64, y: i64) -> Option<u8> {
    let first = reference.get(x - 1, y - 1);
    for ty in -1..=1i64 {
        for tx in -1..=1i64 {
            if reference.get(x + tx, y + ty) != first {
                return None;
            }
        }
    }
    Some(first)
}

#[cfg(test)]
/// The encoder side of 6.3.5.6 with its own coder and fresh statistics, which
/// is the shape a refinement region segment's fixture wants. Returns the coded
/// bytes and the number of pixels coded explicitly — the ones typical
/// prediction did not cover.
pub(crate) fn encode_refinement_at(
    target: &Bitmap,
    reference: &Bitmap,
    params: &RefinementParams,
    dx: i32,
    dy: i32,
) -> (Vec<u8>, usize) {
    let mut cx = vec![MqContext::default(); GR_CONTEXT_LEN];
    let mut enc = MqEncoder::new();
    let explicit = encode_refinement_into(&mut enc, &mut cx, target, reference, params, dx, dy);
    (enc.finish(), explicit)
}

#[cfg(test)]
/// The encoder side of 6.3.5.6 into a caller-owned coder and context array,
/// mirroring the decoder decision for decision — including skipping the pixels
/// typical prediction covers. Returns the number of pixels coded explicitly.
///
/// The coder and contexts are the caller's because a text region owns both
/// across its instances: the arithmetic variant braids every refinement into
/// the segment's one codeword, and even the Huffman variant, whose refinements
/// are separate codewords, adapts one set of GR statistics across them —
/// E.3.7 resets statistics per segment, not per bitmap.
pub(crate) fn encode_refinement_into(
    enc: &mut MqEncoder,
    cx: &mut [MqContext],
    target: &Bitmap,
    reference: &Bitmap,
    params: &RefinementParams,
    dx: i32,
    dy: i32,
) -> usize {
    let taps = taps_for(params.template);
    let sltp_cx = usize::from(sltp_context(params.template));
    let (dx, dy) = (i64::from(dx), i64::from(dy));
    let mut ltp = false;
    let mut explicit = 0usize;

    for y in 0..target.height() {
        if params.tpgron {
            // Turn typical prediction on for a row when it would save
            // work: every pixel the 3x3 rule covers must already match.
            let ry = i64::from(y) - dy;
            let want = (0..target.width()).all(|x| {
                let rx = i64::from(x) - dx;
                match uniform_reference(reference, rx, ry) {
                    Some(v) => v == target.get(i64::from(x), i64::from(y)),
                    None => true,
                }
            });
            let sltp = want != ltp;
            enc.encode(&mut cx[sltp_cx], u8::from(sltp));
            ltp = want;
        }
        let ry = i64::from(y) - dy;
        for x in 0..target.width() {
            let rx = i64::from(x) - dx;
            let pixel = target.get(i64::from(x), i64::from(y));
            if ltp && uniform_reference(reference, rx, ry).is_some() {
                continue;
            }
            let mut context = 0usize;
            for tap in taps {
                let value = match *tap {
                    Tap::Region(tx, ty) => read_decoded(target, x, y, tx, ty),
                    Tap::Reference(tx, ty) => reference.get(rx + tx, ry + ty),
                    Tap::RegionAt(n) => {
                        let (ax, ay) = params.at[n];
                        read_decoded(target, x, y, i64::from(ax), i64::from(ay))
                    }
                    Tap::ReferenceAt(n) => {
                        let (ax, ay) = params.at[n];
                        reference.get(rx + i64::from(ax), ry + i64::from(ay))
                    }
                };
                context = (context << 1) | usize::from(value);
            }
            enc.encode(&mut cx[context], pixel);
            explicit += 1;
        }
    }
    explicit
}

/// A template pixel of the region being decoded, as the decoder would see
/// it: pixels the raster order has not reached yet read as 0.
#[cfg(test)]
fn read_decoded(target: &Bitmap, x: u32, y: u32, tx: i64, ty: i64) -> u8 {
    let (px, py) = (i64::from(x) + tx, i64::from(y) + ty);
    if py > i64::from(y) || (py == i64::from(y) && px >= i64::from(x)) {
        return 0;
    }
    target.get(px, py)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bitmap_from(rows: &[&str]) -> Bitmap {
        let mut bm = Bitmap::new(rows[0].len() as u32, rows.len() as u32).expect("small");
        for (y, row) in rows.iter().enumerate() {
            for (x, ch) in row.bytes().enumerate() {
                bm.set(x as u32, y as u32, u8::from(ch == b'1'));
            }
        }
        bm
    }

    fn rows_of(bm: &Bitmap) -> Vec<String> {
        (0..bm.height())
            .map(|y| {
                (0..bm.width())
                    .map(|x| {
                        if bm.get(i64::from(x), i64::from(y)) == 1 {
                            '1'
                        } else {
                            '0'
                        }
                    })
                    .collect()
            })
            .collect()
    }

    /// Figure 14 shows every template 0 pixel as 0 but the reference pixel
    /// under the current one, and Figure 15 does the same for template 1.
    /// Gathered in this module's order those are 0x0010 and 0x0008.
    ///
    /// The second of the two is the one the standard itself checks: the EXAMPLE
    /// in 6.3.5.6 step 3 c) iii) says that gathering Figure 15's values in
    /// reading order, region before reference, yields "GR0000001000". That
    /// binary literal is 8, so this assertion is the spec's own worked example
    /// and it pins template 1's geometry, not merely its arithmetic.
    #[test]
    fn the_sltp_contexts_match_the_figures_that_define_them() {
        assert_eq!(sltp_context(1), 0b0000001000, "the EXAMPLE in 6.3.5.6");
        assert_eq!(sltp_context(0), 0b0000000010000);
    }

    /// Every template pixel has to be a distinct location, or two of them would
    /// share a context bit and the template would address fewer contexts than
    /// it claims.
    #[test]
    fn no_template_reads_the_same_pixel_twice() {
        for template in 0..=MAX_TEMPLATE {
            let mut seen = Vec::new();
            for tap in taps_for(template) {
                let key = match *tap {
                    Tap::Region(x, y) => (0, x, y),
                    Tap::Reference(x, y) => (1, x, y),
                    Tap::RegionAt(n) => (0, i64::from(NOMINAL_AT[n].0), i64::from(NOMINAL_AT[n].1)),
                    Tap::ReferenceAt(n) => {
                        (1, i64::from(NOMINAL_AT[n].0), i64::from(NOMINAL_AT[n].1))
                    }
                };
                assert!(!seen.contains(&key), "template {template} repeats {key:?}");
                seen.push(key);
            }
        }
        assert_eq!(TEMPLATE_0.len(), 13, "6.3.5.3 calls this the 13-pixel one");
        assert_eq!(TEMPLATE_1.len(), 10, "and this the 10-pixel one");
    }

    /// The widest template must not address more contexts than the array holds.
    #[test]
    fn the_context_array_covers_the_widest_template() {
        assert_eq!(GR_CONTEXT_LEN, 1 << TEMPLATE_0.len());
        assert!(TEMPLATE_1.len() <= TEMPLATE_0.len());
    }

    /// Encoding a region against a reference and decoding it back must return
    /// the region, for both templates and with typical prediction either way.
    /// This is the whole procedure end to end, driven by the test encoder.
    #[test]
    fn a_refined_region_decodes_back_to_what_was_encoded() {
        let reference = bitmap_from(&[
            "00000000", "00111100", "01111110", "01111110", "01111110", "00111100", "00000000",
            "00000000",
        ]);
        // Differs from the reference in a handful of places, which is the case
        // refinement exists for.
        let target = bitmap_from(&[
            "00000000", "00111100", "01111110", "01100110", "01111110", "00111100", "00010000",
            "00000000",
        ]);

        for template in 0..=MAX_TEMPLATE {
            for tpgron in [false, true] {
                let params = RefinementParams {
                    tpgron,
                    ..RefinementParams::nominal(template)
                };
                let coded = encode_refinement(&target, &reference, &params);

                let mut dec = MqDecoder::new(&coded);
                let mut cx = MqContexts::new(GR_CONTEXT_LEN);
                let mut budget = Budget::new();
                let got = decode_refinement_region(
                    &mut dec,
                    &mut cx,
                    &mut budget,
                    target.width(),
                    target.height(),
                    Reference::aligned(&reference),
                    &params,
                )
                .expect("decodes");
                assert_eq!(
                    rows_of(&got),
                    rows_of(&target),
                    "template {template}, tpgron {tpgron}"
                );
            }
        }
    }

    /// A reference offset moves which reference pixel each target pixel is
    /// coded against, so a round trip has to survive one.
    #[test]
    fn a_reference_offset_round_trips() {
        let reference = bitmap_from(&["1111", "1001", "1001", "1111"]);
        let target = bitmap_from(&["0110", "0110", "0000", "0110"]);
        for (dx, dy) in [(1i32, 0i32), (0, 1), (-2, 1), (3, -2)] {
            let params = RefinementParams::nominal(0);
            let (coded, _) = encode_refinement_at(&target, &reference, &params, dx, dy);
            let mut dec = MqDecoder::new(&coded);
            let mut cx = MqContexts::new(GR_CONTEXT_LEN);
            let mut budget = Budget::new();
            let got = decode_refinement_region(
                &mut dec,
                &mut cx,
                &mut budget,
                target.width(),
                target.height(),
                Reference {
                    bitmap: &reference,
                    dx,
                    dy,
                },
                &params,
            )
            .expect("decodes");
            assert_eq!(rows_of(&got), rows_of(&target), "offset ({dx}, {dy})");
        }
    }

    /// Relocating the adaptive pixels must not change what a round trip
    /// produces — only how well it compresses.
    #[test]
    fn relocated_adaptive_pixels_round_trip() {
        let reference = bitmap_from(&["0110", "1111", "1111", "0110"]);
        let target = bitmap_from(&["0110", "1101", "1011", "0110"]);
        for at in [
            [(-1i8, -1i8), (-1, -1)],
            [(-2, -1), (1, 1)],
            [(2, -2), (0, 2)],
        ] {
            let params = RefinementParams {
                template: 0,
                at,
                tpgron: false,
            };
            let coded = encode_refinement(&target, &reference, &params);
            let mut dec = MqDecoder::new(&coded);
            let mut cx = MqContexts::new(GR_CONTEXT_LEN);
            let mut budget = Budget::new();
            let got = decode_refinement_region(
                &mut dec,
                &mut cx,
                &mut budget,
                target.width(),
                target.height(),
                Reference::aligned(&reference),
                &params,
            )
            .expect("decodes");
            assert_eq!(rows_of(&got), rows_of(&target), "at {at:?}");
        }
    }

    /// A region identical to its reference is what typical prediction is for.
    ///
    /// The claim worth checking is not that the coded bytes shrink — at these
    /// sizes the arithmetic coder's flush dominates and both come to seven
    /// bytes — but that the mechanism engages: a pixel whose 3x3 reference
    /// neighbourhood is uniform costs no decision at all. So the shape needs a
    /// real interior. A small one is nearly all boundary, where the rule
    /// cannot fire, and would measure the fixture rather than the code.
    #[test]
    fn typical_prediction_codes_almost_nothing_for_an_unchanged_region() {
        let (w, h) = (32u32, 24u32);
        let mut reference = Bitmap::new(w, h).expect("small");
        for y in 5..19u32 {
            for x in 6..26u32 {
                reference.set(x, y, 1);
            }
        }
        let target = reference.clone();
        let pixels = (w * h) as usize;

        let (_, with_prediction) = encode_refinement_at(
            &target,
            &reference,
            &RefinementParams {
                tpgron: true,
                ..RefinementParams::nominal(0)
            },
            0,
            0,
        );
        let (_, without) =
            encode_refinement_at(&target, &reference, &RefinementParams::nominal(0), 0, 0);

        assert_eq!(without, pixels, "every pixel is coded with TPGRON off");
        assert!(
            with_prediction * 3 < pixels,
            "typical prediction should cover most of an unchanged region, \
             but {with_prediction} of {pixels} pixels stayed explicit"
        );

        // And it must still decode to the region it started from.
        let (coded, _) = encode_refinement_at(
            &target,
            &reference,
            &RefinementParams {
                tpgron: true,
                ..RefinementParams::nominal(0)
            },
            0,
            0,
        );
        let mut dec = MqDecoder::new(&coded);
        let mut cx = MqContexts::new(GR_CONTEXT_LEN);
        let mut budget = Budget::new();
        let got = decode_refinement_region(
            &mut dec,
            &mut cx,
            &mut budget,
            w,
            h,
            Reference::aligned(&reference),
            &RefinementParams {
                tpgron: true,
                ..RefinementParams::nominal(0)
            },
        )
        .expect("decodes");
        assert_eq!(rows_of(&got), rows_of(&target));
    }

    /// The reference is read through the same out-of-bounds rule as everything
    /// else (6.3.5.2), so a reference smaller than the region must decode
    /// rather than fail.
    #[test]
    fn a_reference_smaller_than_the_region_reads_zero_outside_it() {
        let reference = bitmap_from(&["11", "11"]);
        let target = bitmap_from(&["1100", "1100", "0000", "0000"]);
        let params = RefinementParams::nominal(1);
        let coded = encode_refinement(&target, &reference, &params);
        let mut dec = MqDecoder::new(&coded);
        let mut cx = MqContexts::new(GR_CONTEXT_LEN);
        let mut budget = Budget::new();
        let got = decode_refinement_region(
            &mut dec,
            &mut cx,
            &mut budget,
            4,
            4,
            Reference::aligned(&reference),
            &params,
        )
        .expect("decodes");
        assert_eq!(rows_of(&got), rows_of(&target));
    }

    /// An empty reference and a zero-sized region are both legal and must not
    /// panic; the budget, not this procedure, is what bounds a large one.
    #[test]
    fn degenerate_sizes_decode_without_panicking() {
        let reference = Bitmap::new(0, 0).expect("empty");
        let mut dec = MqDecoder::new(&[]);
        let mut cx = MqContexts::new(GR_CONTEXT_LEN);
        let mut budget = Budget::new();
        for (w, h) in [(0u32, 0u32), (0, 5), (5, 0)] {
            let got = decode_refinement_region(
                &mut dec,
                &mut cx,
                &mut budget,
                w,
                h,
                Reference::aligned(&reference),
                &RefinementParams::nominal(0),
            )
            .expect("decodes");
            assert_eq!((got.width(), got.height()), (w, h));
        }
    }

    /// A region far larger than the work budget allows must be refused from its
    /// declared size, before a single pixel is decoded.
    #[test]
    fn an_oversized_region_is_refused_rather_than_decoded() {
        let reference = Bitmap::new(1, 1).expect("tiny");
        let mut dec = MqDecoder::new(&[]);
        let mut cx = MqContexts::new(GR_CONTEXT_LEN);
        let mut budget = Budget::new();
        let refused = decode_refinement_region(
            &mut dec,
            &mut cx,
            &mut budget,
            u32::MAX,
            u32::MAX,
            Reference::aligned(&reference),
            &RefinementParams::nominal(0),
        );
        assert!(refused.is_err(), "must not attempt this region");
    }

    /// Encodes `target` against `reference` with no offset, mirroring
    /// [`decode_refinement_region`] decision for decision.
    fn encode_refinement(
        target: &Bitmap,
        reference: &Bitmap,
        params: &RefinementParams,
    ) -> Vec<u8> {
        encode_refinement_at(target, reference, params, 0, 0).0
    }
}
