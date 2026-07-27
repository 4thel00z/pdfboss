//! Fixture builders shared by the facsimile test modules. Test-only.
//!
//! A `#[cfg(test)] mod tests` is private to the module that declares it, so a
//! builder written inside one cannot be called from another's. The code-table
//! writers below are wanted by the table tests, by the row decoder's tests and
//! by the callers that wrap the codec, so they live here in one copy.
//!
//! The centrepiece is [`encode_g4`], a miniature two-dimensional encoder. A
//! hand-written byte string is not a realistic sample of a T.6 stream — every
//! row is coded against the row above it, so the interesting cases only arise
//! from a whole image — and round-tripping a real image through an encoder is
//! the only way to reach them in quantity.
//!
//! **The encoder derives `b1` and `b2` from the reference row's pixels, by the
//! definition in ITU-T T.4 §4.2.1.3, and shares no code with the decoder's
//! changing-element search.** That is deliberate and load-bearing. Were both
//! sides to call one `find_b1`, a wrong answer would cancel out — the encoder
//! would code a vertical offset from the same wrong `b1` the decoder resolves
//! it against — and every round trip would pass while the codec disagreed with
//! every other implementation on earth. Two independent derivations of the same
//! quantity is what makes the round trip evidence of anything.

use super::codes::{
    Code, Mode, BLACK_CODES, EOL_BITS, EOL_LEN, EXT_MAKEUP_CODES, MODE_CODES, WHITE_CODES,
};
use crate::filters::jbig2::bitmap::Bitmap;

/// Builds a bitmap from rows of `'1'` (black, a set pixel) and anything else
/// (white).
///
/// Every row must be the same length as the first; a shorter one is a mistake
/// in the fixture rather than an image with a ragged edge.
pub(crate) fn bitmap_from_rows(rows: &[&str]) -> Bitmap {
    let height = rows.len() as u32;
    let width = match rows.first() {
        Some(row) => row.len() as u32,
        None => 0,
    };
    let mut bm = Bitmap::new(width, height).expect("fixture bitmaps are small");
    for (y, row) in rows.iter().enumerate() {
        assert_eq!(row.len() as u32, width, "row {y} is a different width");
        for (x, ch) in row.bytes().enumerate() {
            bm.set(x as u32, y as u32, u8::from(ch == b'1'));
        }
    }
    bm
}

/// The code for `run` in the white or black table (T.4 Tables 1 and 2).
pub(crate) fn lookup(white: bool, run: u16) -> Code {
    let table = if white { WHITE_CODES } else { BLACK_CODES };
    match table.iter().find(|c| c.run == run) {
        Some(code) => *code,
        None => panic!("no code for run {run}"),
    }
}

/// The extended make-up code for `run` (T.4 Table 3).
pub(crate) fn ext_lookup(run: u16) -> Code {
    match EXT_MAKEUP_CODES.iter().find(|c| c.run == run) {
        Some(code) => *code,
        None => panic!("no extended make-up for run {run}"),
    }
}

/// Appends `len` bits of `pattern`, most significant first, as one `u8` of
/// value 0 or 1 per bit.
///
/// Bits rather than packed bytes throughout the builders: a facsimile stream is
/// a bit stream, and packing once at the end ([`pack`]) is far less error-prone
/// than carrying a partial byte through every writer.
pub(crate) fn push_bits(bits: &mut Vec<u8>, pattern: u16, len: u8) {
    for i in (0..len).rev() {
        bits.push(((pattern >> i) & 1) as u8);
    }
}

/// Appends one variable-length code.
pub(crate) fn push_code(bits: &mut Vec<u8>, code: Code) {
    push_bits(bits, code.bits, code.len);
}

/// Appends one end-of-line pattern: eleven zero bits and a one (T.4 §4.1.1).
pub(crate) fn push_eol(bits: &mut Vec<u8>) {
    push_bits(bits, EOL_BITS, EOL_LEN);
}

/// Appends one two-dimensional mode code (T.6 §2.2, Table 4).
pub(crate) fn push_mode(bits: &mut Vec<u8>, mode: Mode) {
    match MODE_CODES.iter().find(|(_, _, m)| *m == mode) {
        Some((pattern, len, _)) => push_bits(bits, *pattern, *len),
        None => panic!("no code for {mode:?}"),
    }
}

/// Appends a complete run length: as many make-up codes as it takes, then the
/// terminating code for the remainder (T.4 §4.1.2).
pub(crate) fn push_run(bits: &mut Vec<u8>, white: bool, run: u32) {
    let mut remaining = run;
    while remaining >= 64 {
        // The largest make-up that does not overshoot. Every multiple of 64
        // from 64 to 2560 is assigned a code, so this always resolves.
        let makeup = ((remaining / 64) * 64).min(2560) as u16;
        if makeup > 1728 {
            push_code(bits, ext_lookup(makeup));
        } else {
            push_code(bits, lookup(white, makeup));
        }
        remaining -= u32::from(makeup);
    }
    push_code(bits, lookup(white, remaining as u16));
}

