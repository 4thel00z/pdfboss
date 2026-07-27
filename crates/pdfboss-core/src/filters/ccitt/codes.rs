//! The ITU-T T.4 run-length code tables and the ITU-T T.6 mode codes.
//!
//! A row of a bilevel image is a sequence of runs of alternating colour,
//! always beginning with a white run that may be empty. T.4 §4.1.2 writes each
//! run as an optional **make-up** code carrying a multiple of 64, followed by
//! a mandatory **terminating** code carrying the remainder 0..=63; the two add
//! to the run length, and a run past 2560 simply repeats make-ups. White and
//! black have separate tables (Tables 1 and 2) because the two colours have
//! very different length distributions in scanned text; the extended make-ups
//! from 1792 to 2560 (Table 3) are shared.
//!
//! These are assigned values, not computed ones, so they are transcribed
//! rather than derived. Every bit pattern below is written as a binary literal
//! with **exactly** as many digits as its `len`, leading zeros included, so
//! that a line can be read straight off against the published table. The
//! module's tests then check the transcription structurally — prefix freedom,
//! coverage of every run value, and the shape of the code space T.4 leaves
//! unused — which catches a mistyped bit without a second copy of the data to
//! compare against.
//!
//! The two-dimensional mode codes of T.6 §2.2 sit here too, since they share
//! the same peek-and-match lookup.

use super::bits::BitReader;
use super::CcittError;

/// A single variable-length code: `len` bits, right-aligned in `bits`, worth
/// `run` pixels.
///
/// The length has to be carried separately because the patterns have leading
/// zeros — `0000100` and `100` are different codes, and a bare integer would
/// conflate them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Code {
    /// The bit pattern, most significant bit first, right-aligned.
    pub(crate) bits: u16,
    /// How many bits of `bits` are significant; 1..=13.
    pub(crate) len: u8,
    /// The run length in pixels this code stands for.
    pub(crate) run: u16,
}

/// The width of the peek every run-code lookup takes.
///
/// Thirteen is the longest code T.4 assigns (the black extended make-ups), so
/// one peek this wide always contains whichever code comes next in full.
pub(crate) const WINDOW_BITS: u32 = 13;

/// The width of the peek every mode lookup takes (T.6 §2.2, Table 4).
const MODE_WINDOW_BITS: u32 = 7;

/// The end-of-line pattern: eleven zero bits and a one (T.4 §4.1.1).
///
/// It is not a run code and never appears in the tables below, but it shares
/// their code space — the whole reason T.4's code is incomplete is to keep any
/// sequence of runs from ever producing this pattern.
pub(crate) const EOL_BITS: u16 = 0b0_0000_0000_0001;

/// The length of [`EOL_BITS`], in bits.
pub(crate) const EOL_LEN: u8 = 12;

/// The widest run this module will accumulate, in pixels.
///
/// Nothing in T.4 §4.1.2 bounds how many make-up codes may precede a
/// terminating one, so an attacker-supplied chain of them would otherwise
/// accumulate until the accumulator wrapped. The real bound is the row width,
/// but the caller applies that, so the chain needs a ceiling here too. This is
/// the pixel cap on any bitmap this build will allocate, so a run that reaches
/// it cannot describe a row that could exist. It also makes the loop's
/// termination structural: every iteration either returns or adds at least 64.
const MAX_RUN: u32 = 1 << 27;

