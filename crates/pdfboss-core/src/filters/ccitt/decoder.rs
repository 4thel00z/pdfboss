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
use super::codes::{read_mode, read_run, Mode, EOL_BITS, EOL_LEN, WINDOW_BITS};
use super::{CcittError, MAX_IMAGE_SIDE};
use crate::filters::jbig2::bitmap::{Bitmap, MAX_PIXELS};

/// Where `a0` sits before the first changing element of a row.
///
/// Not 0. T.4 §4.2.1.3 puts the reference position on an imaginary white
/// element *just before* the row, so that a row beginning with a black pixel
/// has its first changing element at column 0 and that element is still
/// strictly to the right of `a0`. Starting at 0 instead loses it, and every run
/// of every row comes out one pixel short.
const BEFORE_ROW: i64 = -1;

/// The width of the end-of-line pattern, as the bit reader counts widths.
const EOL_WINDOW: u32 = EOL_LEN as u32;

/// How a facsimile stream is laid out, from ISO 32000-1 Table 11.
///
/// The JBIG2 use of this codec (ITU-T T.88 §6.2.6) is one particular setting of
/// these: `k` below zero, no end-of-line patterns, no byte alignment, and the
/// region's width and height as `columns` and `rows`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Params {
    /// Pixels per row. Zero cannot describe an image and is refused.
    pub(crate) columns: u32,
    /// Rows in the image, or 0 for "as many as the data holds", which is what
    /// ISO 32000-1 gives that value to mean. An inferred count is capped by
    /// both of the bounds a stated one is held to; see `inferred_row_cap`.
    pub(crate) rows: u32,
    /// Below zero selects pure two-dimensional coding; 0 pure
    /// one-dimensional; above zero a mixture, each row carrying a bit that
    /// says which it is. Above zero, T.4 also gives the value itself a
    /// meaning — how many two-dimensional rows may follow a one-dimensional
    /// one — but that is advisory, and what decides how a row is read is the
    /// bit in front of it.
    pub(crate) k: i32,
    /// Whether rows are separated by end-of-line patterns.
    ///
    /// This does **not** decide whether the pattern is recognised: producers
    /// disagree about the flag, so an end-of-line pattern is stepped over
    /// wherever it appears. What the flag decides is whether *fill* may
    /// precede one — see `Rows::consume_eol`.
    pub(crate) end_of_line: bool,
    /// Whether each row starts on a byte boundary.
    pub(crate) byte_align: bool,
}

/// Decodes a facsimile stream into a bitmap in which a set pixel is black.
///
/// All three forms ISO 32000-1 Table 11 selects between are read here: pure
/// two-dimensional coding (`k` below zero, which is also the whole of what a
/// JBIG2 generic region uses), pure one-dimensional coding (`k` of 0), and the
/// mixture in which every row carries a bit saying which it is (`k` above
/// zero).
///
/// Truncation is not an error. A stream that stops in the middle yields the
/// rows it did decode and white ones after them, because a page that is mostly
/// readable is worth more than an error — real scanners produce such files. A
/// bit pattern that is *not* the end of the data but matches no code is
/// corruption, and is reported: guessing at it would displace every pixel
/// below.
///
/// # What bounds the cost
///
/// Both dimensions are checked against `MAX_IMAGE_SIDE` before anything is
/// allocated, and every caller's own product cap still applies on top. The
/// per-side check is the one that matters here rather than in the callers,
/// because the row state this decoder keeps is not proportional to the image:
/// it is proportional to the row *width* alone, at eight bytes per pixel of
/// width against the bitmap's one, so a product cap that admits any aspect
/// ratio admits a short, absurdly wide image whose row state dwarfs the bitmap
/// the product cap was sizing.
pub(crate) fn decode(data: &[u8], params: &Params) -> Result<Bitmap, CcittError> {
    check_dimensions(params.columns, params.rows)?;
    match params.rows {
        // 0 is not a height but a request to take one from the data, so the
        // bitmap cannot be allocated until the rows have been read.
        0 => Rows::new(data, params).paint_inferred(),
        // A stated height is allocated before a bit is read, so an image too
        // large to hold is refused for free.
        stated => Rows::new(data, params).paint(stated),
    }
}

/// Refuses dimensions no facsimile decode may be attempted at.
///
/// Separate from [`decode`] so that a caller holding a resource of its own can
/// ask the question before committing that resource. The JBIG2 generic region
/// is the caller that needs it: it charges a shared work budget from the
/// declared dimensions, and a region refused for its shape should leave that
/// budget untouched rather than spend it on a decode that never happened.
///
/// A row count of 0 passes, because for the `CCITTFaxDecode` filter it is not a
/// height at all but a request to take the height from the data (ISO 32000-1
/// Table 11). Callers for whom 0 is a real height refuse it themselves.
pub(crate) fn check_dimensions(columns: u32, rows: u32) -> Result<(), CcittError> {
    if columns == 0 {
        return Err(CcittError::BadParameter("an image with no columns"));
    }
    if columns > MAX_IMAGE_SIDE || rows > MAX_IMAGE_SIDE {
        return Err(CcittError::TooLarge {
            width: columns,
            height: rows,
        });
    }
    Ok(())
}

