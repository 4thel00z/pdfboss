//! Row decoding: the changing-element model of ITU-T T.4 §4.2.1.3 and the
//! two-dimensional modes of ITU-T T.6 §2.2.
//!
//! A row is decoded into a **changing-element list** — the ascending positions
//! at which the row's colour flips, starting from white — rather than straight
//! into pixels. Every coding decision on the next row is expressed relative to
//! the two changing elements `b1` and `b2` on this one, so keeping the row in
//! that form makes finding them a search over a short list instead of a scan
//! over every pixel. The list becomes pixels once, when the row is finished.
//!
//! # Polarity
//!
//! Nothing in T.4 or T.6 is written in terms of bit values: the two colours are
//! *white* and *black*, and which bit stands for which is the caller's
//! business. This module resolves that once, here, and never again: **a set
//! pixel in the decoded bitmap is black**, matching the JBIG2 convention that a
//! 1 is foreground. `/BlackIs1` and the `/DeviceGray` sample convention are
//! reconciled at the PDF filter boundary by choosing between the bitmap's
//! packing and its inverting packer; no code between here and there needs to
//! know which the caller asked for.

use super::bits::BitReader;
use super::codes::{read_mode, read_run, Mode, WINDOW_BITS};
use super::CcittError;
use crate::filters::jbig2::bitmap::Bitmap;

/// Where `a0` sits before the first changing element of a row.
///
/// Not 0. T.4 §4.2.1.3 puts the reference position on an imaginary white
/// element *just before* the row, so that a row beginning with a black pixel
/// has its first changing element at column 0 and that element is still
/// strictly to the right of `a0`. Starting at 0 instead loses it, and every run
/// of every row comes out one pixel short.
const BEFORE_ROW: i64 = -1;

/// How a facsimile stream is laid out, from ISO 32000-1 Table 11.
///
/// The JBIG2 use of this codec (ITU-T T.88 §6.2.6) is one particular setting of
/// these: `k` below zero, no end-of-line patterns, no byte alignment, and the
/// region's width and height as `columns` and `rows`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Params {
    /// Pixels per row. Zero cannot describe an image and is refused.
    pub(crate) columns: u32,
    /// Rows in the image, or 0 for "as many as the data holds".
    pub(crate) rows: u32,
    /// Below zero selects pure two-dimensional coding; 0 pure
    /// one-dimensional; above zero a mixture, each row carrying a bit that
    /// says which it is.
    pub(crate) k: i32,
    /// Whether rows are separated by end-of-line patterns.
    pub(crate) end_of_line: bool,
    /// Whether each row starts on a byte boundary.
    pub(crate) byte_align: bool,
}

/// Decodes a facsimile stream into a bitmap in which a set pixel is black.
///
/// Truncation is not an error. A stream that stops in the middle yields the
/// rows it did decode and white ones after them, because a page that is mostly
/// readable is worth more than an error — real scanners produce such files. A
/// bit pattern that is *not* the end of the data but matches no code is
/// corruption, and is reported: guessing at it would displace every pixel
/// below.
pub(crate) fn decode(data: &[u8], params: &Params) -> Result<Bitmap, CcittError> {
    if params.columns == 0 {
        return Err(CcittError::BadParameter("an image with no columns"));
    }
    if params.rows == 0 {
        return Err(CcittError::Unimplemented("a stream of unstated length"));
    }
    if params.k >= 0 {
        return Err(CcittError::Unimplemented("one-dimensional coding"));
    }
    if params.end_of_line {
        return Err(CcittError::Unimplemented("end-of-line delimited rows"));
    }
    if params.byte_align {
        return Err(CcittError::Unimplemented("byte-aligned rows"));
    }

    // Allocated before anything is decoded, and from the declared dimensions,
    // so an image too large to hold is refused without a bit being read. It is
    // also what bounds the outer loop: with at least one column, the pixel cap
    // the allocation applies is a bound on the row count too.
    let mut out = Bitmap::new(params.columns, params.rows).map_err(|_| CcittError::TooLarge {
        width: params.columns,
        height: params.rows,
    })?;

    let mut r = BitReader::new(data);
    // Two lists, swapped each row: the row just finished becomes the reference
    // for the next one. Row 0's reference is empty, which is the all-white row
    // T.6 §2.2 imagines above the image.
    let mut reference: Vec<u32> = Vec::new();
    let mut coding: Vec<u32> = Vec::new();
    for y in 0..params.rows {
        if r.is_exhausted() {
            break;
        }
        match decode_row_2d(&mut r, &reference, params.columns, &mut coding) {
            Ok(()) => {}
            // A failure with less than a full code window left is the data
            // running out mid-code, not a stream that is wrong. The rows
            // already decoded stand; the partial one is dropped.
            Err(_) if r.remaining() < WINDOW_BITS as usize => break,
            Err(err) => return Err(err),
        }
        paint_row(&mut out, y, &coding);
        std::mem::swap(&mut reference, &mut coding);
    }
    Ok(out)
}