/// White run lengths 0..=63 (T.4 Table 1, terminating codes) followed by the
/// white make-up codes 64..=1728 (T.4 Table 2).
///
/// Laid out one code per line and left that way deliberately: the point of the
/// layout is that a reader can run a finger down it against the published
/// table, which a reflowed version would make impossible.
#[rustfmt::skip]
pub(crate) const WHITE_CODES: &[Code] = &[
    Code { bits: 0b00110101, len: 8, run: 0 },
    Code { bits: 0b000111, len: 6, run: 1 },
    Code { bits: 0b0111, len: 4, run: 2 },
    Code { bits: 0b1000, len: 4, run: 3 },
    Code { bits: 0b1011, len: 4, run: 4 },
    Code { bits: 0b1100, len: 4, run: 5 },
    Code { bits: 0b1110, len: 4, run: 6 },
    Code { bits: 0b1111, len: 4, run: 7 },
    Code { bits: 0b10011, len: 5, run: 8 },
    Code { bits: 0b10100, len: 5, run: 9 },
    Code { bits: 0b00111, len: 5, run: 10 },
    Code { bits: 0b01000, len: 5, run: 11 },
    Code { bits: 0b001000, len: 6, run: 12 },
    Code { bits: 0b000011, len: 6, run: 13 },
    Code { bits: 0b110100, len: 6, run: 14 },
    Code { bits: 0b110101, len: 6, run: 15 },
    Code { bits: 0b101010, len: 6, run: 16 },
    Code { bits: 0b101011, len: 6, run: 17 },
    Code { bits: 0b0100111, len: 7, run: 18 },
    Code { bits: 0b0001100, len: 7, run: 19 },
    Code { bits: 0b0001000, len: 7, run: 20 },
    Code { bits: 0b0010111, len: 7, run: 21 },
    Code { bits: 0b0000011, len: 7, run: 22 },
    Code { bits: 0b0000100, len: 7, run: 23 },
    Code { bits: 0b0101000, len: 7, run: 24 },
    Code { bits: 0b0101011, len: 7, run: 25 },
    Code { bits: 0b0010011, len: 7, run: 26 },
    Code { bits: 0b0100100, len: 7, run: 27 },
    Code { bits: 0b0011000, len: 7, run: 28 },
    Code { bits: 0b00000010, len: 8, run: 29 },
    Code { bits: 0b00000011, len: 8, run: 30 },
    Code { bits: 0b00011010, len: 8, run: 31 },
    Code { bits: 0b00011011, len: 8, run: 32 },
    Code { bits: 0b00010010, len: 8, run: 33 },
    Code { bits: 0b00010011, len: 8, run: 34 },
    Code { bits: 0b00010100, len: 8, run: 35 },
    Code { bits: 0b00010101, len: 8, run: 36 },
    Code { bits: 0b00010110, len: 8, run: 37 },
    Code { bits: 0b00010111, len: 8, run: 38 },
    Code { bits: 0b00101000, len: 8, run: 39 },
    Code { bits: 0b00101001, len: 8, run: 40 },
    Code { bits: 0b00101010, len: 8, run: 41 },
    Code { bits: 0b00101011, len: 8, run: 42 },
    Code { bits: 0b00101100, len: 8, run: 43 },
    Code { bits: 0b00101101, len: 8, run: 44 },
    Code { bits: 0b00000100, len: 8, run: 45 },
    Code { bits: 0b00000101, len: 8, run: 46 },
    Code { bits: 0b00001010, len: 8, run: 47 },
    Code { bits: 0b00001011, len: 8, run: 48 },
    Code { bits: 0b01010010, len: 8, run: 49 },
    Code { bits: 0b01010011, len: 8, run: 50 },
    Code { bits: 0b01010100, len: 8, run: 51 },
    Code { bits: 0b01010101, len: 8, run: 52 },
    Code { bits: 0b00100100, len: 8, run: 53 },
    Code { bits: 0b00100101, len: 8, run: 54 },
    Code { bits: 0b01011000, len: 8, run: 55 },
    Code { bits: 0b01011001, len: 8, run: 56 },
    Code { bits: 0b01011010, len: 8, run: 57 },
    Code { bits: 0b01011011, len: 8, run: 58 },
    Code { bits: 0b01001010, len: 8, run: 59 },
    Code { bits: 0b01001011, len: 8, run: 60 },
    Code { bits: 0b00110010, len: 8, run: 61 },
    Code { bits: 0b00110011, len: 8, run: 62 },
    Code { bits: 0b00110100, len: 8, run: 63 },
    Code { bits: 0b11011, len: 5, run: 64 },
    Code { bits: 0b10010, len: 5, run: 128 },
    Code { bits: 0b010111, len: 6, run: 192 },
    Code { bits: 0b0110111, len: 7, run: 256 },
    Code { bits: 0b00110110, len: 8, run: 320 },
    Code { bits: 0b00110111, len: 8, run: 384 },
    Code { bits: 0b01100100, len: 8, run: 448 },
    Code { bits: 0b01100101, len: 8, run: 512 },
    Code { bits: 0b01101000, len: 8, run: 576 },
    Code { bits: 0b01100111, len: 8, run: 640 },
    Code { bits: 0b011001100, len: 9, run: 704 },
    Code { bits: 0b011001101, len: 9, run: 768 },
    Code { bits: 0b011010010, len: 9, run: 832 },
    Code { bits: 0b011010011, len: 9, run: 896 },
    Code { bits: 0b011010100, len: 9, run: 960 },
    Code { bits: 0b011010101, len: 9, run: 1024 },
    Code { bits: 0b011010110, len: 9, run: 1088 },
    Code { bits: 0b011010111, len: 9, run: 1152 },
    Code { bits: 0b011011000, len: 9, run: 1216 },
    Code { bits: 0b011011001, len: 9, run: 1280 },
    Code { bits: 0b011011010, len: 9, run: 1344 },
    Code { bits: 0b011011011, len: 9, run: 1408 },
    Code { bits: 0b010011000, len: 9, run: 1472 },
    Code { bits: 0b010011001, len: 9, run: 1536 },
    Code { bits: 0b010011010, len: 9, run: 1600 },
    // Six bits, out of step with its nine-bit neighbours. T.4 assigns it that
    // way; it is not a transcription slip.
    Code { bits: 0b011000, len: 6, run: 1664 },
    Code { bits: 0b010011011, len: 9, run: 1728 },
];

