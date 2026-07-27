//! Generic region decoding (ITU-T T.88 6.2).
//!
//! A generic region is a rectangle of pixels decoded one at a time, each
//! against an adaptive arithmetic context formed from the pixels already
//! decoded around it. Four templates (6.2.5.3, figures 4 to 7) select which
//! neighbours take part; each template reserves one to four *adaptive* slots
//! whose offsets the segment header carries, so an encoder can point them at
//! whatever correlates best with the image.
//!
//! Two decoding paths live here and they must agree pixel for pixel. The
//! general one reads every template pixel through the bounds-checked accessor,
//! so any AT offset a stream declares is honoured and none of them can read
//! outside the bitmap. The windowed one applies when the AT pixels sit where
//! 6.2.5.3 puts them by default, which is what almost every encoder emits: the
//! template then degenerates into two or three contiguous runs of pixels that
//! shift one position as `x` advances, so a whole context can be carried
//! forward with two reads instead of sixteen.
//!
//! A generic region need not be arithmetically coded at all. When its MMR flag
//! is set (6.2.6) the pixels are written with the two-dimensional facsimile
//! coding of ITU-T T.6 instead, and neither the templates nor the adaptive
//! pixels above have anything to do with it: [`decode_mmr_region`] hands the
//! region to the facsimile codec beside this one, which the `CCITTFaxDecode`
//! filter also uses.