/// Packs a vector of 0/1 bytes into bytes, most significant bit first, zero
/// filling the last byte.
pub(crate) fn pack(bits: &[u8]) -> Vec<u8> {
    let mut out = vec![0u8; bits.len().div_ceil(8)];
    for (i, bit) in bits.iter().enumerate() {
        if *bit == 1 {
            if let Some(byte) = out.get_mut(i / 8) {
                *byte |= 1 << (7 - (i % 8));
            }
        }
    }
    out
}

/// How many times each of the three mode families was used to code an image.
///
/// A round-trip test proves only that the decoder undoes what the encoder did;
/// it says nothing about *which* modes were involved. An image chosen to force
/// pass mode is worthless as a test if the encoder happened to code it with
/// vertical modes throughout, so the tests assert against this tally to confirm
/// their fixture reaches the code path it was written for.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct ModeTally {
    /// Pass mode codes emitted.
    pub(crate) pass: usize,
    /// Horizontal mode codes emitted, each followed by two run lengths.
    pub(crate) horizontal: usize,
    /// Vertical mode codes emitted, of any offset.
    pub(crate) vertical: usize,
    /// Vertical mode codes emitted per offset, indexed by `delta + 3` so that
    /// V(L3) is entry 0 and V(R3) entry 6. All seven are separate codes and
    /// separate arithmetic in the decoder, so a fixture claiming to cover them
    /// can be held to it.
    pub(crate) vertical_offsets: [usize; 7],
}

/// Encodes a bitmap as a pure two-dimensional stream: no EOLs, no byte
/// alignment, no EOFB — the form ITU-T T.88 §6.2.6 puts inside a JBIG2 generic
/// region, and the form `/K` below zero selects in PDF.
pub(crate) fn encode_g4(bm: &Bitmap) -> Vec<u8> {
    encode_g4_tallied(bm).0
}

/// [`encode_g4`], reporting which modes it chose.
pub(crate) fn encode_g4_tallied(bm: &Bitmap) -> (Vec<u8>, ModeTally) {
    let mut bits = Vec::new();
    let mut tally = ModeTally::default();
    for y in 0..bm.height() {
        encode_row_2d(&mut bits, bm, y, &mut tally);
    }
    (pack(&bits), tally)
}

/// Codes one row against the row above it (T.6 §2.2).
///
/// The reference row for row 0 is the imaginary all-white row above the image,
/// which falls out of reading row `-1` of the bitmap: every pixel outside a
/// bitmap reads as white.
fn encode_row_2d(bits: &mut Vec<u8>, bm: &Bitmap, y: u32, tally: &mut ModeTally) {
    let columns = bm.width();
    let coding = i64::from(y);
    let reference = i64::from(y) - 1;
    let mut a0: i64 = -1;
    let mut white = true;
    while a0 < i64::from(columns) {
        let a1 = next_change(bm, coding, a0, columns);
        let a2 = next_change(bm, coding, i64::from(a1), columns);
        let b1 = opposite_change(bm, reference, a0, white, columns);
        let b2 = next_change(bm, reference, i64::from(b1), columns);

        if b2 < a1 {
            push_mode(bits, Mode::Pass);
            tally.pass += 1;
            a0 = i64::from(b2);
        } else if (i64::from(a1) - i64::from(b1)).abs() <= 3 {
            let delta = (i64::from(a1) - i64::from(b1)) as i8;
            push_mode(bits, Mode::Vertical(delta));
            tally.vertical += 1;
            if let Some(slot) = tally.vertical_offsets.get_mut((delta + 3) as usize) {
                *slot += 1;
            }
            a0 = i64::from(a1);
            white = !white;
        } else {
            push_mode(bits, Mode::Horizontal);
            tally.horizontal += 1;
            // The first run is measured from the start of the row when `a0` is
            // still on the imaginary element before it.
            let start = a0.max(0) as u32;
            push_run(bits, white, a1 - start);
            push_run(bits, !white, a2 - a1);
            a0 = i64::from(a2);
        }
    }
}

/// What a test stream puts around its rows.
///
/// The fields are the ISO 32000-1 Table 11 parameters that decide what sits
/// *between* rows, so a fixture is described in the same words the decoder's
/// parameters use, plus the two the encoder alone needs: how much fill to
/// write, and how a stream ends.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct Layout {
    /// Write an end-of-line pattern before every row (T.4 §4.1.1).
    pub(crate) end_of_line: bool,
    /// Pad with zero bits so every row begins on a byte boundary.
    pub(crate) byte_align: bool,
    /// Write a bit before every row saying whether that row is coded
    /// one-dimensionally (T.4 §4.2.3). This is what `/K` above zero selects.
    pub(crate) tagged: bool,
    /// Zero bits to write before each end-of-line pattern. T.4 §4.1.1 allows
    /// them so a row occupies a minimum transmission time; they carry nothing
    /// and a decoder must step over them.
    pub(crate) fill_bits: usize,
    /// End-of-line patterns to write after the last row. Two of them are the
    /// end-of-facsimile block of T.6 §2.2.1; six are T.4's return to control.
    pub(crate) trailing_eols: usize,
}