/// Decodes one two-dimensionally coded row into `out` as a changing-element
/// list (T.6 §2.2).
///
/// `reference` is the previous row in the same form: ascending positions, none
/// past `columns`, the first of them beginning a black run. Both properties are
/// established by this function for the row it writes, so they hold for the row
/// that reads it.
fn decode_row_2d(
    r: &mut BitReader,
    reference: &[u32],
    columns: u32,
    out: &mut Vec<u32>,
) -> Result<(), CcittError> {
    out.clear();
    let mut a0: i64 = BEFORE_ROW;
    let mut white = true;
    // A backstop that does not depend on the advance check below being right:
    // `a0` starts before the row and every mode is required to increase it, so
    // no row can need more decisions than it has columns.
    let mut steps: u32 = 0;
    while a0 < i64::from(columns) {
        steps = steps.saturating_add(1);
        if steps > columns.saturating_add(2) {
            return Err(CcittError::Malformed("a row that never reaches its end"));
        }

        let index = find_b1(reference, a0, white);
        let b1 = reference_at(reference, index, columns);
        let b2 = reference_at(reference, index.saturating_add(1), columns);

        let next_a0 = match read_mode(r)? {
            // The run continues past b2: no changing element is recorded and
            // the colour does not flip. Recording one here would corrupt every
            // row below, since this row is the next one's reference.
            Mode::Pass => i64::from(b2),
            Mode::Horizontal => {
                // Measured from the start of the row while a0 is still on the
                // imaginary element before it: a run starting there would come
                // out one pixel too long.
                let start = a0.clamp(0, i64::from(columns)) as u32;
                let first = read_run(r, white)?;
                let second = read_run(r, !white)?;
                let a1 = start.checked_add(first).ok_or(CcittError::RunTooLong)?;
                let a2 = a1.checked_add(second).ok_or(CcittError::RunTooLong)?;
                // a1 <= a2, so one test covers both ends of the pair.
                if a2 > columns {
                    return Err(CcittError::RunTooLong);
                }
                push_change(out, a1);
                push_change(out, a2);
                i64::from(a2)
            }
            Mode::Vertical(delta) => {
                let a1 = i64::from(b1) + i64::from(delta);
                if a1 < 0 || a1 > i64::from(columns) {
                    return Err(CcittError::RunTooLong);
                }
                push_change(out, a1 as u32);
                white = !white;
                a1
            }
        };

        if next_a0 <= a0 {
            return Err(CcittError::Malformed("a coding mode that does not advance"));
        }
        a0 = next_a0;
    }
    Ok(())
}