use super::bitmap::Bitmap;
use super::budget::Budget;
use super::mq::{MqContexts, MqDecoder};
use super::reader::Reader;
use super::Jbig2Error;
use crate::filters::ccitt::decoder as facsimile;

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

    /// Whether every AT slot this template actually reads sits at its nominal
    /// position, which is the precondition for the windowed context update.
    ///
    /// Only the slots the template uses are compared: templates 1 to 3 read A1
    /// alone, and a stream that leaves junk in A2 to A4 — which it may, since
    /// it never transmits them — must not be pushed onto the slower path for
    /// it.
    ///
    /// A template number outside 0 to 3 has no window geometry, so it is never
    /// nominal. That keeps the two paths in agreement on a value neither the
    /// flags parser nor the standard can produce.
    pub(crate) fn is_nominal(&self) -> bool {
        if self.template > MAX_TEMPLATE {
            return false;
        }
        let used = if self.template == 0 { 4 } else { 1 };
        let nominal = NOMINAL_AT[usize::from(self.template)];
        self.at[..used] == nominal[..used]
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

/// The rows the three windows walk, as offsets from the row being decoded.
const WINDOW_DY: [i64; 3] = [-2, -1, 0];

/// The contiguous pixel runs each template's context decomposes into when the
/// AT pixels are at their nominal offsets, as `(leftmost dx, bit width)` for
/// rows y-2, y-1 and y (T.88 6.2.5.3, figures 4 to 7).
///
/// The runs are read left to right into the context's bits in the same order,
/// so the context is simply the three runs concatenated. Template 3 reads a
/// single reference row and so has no y-2 run, recorded here as a zero width.
///
/// Cross-check against [`context_at`]: template 0's bits 15 to 11 are the
/// pixels (x-2, y-2) through (x+2, y-2), because nominal A4 is (x-2, y-2) and
/// nominal A3 is (x+2, y-2); bits 10 to 4 are (x-3, y-1) through (x+3, y-1),
/// bracketed by nominal A2 and A1; bits 3 to 0 are (x-4, y) through (x-1, y).
const WINDOW_SPANS: [[(i64, u32); 3]; 4] = [
    [(-2, 5), (-3, 7), (-4, 4)],
    [(-1, 4), (-2, 6), (-3, 3)],
    [(-1, 3), (-2, 5), (-2, 2)],
    [(0, 0), (-3, 6), (-4, 4)],
];

/// The context of one pixel, held as shift registers that can be carried to
/// the next pixel in the row instead of being rebuilt (T.88 6.2.5.7).
///
/// With nominal AT pixels each template's neighbourhood is two or three
/// contiguous runs, one per template row. Moving from `x` to `x + 1` slides
/// every run one pixel right: the leftmost pixel falls out of the top of the
/// register and one new pixel enters at the bottom. So a context costs two
/// reads, one per reference row, instead of the sixteen bounds-checked reads
/// [`context_at`] performs — and the pixel entering the current row's run is
/// the one just decoded, which costs no read at all.
///
/// Template 3 has no y-2 row, so its top run is zero bits wide and its read is
/// discarded by a zero mask. Branching around it would trade a read for a
/// mispredictable test on the hot path, and templates other than 0 are rare
/// enough that the branch would never pay for itself.
///
/// The windows are only valid for the row they were started on, and only while
/// they are advanced at every `x` in turn. Rows a typical-prediction run
/// duplicates are never walked, so nothing has to be kept in step across them:
/// the next decoded row starts a fresh set.
pub(crate) struct ContextWindows {
    /// The three runs, right-aligned in their own widths, for rows y-2, y-1
    /// and y.
    words: [u16; 3],
    /// Width mask per run, which is what discards the pixel leaving on the
    /// left when the run shifts.
    masks: [u16; 3],
    /// The dx, relative to the current x, of the pixel entering each run when
    /// x advances by one. Always 0 for the current row's run.
    entering: [i64; 3],
    /// Left shift placing each run in the assembled context.
    shifts: [u32; 3],
    /// The row being decoded, so the reference rows can be located.
    y: i64,
}

impl ContextWindows {
    /// Builds the windows for `x == 0` of row `y`, reading each run's span
    /// pixel by pixel.
    ///
    /// At `x == 0` most of every span lies left of the bitmap and reads as 0,
    /// as 6.2.5.2 requires; the same is true of the reference rows for `y == 0`
    /// and `y == 1`. Nothing special-cases those, because this runs once per
    /// row rather than once per pixel and the general accessor already gives
    /// the right answer.
    ///
    /// An undefined template is clamped rather than indexing out of bounds. It
    /// is unreachable — [`GenericParams::is_nominal`] refuses those before the
    /// windowed path is chosen — but the clamp costs nothing and a panic here
    /// would be reachable from stream data if that ever slipped.
    pub(crate) fn start(bm: &Bitmap, y: u32, template: u8) -> ContextWindows {
        let spans = WINDOW_SPANS[usize::from(template.min(MAX_TEMPLATE))];
        let y = i64::from(y);
        let mut win = ContextWindows {
            words: [0; 3],
            masks: [0; 3],
            entering: [0; 3],
            shifts: [0; 3],
            y,
        };
        // Assembled from the bottom row up, so each run's shift is the total
        // width of the runs below it.
        let mut shift = 0u32;
        for row in (0..3usize).rev() {
            let (left, width) = spans[row];
            let mut word = 0u16;
            for step in 0..i64::from(width) {
                word = (word << 1) | u16::from(bm.get(left + step, y + WINDOW_DY[row]));
            }
            win.words[row] = word;
            win.masks[row] = (1u16 << width) - 1;
            // The pixel entering on the right is the one just past the run's
            // rightmost, which is `left + width - 1`.
            win.entering[row] = left + i64::from(width);
            win.shifts[row] = shift;
            shift += width;
        }
        win
    }

    /// The context for the current `x`, the runs concatenated most-significant
    /// row first.
    pub(crate) fn value(&self) -> u16 {
        (self.words[0] << self.shifts[0]) | (self.words[1] << self.shifts[1]) | self.words[2]
    }

    /// Slides every run one pixel right, moving the windows from `x` to
    /// `x + 1`.
    ///
    /// `just_decoded` is the pixel at `(x, y)`. It is passed rather than read
    /// back out of the bitmap because it is the one pixel entering a run whose
    /// value the caller already holds — and a skipped pixel is stored as 0, so
    /// passing the stored value keeps skip handling out of here entirely.
    pub(crate) fn advance(&mut self, bm: &Bitmap, x: u32, just_decoded: u8) {
        let x = i64::from(x);
        let above = u16::from(bm.get(x + self.entering[0], self.y + WINDOW_DY[0]));
        let previous = u16::from(bm.get(x + self.entering[1], self.y + WINDOW_DY[1]));
        self.words[0] = ((self.words[0] << 1) | above) & self.masks[0];
        self.words[1] = ((self.words[1] << 1) | previous) & self.masks[1];
        self.words[2] = ((self.words[2] << 1) | u16::from(just_decoded & 1)) & self.masks[2];
    }
}

/// Decodes a generic region into a fresh bitmap (T.88 6.2.5.7).
///
/// Dispatches on the AT pixel offsets: nominal ones take the windowed context
/// update, anything else the general path. The two are required to be
/// indistinguishable in their output, and the test module holds that as an
/// explicit property over every pixel of every template.
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
/// `budget` is the stream's remaining allowance of decoding work, and the whole
/// region is charged against it before the first row is entered. That charge is
/// what bounds the loops: they run `height` times and `width * height` times
/// respectively, both figures taken from a segment header a hostile stream
/// wrote, and the coded data need not exist for them to run — past the end of
/// the input the arithmetic decoder keeps answering (T.88 E.3.4). The
/// allocation cap cannot stand in for the charge, because a region no pixels
/// wide allocates nothing and still walks every row it declares.
pub(crate) fn decode_generic_region(
    dec: &mut MqDecoder,
    cx: &mut MqContexts,
    budget: &mut Budget,
    width: u32,
    height: u32,
    params: &GenericParams,
    skip: Option<&Bitmap>,
) -> Result<Bitmap, Jbig2Error> {
    if params.is_nominal() {
        decode_generic_region_windowed(dec, cx, budget, width, height, params, skip)
    } else {
        decode_generic_region_general(dec, cx, budget, width, height, params, skip)
    }
}

/// Decodes a generic region forming each context from scratch
/// (T.88 6.2.5.7).
///
/// This is the path for a stream that has relocated its AT pixels, and it is
/// also the reference the windowed path is tested against, so it stays whether
/// or not a relocated AT pixel is ever seen in the wild. See
/// [`decode_generic_region`] for what the parameters mean.
///
/// The budget charge lives here rather than in the dispatcher so that it cannot
/// be walked around: this entry point is reachable on its own, and a path into
/// the pixel loop that skipped the charge would undo the bound for every caller
/// that found it.
pub(crate) fn decode_generic_region_general(
    dec: &mut MqDecoder,
    cx: &mut MqContexts,
    budget: &mut Budget,
    width: u32,
    height: u32,
    params: &GenericParams,
    skip: Option<&Bitmap>,
) -> Result<Bitmap, Jbig2Error> {
    budget.charge_region(width, height)?;
    let mut bm = Bitmap::new(width, height)?;
    let mut ltp = 0u8;
    for y in 0..height {
        if typical_prediction_repeats_row(dec, cx, params, &mut ltp) {
            bm.duplicate_row(y);
            continue;
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

/// Decodes a generic region carrying each context forward across the row
/// (T.88 6.2.5.7), which requires the AT pixels to be nominal.
///
/// Row order, the typical-prediction toggle, the skip mask and the budget
/// charge behave exactly as in [`decode_generic_region_general`]; the only
/// difference is where the context comes from. A skipped pixel still shifts a 0
/// into the current row's run, because that is the value stored for it.
///
/// See [`decode_generic_region`] for what the parameters mean.
fn decode_generic_region_windowed(
    dec: &mut MqDecoder,
    cx: &mut MqContexts,
    budget: &mut Budget,
    width: u32,
    height: u32,
    params: &GenericParams,
    skip: Option<&Bitmap>,
) -> Result<Bitmap, Jbig2Error> {
    budget.charge_region(width, height)?;
    let mut bm = Bitmap::new(width, height)?;
    let mut ltp = 0u8;
    for y in 0..height {
        if typical_prediction_repeats_row(dec, cx, params, &mut ltp) {
            bm.duplicate_row(y);
            continue;
        }
        let mut win = ContextWindows::start(&bm, y, params.template);
        for x in 0..width {
            let pixel = if skip.is_some_and(|s| s.get(i64::from(x), i64::from(y)) == 1) {
                0
            } else {
                dec.decode(cx.get_mut(usize::from(win.value())))
            };
            bm.set(x, y, pixel);
            win.advance(&bm, x, pixel);
        }
    }
    Ok(bm)
}

/// Decodes a generic region coded with the two-dimensional facsimile scheme of
/// ITU-T T.6 (T.88 6.2.6), which is what the MMR flag of the segment's flags
/// byte selects.
///
/// Nothing about the region is arithmetic: `data` is a bit stream of run-length
/// codes, and the AT pixels and the typical-prediction flag are absent from the
/// segment header because neither has any meaning here. What the region
/// contributes is the row width and the row count; the coding itself is the
/// same one the `CCITTFaxDecode` filter reads, so it is decoded by the same
/// module, with the layout 6.2.6 fixes — pure two-dimensional, no end-of-line
/// patterns, no byte alignment.
///
/// **No polarity conversion happens here, and none may be added.** A set pixel
/// is black in the facsimile codec and ink in JBIG2; those are the same thing.
/// The single inversion that reconciles ink with `/DeviceGray` is applied to the
/// assembled page at the filter boundary.
///
/// `budget` is charged from the declared dimensions before any decoding, for
/// the reasons set out on [`decode_generic_region`] and in the budget module:
/// a region declares what it costs and need not carry the bits to back it up.
/// Two further points are particular to this path.
///
/// A row count of zero is refused rather than passed on. In the facsimile codec
/// that value means "as many rows as the data holds" (ISO 32000-1 Table 11),
/// which is a count taken from the coded data — and a count taken from the data
/// is one the charge, computed from the declared height of zero, has not paid
/// for. JBIG2 never uses that encoding: 7.4.6.1 states the height in the region
/// information field, and a region of no rows contributes no pixels to a page
/// in any case.
///
/// A row width of zero is refused too, by the codec itself, for the same reason
/// it refuses one from a PDF: a row of no pixels is not a narrow image.
pub(crate) fn decode_mmr_region(
    data: &[u8],
    budget: &mut Budget,
    width: u32,
    height: u32,
) -> Result<Bitmap, Jbig2Error> {
    budget.charge_region(width, height)?;
    if height == 0 {
        return Err(Jbig2Error::Malformed("MMR region of no rows"));
    }
    let layout = facsimile::Params {
        columns: width,
        rows: height,
        k: -1,
        end_of_line: false,
        byte_align: false,
    };
    Ok(facsimile::decode(data, &layout)?)
}

/// Decodes the typical-prediction decision that precedes a row when TPGDON is
/// set, and reports whether the row is a copy of the one above
/// (T.88 6.2.5.7).
///
/// The decision toggles LTP rather than setting it; while LTP is 1 each row
/// repeats its predecessor and carries no coded pixels at all. With TPGDON
/// clear no decision is coded and every row is decoded normally.
fn typical_prediction_repeats_row(
    dec: &mut MqDecoder,
    cx: &mut MqContexts,
    params: &GenericParams,
    ltp: &mut u8,
) -> bool {
    if !params.tpgdon {
        return false;
    }
    let slot = usize::from(TPGD_CONTEXT[usize::from(params.template.min(MAX_TEMPLATE))]);
    *ltp ^= dec.decode(cx.get_mut(slot));
    *ltp == 1
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
    use crate::filters::ccitt::testing::{bitmap_from_rows, encode_g4};
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
        decode_generic_region(
            &mut dec,
            &mut cx,
            &mut Budget::new(),
            bm.width(),
            bm.height(),
            params,
            None,
        )
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
        let out = decode_generic_region(
            &mut dec,
            &mut cx,
            &mut Budget::new(),
            24,
            12,
            &params,
            Some(&skip),
        )
        .expect("decode");
        for y in 0..12u32 {
            assert_eq!(out.row(y), source.row(y), "row {y}");
        }
    }

    #[test]
    fn a_zero_sized_region_decodes_to_an_empty_bitmap() {
        let params = GenericParams::nominal(0);
        let mut dec = MqDecoder::new(&[]);
        let mut cx = MqContexts::new(GB_CONTEXT_LEN);
        let out = decode_generic_region(&mut dec, &mut cx, &mut Budget::new(), 0, 0, &params, None)
            .expect("decode");
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
                let out = decode_generic_region(
                    &mut dec,
                    &mut cx,
                    &mut Budget::new(),
                    64,
                    64,
                    &params,
                    None,
                )
                .expect("decode must not fail on garbage, only produce garbage");
                assert_eq!((out.width(), out.height()), (64, 64));
            }
        }
    }

    /// Decodes with a fresh decoder, context array and full budget, so the
    /// refusal tests below read as the dimensions they are about.
    fn decode_dimensions(
        width: u32,
        height: u32,
        params: &GenericParams,
    ) -> Result<Bitmap, Jbig2Error> {
        let mut dec = MqDecoder::new(&[]);
        let mut cx = MqContexts::new(GB_CONTEXT_LEN);
        decode_generic_region(
            &mut dec,
            &mut cx,
            &mut Budget::new(),
            width,
            height,
            params,
            None,
        )
    }

    /// The two ceilings are distinct and both hold. A region just past the
    /// allocation cap is refused for its size; one far past it is refused for
    /// its cost, before a byte is reserved either way.
    #[test]
    fn an_oversized_region_is_refused() {
        let params = GenericParams::nominal(0);
        // 8192 x 16385 is one row more than MAX_PIXELS: too big to allocate,
        // but still affordable, so the allocation cap is what catches it.
        assert_eq!(
            decode_dimensions(8192, 16385, &params),
            Err(Jbig2Error::TooLarge {
                width: 8192,
                height: 16385,
            }),
        );
        assert_eq!(
            decode_dimensions(u32::MAX, u32::MAX, &params),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// A region no pixels wide allocates nothing whatever its height, so the
    /// allocation cap never sees it — yet the decoding procedure still makes a
    /// pass over every row it declares. The budget is what charges for those
    /// rows, and it must do so on both decoding paths and with typical
    /// prediction either way, since each is a separate row loop.
    #[test]
    fn a_narrow_region_cannot_declare_unbounded_rows() {
        for width in [0u32, 1, 2] {
            for tpgdon in [false, true] {
                let mut nominal = GenericParams::nominal(0);
                nominal.tpgdon = tpgdon;
                assert_eq!(
                    decode_dimensions(width, u32::MAX, &nominal),
                    Err(Jbig2Error::WorkLimit),
                    "windowed path, width {width}, tpgdon {tpgdon}",
                );

                // A relocated AT pixel selects the general path instead.
                let mut relocated = nominal;
                relocated.at[0] = (-2, 0);
                assert!(!relocated.is_nominal());
                assert_eq!(
                    decode_dimensions(width, u32::MAX, &relocated),
                    Err(Jbig2Error::WorkLimit),
                    "general path, width {width}, tpgdon {tpgdon}",
                );
            }
        }
    }

    /// The charge is spent before the loop is entered, so a refused region
    /// leaves the arithmetic decoder untouched — nothing was decoded to reach
    /// the refusal.
    #[test]
    fn a_refused_region_consumes_no_coded_data() {
        let params = GenericParams::nominal(0);
        let coded = [0x55u8; 32];
        let mut dec = MqDecoder::new(&coded);
        let mut cx = MqContexts::new(GB_CONTEXT_LEN);
        let mut budget = Budget::new();
        assert_eq!(
            decode_generic_region(&mut dec, &mut cx, &mut budget, 0, u32::MAX, &params, None),
            Err(Jbig2Error::WorkLimit),
        );
        assert_eq!(budget, Budget::new(), "a refused region spends nothing");

        // And the refusal left the decoder and the contexts exactly where they
        // started: a region decoded through them now yields what a decoder
        // that never saw the refused region yields.
        let after = decode_generic_region(&mut dec, &mut cx, &mut budget, 8, 4, &params, None)
            .expect("8x4");
        let mut fresh_dec = MqDecoder::new(&coded);
        let mut fresh_cx = MqContexts::new(GB_CONTEXT_LEN);
        let expected = decode_generic_region(
            &mut fresh_dec,
            &mut fresh_cx,
            &mut Budget::new(),
            8,
            4,
            &params,
            None,
        )
        .expect("8x4");
        assert_eq!(after, expected);
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

    /// The windowed path is only allowed to exist if it is indistinguishable
    /// from the general one. Check every pixel of a bitmap wide and tall
    /// enough to exercise both edges and the interior, for all four templates.
    #[test]
    fn windowed_contexts_match_the_general_path_exactly() {
        for seed in [0x1u32, 0xFFFF, 0xA5A5_5A5A] {
            let bm = pseudo_random_bitmap(41, 17, seed);
            for template in 0..4u8 {
                let params = GenericParams::nominal(template);
                for y in 0..bm.height() {
                    let mut win = ContextWindows::start(&bm, y, template);
                    for x in 0..bm.width() {
                        assert_eq!(
                            win.value(),
                            context_at(&bm, x, y, &params),
                            "template {template} at ({x}, {y}), seed {seed:#x}",
                        );
                        let pixel = bm.get(i64::from(x), i64::from(y));
                        win.advance(&bm, x, pixel);
                    }
                }
            }
        }
    }

    /// Narrow and short bitmaps are where a window's initial fill is most
    /// likely to be wrong, because the whole row is edge.
    #[test]
    fn windowed_contexts_match_on_degenerate_shapes() {
        for (w, h) in [(1u32, 1u32), (1, 9), (9, 1), (2, 2), (3, 8), (8, 3)] {
            let bm = pseudo_random_bitmap(w, h, w * 31 + h);
            for template in 0..4u8 {
                let params = GenericParams::nominal(template);
                for y in 0..h {
                    let mut win = ContextWindows::start(&bm, y, template);
                    for x in 0..w {
                        assert_eq!(
                            win.value(),
                            context_at(&bm, x, y, &params),
                            "{w}x{h} template {template} at ({x}, {y})",
                        );
                        win.advance(&bm, x, bm.get(i64::from(x), i64::from(y)));
                    }
                }
            }
        }
    }

    /// The worked example the window spans were derived against: for template
    /// 0 at (4, 3) the three windows hold 0b10100, 0b1001100 and 0b0010, and
    /// they assemble to the hand-computed 0xA4C2.
    #[test]
    fn the_windows_assemble_the_hand_computed_context() {
        let bm = subject();
        let mut win = ContextWindows::start(&bm, 3, 0);
        for x in 0..4u32 {
            win.advance(&bm, x, bm.get(i64::from(x), 3));
        }
        assert_eq!(win.words, [0b10100, 0b100_1100, 0b0010]);
        assert_eq!(win.value(), 0xA4C2);
    }

    /// The dispatch itself: nominal parameters must take the fast path,
    /// anything else must not.
    #[test]
    fn is_nominal_recognises_exactly_the_default_at_values() {
        for template in 0..4u8 {
            assert!(GenericParams::nominal(template).is_nominal());
        }
        // Template 0 checks all four slots.
        let mut p = GenericParams::nominal(0);
        p.at[3] = (-1, -2);
        assert!(!p.is_nominal());
        // Templates 1 to 3 use only A1, so the unused slots must not matter.
        let mut p = GenericParams::nominal(2);
        p.at[1] = (7, 7);
        p.at[2] = (-7, -7);
        p.at[3] = (0, 0);
        assert!(
            p.is_nominal(),
            "unused AT slots must not defeat the fast path"
        );
        p.at[0] = (1, -1);
        assert!(!p.is_nominal());
    }

    /// A template outside 0 to 3 has no window geometry, so it must never
    /// reach the windowed path however its AT slots are set.
    #[test]
    fn an_undefined_template_is_never_nominal() {
        for template in 4..=u8::MAX {
            let params = GenericParams {
                template,
                at: NOMINAL_AT[0],
                tpgdon: false,
            };
            assert!(!params.is_nominal(), "template {template}");
        }
    }

    /// And the end-to-end consequence: both paths decode the same stream to
    /// the same pixels. The relocated-AT case above already covers the general
    /// path on its own; this pins that the two agree on one input.
    #[test]
    fn both_paths_decode_the_same_stream_identically() {
        let bm = pseudo_random_bitmap(53, 29, 0xC0FFEE);
        for template in 0..4u8 {
            let params = GenericParams::nominal(template);
            let coded = encode(&bm, &params, None);

            let mut dec = MqDecoder::new(&coded);
            let mut cx = MqContexts::new(GB_CONTEXT_LEN);
            let fast =
                decode_generic_region(&mut dec, &mut cx, &mut Budget::new(), 53, 29, &params, None)
                    .expect("fast path");

            let mut dec = MqDecoder::new(&coded);
            let mut cx = MqContexts::new(GB_CONTEXT_LEN);
            let slow = decode_generic_region_general(
                &mut dec,
                &mut cx,
                &mut Budget::new(),
                53,
                29,
                &params,
                None,
            )
            .expect("general path");

            for y in 0..29u32 {
                assert_eq!(fast.row(y), slow.row(y), "template {template}, row {y}");
            }
        }
    }

    /// Typical prediction and a skip mask both alter which pixels the windowed
    /// loop decodes, so the two paths have to agree with those in play too.
    #[test]
    fn both_paths_agree_with_typical_prediction_and_a_skip_mask() {
        let seed = pseudo_random_bitmap(23, 4, 0xBEEF);
        let mut source = Bitmap::new(23, 18).expect("23x18");
        for y in 0..18u32 {
            let src = if (5..11).contains(&y) { 4 } else { y % 4 };
            for x in 0..23 {
                source.set(x, y, seed.get(i64::from(x), i64::from(src)));
            }
        }
        let mut skip = Bitmap::new(23, 18).expect("23x18");
        for y in 0..18u32 {
            for x in 0..23u32 {
                skip.set(x, y, u8::from((x + y) % 5 == 0));
            }
        }
        for y in 0..18u32 {
            for x in 0..23u32 {
                if skip.get(i64::from(x), i64::from(y)) == 1 {
                    source.set(x, y, 0);
                }
            }
        }

        for template in 0..4u8 {
            let mut params = GenericParams::nominal(template);
            params.tpgdon = true;
            let coded = encode(&source, &params, Some(&skip));

            let mut dec = MqDecoder::new(&coded);
            let mut cx = MqContexts::new(GB_CONTEXT_LEN);
            let fast = decode_generic_region(
                &mut dec,
                &mut cx,
                &mut Budget::new(),
                23,
                18,
                &params,
                Some(&skip),
            )
            .expect("fast path");

            let mut dec = MqDecoder::new(&coded);
            let mut cx = MqContexts::new(GB_CONTEXT_LEN);
            let slow = decode_generic_region_general(
                &mut dec,
                &mut cx,
                &mut Budget::new(),
                23,
                18,
                &params,
                Some(&skip),
            )
            .expect("general path");

            for y in 0..18u32 {
                assert_eq!(fast.row(y), source.row(y), "template {template}, row {y}");
                assert_eq!(slow.row(y), source.row(y), "template {template}, row {y}");
            }
        }
    }

    /// A JBIG2 MMR region is a pure two-dimensional facsimile stream: no
    /// end-of-line patterns, no byte alignment, and the region's own width and
    /// height as the row width and the row count (T.88 6.2.6).
    #[test]
    fn an_mmr_region_decodes() {
        let bm = bitmap_from_rows(&[
            "0000000000",
            "0011111000",
            "0011111000",
            "0000110000",
            "1111111111",
            "0000000000",
        ]);
        let out = decode_mmr_region(&encode_g4(&bm), &mut Budget::new(), bm.width(), bm.height())
            .expect("mmr region");
        assert_eq!(out, bm);
    }

    /// The MMR flag selects a different *coding* of a region, not a different
    /// image and not a different polarity: a set pixel is ink under both. So
    /// the two decoders must agree bit for bit. The inversion that reconciles
    /// JBIG2's convention with `/DeviceGray` happens once, at the filter
    /// boundary, and a second one here would show up as these two disagreeing.
    #[test]
    fn mmr_and_arithmetic_coding_of_one_image_agree() {
        let bm = pseudo_random_bitmap(43, 21, 0x2468);
        let arithmetic = round_trip(&bm, &GenericParams::nominal(0));
        let mmr = decode_mmr_region(&encode_g4(&bm), &mut Budget::new(), bm.width(), bm.height())
            .expect("mmr region");
        assert_eq!(mmr, arithmetic);
        assert_eq!(mmr, bm, "and both are the image that was coded");
    }

    /// The shared work budget is charged from the declared dimensions before
    /// any decoding, exactly as on the arithmetic paths — a region no pixels
    /// wide allocates nothing whatever its height, so the allocation cap never
    /// sees it.
    #[test]
    fn an_mmr_region_cannot_declare_unbounded_rows() {
        for width in [0u32, 1, 2] {
            let mut budget = Budget::new();
            assert_eq!(
                decode_mmr_region(&[0xFFu8; 64], &mut budget, width, u32::MAX),
                Err(Jbig2Error::WorkLimit),
                "width {width}",
            );
            assert_eq!(budget, Budget::new(), "a refused region spends nothing");
        }
    }

    /// The allocation cap is the second ceiling and still applies: a region the
    /// budget can afford but no bitmap can hold is refused for its size.
    #[test]
    fn an_oversized_mmr_region_is_refused() {
        // 8192 x 16385 is one row more than MAX_PIXELS, and costs about half
        // the budget, so the size is what catches it.
        assert_eq!(
            decode_mmr_region(&[], &mut Budget::new(), 8192, 16385),
            Err(Jbig2Error::TooLarge {
                width: 8192,
                height: 16385,
            }),
        );
    }

    /// A region declaring no rows is refused here rather than handed to the
    /// codec, where a row count of 0 means "as many rows as the data holds"
    /// (ISO 32000-1 Table 11). Rows counted from the data are rows the budget
    /// charge — taken from the declared height, and so zero — never paid for,
    /// which is exactly the shape of bypass this module has shipped twice
    /// before.
    #[test]
    fn an_mmr_region_of_no_rows_is_refused() {
        let bm = bitmap_from_rows(&["0011", "1100", "0110"]);
        let mut budget = Budget::new();
        assert_eq!(
            decode_mmr_region(&encode_g4(&bm), &mut budget, bm.width(), 0),
            Err(Jbig2Error::Malformed("MMR region of no rows")),
        );
        assert_eq!(budget, Budget::new(), "nothing was decoded to reach it");
    }

    /// What the refusal above is protecting against, stated as a fact about the
    /// codec rather than as a comment on it: asked for zero rows, the facsimile
    /// decoder counts them out of the data. Forwarding a declared height of
    /// zero would therefore turn a region charged for no rows at all into as
    /// many rows as its bytes could describe.
    #[test]
    fn the_facsimile_codec_reads_a_zero_row_count_as_count_them_yourself() {
        let bm = bitmap_from_rows(&["1010", "0101", "1100", "0011", "1111"]);
        let counted = facsimile::decode(
            &encode_g4(&bm),
            &facsimile::Params {
                columns: bm.width(),
                rows: 0,
                k: -1,
                end_of_line: false,
                byte_align: false,
            },
        )
        .expect("facsimile");
        assert_eq!(
            counted.height(),
            bm.height(),
            "the row count came from the data, not from the parameter",
        );
    }

    /// A region no pixels wide is refused as well, rather than decoding rows
    /// that hold nothing.
    #[test]
    fn an_mmr_region_of_no_columns_is_refused() {
        assert!(matches!(
            decode_mmr_region(&[0xFFu8; 32], &mut Budget::new(), 0, 4),
            Err(Jbig2Error::Malformed(_)),
        ));
    }

    /// Region data is attacker-controlled, so arbitrary bytes under a plausible
    /// declared size must yield pixels or an error, never a panic or a hang.
    #[test]
    fn arbitrary_mmr_bytes_decode_without_hanging() {
        let mut state: u32 = 0xC0FF_EE01;
        for _ in 0..500 {
            let len = (state % 97) as usize;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            let _ = decode_mmr_region(&data, &mut Budget::new(), 32, 16);
        }
    }
}