/// A facsimile stream being read: the bit cursor, the layout it is read under,
/// and the two changing-element lists the rows alternate between.
struct Rows<'a> {
    r: BitReader<'a>,
    /// Pixels per row, never 0 — [`decode`] refuses that before constructing
    /// this.
    columns: u32,
    /// See [`Params::k`].
    k: i32,
    /// Whether fill bits may precede an end-of-line pattern.
    fill: bool,
    /// Whether every row begins on a byte boundary.
    byte_align: bool,
    /// The row above the one being decoded, as changing-element positions. It
    /// is empty for row 0, which is the all-white row T.6 §2.2 imagines above
    /// the image.
    reference: Vec<u32>,
    /// The row being decoded, in the same form. The two are swapped once a row
    /// is finished, so the row just read becomes the next one's reference.
    coding: Vec<u32>,
}

impl<'a> Rows<'a> {
    /// A reader over `data`, positioned at its first bit.
    fn new(data: &'a [u8], params: &Params) -> Rows<'a> {
        Rows {
            r: BitReader::new(data),
            columns: params.columns,
            k: params.k,
            fill: params.end_of_line,
            byte_align: params.byte_align,
            reference: Vec::new(),
            coding: Vec::new(),
        }
    }

    /// Decodes the stream into a bitmap `height` rows tall.
    ///
    /// A stream holding fewer rows than that leaves the rest white, which is
    /// what makes both truncation and an early terminator harmless.
    fn paint(mut self, height: u32) -> Result<Bitmap, CcittError> {
        let mut out = Bitmap::new(self.columns, height).map_err(|_| CcittError::TooLarge {
            width: self.columns,
            height,
        })?;
        for y in 0..height {
            if !self.next_row()? {
                break;
            }
            paint_row(&mut out, y, &self.coding);
            std::mem::swap(&mut self.reference, &mut self.coding);
        }
        Ok(out)
    }

    /// Decodes a stream that does not state a height (`/Rows 0`, ISO 32000-1
    /// Table 11), whose height is however many rows the data turns out to hold.
    ///
    /// The bitmap cannot be allocated until that count is known, and the count
    /// is not known until the rows have been decoded, so the rows are held in a
    /// buffer until the last one has been read. The buffer is *packed*, one bit
    /// per pixel, for the reason the bitmap is not: at a byte per pixel it
    /// would be a second copy of the image, where at a bit per pixel it is an
    /// eighth of one, bounded by `MAX_PIXELS / 8` however wide or tall the
    /// image turns out to be.
    ///
    /// Decoding once and buffering is what this does instead of counting the
    /// rows in one pass and painting them in a second. The two-pass form needs
    /// no buffer, but it decodes every row twice — and `/Rows 0` is the Table
    /// 11 default, so that is the cost an unspecified stream pays. Two-
    /// dimensional coding leaves no cheaper way to count: a row is coded
    /// against the row above it, so a row cannot be measured without being
    /// decoded.
    ///
    /// The loop is bounded twice over: by `inferred_row_cap`, and by every row
    /// consuming at least one bit of a finite stream. The second bound is
    /// checked rather than assumed, because it rests on properties of two other
    /// functions — no mode code and no run code is empty — and this loop should
    /// terminate whatever they do.
    fn paint_inferred(mut self) -> Result<Bitmap, CcittError> {
        let cap = inferred_row_cap(self.columns);
        let stride = (self.columns as usize).div_ceil(8);
        let mut packed: Vec<u8> = Vec::new();
        let mut height: u32 = 0;
        while height < cap {
            let before = self.r.bit_pos();
            if !self.next_row()? || self.r.bit_pos() == before {
                break;
            }
            let base = packed.len();
            packed.resize(base + stride, 0);
            pack_row(&mut packed[base..], &self.coding, self.columns);
            height += 1;
            std::mem::swap(&mut self.reference, &mut self.coding);
        }
        // The lists are the largest thing still live, and nothing below reads
        // them; dropping them now keeps them out of the bitmap's peak.
        self.reference = Vec::new();
        self.coding = Vec::new();
        let mut out = Bitmap::new(self.columns, height).map_err(|_| CcittError::TooLarge {
            width: self.columns,
            height,
        })?;
        for (y, row) in packed.chunks_exact(stride.max(1)).enumerate() {
            unpack_row(&mut out, y as u32, row);
        }
        Ok(out)
    }

    /// Decodes the next row into `self.coding`, reporting whether there was
    /// one.
    fn next_row(&mut self) -> Result<bool, CcittError> {
        let Some(one_dimensional) = self.start_row() else {
            return Ok(false);
        };
        let outcome = if one_dimensional {
            decode_row_1d(&mut self.r, self.columns, &mut self.coding)
        } else {
            decode_row_2d(&mut self.r, &self.reference, self.columns, &mut self.coding)
        };
        match outcome {
            Ok(()) => Ok(true),
            // A failure with less than a full code window left is the data
            // running out mid-code, not a stream that is wrong. The rows
            // already decoded stand; the partial one is dropped.
            Err(_) if self.r.remaining() < WINDOW_BITS as usize => Ok(false),
            Err(err) => Err(err),
        }
    }

    /// Steps over whatever separates one row from the next and reports how the
    /// row that follows is coded, or reports that the stream is over.
    ///
    /// The order is the one ISO 32000-1 Table 11 implies and T.4 §4.2.3 fixes:
    /// byte alignment first, since it pads out the previous row; then any
    /// end-of-line pattern; then, in mixed coding, the bit that says how this
    /// row is written.
    fn start_row(&mut self) -> Option<bool> {
        if self.byte_align {
            self.r.align_to_byte();
        }
        if self.r.is_exhausted() {
            return None;
        }
        // Two end-of-line patterns with no row between them terminate the
        // data: that is the end-of-facsimile block of T.6 §2.2.1, and the
        // opening pair of T.4's return to control.
        if self.consume_eols() >= 2 {
            return None;
        }
        // In mixed coding the bit before a row says how the row is coded (T.4
        // §4.2.3): 1 for one-dimensional, 0 for two-dimensional. It sits after
        // the end-of-line pattern when there is one, and directly before the
        // row when there is not.
        let one_dimensional = match self.k {
            k if k < 0 => false,
            0 => true,
            _ => self.r.read_bit()? == 1,
        };
        // Whatever follows now has to be a row, and twelve zero bits are not
        // one: no run code and no mode code has more than seven leading zeros.
        // So this is the padding in the last byte, trailing fill, or the second
        // half of a terminator whose first half the tag bit above stepped into
        // — in every case, the end of the image rather than a row to decode.
        let window = self.r.peek(EOL_WINDOW);
        if window == 0 || window == u32::from(EOL_BITS) {
            return None;
        }
        Some(one_dimensional)
    }

    /// Consumes consecutive end-of-line patterns, reporting how many there
    /// were, and stopping at two.
    ///
    /// Two is as many as any caller has to tell apart: one separates rows, and
    /// two end the data.
    fn consume_eols(&mut self) -> u32 {
        let mut count = 0;
        while self.consume_eol() {
            count += 1;
            if count >= 2 {
                break;
            }
        }
        count
    }

    /// Consumes one end-of-line pattern, and any fill before it, if the cursor
    /// is on one (T.4 §4.1.1). Reports whether it did.
    ///
    /// The pattern is recognised whether or not `/EndOfLine` was set. Producers
    /// disagree about that flag, and a decoder that trusts it reads a row
    /// separator as image data — twelve bits of displacement that ruins every
    /// row after.
    ///
    /// What the flag does decide is *fill*: a run of zero bits padding a row
    /// out to a minimum transmission time, which T.4 §4.1.1 puts only before an
    /// end-of-line pattern. A stream without those patterns has no fill, and a
    /// long run of zeros in one is instead the padding in its last byte — which
    /// the caller has to see as the end of the data rather than step over.
    fn consume_eol(&mut self) -> bool {
        if self.r.peek(EOL_WINDOW) == u32::from(EOL_BITS) {
            self.r.skip(EOL_WINDOW);
            return true;
        }
        if !self.fill || self.r.peek(EOL_WINDOW) != 0 {
            return false;
        }
        // Twelve zero bits begin no code in either table, so consuming them
        // cannot eat a row. The loop ends on the data, which is finite.
        while let Some(bit) = self.r.read_bit() {
            if bit == 1 {
                return true;
            }
        }
        false
    }
}

/// The most rows this build will infer for a stream that does not state its
/// height (`/Rows 0`, ISO 32000-1 Table 11).
///
/// Without a stated height the row count is whatever the data says, which for
/// attacker-supplied data is not a bound at all. Two bounds are applied, and
/// the smaller wins.
///
/// The first is the one the bitmap itself would apply: a row count past
/// `MAX_PIXELS / columns` could not be allocated, so decoding further would
/// only be work done before a refusal.
///
/// The second is `MAX_IMAGE_SIDE`, the same per-side bound a stated height is
/// held to. Without it a one-pixel-wide image could infer a hundred million
/// rows — a shape no scanner produces, and one the first bound alone waves
/// through because its product is what the first bound measures.
fn inferred_row_cap(columns: u32) -> u32 {
    let cap = MAX_PIXELS / u64::from(columns.max(1));
    u32::try_from(cap).unwrap_or(u32::MAX).min(MAX_IMAGE_SIDE)
}

/// Decodes one one-dimensionally coded row into `out` as a changing-element
/// list (T.4 §4.1.2).
///
/// A row is runs of alternating colour, white first, each read from the table
/// for its own colour. The leading white run may be empty — that is how a row
/// beginning with a black pixel is written, and it is the common case for any
/// image with ink at its left edge — so the colour alternates once per *run*,
/// whether or not the run wrote a pixel.
///
/// The row ends when the accumulated position reaches `columns`. Past `columns`
/// is corruption rather than a long run: a run cannot leave the row it is in,
/// and honouring one would write over the row below.
///
/// Unlike a two-dimensional row, nothing here refers to the row above, so this
/// one both starts and ends independent of its neighbours — which is the whole
/// reason T.4 keeps it, and the reason a mixed stream can resynchronise on it.
fn decode_row_1d(r: &mut BitReader, columns: u32, out: &mut Vec<u32>) -> Result<(), CcittError> {
    out.clear();
    let mut at: u32 = 0;
    let mut white = true;
    // Only the leading run may be empty, so a row cannot need more runs than it
    // has columns. That makes the loop's termination structural rather than
    // dependent on the decoded lengths being sane: a stream of empty runs stops
    // here instead of being read forever.
    let mut runs: u32 = 0;
    while at < columns {
        runs = runs.saturating_add(1);
        if runs > columns.saturating_add(2) {
            return Err(CcittError::Malformed("a row that never reaches its end"));
        }
        let run = read_run(r, white)?;
        at = at.checked_add(run).ok_or(CcittError::RunTooLong)?;
        if at > columns {
            return Err(CcittError::RunTooLong);
        }
        push_change(out, at);
        white = !white;
    }
    Ok(())
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
/// The bitmap arrives white, so only the black spans are written.
fn paint_row(out: &mut Bitmap, y: u32, changes: &[u32]) {
    let columns = out.width();
    for (start, end) in black_spans(changes, columns) {
        for x in start..end {
            out.set(x, y, 1);
        }
    }
}

/// Writes a finished row's changing-element list into `dst` as packed bits,
/// MSB first, a set bit being black.
///
/// This is the same row in the same polarity as [`paint_row`] writes, only an
/// eighth the size, which is what makes it affordable to hold every row of an
/// image whose height is not known until the last one has been read. Bits past
/// the last column are padding and stay clear, which is white.
fn pack_row(dst: &mut [u8], changes: &[u32], columns: u32) {
    for (start, end) in black_spans(changes, columns) {
        for x in start..end {
            if let Some(byte) = dst.get_mut(x as usize / 8) {
                *byte |= 0x80 >> (x % 8);
            }
        }
    }
}

/// Paints a row [`pack_row`] wrote into the bitmap.
///
/// The bitmap arrives white, so only the set bits are written, exactly as
/// [`paint_row`] writes only the black spans.
///
/// Whole white bytes are stepped over rather than tested bit by bit, which is
/// what keeps this proportional to the ink on the row rather than to its
/// width. The difference is not a micro-optimisation: a facsimile page is
/// mostly white, and a row coded as a single vertical mode code — one bit for
/// an entire white row — must not cost a pass over every pixel it covers to
/// paint, or the cheapest row in the format becomes the most expensive one to
/// store.
///
/// Bits past the last column are padding, which [`pack_row`] leaves clear, and
/// a set bit there would in any case be dropped by the bitmap's own bounds
/// check rather than wrap onto the next row.
fn unpack_row(out: &mut Bitmap, y: u32, packed: &[u8]) {
    for (index, &byte) in packed.iter().enumerate() {
        if byte == 0 {
            continue;
        }
        let base = (index as u32).saturating_mul(8);
        for bit in 0..8u32 {
            if byte & (0x80 >> bit) != 0 {
                out.set(base.saturating_add(bit), y, 1);
            }
        }
    }
}

/// The half-open column spans a changing-element list paints black.
///
/// The list starts from white, so the span from an even-indexed element to the
/// next one is black and the span after an odd-indexed one is white. An
/// odd-length list leaves its last black run open to the end of the row.
fn black_spans(changes: &[u32], columns: u32) -> impl Iterator<Item = (u32, u32)> + '_ {
    changes.chunks(2).filter_map(move |pair| {
        let &start = pair.first()?;
        Some((start, pair.get(1).copied().unwrap_or(columns).min(columns)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::jbig2::bitmap::MAX_PIXELS;

    use crate::filters::ccitt::testing::{
        bitmap_from_rows, encode_g3, encode_g3_1d, encode_g3_1d_byte_aligned,
        encode_g3_1d_with_eol, encode_g4, encode_g4_tallied, lookup, pack, push_code, push_mode,
        push_run, Layout,
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

    /// The per-side cap is what the area cap cannot do, and it is applied here
    /// rather than in the callers so that both of them get it. An image whose
    /// area is affordable can still be a shape that is not: the row state costs
    /// eight bytes per column whatever the row count, and the packed output
    /// costs a whole byte per row whatever the column count.
    #[test]
    fn a_shape_past_the_per_side_cap_is_refused_even_within_the_area_cap() {
        let over = MAX_IMAGE_SIDE + 1;
        for (width, height) in [(over, 1), (1, over), (1 << 26, 2), (2, 1 << 26)] {
            assert_eq!(
                decode(&[], &g4(width, height)),
                Err(CcittError::TooLarge { width, height }),
                "{width} x {height}",
            );
            // Each of these is inside the area cap, so that alone would let it
            // through — which is the defect the per-side cap exists for.
            assert!(u64::from(width) * u64::from(height) <= MAX_PIXELS);
        }
    }

    /// The largest shape both caps allow is still decodable, so the per-side
    /// cap has not quietly closed the door on real images.
    #[test]
    fn the_largest_shape_both_caps_allow_still_decodes() {
        let bm = decode(&[], &g4(MAX_IMAGE_SIDE, 1)).expect("a single maximal row");
        assert_eq!((bm.width(), bm.height()), (MAX_IMAGE_SIDE, 1));
        let tall = decode(&[], &g4(1, MAX_IMAGE_SIDE)).expect("a single maximal column");
        assert_eq!((tall.width(), tall.height()), (1, MAX_IMAGE_SIDE));
    }

    /// An unstated height decodes the rows once, not twice, and must land on
    /// exactly the image a stated height lands on — the buffering that replaced
    /// the counting pass is not allowed to change a pixel.
    #[test]
    fn an_inferred_height_reproduces_the_stated_height_decode() {
        // Widths on and off a byte boundary, since the buffer packs to bytes
        // and a row whose last byte is partly padding is where that shows.
        for columns in [1u32, 7, 8, 9, 16, 23] {
            let rows: Vec<String> = (0..6)
                .map(|y| {
                    (0..columns)
                        .map(|x| if (x + y) % 3 == 0 { '1' } else { '0' })
                        .collect()
                })
                .collect();
            let refs: Vec<&str> = rows.iter().map(String::as_str).collect();
            let bm = bitmap_from_rows(&refs);
            let data = encode_g4(&bm);
            let stated = decode(&data, &g4(columns, 6)).expect("stated height");
            let inferred = decode(&data, &g4(columns, 0)).expect("inferred height");
            assert_eq!(stated, bm, "{columns} columns");
            assert_eq!(inferred, stated, "{columns} columns");
        }
    }

    /// A stream holding no rows at all infers a height of zero rather than
    /// failing, which is the empty end of the same path.
    #[test]
    fn an_inferred_height_of_no_rows_is_an_empty_image() {
        let bm = decode(&[], &g4(8, 0)).expect("an empty stream");
        assert_eq!((bm.width(), bm.height()), (8, 0));
    }

    /// Pure one-dimensional parameters for a stream of the given shape.
    fn g3(columns: u32, rows: u32) -> Params {
        Params {
            columns,
            rows,
            k: 0,
            end_of_line: false,
            byte_align: false,
        }
    }

    /// Asserts that decoding `data` under `params` reproduces `bm` exactly.
    fn assert_decodes_to(bm: &Bitmap, data: &[u8], params: &Params) {
        let out = decode(data, params).expect("decode");
        assert_eq!((out.width(), out.height()), (bm.width(), bm.height()));
        for y in 0..bm.height() {
            assert_eq!(out.row(y), bm.row(y), "row {y}");
        }
    }

    #[test]
    fn round_trips_pure_one_dimensional_coding() {
        let bm = bitmap_from_rows(&["0011110000", "1111111111", "0000000000", "1010101010"]);
        assert_decodes_to(&bm, &encode_g3_1d(&bm), &g3(10, 4));
    }

    /// A row written out by hand against T.4 Table 1, so that the round trip
    /// above is anchored to the published table rather than only to the
    /// encoder beside it: white 2 is `0111`, black 4 is `011`, white 2 is
    /// `0111`. Eleven bits, zero filled: `0111_0110`, `111_00000`.
    #[test]
    fn a_hand_written_one_dimensional_row_decodes() {
        let out = decode(&[0x76, 0xE0], &g3(8, 1)).expect("decode");
        assert_eq!(out.row(0), [0, 0, 1, 1, 1, 1, 0, 0]);
    }

    /// A row beginning with black is written as an empty white run and then
    /// the black one. White 0 is `00110101`, black 4 is `011`, white 4 is
    /// `1011`: fifteen bits, `0011_0101`, `0111_0110`.
    #[test]
    fn a_one_dimensional_row_beginning_with_black_decodes() {
        let out = decode(&[0x35, 0x76], &g3(8, 1)).expect("decode");
        assert_eq!(out.row(0), [1, 1, 1, 1, 0, 0, 0, 0]);
    }

    /// A make-up code carries a multiple of 64 and the terminating code after
    /// it the remainder; the two add (T.4 §4.1.2).
    #[test]
    fn a_one_dimensional_make_up_code_adds_to_its_terminating_code() {
        let mut bits = Vec::new();
        push_run(&mut bits, true, 70);
        push_run(&mut bits, false, 10);
        let data = pack(&bits);
        let out = decode(&data, &g3(80, 1)).expect("decode");
        assert_eq!(out.row(0)[..70], [0u8; 70]);
        assert_eq!(out.row(0)[70..], [1u8; 10]);
    }

    /// A make-up code alone is a malformed row, not a run of 64: T.4 §4.1.2
    /// requires a terminating code of the same colour to close it.
    #[test]
    fn a_make_up_code_without_a_terminating_code_is_rejected() {
        let mut bits = Vec::new();
        push_code(&mut bits, lookup(true, 64));
        // Zeros are in neither table, and there are enough of them behind the
        // failure that it cannot be read as the stream simply having stopped.
        bits.extend(std::iter::repeat_n(0u8, 64));
        let data = pack(&bits);
        assert_eq!(decode(&data, &g3(80, 1)), Err(CcittError::UnknownCode));
    }

    /// A one-dimensional run cannot leave the row it is in.
    #[test]
    fn a_one_dimensional_run_past_the_row_end_is_rejected() {
        let mut bits = Vec::new();
        push_run(&mut bits, true, 1728);
        bits.extend(std::iter::repeat_n(0u8, 64));
        let data = pack(&bits);
        assert_eq!(decode(&data, &g3(10, 1)), Err(CcittError::RunTooLong));
    }

    /// Only the leading white run of a row may be empty. A stream of empty
    /// runs would otherwise be decoded forever; the row loop refuses it.
    #[test]
    fn a_one_dimensional_row_that_never_reaches_its_end_is_refused() {
        let mut bits = Vec::new();
        for _ in 0..40 {
            push_run(&mut bits, true, 0);
            push_run(&mut bits, false, 0);
        }
        bits.extend(std::iter::repeat_n(0u8, 64));
        let data = pack(&bits);
        assert!(matches!(
            decode(&data, &g3(10, 1)),
            Err(CcittError::Malformed(_)),
        ));
    }

    /// With `/K` above zero each row carries a bit saying how it is coded, so
    /// a reference row produced one-dimensionally has to serve a
    /// two-dimensional row after it.
    #[test]
    fn mixed_coding_follows_the_per_row_bit() {
        let bm = bitmap_from_rows(&["0011110000", "0011110000", "1100001111", "1100001111"]);
        let layout = Layout {
            end_of_line: true,
            tagged: true,
            ..Layout::default()
        };
        let data = encode_g3(&bm, layout, &[true, false, true, false]);
        let params = Params {
            columns: 10,
            rows: 4,
            k: 4,
            end_of_line: true,
            byte_align: false,
        };
        assert_decodes_to(&bm, &data, &params);
    }

    /// The tag bit is a property of mixed coding, not of end-of-line patterns:
    /// a producer may write `/K` above zero with `/EndOfLine` false, and the
    /// bit still precedes every row.
    #[test]
    fn mixed_coding_without_end_of_line_patterns_follows_the_tag_bit() {
        let bm = bitmap_from_rows(&["1100001111", "1100001111", "0011110000"]);
        let layout = Layout {
            tagged: true,
            ..Layout::default()
        };
        let data = encode_g3(&bm, layout, &[true, false, true]);
        let params = Params {
            columns: 10,
            rows: 3,
            k: 2,
            end_of_line: false,
            byte_align: false,
        };
        assert_decodes_to(&bm, &data, &params);
    }

    #[test]
    fn end_of_line_patterns_are_consumed() {
        let bm = bitmap_from_rows(&["0011110000", "1111000011"]);
        let params = Params {
            columns: 10,
            rows: 2,
            k: 0,
            end_of_line: true,
            byte_align: false,
        };
        assert_decodes_to(&bm, &encode_g3_1d_with_eol(&bm), &params);
    }

    /// An end-of-line pattern is legal even when `/EndOfLine` says otherwise:
    /// producers disagree about that flag, so the pattern is recognised
    /// wherever it appears rather than only where it was promised.
    #[test]
    fn an_unexpected_end_of_line_is_tolerated() {
        let bm = bitmap_from_rows(&["0011110000", "1111000011"]);
        assert_decodes_to(&bm, &encode_g3_1d_with_eol(&bm), &g3(10, 2));
    }

    /// Fill is a run of zero bits before an end-of-line pattern, padding a row
    /// out to a minimum transmission time (T.4 §4.1.1). It carries nothing.
    #[test]
    fn fill_bits_before_an_end_of_line_are_skipped() {
        let bm = bitmap_from_rows(&["0011110000", "1111000011", "0000111100"]);
        let layout = Layout {
            end_of_line: true,
            fill_bits: 21,
            ..Layout::default()
        };
        let params = Params {
            columns: 10,
            rows: 3,
            k: 0,
            end_of_line: true,
            byte_align: false,
        };
        assert_decodes_to(&bm, &encode_g3(&bm, layout, &[]), &params);
    }

    #[test]
    fn byte_aligned_rows_round_trip() {
        let bm = bitmap_from_rows(&["0011110000", "1111000011", "0000111100"]);
        let params = Params {
            columns: 10,
            rows: 3,
            k: 0,
            end_of_line: false,
            byte_align: true,
        };
        assert_decodes_to(&bm, &encode_g3_1d_byte_aligned(&bm), &params);
    }

    /// With `/Rows` of 0 the height is however many rows the data holds.
    #[test]
    fn an_unknown_row_count_decodes_until_the_data_ends() {
        let bm = bitmap_from_rows(&["0011110000", "1111000011", "0000111100", "1010101010"]);
        assert_decodes_to(&bm, &encode_g3_1d(&bm), &g3(10, 0));
    }

    /// The same, two-dimensionally: the terminator a JBIG2 MMR region may end
    /// with is a run of zero bits, which is not the start of any row.
    #[test]
    fn an_unknown_row_count_works_for_two_dimensional_coding() {
        let bm = bitmap_from_rows(&["0011110000", "0011111000", "0000110000", "1111111111"]);
        assert_decodes_to(&bm, &encode_g4(&bm), &g4(10, 0));
    }

    /// A stream that decodes no rows at all is an image of no height rather
    /// than an error.
    #[test]
    fn an_unknown_row_count_over_no_data_is_an_empty_image() {
        let out = decode(&[], &g3(10, 0)).expect("decode");
        assert_eq!((out.width(), out.height()), (10, 0));
    }

    /// Two end-of-line patterns with no row between them end the data: the
    /// end-of-facsimile block of T.6 §2.2.1, and the opening pair of T.4's
    /// return to control.
    #[test]
    fn an_end_of_facsimile_block_ends_the_image() {
        let bm = bitmap_from_rows(&["0011110000"; 3]);
        for trailing_eols in [2, 6] {
            let layout = Layout {
                trailing_eols,
                ..Layout::default()
            };
            let data = encode_g3(&bm, layout, &[]);
            let out = decode(&data, &g3(10, 0)).expect("decode");
            assert_eq!(out.height(), 3, "{trailing_eols} trailing patterns");
        }
    }

    /// A stream may simply be followed by zero bytes: T.88 §6.2.6 lets a JBIG2
    /// MMR region end that way, and a buffer padded to a byte or a boundary
    /// ends that way by accident. Twelve zero bits are not a row, and reading
    /// them as one fails an image that decoded perfectly — the zeros are too
    /// many to be dismissed as the data merely running out mid-code.
    #[test]
    fn zero_bytes_after_the_last_row_end_the_image_rather_than_failing_it() {
        let bm = bitmap_from_rows(&["0011110000", "1100001111", "0000111100"]);
        let mut data = encode_g4(&bm);
        data.extend_from_slice(&[0x00; 4]);
        assert_decodes_to(&bm, &data, &g4(10, 3));
        assert_decodes_to(&bm, &data, &g4(10, 0));
    }

    /// Fill exists only before an end-of-line pattern, so a stream that has no
    /// such patterns has no fill either, and a long run of zeros in one is the
    /// end of its image rather than something to step over. A decoder that
    /// stepped over it would resume on whatever followed and decode that as
    /// rows.
    #[test]
    fn a_long_run_of_zeros_ends_a_stream_that_cannot_contain_fill() {
        let bm = bitmap_from_rows(&["0011110000", "1100001111"]);
        let mut data = encode_g4(&bm);
        data.extend_from_slice(&[0x00, 0x00, 0x00, 0xFF, 0xFF]);
        assert_decodes_to(&bm, &data, &g4(10, 0));
    }

    /// A terminator ends the image even when bytes follow it, and only then is
    /// it doing any work: a stream may be padded out, or carry data the image
    /// does not, and a decoder that reads on turns those bytes into rows.
    #[test]
    fn data_after_a_terminator_is_not_decoded_as_rows() {
        let bm = bitmap_from_rows(&["0011110000", "1100001111"]);
        let layout = Layout {
            trailing_eols: 2,
            ..Layout::default()
        };
        let mut data = encode_g3(&bm, layout, &[]);
        data.extend_from_slice(&[0xFF; 4]);
        assert_decodes_to(&bm, &data, &g3(10, 0));
    }

    /// The same, with mixed coding: the terminator has to be recognised
    /// *before* the tag bit is read, or the read lands inside the second
    /// pattern and everything after it is decoded as image.
    #[test]
    fn a_bare_end_of_facsimile_block_ends_a_mixed_stream() {
        let bm = bitmap_from_rows(&["0011110000", "1100001111"]);
        let layout = Layout {
            tagged: true,
            trailing_eols: 2,
            ..Layout::default()
        };
        let mut data = encode_g3(&bm, layout, &[true, false]);
        data.extend_from_slice(&[0xFF; 4]);
        let params = Params { k: 4, ..g3(10, 0) };
        assert_decodes_to(&bm, &data, &params);
    }

    /// T.4 §4.2.3 writes the return to control of a mixed stream as six
    /// end-of-line patterns each carrying a tag bit, so no two of them are
    /// adjacent and the count alone cannot recognise it.
    #[test]
    fn a_tagged_return_to_control_ends_a_mixed_stream() {
        let bm = bitmap_from_rows(&["0011110000", "1100001111"]);
        let layout = Layout {
            end_of_line: true,
            tagged: true,
            trailing_eols: 6,
            trailing_tags: true,
            ..Layout::default()
        };
        let mut data = encode_g3(&bm, layout, &[true, false]);
        data.extend_from_slice(&[0xFF; 4]);
        let params = Params {
            k: 4,
            end_of_line: true,
            ..g3(10, 0)
        };
        assert_decodes_to(&bm, &data, &params);
    }

    /// A terminator before the stated row count leaves the rest of the image
    /// white rather than failing it.
    #[test]
    fn an_end_of_block_leaves_the_stated_rows_after_it_white() {
        let bm = bitmap_from_rows(&["1111111111"; 3]);
        let layout = Layout {
            trailing_eols: 2,
            ..Layout::default()
        };
        let out = decode(&encode_g3(&bm, layout, &[]), &g3(10, 5)).expect("decode");
        assert_eq!(out.row(2), [1; 10], "the last coded row survived");
        assert_eq!(
            out.row(3),
            [0; 10],
            "the rows past the terminator are white"
        );
    }

    /// The height inferred for a stream that does not state one is capped by
    /// both bounds at once, so no stream can ask for an unbounded row count and
    /// none can ask for a sliver of an image either.
    #[test]
    fn an_inferred_height_cannot_exceed_either_cap() {
        // Narrow enough that the area cap alone would allow 2^27 rows.
        assert_eq!(inferred_row_cap(1), MAX_IMAGE_SIDE);
        // Wide enough that the area cap is the binding one.
        let wide = MAX_IMAGE_SIDE;
        assert_eq!(
            u64::from(inferred_row_cap(wide)) * u64::from(wide),
            MAX_PIXELS
        );
        assert!(inferred_row_cap(wide) < MAX_IMAGE_SIDE);
        // A width past the per-side cap never reaches this, but were it to, the
        // area cap must not divide its way to a usable row count.
        assert_eq!(inferred_row_cap(u32::MAX), 0);
    }

    /// Every byte string is either an image or an error, never a panic and
    /// never a hang — under every combination of the layout parameters, since
    /// each selects a different path through the row loop.
    #[test]
    fn arbitrary_bytes_never_panic_or_hang() {
        let mut state = 0xFEED_FACEu32;
        let layouts = [
            g4(32, 16),
            g3(32, 16),
            Params { k: 4, ..g3(32, 16) },
            Params {
                end_of_line: true,
                byte_align: true,
                ..g3(32, 16)
            },
            g4(32, 0),
            g3(32, 0),
            Params { k: 4, ..g3(32, 0) },
        ];
        for _ in 0..2_000 {
            let len = (state % 97) as usize;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            for params in &layouts {
                if let Ok(bm) = decode(&data, params) {
                    assert_eq!(bm.width(), params.columns);
                    if params.rows != 0 {
                        assert_eq!(bm.height(), params.rows);
                    }
                }
            }
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        }
    }
}