/// The index into `reference` of `b1`: the first changing element strictly
/// right of `a0` whose colour is **opposite** to the current colour (T.4
/// §4.2.1.3).
///
/// The opposite-colour qualifier is the whole difficulty of this function, and
/// dropping it produces output that is plausible for simple images and wrong
/// for real ones. It costs nothing to honour here, because changing elements
/// alternate colour: the list starts from white, so the element at an even
/// index begins a black run and the one at an odd index begins a white run.
/// "Opposite to the current colour" is therefore a parity test on the index,
/// which is the reason a row is kept as positions rather than as pixels.
///
/// The index returned may be past the end of the list; [`reference_at`] turns
/// that into the row width, which is what ends the last run of a row.
fn find_b1(reference: &[u32], a0: i64, white: bool) -> usize {
    // The list ascends, so the elements at or left of a0 are a prefix of it.
    let mut index = reference.partition_point(|position| i64::from(*position) <= a0);
    if (index % 2 == 0) != white {
        index = index.saturating_add(1);
    }
    index
}

/// The reference row's changing element at `index`, or the row width when
/// there is none.
///
/// Saturating at the width is deliberate rather than a bounds accident: it is
/// what terminates the last run of every row, and what lets an all-white
/// reference row be the empty list.
fn reference_at(reference: &[u32], index: usize, columns: u32) -> u32 {
    reference.get(index).copied().unwrap_or(columns)
}

/// Records a changing element, cancelling it against an identical one already
/// at the end of the list.
///
/// The list is a list of colour *toggles*, so two at one position are no toggle
/// at all — a zero-length run changes no pixel. T.4 §4.2.1.3 defines a changing
/// element by the pixels of the line rather than by the codes that produced it,
/// so cancelling is what the definition asks for, and it keeps the list
/// strictly ascending, which is what the parity test in [`find_b1`] rests on.
fn push_change(out: &mut Vec<u32>, position: u32) {
    if out.last() == Some(&position) {
        out.pop();
    } else {
        out.push(position);
    }
}