/// Black run lengths 0..=63 (T.4 Table 1, terminating codes) followed by the
/// black make-up codes 64..=1728 (T.4 Table 2).
///
/// Black runs in scanned text are short, so the short codes go to the short
/// runs and the table reaches 13 bits at the top — where the white one stops
/// at 9.
#[rustfmt::skip]
pub(crate) const BLACK_CODES: &[Code] = &[
    Code { bits: 0b0000110111, len: 10, run: 0 },
    Code { bits: 0b010, len: 3, run: 1 },
    Code { bits: 0b11, len: 2, run: 2 },
    Code { bits: 0b10, len: 2, run: 3 },
    Code { bits: 0b011, len: 3, run: 4 },
    Code { bits: 0b0011, len: 4, run: 5 },
    Code { bits: 0b0010, len: 4, run: 6 },
    Code { bits: 0b00011, len: 5, run: 7 },
    Code { bits: 0b000101, len: 6, run: 8 },
    Code { bits: 0b000100, len: 6, run: 9 },
    Code { bits: 0b0000100, len: 7, run: 10 },
    Code { bits: 0b0000101, len: 7, run: 11 },
    Code { bits: 0b0000111, len: 7, run: 12 },
    Code { bits: 0b00000100, len: 8, run: 13 },
    Code { bits: 0b00000111, len: 8, run: 14 },
    Code { bits: 0b000011000, len: 9, run: 15 },
    Code { bits: 0b0000010111, len: 10, run: 16 },
    Code { bits: 0b0000011000, len: 10, run: 17 },
    Code { bits: 0b0000001000, len: 10, run: 18 },
    Code { bits: 0b00001100111, len: 11, run: 19 },
    Code { bits: 0b00001101000, len: 11, run: 20 },
    Code { bits: 0b00001101100, len: 11, run: 21 },
    Code { bits: 0b00000110111, len: 11, run: 22 },
    Code { bits: 0b00000101000, len: 11, run: 23 },
    Code { bits: 0b00000010111, len: 11, run: 24 },
    Code { bits: 0b00000011000, len: 11, run: 25 },
    Code { bits: 0b000011001010, len: 12, run: 26 },
    Code { bits: 0b000011001011, len: 12, run: 27 },
    Code { bits: 0b000011001100, len: 12, run: 28 },
    Code { bits: 0b000011001101, len: 12, run: 29 },
    Code { bits: 0b000001101000, len: 12, run: 30 },
    Code { bits: 0b000001101001, len: 12, run: 31 },
    Code { bits: 0b000001101010, len: 12, run: 32 },
    Code { bits: 0b000001101011, len: 12, run: 33 },
    Code { bits: 0b000011010010, len: 12, run: 34 },
    Code { bits: 0b000011010011, len: 12, run: 35 },
    Code { bits: 0b000011010100, len: 12, run: 36 },
    Code { bits: 0b000011010101, len: 12, run: 37 },
    Code { bits: 0b000011010110, len: 12, run: 38 },
    Code { bits: 0b000011010111, len: 12, run: 39 },
    Code { bits: 0b000001101100, len: 12, run: 40 },
    Code { bits: 0b000001101101, len: 12, run: 41 },
    Code { bits: 0b000011011010, len: 12, run: 42 },
    Code { bits: 0b000011011011, len: 12, run: 43 },
    Code { bits: 0b000001010100, len: 12, run: 44 },
    Code { bits: 0b000001010101, len: 12, run: 45 },
    Code { bits: 0b000001010110, len: 12, run: 46 },
    Code { bits: 0b000001010111, len: 12, run: 47 },
    Code { bits: 0b000001100100, len: 12, run: 48 },
    Code { bits: 0b000001100101, len: 12, run: 49 },
    Code { bits: 0b000001010010, len: 12, run: 50 },
    Code { bits: 0b000001010011, len: 12, run: 51 },
    Code { bits: 0b000000100100, len: 12, run: 52 },
    Code { bits: 0b000000110111, len: 12, run: 53 },
    Code { bits: 0b000000111000, len: 12, run: 54 },
    Code { bits: 0b000000100111, len: 12, run: 55 },
    Code { bits: 0b000000101000, len: 12, run: 56 },
    Code { bits: 0b000001011000, len: 12, run: 57 },
    Code { bits: 0b000001011001, len: 12, run: 58 },
    Code { bits: 0b000000101011, len: 12, run: 59 },
    Code { bits: 0b000000101100, len: 12, run: 60 },
    Code { bits: 0b000001011010, len: 12, run: 61 },
    Code { bits: 0b000001100110, len: 12, run: 62 },
    Code { bits: 0b000001100111, len: 12, run: 63 },
    Code { bits: 0b0000001111, len: 10, run: 64 },
    Code { bits: 0b000011001000, len: 12, run: 128 },
    Code { bits: 0b000011001001, len: 12, run: 192 },
    Code { bits: 0b000001011011, len: 12, run: 256 },
    Code { bits: 0b000000110011, len: 12, run: 320 },
    Code { bits: 0b000000110100, len: 12, run: 384 },
    Code { bits: 0b000000110101, len: 12, run: 448 },
    Code { bits: 0b0000001101100, len: 13, run: 512 },
    Code { bits: 0b0000001101101, len: 13, run: 576 },
    Code { bits: 0b0000001001010, len: 13, run: 640 },
    Code { bits: 0b0000001001011, len: 13, run: 704 },
    Code { bits: 0b0000001001100, len: 13, run: 768 },
    Code { bits: 0b0000001001101, len: 13, run: 832 },
    Code { bits: 0b0000001110010, len: 13, run: 896 },
    Code { bits: 0b0000001110011, len: 13, run: 960 },
    Code { bits: 0b0000001110100, len: 13, run: 1024 },
    Code { bits: 0b0000001110101, len: 13, run: 1088 },
    Code { bits: 0b0000001110110, len: 13, run: 1152 },
    Code { bits: 0b0000001110111, len: 13, run: 1216 },
    Code { bits: 0b0000001010010, len: 13, run: 1280 },
    Code { bits: 0b0000001010011, len: 13, run: 1344 },
    Code { bits: 0b0000001010100, len: 13, run: 1408 },
    Code { bits: 0b0000001010101, len: 13, run: 1472 },
    Code { bits: 0b0000001011010, len: 13, run: 1536 },
    Code { bits: 0b0000001011011, len: 13, run: 1600 },
    Code { bits: 0b0000001100100, len: 13, run: 1664 },
    Code { bits: 0b0000001100101, len: 13, run: 1728 },
];