/// Encodes a bitmap row by row, each row coded as `one_dimensional` says.
///
/// `one_dimensional` is indexed by row; a row past its end is coded
/// one-dimensionally, so passing an empty slice yields a pure T.4 stream.
///
/// Every row is coded against the bitmap's own previous row, whichever way
/// that row was written, because that is what a decoder reconstructs — mixing
/// the two forms and still agreeing on the reference line is the whole point of
/// the per-row tag.
pub(crate) fn encode_g3(bm: &Bitmap, layout: Layout, one_dimensional: &[bool]) -> Vec<u8> {
    let mut bits = Vec::new();
    let mut tally = ModeTally::default();
    for y in 0..bm.height() {
        if layout.byte_align {
            while bits.len() % 8 != 0 {
                bits.push(0);
            }
        }
        if layout.end_of_line {
            bits.extend(std::iter::repeat_n(0u8, layout.fill_bits));
            push_eol(&mut bits);
        }
        let one_d = one_dimensional.get(y as usize).copied().unwrap_or(true);
        if layout.tagged {
            bits.push(u8::from(one_d));
        }
        if one_d {
            encode_row_1d(&mut bits, bm, y);
        } else {
            encode_row_2d(&mut bits, bm, y, &mut tally);
        }
    }
    for _ in 0..layout.trailing_eols {
        push_eol(&mut bits);
    }
    pack(&bits)
}

/// Encodes a bitmap as pure one-dimensional rows: no end-of-line patterns, no
/// byte alignment, no terminator — the form `/K` of 0 selects.
pub(crate) fn encode_g3_1d(bm: &Bitmap) -> Vec<u8> {
    encode_g3(bm, Layout::default(), &[])
}

/// [`encode_g3_1d`] with an end-of-line pattern before every row.
pub(crate) fn encode_g3_1d_with_eol(bm: &Bitmap) -> Vec<u8> {
    encode_g3(
        bm,
        Layout {
            end_of_line: true,
            ..Layout::default()
        },
        &[],
    )
}

/// [`encode_g3_1d`] with every row starting on a byte boundary.
pub(crate) fn encode_g3_1d_byte_aligned(bm: &Bitmap) -> Vec<u8> {
    encode_g3(
        bm,
        Layout {
            byte_align: true,
            ..Layout::default()
        },
        &[],
    )
}

/// Codes one row as the alternating run lengths of T.4 §4.1.2, white first.
///
/// The leading white run is written even when it is empty, which is how a row
/// beginning with a black pixel is coded and the case a decoder is most likely
/// to get wrong. Runs are derived from the pixels here rather than from the
/// decoder's changing-element list, for the same reason `b1` is: two
/// independent derivations are what make a round trip evidence of anything.
fn encode_row_1d(bits: &mut Vec<u8>, bm: &Bitmap, y: u32) {
    let columns = bm.width();
    let mut at: u32 = 0;
    let mut white = true;
    while at < columns {
        let mut run = 0u32;
        while at + run < columns && (bm.get(i64::from(at + run), i64::from(y)) == 0) == white {
            run += 1;
        }
        push_run(bits, white, run);
        at += run;
        white = !white;
    }
}

/// Whether the pixel at `x` on row `y` differs from the one to its left.
///
/// This is the definition of a changing element (T.4 §4.2.1.3). Column 0
/// compares against the imaginary white pixel that precedes every row, which
/// falls out of reading column `-1`.
fn is_change(bm: &Bitmap, y: i64, x: i64) -> bool {
    bm.get(x, y) != bm.get(x - 1, y)
}

/// The first changing element on row `y` strictly right of `after`, saturating
/// at `columns`.
fn next_change(bm: &Bitmap, y: i64, after: i64, columns: u32) -> u32 {
    let mut x = (after + 1).max(0);
    while x < i64::from(columns) {
        if is_change(bm, y, x) {
            return x as u32;
        }
        x += 1;
    }
    columns
}

/// `b1`: the first changing element on row `y` strictly right of `a0` whose
/// colour is opposite to the current colour (T.4 §4.2.1.3), saturating at
/// `columns`.
///
/// A changing element's colour is the colour of the pixel at it — the run it
/// begins. So with a white current colour the search wants an element that
/// begins a black run, and with a black one an element that begins a white run.
/// Dropping that qualifier yields an image that is plausible for simple
/// fixtures and wrong for real ones, which is exactly why this is derived here
/// from pixels rather than shared with the decoder.
fn opposite_change(bm: &Bitmap, y: i64, a0: i64, white: bool, columns: u32) -> u32 {
    let mut x = (a0 + 1).max(0);
    while x < i64::from(columns) {
        if is_change(bm, y, x) && (bm.get(x, y) == 1) == white {
            return x as u32;
        }
        x += 1;
    }
    columns
}