/// Paints a finished row's changing-element list into the bitmap.
///
/// The list starts from white, so the span from an even-indexed element to the
/// next one is black and the span after an odd-indexed one is white. An
/// odd-length list leaves its last black run open to the end of the row. The
/// bitmap arrives white, so only the black spans are written.
fn paint_row(out: &mut Bitmap, y: u32, changes: &[u32]) {
    let columns = out.width();
    for pair in changes.chunks(2) {
        let Some(&start) = pair.first() else {
            continue;
        };
        let end = pair.get(1).copied().unwrap_or(columns).min(columns);
        for x in start..end {
            out.set(x, y, 1);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::ccitt::testing::{
        bitmap_from_rows, encode_g4, encode_g4_tallied, pack, push_mode, push_run,
    };

    /// Asserts that decoding `data` reproduces `bm` exactly.
    fn assert_round_trip(bm: &Bitmap, data: &[u8]) {
        let params = Params {
            columns: bm.width(),
            rows: bm.height(),
            k: -1,
            end_of_line: false,
            byte_align: false,
        };
        let out = decode(data, &params).expect("decode");
        assert_eq!((out.width(), out.height()), (bm.width(), bm.height()));
        for y in 0..bm.height() {
            assert_eq!(out.row(y), bm.row(y), "row {y}");
        }
    }

    /// Pure two-dimensional parameters for a stream of the given shape.
    fn g4(columns: u32, rows: u32) -> Params {
        Params {
            columns,
            rows,
            k: -1,
            end_of_line: false,
            byte_align: false,
        }
    }

    #[test]
    fn round_trips_a_simple_image() {
        let bm = bitmap_from_rows(&[
            "0000000000",
            "0011111000",
            "0011111000",
            "0000110000",
            "1111111111",
            "0000000000",
        ]);
        assert_round_trip(&bm, &encode_g4(&bm));
    }

    /// The first row is coded against an imaginary all-white row above it, so
    /// an image whose very first pixel is black is the case most likely to
    /// expose an `a0` started at 0 instead of before the row.
    #[test]
    fn a_row_starting_black_round_trips() {
        let bm = bitmap_from_rows(&["1110000111", "1111111111", "1000000001"]);
        assert_round_trip(&bm, &encode_g4(&bm));
    }

    /// Pass mode arises only when a run on the reference line ends before the
    /// coding line's next change. A wide black bar above an empty row forces
    /// it — and the tally confirms the fixture really does.
    #[test]
    fn pass_mode_round_trips() {
        let bm = bitmap_from_rows(&["0111111110", "0000000000", "0111111110", "0000110000"]);
        let (data, tally) = encode_g4_tallied(&bm);
        assert!(tally.pass > 0, "fixture never reaches pass mode: {tally:?}");
        assert_round_trip(&bm, &data);
    }

    /// Horizontal mode is chosen when the vertical offset would exceed 3, so a
    /// row whose edges jump far from the row above exercises it.
    #[test]
    fn horizontal_mode_round_trips() {
        let bm = bitmap_from_rows(&["1100000000000000", "0000000000000011", "0000011111000000"]);
        let (data, tally) = encode_g4_tallied(&bm);
        assert!(
            tally.horizontal > 0,
            "fixture never reaches horizontal mode: {tally:?}",
        );
        assert_round_trip(&bm, &data);
    }

    /// Every vertical offset from V(L3) to V(R3), by walking one edge of a bar
    /// left and right by one, two and three pixels a row while the other edge
    /// stays put.
    #[test]
    fn every_vertical_offset_round_trips() {
        let mut rows: Vec<String> = Vec::new();
        let mut left = 10usize;
        for delta in [0i32, 1, 2, 3, -1, -2, -3] {
            left = (left as i32 + delta) as usize;
            let mut row = String::new();
            row.push_str(&"0".repeat(left));
            row.push_str(&"1".repeat(20 - left));
            rows.push(row);
        }
        let spec: Vec<&str> = rows.iter().map(String::as_str).collect();
        let bm = bitmap_from_rows(&spec);
        let (data, tally) = encode_g4_tallied(&bm);
        for delta in -3..=3i8 {
            assert!(
                tally.vertical_offsets[(delta + 3) as usize] > 0,
                "fixture never codes V({delta}): {tally:?}",
            );
        }
        assert_round_trip(&bm, &data);
    }

    /// Noise is the worst case for two-dimensional coding and the best case for
    /// finding an edge in the changing-element bookkeeping: rows disagree in
    /// colour at the same position constantly, which is the only way a `b1`
    /// search that ignores the opposite-colour qualifier shows itself.
    #[test]
    fn round_trips_a_pseudo_random_image() {
        let mut state = 0x1234_5678u32;
        let mut bm = Bitmap::new(61, 37).expect("bitmap");
        for y in 0..37 {
            for x in 0..61 {
                state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                bm.set(x, y, u8::from((state >> 24) & 1 == 1));
            }
        }
        let (data, tally) = encode_g4_tallied(&bm);
        assert!(
            tally.pass > 0 && tally.horizontal > 0 && tally.vertical > 0,
            "{tally:?}"
        );
        assert_round_trip(&bm, &data);
    }

    /// A truncated stream yields the rows it did decode. Real scanners produce
    /// these, and a hard error would lose a page that is 95% readable.
    #[test]
    fn a_truncated_stream_keeps_the_rows_it_decoded() {
        let bm = bitmap_from_rows(&["0011110000"; 8]);
        let data = encode_g4(&bm);
        let out = decode(&data[..data.len() / 2], &g4(10, 8)).expect("truncation is not an error");
        assert_eq!((out.width(), out.height()), (10, 8));
        assert_eq!(out.row(0), bm.row(0), "the first row survived");
        assert_eq!(out.row(7), [0; 10], "the rows past the data are white");
    }

    /// A run that would overflow the row is corruption, not a long run.
    #[test]
    fn a_run_past_the_row_end_is_rejected() {
        // A make-up of 1728 cannot be honoured in a row of 10 columns.
        let mut bits = Vec::new();
        push_mode(&mut bits, Mode::Horizontal);
        push_run(&mut bits, true, 1728);
        push_run(&mut bits, false, 0);
        // Enough data behind the failure that it cannot be read as the stream
        // simply having stopped, which is not an error.
        bits.extend(std::iter::repeat_n(0u8, 32));
        let data = pack(&bits);
        assert_eq!(decode(&data, &g4(10, 1)), Err(CcittError::RunTooLong));
    }

    /// A vertical offset may place the next changing element before the start
    /// of the row, which no row can contain.
    #[test]
    fn a_vertical_offset_before_the_start_of_the_row_is_rejected() {
        let mut bits = Vec::new();
        // Row 0: an all-black row, so row 1's reference has a changing element
        // at column 0 for V(L3) to be measured against.
        push_mode(&mut bits, Mode::Horizontal);
        push_run(&mut bits, true, 0);
        push_run(&mut bits, false, 10);
        // Row 1: three pixels left of column 0.
        push_mode(&mut bits, Mode::Vertical(-3));
        // Enough data behind the failure that it cannot be read as truncation.
        bits.extend(std::iter::repeat_n(0u8, 32));
        let data = pack(&bits);
        assert_eq!(decode(&data, &g4(10, 2)), Err(CcittError::RunTooLong));
    }

    /// A mode that leaves `a0` where it was would be decoded forever. The row
    /// loop refuses it rather than spinning on it.
    #[test]
    fn a_mode_that_does_not_advance_the_row_is_refused() {
        let mut bits = Vec::new();
        // Two horizontal modes carrying nothing but zero-length runs. The first
        // is legal — it moves `a0` from before the row onto column 0 — and the
        // second cannot move it anywhere.
        for _ in 0..2 {
            push_mode(&mut bits, Mode::Horizontal);
            push_run(&mut bits, true, 0);
            push_run(&mut bits, false, 0);
        }
        bits.extend(std::iter::repeat_n(0u8, 32));
        let data = pack(&bits);
        assert!(matches!(
            decode(&data, &g4(10, 1)),
            Err(CcittError::Malformed(_)),
        ));
    }

    #[test]
    fn zero_columns_is_a_parameter_error() {
        assert!(matches!(
            decode(&[], &g4(0, 4)),
            Err(CcittError::BadParameter(_)),
        ));
    }

    /// An image too large to allocate is refused before anything is decoded,
    /// and the dimensions that were refused are reported.
    #[test]
    fn an_image_past_the_allocation_cap_is_refused() {
        assert_eq!(
            decode(&[], &g4(1 << 20, 1 << 12)),
            Err(CcittError::TooLarge {
                width: 1 << 20,
                height: 1 << 12,
            }),
        );
    }

    /// What this build reads is the pure two-dimensional form. The parameters
    /// that select the other forms are reported rather than ignored, because
    /// ignoring one decodes the wrong bits into a plausible-looking image.
    #[test]
    fn the_forms_this_build_does_not_decode_are_reported() {
        let cases = [
            Params { k: 0, ..g4(10, 4) },
            Params { k: 4, ..g4(10, 4) },
            Params {
                end_of_line: true,
                ..g4(10, 4)
            },
            Params {
                byte_align: true,
                ..g4(10, 4)
            },
            g4(10, 0),
        ];
        for params in cases {
            assert!(
                matches!(decode(&[], &params), Err(CcittError::Unimplemented(_))),
                "{params:?}",
            );
        }
    }

    /// Every byte string is either an image or an error, never a panic and
    /// never a hang.
    #[test]
    fn arbitrary_bytes_never_panic_or_hang() {
        let mut state = 0xFEED_FACEu32;
        for _ in 0..2_000 {
            let len = (state % 97) as usize;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            let outcome = decode(&data, &g4(32, 16));
            if let Ok(bm) = outcome {
                assert_eq!((bm.width(), bm.height()), (32, 16));
            }
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        }
    }
}