/// The extended make-up codes 1792..=2560 (T.4 Table 3).
///
/// These are common to both colours, which is why they live apart from the two
/// tables above rather than being duplicated into each.
#[rustfmt::skip]
pub(crate) const EXT_MAKEUP_CODES: &[Code] = &[
    Code { bits: 0b00000001000, len: 11, run: 1792 },
    Code { bits: 0b00000001100, len: 11, run: 1856 },
    Code { bits: 0b00000001101, len: 11, run: 1920 },
    Code { bits: 0b000000010010, len: 12, run: 1984 },
    Code { bits: 0b000000010011, len: 12, run: 2048 },
    Code { bits: 0b000000010100, len: 12, run: 2112 },
    Code { bits: 0b000000010101, len: 12, run: 2176 },
    Code { bits: 0b000000010110, len: 12, run: 2240 },
    Code { bits: 0b000000010111, len: 12, run: 2304 },
    Code { bits: 0b000000011100, len: 12, run: 2368 },
    Code { bits: 0b000000011101, len: 12, run: 2432 },
    Code { bits: 0b000000011110, len: 12, run: 2496 },
    Code { bits: 0b000000011111, len: 12, run: 2560 },
];

/// How the next changing element of a two-dimensionally coded row is placed
/// relative to the row above (T.6 §2.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Mode {
    /// The current run continues past `b2`; nothing is placed.
    Pass,
    /// Two run lengths follow, coded with the tables above.
    Horizontal,
    /// The next changing element sits `delta` pixels from `b1`, `delta` in
    /// −3..=3.
    Vertical(i8),
}

/// The nine two-dimensional mode codes (T.6 §2.2, Table 4).
///
/// Ordered as the specification lists them rather than by length: the lookup
/// matches on the peeked window, and the set is prefix-free, so order carries
/// no meaning.
///
/// Visible to the crate so that the test encoder writes the same patterns this
/// reader accepts. That makes a round trip blind to a mistyped mode code, which
/// is why the patterns are also checked directly against Table 4 by this
/// module's tests.
pub(crate) const MODE_CODES: &[(u16, u8, Mode)] = &[
    (0b1, 1, Mode::Vertical(0)),
    (0b011, 3, Mode::Vertical(1)),
    (0b010, 3, Mode::Vertical(-1)),
    (0b001, 3, Mode::Horizontal),
    (0b0001, 4, Mode::Pass),
    (0b000011, 6, Mode::Vertical(2)),
    (0b000010, 6, Mode::Vertical(-2)),
    (0b0000011, 7, Mode::Vertical(3)),
    (0b0000010, 7, Mode::Vertical(-3)),
];

/// The escape introducing a two-dimensional extension (T.6 §2.2).
const EXTENSION_BITS: u16 = 0b0000001;

/// The length of [`EXTENSION_BITS`], in bits.
const EXTENSION_LEN: u8 = 7;

/// Finds the code whose bits are the next bits of the stream, and consumes it.
///
/// One pass over the table suffices, and one peek: the set is prefix-free, so
/// at most one code can match the window at all, and the first match is
/// therefore the only match. That property is not assumed — it is asserted
/// exhaustively over every pair of codes by this module's tests, which is
/// strictly stronger than any check a per-call assertion could make.
///
/// Total for every input: past the end of the data the peek reads zeros, and
/// no code in either table is all zeros, so a truncated stream returns `None`
/// rather than reading out of bounds.
fn match_code(r: &mut BitReader, table: &[Code]) -> Option<Code> {
    let window = r.peek(WINDOW_BITS);
    let found = table
        .iter()
        .find(|c| window >> (WINDOW_BITS - u32::from(c.len)) == u32::from(c.bits))?;
    r.skip(u32::from(found.len));
    Some(*found)
}

/// Reads one complete run length: any make-up codes, then the terminating code
/// that closes them (T.4 §4.1.2).
///
/// `white` selects the table, and must track the colour of the run being read
/// — in horizontal mode the two runs use opposite tables.
///
/// The loop terminates structurally rather than on the data making sense:
/// every iteration either returns, or adds at least 64 to a total capped at
/// [`MAX_RUN`]. A pattern in neither table is [`CcittError::UnknownCode`], not
/// a guess, because a guessed run length silently displaces every pixel after
/// it.
pub(crate) fn read_run(r: &mut BitReader, white: bool) -> Result<u32, CcittError> {
    let table = if white { WHITE_CODES } else { BLACK_CODES };
    let mut total: u32 = 0;
    loop {
        let code = match_code(r, table)
            .or_else(|| match_code(r, EXT_MAKEUP_CODES))
            .ok_or(CcittError::UnknownCode)?;
        total = total
            .checked_add(u32::from(code.run))
            .filter(|t| *t <= MAX_RUN)
            .ok_or(CcittError::RunTooLong)?;
        // Under 64 means a terminating code, which ends the run. The extended
        // make-ups are never under 64, so they cannot end one.
        if code.run < 64 {
            return Ok(total);
        }
    }
}

/// Reads one two-dimensional mode code (T.6 §2.2, Table 4).
///
/// The extension escape is reported as unimplemented rather than skipped: its
/// payload sets the length of what follows, so guessing would desynchronise
/// the bit stream rather than lose one construct. A window of zeros is EOL or
/// fill, never a mode, and comes back as [`CcittError::UnknownCode`] so the
/// row decoder can go looking for the EOL itself.
pub(crate) fn read_mode(r: &mut BitReader) -> Result<Mode, CcittError> {
    let window = r.peek(MODE_WINDOW_BITS);
    for (bits, len, mode) in MODE_CODES {
        if window >> (MODE_WINDOW_BITS - u32::from(*len)) == u32::from(*bits) {
            r.skip(u32::from(*len));
            return Ok(*mode);
        }
    }
    if window >> (MODE_WINDOW_BITS - u32::from(EXTENSION_LEN)) == u32::from(EXTENSION_BITS) {
        return Err(CcittError::Unimplemented("2D extension code"));
    }
    Err(CcittError::UnknownCode)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::ccitt::bits::BitReader;
    use crate::filters::ccitt::testing::{ext_lookup, lookup, pack, push_code, push_run};

    /// A code, its length and a name, in the shape the structural checks want.
    type Entry = (u16, u8, String);

    fn entries(codes: &[Code], colour: &str) -> Vec<Entry> {
        codes
            .iter()
            .map(|c| (c.bits, c.len, format!("{colour} run {}", c.run)))
            .collect()
    }

    /// One colour's whole code set: terminating, make-up and the extended
    /// make-ups both colours share.
    fn full_set(base: &[Code]) -> Vec<Code> {
        base.iter().chain(EXT_MAKEUP_CODES).copied().collect()
    }

    fn eol_entry() -> Entry {
        (EOL_BITS, EOL_LEN, "EOL".to_string())
    }

    /// Reports the first pair where one code is a prefix of another.
    ///
    /// This is the single most valuable check on a hand transcription: almost
    /// any single mistyped bit turns some code into a prefix of another, and
    /// the message names both entries so the offending row is immediate.
    fn prefix_clash(codes: &[Entry]) -> Option<String> {
        for (i, a) in codes.iter().enumerate() {
            for b in codes.iter().skip(i + 1) {
                let (short, long) = if a.1 <= b.1 { (a, b) } else { (b, a) };
                if long.0 >> (long.1 - short.1) == short.0 {
                    return Some(format!(
                        "{} ({:0w1$b}) is a prefix of {} ({:0w2$b})",
                        short.2,
                        short.0,
                        long.2,
                        long.0,
                        w1 = usize::from(short.1),
                        w2 = usize::from(long.1),
                    ));
                }
            }
        }
        None
    }

    #[test]
    fn each_colour_code_set_is_prefix_free() {
        for (colour, codes) in [("white", WHITE_CODES), ("black", BLACK_CODES)] {
            let mut set = entries(&full_set(codes), colour);
            set.push(eol_entry());
            assert_eq!(prefix_clash(&set), None, "{colour} table");
        }
    }

    #[test]
    fn the_mode_codes_are_prefix_free_against_each_other_and_against_eol() {
        let mut set: Vec<Entry> = MODE_CODES
            .iter()
            .map(|(bits, len, mode)| (*bits, *len, format!("{mode:?}")))
            .collect();
        set.push((
            EXTENSION_BITS,
            EXTENSION_LEN,
            "extension escape".to_string(),
        ));
        set.push(eol_entry());
        assert_eq!(prefix_clash(&set), None);
    }

    #[test]
    fn terminating_codes_cover_every_run_from_zero_to_sixty_three() {
        for (colour, codes) in [("white", WHITE_CODES), ("black", BLACK_CODES)] {
            for run in 0..=63u16 {
                assert_eq!(
                    codes.iter().filter(|c| c.run == run).count(),
                    1,
                    "{colour} run {run} must appear exactly once",
                );
            }
        }
    }

    #[test]
    fn makeup_codes_are_the_multiples_of_sixty_four() {
        for (colour, codes) in [("white", WHITE_CODES), ("black", BLACK_CODES)] {
            let mut got: Vec<u16> = codes.iter().map(|c| c.run).filter(|r| *r >= 64).collect();
            got.sort_unstable();
            let want: Vec<u16> = (64..=1728).step_by(64).collect();
            assert_eq!(got, want, "{colour} make-ups");
        }
        let ext: Vec<u16> = EXT_MAKEUP_CODES.iter().map(|c| c.run).collect();
        assert_eq!(ext, (1792..=2560).step_by(64).collect::<Vec<u16>>());
    }

    #[test]
    fn each_table_holds_exactly_the_number_of_codes_the_specification_assigns() {
        assert_eq!(WHITE_CODES.len(), 64 + 27, "white terminating plus make-up");
        assert_eq!(BLACK_CODES.len(), 64 + 27, "black terminating plus make-up");
        assert_eq!(EXT_MAKEUP_CODES.len(), 13, "shared extended make-up");
    }

    #[test]
    fn no_code_is_empty_or_longer_than_thirteen_bits() {
        let all = WHITE_CODES
            .iter()
            .chain(BLACK_CODES)
            .chain(EXT_MAKEUP_CODES);
        for code in all {
            assert!(
                code.len >= 1 && u32::from(code.len) <= WINDOW_BITS,
                "run {} has {} bits",
                code.run,
                code.len,
            );
            assert!(
                u32::from(code.bits) < (1u32 << code.len),
                "run {} has bits wider than its length",
                code.run,
            );
        }
    }

    /// A bit pattern may not be reused inside one colour's table. Prefix
    /// freeness already implies this, but a duplicate reports far more
    /// legibly as a duplicate than as a zero-length prefix clash.
    #[test]
    fn no_bit_pattern_repeats_within_a_colour_table() {
        for (colour, codes) in [("white", WHITE_CODES), ("black", BLACK_CODES)] {
            let set = full_set(codes);
            for (i, a) in set.iter().enumerate() {
                for b in set.iter().skip(i + 1) {
                    assert!(
                        !(a.bits == b.bits && a.len == b.len),
                        "{colour}: runs {} and {} share a code",
                        a.run,
                        b.run,
                    );
                }
            }
        }
    }

    /// The likeliest transcription failure is pasting the white table into the
    /// black one, which would pass every prefix check while being entirely
    /// wrong.
    ///
    /// The two tables are not disjoint — T.4 assigns `0000100` to white 23 and
    /// to black 10, and `00000100` to white 45 and to black 13 — so "no code
    /// appears in both" is too strong and would reject a correct
    /// transcription. What does hold is that no *run* is written the same way
    /// in both colours, and that the overlap is exactly those two patterns.
    #[test]
    fn the_white_and_black_tables_are_genuinely_different() {
        for run in WHITE_CODES.iter().map(|c| c.run) {
            let white = lookup(true, run);
            let black = lookup(false, run);
            assert!(
                !(white.bits == black.bits && white.len == black.len),
                "run {run} has the same code in both tables",
            );
        }

        let mut shared: Vec<(u16, u8)> = Vec::new();
        for w in WHITE_CODES {
            for b in BLACK_CODES {
                if w.bits == b.bits && w.len == b.len {
                    shared.push((w.bits, w.len));
                }
            }
        }
        shared.sort_unstable();
        assert_eq!(
            shared,
            vec![(0b0000100, 7), (0b00000100, 8)],
            "the colour tables overlap somewhere they should not",
        );
    }

    /// Expands every code of one colour, plus EOL, into the depth-13 leaves it
    /// covers, and reports the leaves left over as contiguous runs.
    fn uncovered_leaves(base: &[Code]) -> Vec<(u32, u32)> {
        let leaf_count = 1usize << WINDOW_BITS;
        let mut covered = vec![false; leaf_count];
        let mut mark = |bits: u16, len: u8| {
            let span = 1usize << (WINDOW_BITS - u32::from(len));
            let start = usize::from(bits) << (WINDOW_BITS - u32::from(len));
            for leaf in covered.iter_mut().skip(start).take(span) {
                *leaf = true;
            }
        };
        for code in full_set(base) {
            mark(code.bits, code.len);
        }
        mark(EOL_BITS, EOL_LEN);

        let mut runs: Vec<(u32, u32)> = Vec::new();
        for (leaf, hit) in covered.iter().enumerate() {
            if *hit {
                continue;
            }
            let leaf = leaf as u32;
            match runs.last_mut() {
                Some(last) if last.1 + 1 == leaf => last.1 = leaf,
                _ => runs.push((leaf, leaf)),
            }
        }
        runs
    }

    /// The sharpest structural check available without a second copy of the
    /// tables.
    ///
    /// T.4's code is deliberately incomplete: it reserves code space so that a
    /// long run of zero bits — fill, followed by EOL — can never be mistaken
    /// for a run length. Expanding one colour's whole code set plus EOL to
    /// depth 13 must therefore leave exactly 30 of the 8192 leaves uncovered,
    /// in exactly two contiguous runs, both inside that reserved zero-prefix
    /// region, and identically for white and for black. A transcription error
    /// moves a gap somewhere else or scatters it, and the gap points straight
    /// at the mistyped row.
    #[test]
    fn the_uncovered_code_space_is_exactly_the_region_reserved_for_eol_and_fill() {
        let want = vec![
            (0b0_0000_0000_0000, 0b0_0000_0000_0001),
            (0b0_0000_0000_0100, 0b0_0000_0001_1111),
        ];
        for (colour, codes) in [("white", WHITE_CODES), ("black", BLACK_CODES)] {
            let got = uncovered_leaves(codes);
            let render = |runs: &[(u32, u32)]| {
                runs.iter()
                    .map(|(lo, hi)| format!("{lo:013b}..{hi:013b}"))
                    .collect::<Vec<String>>()
                    .join(", ")
            };
            assert_eq!(got, want, "{colour}: uncovered leaves are {}", render(&got));
            let uncovered: u32 = got.iter().map(|(lo, hi)| hi - lo + 1).sum();
            assert_eq!(uncovered, 30, "{colour}: uncovered leaf count");
        }
    }

    /// The Kraft sum of a complete prefix code is 1. T.4's is not complete,
    /// and a sum of 1 would mean the transcription had eaten the code space
    /// reserved for EOL and fill. The correct value for one colour's whole set
    /// plus EOL is 4081/4096.
    #[test]
    fn the_kraft_sum_of_each_colour_set_is_four_thousand_and_eighty_one_over_four_thousand_ninety_six(
    ) {
        for (colour, codes) in [("white", WHITE_CODES), ("black", BLACK_CODES)] {
            let leaf = |len: u8| 1u32 << (WINDOW_BITS - u32::from(len));
            let covered: u32 =
                full_set(codes).iter().map(|c| leaf(c.len)).sum::<u32>() + leaf(EOL_LEN);
            // 8192 leaves at depth 13, so 8162/8192 is 4081/4096 exactly.
            assert_eq!(
                covered, 8162,
                "{colour}: Kraft sum is {covered}/8192, want 8162/8192",
            );
        }
    }

    #[test]
    fn reads_a_terminating_run() {
        // White run 0 is 00110101 (8 bits) in T.4 Table 1.
        let mut r = BitReader::new(&[0b0011_0101]);
        assert_eq!(read_run(&mut r, true), Ok(0));
        assert_eq!(r.bit_pos(), 8);
    }

    /// A make-up must be followed by a terminating code, and the two add.
    #[test]
    fn a_makeup_plus_a_terminating_code_sum() {
        for (white, makeup, term) in [(true, 64u32, 5u32), (false, 128, 7)] {
            let mut bits = Vec::new();
            push_code(&mut bits, lookup(white, makeup as u16));
            push_code(&mut bits, lookup(white, term as u16));
            let bytes = pack(&bits);
            let mut r = BitReader::new(&bytes);
            assert_eq!(read_run(&mut r, white), Ok(makeup + term));
        }
    }

    /// Runs above 2560 need repeated make-ups.
    #[test]
    fn repeated_makeups_accumulate() {
        let mut bits = Vec::new();
        push_code(&mut bits, ext_lookup(2560));
        push_code(&mut bits, ext_lookup(2560));
        push_code(&mut bits, lookup(true, 1));
        let bytes = pack(&bits);
        let mut r = BitReader::new(&bytes);
        assert_eq!(read_run(&mut r, true), Ok(5121));
    }

    /// Decode-encode agreement across the whole representable range: every run
    /// a single make-up plus a terminating code can express, in both colours.
    #[test]
    fn every_representable_run_round_trips() {
        for white in [true, false] {
            for run in 0..=2623u16 {
                let mut bits = Vec::new();
                push_run(&mut bits, white, u32::from(run));
                let bytes = pack(&bits);
                let mut r = BitReader::new(&bytes);
                assert_eq!(
                    read_run(&mut r, white),
                    Ok(u32::from(run)),
                    "{} run {run}",
                    if white { "white" } else { "black" },
                );
            }
        }
    }

    #[test]
    fn an_unrecognised_pattern_is_an_error_not_a_guess() {
        // Thirteen zero bits are reserved for fill and match no run code.
        for white in [true, false] {
            let mut r = BitReader::new(&[0x00, 0x00]);
            assert_eq!(read_run(&mut r, white), Err(CcittError::UnknownCode));
        }
    }

    #[test]
    fn a_truncated_stream_ends_the_run_rather_than_reading_past_the_data() {
        // One make-up and nothing after it: the zero-filled peek past the end
        // matches no terminating code.
        let mut bits = Vec::new();
        push_code(&mut bits, lookup(true, 1728));
        let bytes = pack(&bits);
        let mut r = BitReader::new(&bytes);
        assert_eq!(read_run(&mut r, true), Err(CcittError::UnknownCode));
    }

    /// A chain of make-up codes describes an unbounded run, so the accumulator
    /// needs a ceiling of its own rather than trusting the caller's row width.
    #[test]
    fn a_run_wider_than_any_decodable_row_is_rejected_rather_than_accumulated() {
        let makeups = (MAX_RUN / 2560) + 1;
        let mut bits = Vec::with_capacity(makeups as usize * 12);
        for _ in 0..makeups {
            push_code(&mut bits, ext_lookup(2560));
        }
        push_code(&mut bits, lookup(true, 0));
        let bytes = pack(&bits);
        let mut r = BitReader::new(&bytes);
        assert_eq!(read_run(&mut r, true), Err(CcittError::RunTooLong));
    }

    #[test]
    fn reads_every_two_dimensional_mode() {
        let cases: [(&[u8], usize, Mode); 9] = [
            (&[0b1000_0000], 1, Mode::Vertical(0)),
            (&[0b0110_0000], 3, Mode::Vertical(1)),
            (&[0b0100_0000], 3, Mode::Vertical(-1)),
            (&[0b0010_0000], 3, Mode::Horizontal),
            (&[0b0001_0000], 4, Mode::Pass),
            (&[0b0000_1100], 6, Mode::Vertical(2)),
            (&[0b0000_1000], 6, Mode::Vertical(-2)),
            (&[0b0000_0110], 7, Mode::Vertical(3)),
            (&[0b0000_0100], 7, Mode::Vertical(-3)),
        ];
        for (bytes, len, want) in cases {
            let mut r = BitReader::new(bytes);
            assert_eq!(read_mode(&mut r), Ok(want), "{bytes:?}");
            assert_eq!(r.bit_pos(), len, "wrong number of bits consumed");
        }
    }

    /// All seven vertical offsets are distinct modes, and no two of them share
    /// a code — the vertical set is where an off-by-one in the sign convention
    /// hides.
    #[test]
    fn the_vertical_offsets_span_minus_three_to_three() {
        let mut offsets: Vec<i8> = MODE_CODES
            .iter()
            .filter_map(|(_, _, mode)| match mode {
                Mode::Vertical(delta) => Some(*delta),
                _ => None,
            })
            .collect();
        offsets.sort_unstable();
        assert_eq!(offsets, vec![-3, -2, -1, 0, 1, 2, 3]);
        assert_eq!(
            MODE_CODES.len(),
            9,
            "seven vertical, one pass, one horizontal"
        );
    }

    #[test]
    fn the_two_dimensional_extension_escape_is_reported_not_guessed() {
        let mut r = BitReader::new(&[0b0000_0010, 0x00]);
        assert_eq!(
            read_mode(&mut r),
            Err(CcittError::Unimplemented("2D extension code")),
        );
    }

    /// A run of zero bits is EOL or fill, never a mode. Reporting it lets the
    /// row decoder look for the EOL itself rather than having a mode invented
    /// for it.
    #[test]
    fn a_zero_fill_pattern_is_not_a_mode() {
        let mut r = BitReader::new(&[0x00, 0x00]);
        assert_eq!(read_mode(&mut r), Err(CcittError::UnknownCode));
    }

    #[test]
    fn an_exhausted_reader_yields_an_error_from_both_readers() {
        let mut r = BitReader::new(&[]);
        assert_eq!(read_mode(&mut r), Err(CcittError::UnknownCode));
        let mut r = BitReader::new(&[]);
        assert_eq!(read_run(&mut r, true), Err(CcittError::UnknownCode));
    }
}
