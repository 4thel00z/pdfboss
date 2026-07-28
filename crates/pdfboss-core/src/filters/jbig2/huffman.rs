//! Huffman tables (T.88 Annex B).
//!
//! The Huffman variant of JBIG2 codes its integers with prefix codes rather
//! than arithmetically. A table is a list of *lines* (B.2): each line owns a
//! prefix code and a run of values, and the bits following the prefix say
//! which value of that run was meant. Three lines are special — a *lower* and
//! an *upper* range line closing the two open ends, and an optional
//! out-of-band line that codes no value at all — and the rest are ordinary
//! ranges laid end to end from HTLOW upwards.
//!
//! Where a table comes from does not change how it decodes, so one [`Table`]
//! type serves all three sources: the fifteen standard tables of B.5, the
//! custom tables a code table segment carries (7.4.13, decoded by B.2), and —
//! once the text region parser reaches it — the symbol ID table assembled in
//! 7.4.3.1.7. What they share is B.3, the canonical assignment that turns a
//! list of prefix *lengths* into actual codes; that is the one algorithm this
//! module really implements, and [`assign_prefix_codes`] is the single place
//! it lives.
//!
//! The bit cursor is the facsimile module's [`BitReader`], not a second one
//! written here. It is the same MSB-first cursor over a borrowed slice that
//! this format needs, and sharing it matters beyond saving a type: a Huffman
//! symbol dictionary hands the very same byte stream to the MMR decoder for
//! its height class collective bitmap (6.5.9), so having one notion of "where
//! we are in the bits" is what makes that handoff exact.

// Nothing outside this module's own tests reaches any of it yet. The first
// caller is phase B of the Huffman plan (`SDHUFF = 1` symbol dictionaries) and
// the second is phase C (`SBHUFF = 1` text regions); until those land the
// module is dead by construction rather than by oversight, and a narrower
// attribute would have to be repeated on every item in the file.
#![allow(dead_code)]

use super::budget::Budget;
use super::reader::Reader;
use super::Jbig2Error;
use crate::filters::ccitt::bits::BitReader;

/// The longest prefix code this decoder will match.
///
/// B.3 puts no ceiling on PREFLEN, and a custom table declares it in up to
/// eight bits (B.2.1), so a hostile stream can ask for a 255-bit code. A code
/// longer than a machine word cannot be accumulated, let alone matched, and no
/// table that codes anything needs one: 32 distinct lengths already admit more
/// lines than any table this decoder will accept.
const MAX_PREFIX_LEN: u8 = 32;

/// The widest range field a table line may declare.
///
/// The two escape lines are given RANGELEN 32 by B.2 steps 7 and 9, and B.4
/// step 2 reads RANGELEN bits into a single offset, so 32 is both the largest
/// the standard uses and the largest that fits the value it is added to.
const MAX_RANGE_LEN: u8 = 32;

/// The most ordinary range lines one custom table may declare.
///
/// B.2 step 5 repeats until CURRANGELOW reaches HTHIGH, advancing by
/// 2^RANGELEN each time. With RANGELEN 0 and HTHIGH at `i32::MAX`, thirteen
/// bytes of header ask for two billion iterations, and the segment need not
/// carry a single bit of the data those lines would read. So the count is
/// bounded from the header *before* the loop is entered rather than discovered
/// inside it. The figure is generous: the largest standard table has 22 lines.
const MAX_TABLE_LINES: u64 = 1 << 12;

/// What one declared table line costs against the embedded stream's work
/// budget.
///
/// A table is not spent when it is parsed. A code table segment's table is
/// held for the rest of the segment walk, because any later region may refer
/// to it, so what is being paid for here is a line's existence rather than its
/// decoding. Bounding one segment's line count bounds one table; Annex D.3
/// puts no limit on how many segments an embedded stream holds, so the total
/// has to be tied to the single allowance the stream has. At this rate
/// [`MAX_WORK`](super::budget::MAX_WORK) buys a little over a million lines,
/// which is 256 tables of the maximum size and a few tens of megabytes of
/// bookkeeping if a stream insists on all of them.
const LINE_COST: u64 = 256;

/// What a table line does with the offset that follows its prefix (B.4).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Kind {
    /// HTVAL = RANGELOW + HTOFFSET (B.4 step 5).
    ///
    /// The upper range table line is this as well, being the same rule with
    /// RANGELEN 32 and RANGELOW set to HTHIGH (B.2 step 9).
    Normal,
    /// HTVAL = RANGELOW − HTOFFSET (B.4 step 4): the lower range table line,
    /// whose RANGELOW is HTLOW − 1 and whose offsets therefore run *downwards*
    /// (B.2 step 7). It is the only line that subtracts.
    Lower,
    /// The out-of-band line (B.2 step 10). No range is associated with it, so
    /// no offset bits are read, and decoding it yields OOB rather than a
    /// number.
    Oob,
}

/// One line of a Huffman table (B.2).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct Line {
    /// PREFLEN. Zero means the line is never used and is assigned no code
    /// (B.3); the standard tables print such lines by omitting them.
    pref_len: u8,
    /// RANGELEN: how many bits follow the prefix. Unused by [`Kind::Oob`].
    range_len: u8,
    /// RANGELOW: the value the offset is measured from, upwards for
    /// [`Kind::Normal`] and downwards for [`Kind::Lower`].
    range_low: i32,
    kind: Kind,
}

impl Line {
    /// An ordinary range line covering `range_low` and the `2^range_len − 1`
    /// values above it.
    const fn normal(pref_len: u8, range_len: u8, range_low: i32) -> Line {
        Line {
            pref_len,
            range_len,
            range_low,
            kind: Kind::Normal,
        }
    }

    /// The lower range table line (B.2 step 7). `range_low` is HTLOW − 1.
    const fn lower(pref_len: u8, range_low: i32) -> Line {
        Line {
            pref_len,
            range_len: 32,
            range_low,
            kind: Kind::Lower,
        }
    }

    /// The upper range table line (B.2 step 9). `range_low` is HTHIGH.
    const fn upper(pref_len: u8, range_low: i32) -> Line {
        Line {
            pref_len,
            range_len: 32,
            range_low,
            kind: Kind::Normal,
        }
    }

    /// The out-of-band table line (B.2 step 10).
    const fn oob(pref_len: u8) -> Line {
        Line {
            pref_len,
            range_len: 0,
            range_low: 0,
            kind: Kind::Oob,
        }
    }
}

/// What B.3 assigned to one prefix length, in the form a match needs.
///
/// Because B.3 hands out consecutive codes to the lines of a given length in
/// table order, a matched code identifies its line by subtraction: the codes
/// of length `L` occupy `first_code ..= first_code + count − 1`, and the line
/// that owns the `k`-th of them is the `k`-th entry of [`Table::order`] from
/// `first_index`. That is what keeps matching O(1) per bit instead of a scan
/// over every line.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct LengthSlot {
    /// The smallest code of this length, or 0 when `count` is 0.
    first_code: u32,
    /// Where this length's run begins in [`Table::order`].
    first_index: u32,
    /// How many lines carry this length.
    count: u32,
}

/// A Huffman table: its lines, and the prefix codes B.3 assigned to them.
#[derive(Clone, Debug)]
pub(crate) struct Table {
    /// The lines in the order the table declares them. That order is not
    /// decorative: B.3 breaks ties between equal lengths by it, so two tables
    /// with the same multiset of lengths in a different order are different
    /// codes.
    lines: Vec<Line>,
    /// Indices into `lines`, grouped by prefix length and, within a length, in
    /// table order. Lines with PREFLEN 0 do not appear.
    order: Vec<u32>,
    /// One slot per prefix length, indexed by the length itself; slot 0 is
    /// never used because PREFLEN 0 means "no code".
    lengths: [LengthSlot; MAX_PREFIX_LEN as usize + 1],
    /// LENMAX from B.3 step 2: the longest length any line carries.
    max_len: u8,
}

impl Table {
    /// Builds a table from its lines, assigning the prefix codes with B.3.
    ///
    /// Every rejection this decoder makes about the *shape* of a table happens
    /// here, so that the three ways a table can arrive — standard, custom
    /// segment, symbol ID list — cannot each grow their own idea of what is
    /// acceptable.
    fn new(lines: Vec<Line>) -> Result<Table, Jbig2Error> {
        for line in &lines {
            if line.range_len > MAX_RANGE_LEN {
                return Err(Jbig2Error::Malformed("Huffman range longer than a value"));
            }
        }

        let pref_lens: Vec<u8> = lines.iter().map(|line| line.pref_len).collect();
        let codes = assign_prefix_codes(&pref_lens)?;

        let mut lengths = [LengthSlot::default(); MAX_PREFIX_LEN as usize + 1];
        let mut order: Vec<u32> = Vec::new();
        let mut max_len = 0u8;
        for len in 1..=MAX_PREFIX_LEN {
            let slot = &mut lengths[usize::from(len)];
            slot.first_index = order.len() as u32;
            for (index, line) in lines.iter().enumerate() {
                if line.pref_len != len {
                    continue;
                }
                if slot.count == 0 {
                    slot.first_code = codes[index];
                }
                slot.count += 1;
                order.push(index as u32);
                max_len = len;
            }
        }

        // A table in which every line is unused can never decode anything, and
        // a caller handed one would ask it for a value in a loop and be told
        // "no such code" forever. Refusing it once, here, is cheaper than
        // making every caller defend against it.
        if order.is_empty() {
            return Err(Jbig2Error::Malformed("Huffman table assigns no codes"));
        }

        Ok(Table {
            lines,
            order,
            lengths,
            max_len,
        })
    }

    /// Whether this table can code an out-of-band value: HTOOB, in the terms
    /// of B.2.1.
    ///
    /// The selectors of 7.4.2.1.1 and 7.4.3.1.2 each require the table bound
    /// to them to be OOB-capable or not, so a custom table is checked against
    /// its slot before anything decodes with it.
    pub(crate) fn has_oob(&self) -> bool {
        self.lines.iter().any(|line| line.kind == Kind::Oob)
    }

    /// Decodes one value (B.4).
    ///
    /// `Ok(None)` is OOB. That is the signal which closes a height class in
    /// 6.5.5 and a strip in 6.4.5, so a caller keeps the same
    /// `while let Some(v)` shape it has with the arithmetic integer decoder of
    /// Annex A.
    ///
    /// **Running out of bits is [`Jbig2Error::Truncated`], never OOB.** The
    /// arithmetic decoder behaves the other way round: past the end of its
    /// data T.88 E.3.4 synthesises bits forever and the integer procedure
    /// settles into returning OOB, so a loop written against that habit reads
    /// a truncated stream as a well-formed short one. Nothing in a Huffman
    /// stream means "the data ended", so the end of the data must be an error
    /// or a truncated segment decodes to a plausible wrong answer.
    pub(crate) fn decode(&self, bits: &mut BitReader) -> Result<Option<i32>, Jbig2Error> {
        let line = self.matched_line(bits)?;
        if line.kind == Kind::Oob {
            // B.4 step 3. There is no range on this line, so step 2 reads
            // nothing for it.
            return Ok(None);
        }
        let offset = i64::from(read_bits(bits, line.range_len)?);
        let low = i64::from(line.range_low);
        // B.4 steps 4 and 5. The lower range table line is the only one that
        // counts down from its RANGELOW.
        let value = if line.kind == Kind::Lower {
            low - offset
        } else {
            low + offset
        };
        // B.2: the smallest value a table described by this Recommendation can
        // encode is −2 147 483 648 and the largest is 2 147 483 647. A 32-bit
        // offset on an escape line reaches past both ends, and a value outside
        // them is not something the table was able to mean.
        match i32::try_from(value) {
            Ok(value) => Ok(Some(value)),
            Err(_) => Err(Jbig2Error::Malformed(
                "Huffman value outside the codable range",
            )),
        }
    }

    /// Reads bits until they spell one of the assigned codes (B.4 step 1).
    ///
    /// Canonical assignment is what makes this a subtraction rather than a
    /// search: after reading `L` bits the accumulator either falls inside the
    /// block of codes of length `L` or it does not, and one comparison
    /// settles it.
    fn matched_line(&self, bits: &mut BitReader) -> Result<Line, Jbig2Error> {
        let mut code = 0u32;
        for len in 1..=self.max_len {
            let bit = bits.read_bit().ok_or(Jbig2Error::Truncated)?;
            code = (code << 1) | u32::from(bit);
            let slot = self.lengths[usize::from(len)];
            // Wrapping rather than comparing twice: a code below the block
            // wraps to something far above `count`, and a length with no lines
            // has `count` 0 and so matches nothing.
            let offset = code.wrapping_sub(slot.first_code);
            if offset < slot.count {
                let position = slot.first_index.saturating_add(offset) as usize;
                // `new` fills `order` with indices into `lines` and counts only
                // what it wrote, so both lookups hold; falling through to the
                // error rather than indexing keeps that an assumption about
                // this file and not about the input.
                return self
                    .order
                    .get(position)
                    .and_then(|index| self.lines.get(*index as usize))
                    .copied()
                    .ok_or(Jbig2Error::Malformed("no such Huffman code"));
            }
        }
        Err(Jbig2Error::Malformed("no such Huffman code"))
    }
}

/// Assigns a prefix code to each line, given the lines' prefix lengths (B.3).
///
/// Returns CODES: one entry per input length, holding the code in its low
/// `PREFLEN` bits. Entries whose length is 0 are left at 0 and mean nothing —
/// B.3 assigns those lines no code at all.
///
/// Two failures B.3 does not discuss are rejected here, because both are
/// reachable from a stream and neither is visible afterwards.
///
/// A length past [`MAX_PREFIX_LEN`] cannot be accumulated by a matcher, and
/// HTPS is wide enough for a custom table to declare 255.
///
/// An over-subscribed set is worse, because it is silent: if the codes of one
/// length run past what that length can express, the assignment wraps and two
/// lines end up with the same code, after which which one a stream decodes to
/// depends on the order they were scanned in. The test is made on the code
/// *about to be assigned*, not on the counter afterwards — a table whose
/// longest length exactly fills its code space is complete and perfectly
/// legal, and B.1, B.11 and B.14 are all of that shape.
fn assign_prefix_codes(pref_lens: &[u8]) -> Result<Vec<u32>, Jbig2Error> {
    // B.3 step 1: histogram the lengths.
    let mut len_count = [0u64; MAX_PREFIX_LEN as usize + 1];
    for &len in pref_lens {
        if len > MAX_PREFIX_LEN {
            return Err(Jbig2Error::Malformed(
                "Huffman prefix longer than a code word",
            ));
        }
        len_count[usize::from(len)] += 1;
    }

    // B.3 step 2. LENCOUNT[0] is cleared *after* the histogram is built: a
    // PREFLEN of 0 marks a line that is never used, and leaving those counted
    // would push FIRSTCODE[1] along by one place for each of them and shift
    // every real code in the table.
    let len_max = (1..=MAX_PREFIX_LEN)
        .rev()
        .find(|&len| len_count[usize::from(len)] > 0)
        .unwrap_or(0);
    len_count[0] = 0;

    // B.3 step 3.
    let mut codes = vec![0u32; pref_lens.len()];
    let mut first_code = 0u64;
    for cur_len in 1..=len_max {
        first_code = first_code
            .saturating_add(len_count[usize::from(cur_len) - 1])
            .saturating_mul(2);
        let mut cur_code = first_code;
        let space = 1u64 << cur_len;
        for (index, &len) in pref_lens.iter().enumerate() {
            if len != cur_len {
                continue;
            }
            if cur_code >= space {
                return Err(Jbig2Error::Malformed(
                    "Huffman code lengths over-subscribe the code space",
                ));
            }
            // In range by the test above, so the narrowing is exact.
            codes[index] = cur_code as u32;
            cur_code += 1;
        }
    }
    Ok(codes)
}

/// Reads `n` bits as an unsigned integer, most significant first.
///
/// Unlike [`BitReader::peek`], which zero-fills past the end so that a
/// facsimile code word in the last byte can still be matched, this refuses to
/// invent the bits it was asked for: every caller here is reading a field
/// whose width the stream itself declared, and a field that is not there is a
/// truncated segment.
///
/// `n` is clamped to [`MAX_RANGE_LEN`], which no caller exceeds — the prefix
/// and range size fields are at most 8 bits wide (B.2.1) and RANGELEN is
/// checked when the table is built — so the clamp is only there to keep the
/// shift inside the reader in range without a branch.
fn read_bits(bits: &mut BitReader, n: u8) -> Result<u32, Jbig2Error> {
    let n = u32::from(n.min(MAX_RANGE_LEN));
    if bits.remaining() < n as usize {
        return Err(Jbig2Error::Truncated);
    }
    let value = bits.peek(n);
    bits.skip(n);
    Ok(value)
}

/// Decodes a code table segment into a table.
///
/// 7.4.13 gives this segment type no syntax of its own — "a code table
/// segment's syntax is described in Annex B" — so this is B.2 steps 1 to 11
/// and nothing else: the flags byte, the two signed bounds, the ordinary range
/// lines, then the lower, upper and optional out-of-band lines, and finally
/// the code assignment of B.3.
///
/// `budget` is the embedded stream's allowance of decoding work, the same one
/// the region decoders draw on. It is charged from the header, before the line
/// loop, for the reason [`MAX_TABLE_LINES`] exists: the number of lines a
/// table declares is a function of fields a hostile stream chooses freely, and
/// the segment need not carry the bits those lines would read.
pub(crate) fn parse_table_segment(data: &[u8], budget: &mut Budget) -> Result<Table, Jbig2Error> {
    let mut r = Reader::new(data);

    // B.2 step 1, whose field is B.2.1.
    let flags = r.u8()?;
    if flags & 0x80 != 0 {
        return Err(Jbig2Error::Malformed(
            "reserved bit set in the Huffman table flags",
        ));
    }
    let htoob = flags & 1 == 1;
    let htps = ((flags >> 1) & 0x07) + 1;
    let htrs = ((flags >> 4) & 0x07) + 1;

    // B.2 steps 2 and 3: two signed four-byte fields (B.2.2, B.2.3). HTHIGH is
    // one larger than the upper bound of the last ordinary line.
    let htlow = r.u32()? as i32;
    let hthigh = r.u32()? as i32;

    // Two header quantities bound the loop before it starts. Every line
    // advances CURRANGELOW by at least one, 2^0 being the smallest step, so
    // the loop cannot outrun the span HTLOW..HTHIGH; and step 5 decodes a line
    // before it first tests, so even an empty span yields one line.
    let span = i64::from(hthigh).saturating_sub(i64::from(htlow));
    let max_lines = span.clamp(1, MAX_TABLE_LINES as i64) as u64;

    // The charge is bounded a third way, by the bits the segment carries:
    // every line reads HTPS + HTRS of them, so a header promising thousands of
    // lines out of a handful of bytes is charged for the handful. The three
    // trailing lines of steps 6 to 10 are charged alongside them.
    let per_line = u64::from(htps) + u64::from(htrs);
    let affordable = (r.remaining() as u64).saturating_mul(8) / per_line;
    budget.charge(LINE_COST.saturating_mul(max_lines.min(affordable).saturating_add(3)))?;

    let mut bits = BitReader::new(r.rest());
    let mut lines: Vec<Line> = Vec::new();
    // B.2 step 4.
    let mut cur_range_low = i64::from(htlow);
    let mut covered = false;
    for _ in 0..max_lines {
        // B.2 steps 5a and 5b. HTPS and HTRS are at most 8 (B.2.1), so both
        // fields fit a byte.
        let pref_len = read_bits(&mut bits, htps)? as u8;
        let range_len = read_bits(&mut bits, htrs)? as u8;
        if range_len > MAX_RANGE_LEN {
            // Checked here as well as in `Table::new`, because the step below
            // shifts by it.
            return Err(Jbig2Error::Malformed("Huffman range longer than a value"));
        }
        // B.2 step 5c. CURRANGELOW is still inside the codable range whenever
        // it is used: it starts at HTLOW and the moment it passes `i32::MAX`
        // it has also passed HTHIGH, which ends the loop.
        let range_low = i32::try_from(cur_range_low)
            .map_err(|_| Jbig2Error::Malformed("Huffman table line outside the codable range"))?;
        lines.push(Line::normal(pref_len, range_len, range_low));
        cur_range_low = cur_range_low.saturating_add(1i64 << range_len);
        // B.2 step 5d.
        if cur_range_low >= i64::from(hthigh) {
            covered = true;
            break;
        }
    }
    if !covered {
        // Only reachable when the cap, rather than the span, ended the loop:
        // after `span` lines CURRANGELOW has necessarily reached HTHIGH.
        return Err(Jbig2Error::Malformed("too many Huffman table lines"));
    }

    // B.2 steps 6 and 7: the lower range table line, counting down from
    // HTLOW − 1.
    let low_pref_len = read_bits(&mut bits, htps)? as u8;
    let low_range_low = match i32::try_from(i64::from(htlow) - 1) {
        Ok(value) => value,
        // HTLOW − 1 leaves the codable range only when HTLOW is `i32::MIN`, in
        // which case this line could encode nothing B.2 admits. That is only
        // an error if the table means to use it, which a PREFLEN of 0 says it
        // does not.
        Err(_) if low_pref_len == 0 => i32::MIN,
        Err(_) => {
            return Err(Jbig2Error::Malformed(
                "Huffman lower range line below the codable range",
            ))
        }
    };
    lines.push(Line::lower(low_pref_len, low_range_low));

    // B.2 steps 8 and 9: the upper range table line, counting up from HTHIGH.
    let high_pref_len = read_bits(&mut bits, htps)? as u8;
    lines.push(Line::upper(high_pref_len, hthigh));

    // B.2 step 10.
    if htoob {
        let oob_pref_len = read_bits(&mut bits, htps)? as u8;
        lines.push(Line::oob(oob_pref_len));
    }

    // B.2 step 11.
    Table::new(lines)
}

/// One of the fifteen standard tables of B.5, numbered as the specification
/// numbers them: `standard(1)` is Table B.1, standard Huffman table A.
///
/// A selector field in a symbol dictionary or text region header picks one of
/// these by number (7.4.2.1.1, 7.4.3.1.2), which is why the accessor is by
/// number rather than by a name per table.
///
/// The arrays below are constants, and this module's tests re-derive every one
/// of their prefix codes and compare them against the bit strings the
/// specification prints, so the only failure a caller can reach here is a
/// `number` outside 1 to 15 — a mis-mapped selector rather than a bad stream.
pub(crate) fn standard(number: u8) -> Result<Table, Jbig2Error> {
    let lines: &[Line] = match number {
        1 => &TABLE_B1,
        2 => &TABLE_B2,
        3 => &TABLE_B3,
        4 => &TABLE_B4,
        5 => &TABLE_B5,
        6 => &TABLE_B6,
        7 => &TABLE_B7,
        8 => &TABLE_B8,
        9 => &TABLE_B9,
        10 => &TABLE_B10,
        11 => &TABLE_B11,
        12 => &TABLE_B12,
        13 => &TABLE_B13,
        14 => &TABLE_B14,
        15 => &TABLE_B15,
        _ => return Err(Jbig2Error::Malformed("no such standard Huffman table")),
    };
    Table::new(lines.to_vec())
}

// The fifteen tables of B.5, one source line per table line, in the order the
// specification prints them. That order carries meaning — B.3 breaks ties
// between equal prefix lengths by it — so the arrays are laid out to be read
// beside the page rather than sorted into anything tidier.
//
// The arguments are PREFLEN, RANGELEN and RANGELOW, which are the second,
// third and first columns of the printed tables; `Line::lower` and
// `Line::upper` take no RANGELEN because B.2 steps 7 and 9 fix it at 32, and
// `Line::oob` takes only PREFLEN because that line has no range at all. Where
// the specification omits a lower or upper range line it is saying the line
// has PREFLEN 0 and is never used, and an unused line takes no part in the
// code assignment, so those are omitted here too.
//
// RANGELOW is the low end of the printed VAL column, and for the lower range
// line it is HTLOW − 1: Table B.3 runs from −256, so its lower line carries
// −257 and codes (−257 − VAL), exactly as the Encoding column says.

/// Table B.1, standard Huffman table A. HTOOB is 0.
#[rustfmt::skip]
static TABLE_B1: [Line; 4] = [
    Line::normal(1, 4, 0),
    Line::normal(2, 8, 16),
    Line::normal(3, 16, 272),
    Line::upper(3, 65808),
];

/// Table B.2, standard Huffman table B. HTOOB is 1.
#[rustfmt::skip]
static TABLE_B2: [Line; 7] = [
    Line::normal(1, 0, 0),
    Line::normal(2, 0, 1),
    Line::normal(3, 0, 2),
    Line::normal(4, 3, 3),
    Line::normal(5, 6, 11),
    Line::upper(6, 75),
    Line::oob(6),
];

/// Table B.3, standard Huffman table C. HTOOB is 1.
#[rustfmt::skip]
static TABLE_B3: [Line; 9] = [
    Line::normal(8, 8, -256),
    Line::normal(1, 0, 0),
    Line::normal(2, 0, 1),
    Line::normal(3, 0, 2),
    Line::normal(4, 3, 3),
    Line::normal(5, 6, 11),
    Line::lower(8, -257),
    Line::upper(7, 75),
    Line::oob(6),
];

/// Table B.4, standard Huffman table D. HTOOB is 0.
#[rustfmt::skip]
static TABLE_B4: [Line; 6] = [
    Line::normal(1, 0, 1),
    Line::normal(2, 0, 2),
    Line::normal(3, 0, 3),
    Line::normal(4, 3, 4),
    Line::normal(5, 6, 12),
    Line::upper(5, 76),
];

/// Table B.5, standard Huffman table E. HTOOB is 0.
#[rustfmt::skip]
static TABLE_B5: [Line; 8] = [
    Line::normal(7, 8, -255),
    Line::normal(1, 0, 1),
    Line::normal(2, 0, 2),
    Line::normal(3, 0, 3),
    Line::normal(4, 3, 4),
    Line::normal(5, 6, 12),
    Line::lower(7, -256),
    Line::upper(6, 76),
];

/// Table B.6, standard Huffman table F. HTOOB is 0.
#[rustfmt::skip]
static TABLE_B6: [Line; 14] = [
    Line::normal(5, 10, -2048),
    Line::normal(4, 9, -1024),
    Line::normal(4, 8, -512),
    Line::normal(4, 7, -256),
    Line::normal(5, 6, -128),
    Line::normal(5, 5, -64),
    Line::normal(4, 5, -32),
    Line::normal(2, 7, 0),
    Line::normal(3, 7, 128),
    Line::normal(3, 8, 256),
    Line::normal(4, 9, 512),
    Line::normal(4, 10, 1024),
    Line::lower(6, -2049),
    Line::upper(6, 2048),
];

/// Table B.7, standard Huffman table G. HTOOB is 0.
///
/// The printed VAL column gives the sixth line as −64 . . . −32, which
/// overlaps the line below it; RANGELEN 5 covers 32 values, and the Encoding
/// column reads 11011 + (VAL + 64) in 5 bits, so the line is −64 . . . −33 and
/// RANGELOW is −64. Only RANGELOW is transcribed here, so the discrepancy in
/// the printed upper bound does not reach the code.
#[rustfmt::skip]
static TABLE_B7: [Line; 15] = [
    Line::normal(4, 9, -1024),
    Line::normal(3, 8, -512),
    Line::normal(4, 7, -256),
    Line::normal(5, 6, -128),
    Line::normal(5, 5, -64),
    Line::normal(4, 5, -32),
    Line::normal(4, 5, 0),
    Line::normal(5, 5, 32),
    Line::normal(5, 6, 64),
    Line::normal(4, 7, 128),
    Line::normal(3, 8, 256),
    Line::normal(3, 9, 512),
    Line::normal(3, 10, 1024),
    Line::lower(5, -1025),
    Line::upper(5, 2048),
];

/// Table B.8, standard Huffman table H. HTOOB is 1.
#[rustfmt::skip]
static TABLE_B8: [Line; 21] = [
    Line::normal(8, 3, -15),
    Line::normal(9, 1, -7),
    Line::normal(8, 1, -5),
    Line::normal(9, 0, -3),
    Line::normal(7, 0, -2),
    Line::normal(4, 0, -1),
    Line::normal(2, 1, 0),
    Line::normal(5, 0, 2),
    Line::normal(6, 0, 3),
    Line::normal(3, 4, 4),
    Line::normal(6, 1, 20),
    Line::normal(4, 4, 22),
    Line::normal(4, 5, 38),
    Line::normal(5, 6, 70),
    Line::normal(5, 7, 134),
    Line::normal(6, 7, 262),
    Line::normal(7, 8, 390),
    Line::normal(6, 10, 646),
    Line::lower(9, -16),
    Line::upper(9, 1670),
    Line::oob(2),
];

/// Table B.9, standard Huffman table I. HTOOB is 1.
#[rustfmt::skip]
static TABLE_B9: [Line; 22] = [
    Line::normal(8, 4, -31),
    Line::normal(9, 2, -15),
    Line::normal(8, 2, -11),
    Line::normal(9, 1, -7),
    Line::normal(7, 1, -5),
    Line::normal(4, 1, -3),
    Line::normal(3, 1, -1),
    Line::normal(3, 1, 1),
    Line::normal(5, 1, 3),
    Line::normal(6, 1, 5),
    Line::normal(3, 5, 7),
    Line::normal(6, 2, 39),
    Line::normal(4, 5, 43),
    Line::normal(4, 6, 75),
    Line::normal(5, 7, 139),
    Line::normal(5, 8, 267),
    Line::normal(6, 8, 523),
    Line::normal(7, 9, 779),
    Line::normal(6, 11, 1291),
    Line::lower(9, -32),
    Line::upper(9, 3339),
    Line::oob(2),
];

/// Table B.10, standard Huffman table J. HTOOB is 1.
#[rustfmt::skip]
static TABLE_B10: [Line; 21] = [
    Line::normal(7, 4, -21),
    Line::normal(8, 0, -5),
    Line::normal(7, 0, -4),
    Line::normal(5, 0, -3),
    Line::normal(2, 2, -2),
    Line::normal(5, 0, 2),
    Line::normal(6, 0, 3),
    Line::normal(7, 0, 4),
    Line::normal(8, 0, 5),
    Line::normal(2, 6, 6),
    Line::normal(5, 5, 70),
    Line::normal(6, 5, 102),
    Line::normal(6, 6, 134),
    Line::normal(6, 7, 198),
    Line::normal(6, 8, 326),
    Line::normal(6, 9, 582),
    Line::normal(6, 10, 1094),
    Line::normal(7, 11, 2118),
    Line::lower(8, -22),
    Line::upper(8, 4166),
    Line::oob(2),
];

/// Table B.11, standard Huffman table K. HTOOB is 0.
#[rustfmt::skip]
static TABLE_B11: [Line; 13] = [
    Line::normal(1, 0, 1),
    Line::normal(2, 1, 2),
    Line::normal(4, 0, 4),
    Line::normal(4, 1, 5),
    Line::normal(5, 1, 7),
    Line::normal(5, 2, 9),
    Line::normal(6, 2, 13),
    Line::normal(7, 2, 17),
    Line::normal(7, 3, 21),
    Line::normal(7, 4, 29),
    Line::normal(7, 5, 45),
    Line::normal(7, 6, 77),
    Line::upper(7, 141),
];

/// Table B.12, standard Huffman table L. HTOOB is 0.
#[rustfmt::skip]
static TABLE_B12: [Line; 13] = [
    Line::normal(1, 0, 1),
    Line::normal(2, 0, 2),
    Line::normal(3, 1, 3),
    Line::normal(5, 0, 5),
    Line::normal(5, 1, 6),
    Line::normal(6, 1, 8),
    Line::normal(7, 0, 10),
    Line::normal(7, 1, 11),
    Line::normal(7, 2, 13),
    Line::normal(7, 3, 17),
    Line::normal(7, 4, 25),
    Line::normal(8, 5, 41),
    Line::upper(8, 73),
];

/// Table B.13, standard Huffman table M. HTOOB is 0.
#[rustfmt::skip]
static TABLE_B13: [Line; 13] = [
    Line::normal(1, 0, 1),
    Line::normal(3, 0, 2),
    Line::normal(4, 0, 3),
    Line::normal(5, 0, 4),
    Line::normal(4, 1, 5),
    Line::normal(3, 3, 7),
    Line::normal(6, 1, 15),
    Line::normal(6, 2, 17),
    Line::normal(6, 3, 21),
    Line::normal(6, 4, 29),
    Line::normal(6, 5, 45),
    Line::normal(7, 6, 77),
    Line::upper(7, 141),
];

/// Table B.14, standard Huffman table N. HTOOB is 0.
///
/// The only table with neither escape line: it codes −2 to 2 and nothing else.
#[rustfmt::skip]
static TABLE_B14: [Line; 5] = [
    Line::normal(3, 0, -2),
    Line::normal(3, 0, -1),
    Line::normal(1, 0, 0),
    Line::normal(3, 0, 1),
    Line::normal(3, 0, 2),
];

/// Table B.15, standard Huffman table O. HTOOB is 0.
#[rustfmt::skip]
static TABLE_B15: [Line; 13] = [
    Line::normal(7, 4, -24),
    Line::normal(6, 2, -8),
    Line::normal(5, 1, -4),
    Line::normal(4, 0, -2),
    Line::normal(3, 0, -1),
    Line::normal(1, 0, 0),
    Line::normal(3, 0, 1),
    Line::normal(4, 0, 2),
    Line::normal(5, 1, 3),
    Line::normal(6, 2, 5),
    Line::normal(7, 4, 9),
    Line::lower(7, -25),
    Line::upper(7, 25),
];

/// The encoder side of Annex B. Test-only.
///
/// A table is a decoder here, so the only way to state a test's input is to
/// write the bits an encoder would have produced. That is not something the
/// fixtures can do by hand — a Huffman symbol dictionary's coded data
/// interleaves three tables, a run of raw bit fields and an MMR bitmap — so
/// they build it through this instead, and a round trip is then evidence that
/// the decoder reads back what the standard says an encoder writes.
#[cfg(test)]
pub(crate) mod encoder {
    /// Writes bits most significant first, mirroring
    /// [`BitReader`](super::BitReader).
    #[derive(Default)]
    pub(crate) struct BitWriter {
        bytes: Vec<u8>,
        used: u32,
    }

    impl BitWriter {
        /// Appends the low `len` bits of `value`, most significant first.
        pub(crate) fn push(&mut self, value: u32, len: u8) {
            for i in (0..u32::from(len)).rev() {
                if self.used.is_multiple_of(8) {
                    self.bytes.push(0);
                }
                if (value >> i) & 1 == 1 {
                    let last = self.bytes.len() - 1;
                    self.bytes[last] |= 0x80 >> (self.used % 8);
                }
                self.used += 1;
            }
        }

        /// Pads with zero bits to the next byte boundary, which is what
        /// "skip over any bits remaining in the last byte" asks of an encoder
        /// (T.88 6.5.9 steps 2 and 5).
        pub(crate) fn align(&mut self) {
            while !self.used.is_multiple_of(8) {
                self.push(0, 1);
            }
        }

        /// Appends whole bytes, which requires the cursor to be byte-aligned.
        pub(crate) fn push_bytes(&mut self, bytes: &[u8]) {
            assert!(self.used.is_multiple_of(8), "unaligned byte field");
            for byte in bytes {
                self.push(u32::from(*byte), 8);
            }
        }

        /// The bits written, the last byte padded with zeros.
        pub(crate) fn finish(self) -> Vec<u8> {
            self.bytes
        }
    }
}

#[cfg(test)]
mod tests {
    use super::encoder::BitWriter;
    use super::*;

    /// Formats a code as the spec prints it: exactly `len` binary digits.
    fn bits_of(code: u32, len: u8) -> String {
        (0..len)
            .rev()
            .map(|i| if (code >> i) & 1 == 1 { '1' } else { '0' })
            .collect()
    }

    /// Encodes `line_index`'s prefix followed by `offset`, the way an encoder
    /// using this table would.
    fn encode(table: &Table, line_index: usize, offset: u32) -> Vec<u8> {
        let line = table.lines[line_index];
        let lens: Vec<u8> = table.lines.iter().map(|l| l.pref_len).collect();
        let codes = assign_prefix_codes(&lens).expect("the table was built, so its lengths assign");
        let mut w = BitWriter::default();
        w.push(codes[line_index], line.pref_len);
        if line.kind != Kind::Oob {
            w.push(offset, line.range_len);
        }
        w.finish()
    }

    /// Encodes `offset` on `line_index` and decodes it straight back.
    fn round_trip(
        table: &Table,
        line_index: usize,
        offset: u32,
    ) -> Result<Option<i32>, Jbig2Error> {
        let bytes = encode(table, line_index, offset);
        table.decode(&mut BitReader::new(&bytes))
    }

    /// The prefix lengths of the worked example in B.4, whose assigned codes
    /// the specification prints. Nothing here was invented: PREFLEN is
    /// `1 2 3 0 3` and CODES is `0 10 110 X 111`, where X marks the unused
    /// lower range line.
    #[test]
    fn b3_reproduces_the_worked_example_of_b4() {
        let codes = assign_prefix_codes(&[1, 2, 3, 0, 3]).expect("a legal set of lengths");
        assert_eq!(bits_of(codes[0], 1), "0");
        assert_eq!(bits_of(codes[1], 2), "10");
        assert_eq!(bits_of(codes[2], 3), "110");
        assert_eq!(bits_of(codes[4], 3), "111");
    }

    /// A PREFLEN of 0 must not shift the codes of the lines around it. Drop
    /// the clearing of LENCOUNT[0] in B.3 step 2 and this is what breaks:
    /// FIRSTCODE[1] becomes 2 rather than 0 and every code moves.
    #[test]
    fn unused_lines_do_not_shift_the_others() {
        let with_gaps = assign_prefix_codes(&[0, 1, 0, 2, 0, 3, 3]).expect("legal");
        let without = assign_prefix_codes(&[1, 2, 3, 3]).expect("legal");
        assert_eq!(
            [with_gaps[1], with_gaps[3], with_gaps[5], with_gaps[6]],
            [without[0], without[1], without[2], without[3]],
        );
    }

    /// A code space filled exactly is complete, not over-subscribed. Testing
    /// the counter *after* the increment instead of the code before assigning
    /// it would reject this — and with it B.1, B.11 and B.14.
    #[test]
    fn a_full_code_space_is_accepted() {
        assert_eq!(assign_prefix_codes(&[1, 1]), Ok(vec![0, 1]));
        assert_eq!(assign_prefix_codes(&[1, 2, 3, 3]), Ok(vec![0, 2, 6, 7]));
        assert_eq!(
            assign_prefix_codes(&[3, 3, 1, 3, 3]),
            Ok(vec![4, 5, 0, 6, 7]),
        );
    }

    #[test]
    fn over_subscribed_lengths_are_refused() {
        for lens in [
            &[1u8, 1, 1][..],
            &[1, 1, 2][..],
            &[2, 2, 2, 2, 2][..],
            &[1, 2, 2, 2][..],
        ] {
            assert_eq!(
                assign_prefix_codes(lens),
                Err(Jbig2Error::Malformed(
                    "Huffman code lengths over-subscribe the code space"
                )),
                "{lens:?}",
            );
        }
    }

    #[test]
    fn a_prefix_longer_than_a_code_word_is_refused() {
        assert_eq!(
            assign_prefix_codes(&[1, 33]),
            Err(Jbig2Error::Malformed(
                "Huffman prefix longer than a code word"
            )),
        );
        assert_eq!(
            assign_prefix_codes(&[255]),
            Err(Jbig2Error::Malformed(
                "Huffman prefix longer than a code word"
            )),
        );
        // 32 is the longest that is still a code word.
        assert!(assign_prefix_codes(&[32]).is_ok());
    }

    #[test]
    fn a_range_longer_than_a_value_is_refused() {
        assert_eq!(
            Table::new(vec![Line::normal(1, 33, 0)]).map(|_| ()),
            Err(Jbig2Error::Malformed("Huffman range longer than a value")),
        );
        assert!(Table::new(vec![Line::normal(1, 32, 0)]).is_ok());
    }

    #[test]
    fn a_table_that_assigns_no_codes_is_refused() {
        assert_eq!(
            Table::new(vec![Line::normal(0, 4, 0), Line::normal(0, 4, 16)]).map(|_| ()),
            Err(Jbig2Error::Malformed("Huffman table assigns no codes")),
        );
    }

    /// The table of the B.4 worked example, built by hand: three ranges, an
    /// unused lower range line and an upper range line. This is Table B.1.
    fn worked_example_table() -> Table {
        Table::new(vec![
            Line::normal(1, 4, 0),
            Line::normal(2, 8, 16),
            Line::normal(3, 16, 272),
            Line::lower(0, -1),
            Line::upper(3, 65808),
        ])
        .expect("the encoding printed in B.4")
    }

    #[test]
    fn decoding_follows_the_prefix_to_its_range() {
        let table = worked_example_table();
        // 0 + a 4-bit field: values 0 to 15.
        assert_eq!(round_trip(&table, 0, 9), Ok(Some(9)));
        // 10 + an 8-bit field: 16 to 271.
        assert_eq!(round_trip(&table, 1, 255), Ok(Some(271)));
        // 110 + a 16-bit field: 272 to 65807.
        assert_eq!(round_trip(&table, 2, 0), Ok(Some(272)));
        // 111 + a 32-bit field: the upper escape.
        assert_eq!(round_trip(&table, 4, 1_000), Ok(Some(66_808)));
    }

    /// The lower range line counts downwards from HTLOW − 1 (B.4 step 4). It
    /// is the only line that does, and adding where it should subtract puts
    /// every negative value on the wrong side of zero.
    #[test]
    fn the_lower_range_line_subtracts() {
        let table = Table::new(vec![
            Line::normal(1, 4, 0),
            Line::lower(2, -1),
            Line::upper(2, 16),
        ])
        .expect("legal");
        assert_eq!(round_trip(&table, 1, 0), Ok(Some(-1)));
        assert_eq!(round_trip(&table, 1, 41), Ok(Some(-42)));
        assert_eq!(round_trip(&table, 2, 5), Ok(Some(21)));
    }

    /// An out-of-band line reads no offset bits at all, so whatever follows
    /// its prefix is the next value rather than a field belonging to it.
    #[test]
    fn oob_consumes_only_its_prefix() {
        let table = Table::new(vec![Line::normal(1, 4, 0), Line::oob(2)]).expect("legal");
        assert!(table.has_oob());
        let mut data = BitWriter::default();
        // 10, then 0 followed by 0111.
        data.push(0b10, 2);
        data.push(0b0_0111, 5);
        let bytes = data.finish();
        let mut bits = BitReader::new(&bytes);
        assert_eq!(table.decode(&mut bits), Ok(None));
        assert_eq!(table.decode(&mut bits), Ok(Some(7)));
    }

    #[test]
    fn a_table_without_an_oob_line_never_yields_one() {
        let table = worked_example_table();
        assert!(!table.has_oob());
    }

    /// The asymmetry with the arithmetic decoder, pinned. Exhausted input is a
    /// truncation whether it runs out mid-prefix or mid-offset; it is never
    /// OOB, and it never repeats a plausible value.
    #[test]
    fn exhausted_input_is_truncated_and_not_oob() {
        let table = worked_example_table();
        let mut bits = BitReader::new(&[]);
        assert_eq!(table.decode(&mut bits), Err(Jbig2Error::Truncated));
        // The prefix 10 introduces an eight-bit field, so ten bits are needed
        // and one byte carries eight: the prefix matches and the value it
        // promises is not there.
        let mut bits = BitReader::new(&[0b1000_0000]);
        assert_eq!(table.decode(&mut bits), Err(Jbig2Error::Truncated));
    }

    /// A bit pattern matching no code is corruption, not the end of the data.
    #[test]
    fn an_unmatched_prefix_is_malformed() {
        // Three lines of length 2 leave the fourth 2-bit pattern unassigned.
        let table = Table::new(vec![
            Line::normal(2, 0, 0),
            Line::normal(2, 0, 1),
            Line::normal(2, 0, 2),
        ])
        .expect("legal");
        let mut bits = BitReader::new(&[0xFF]);
        assert_eq!(
            table.decode(&mut bits),
            Err(Jbig2Error::Malformed("no such Huffman code")),
        );
    }

    /// An escape line can be handed an offset that carries the value past what
    /// B.2 says a table may encode. That is a malformed stream, not a wrapped
    /// number.
    #[test]
    fn a_value_past_the_codable_range_is_refused() {
        let table = worked_example_table();
        assert_eq!(
            round_trip(&table, 4, u32::MAX),
            Err(Jbig2Error::Malformed(
                "Huffman value outside the codable range"
            )),
        );
        let low = Table::new(vec![Line::normal(1, 0, 0), Line::lower(1, -1)]).expect("legal");
        assert_eq!(
            round_trip(&low, 1, u32::MAX),
            Err(Jbig2Error::Malformed(
                "Huffman value outside the codable range"
            )),
        );
    }

    /// Line order breaks ties between equal lengths (B.3 step 3b walks the
    /// table in order), so the same lengths in a different order are a
    /// different code.
    #[test]
    fn line_order_decides_which_line_gets_which_code() {
        let a = Table::new(vec![Line::normal(1, 0, 10), Line::normal(1, 0, 20)]).expect("legal");
        let b = Table::new(vec![Line::normal(1, 0, 20), Line::normal(1, 0, 10)]).expect("legal");
        let mut bits = BitReader::new(&[0x00]);
        assert_eq!(a.decode(&mut bits), Ok(Some(10)));
        let mut bits = BitReader::new(&[0x00]);
        assert_eq!(b.decode(&mut bits), Ok(Some(20)));
    }

    /// The `Encoding` column of B.5: the prefix bit string the specification
    /// prints for each line of each standard table, in the printed order.
    ///
    /// This is deliberately a *second* transcription of the same fifteen
    /// tables, taken from a different column of the same page. Transcribing
    /// PREFLEN wrongly is otherwise a silent mistake — the table still decodes,
    /// it decodes a different value — and for it to survive this fixture the
    /// same slip would have to be made twice, in two different notations, in
    /// exactly the same place.
    #[rustfmt::skip]
    static PRINTED_ENCODINGS: [&[&str]; 15] = [
        // Table B.1
        &["0", "10", "110", "111"],
        // Table B.2
        &["0", "10", "110", "1110", "11110", "111110", "111111"],
        // Table B.3
        &["11111110", "0", "10", "110", "1110", "11110", "11111111", "1111110",
          "111110"],
        // Table B.4
        &["0", "10", "110", "1110", "11110", "11111"],
        // Table B.5
        &["1111110", "0", "10", "110", "1110", "11110", "1111111", "111110"],
        // Table B.6
        &["11100", "1000", "1001", "1010", "11101", "11110", "1011", "00",
          "010", "011", "1100", "1101", "111110", "111111"],
        // Table B.7
        &["1000", "000", "1001", "11010", "11011", "1010", "1011", "11100",
          "11101", "1100", "001", "010", "011", "11110", "11111"],
        // Table B.8
        &["11111100", "111111100", "11111101", "111111101", "1111100", "1010",
          "00", "11010", "111010", "100", "111011", "1011", "1100", "11011",
          "11100", "111100", "1111101", "111101", "111111110", "111111111",
          "01"],
        // Table B.9
        &["11111100", "111111100", "11111101", "111111101", "1111100", "1010",
          "010", "011", "11010", "111010", "100", "111011", "1011", "1100",
          "11011", "11100", "111100", "1111101", "111101", "111111110",
          "111111111", "00"],
        // Table B.10
        &["1111010", "11111100", "1111011", "11000", "00", "11001", "110110",
          "1111100", "11111101", "01", "11010", "110111", "111000", "111001",
          "111010", "111011", "111100", "1111101", "11111110", "11111111",
          "10"],
        // Table B.11
        &["0", "10", "1100", "1101", "11100", "11101", "111100", "1111010",
          "1111011", "1111100", "1111101", "1111110", "1111111"],
        // Table B.12
        &["0", "10", "110", "11100", "11101", "111100", "1111010", "1111011",
          "1111100", "1111101", "1111110", "11111110", "11111111"],
        // Table B.13
        &["0", "100", "1100", "11100", "1101", "101", "111010", "111011",
          "111100", "111101", "111110", "1111110", "1111111"],
        // Table B.14
        &["100", "101", "0", "110", "111"],
        // Table B.15
        &["1111100", "111100", "11100", "1100", "100", "0", "101", "1101",
          "11101", "111101", "1111101", "1111110", "1111111"],
    ];

    /// The mandatory golden vector for the standard tables: B.3, run over the
    /// PREFLEN column this module transcribed, must reproduce the `Encoding`
    /// column the specification printed, bit for bit, for all fifteen.
    #[test]
    fn standard_tables_reproduce_their_printed_encodings() {
        for (index, printed) in PRINTED_ENCODINGS.iter().enumerate() {
            let number = index as u8 + 1;
            let table = standard(number).expect("a standard table number");
            assert_eq!(
                table.lines.len(),
                printed.len(),
                "B.{number} has a different number of lines than the page does",
            );
            let lens: Vec<u8> = table.lines.iter().map(|line| line.pref_len).collect();
            let codes = assign_prefix_codes(&lens).expect("a standard table assigns");
            for (line, want) in printed.iter().enumerate() {
                assert_eq!(
                    bits_of(codes[line], lens[line]),
                    *want,
                    "B.{number} line {line}",
                );
            }
        }
    }

    /// All fifteen are *complete* codes: the prefix lengths satisfy Kraft's
    /// relation with equality, so every bit string of the right shape decodes
    /// to something and no code space is wasted.
    ///
    /// This was computed for all fifteen before being asserted, and it holds
    /// uniformly. It is not a property of Huffman tables in general — B.2 lets
    /// a custom table leave lines unused with a PREFLEN of 0, and such a table
    /// sums to less than one — so it is pinned only for the standard set,
    /// where it is an independent check on the transcribed lengths: moving one
    /// line's PREFLEN by one breaks the sum.
    #[test]
    fn every_standard_table_is_a_complete_code() {
        for number in 1..=15u8 {
            let table = standard(number).expect("a standard table number");
            // Summed over a common denominator of 2^32 so the arithmetic is
            // exact; no standard table has an unused line, which is what makes
            // the shift below well defined for every one of them.
            let mut total = 0u64;
            for line in &table.lines {
                assert!(line.pref_len > 0, "B.{number} has an unused line");
                total += 1u64 << (32 - line.pref_len);
            }
            assert_eq!(total, 1u64 << 32, "B.{number} is not a complete code");
        }
    }

    /// HTOOB, as each table's header line gives it.
    #[test]
    fn standard_tables_carry_the_printed_htoob() {
        for number in 1..=15u8 {
            let table = standard(number).expect("a standard table number");
            assert_eq!(
                table.has_oob(),
                matches!(number, 2 | 3 | 8 | 9 | 10),
                "B.{number}",
            );
        }
    }

    /// Every line of every standard table round-trips: the first value of its
    /// range, the second, and the last, back through the decoder as itself.
    ///
    /// The escape lines carry a 32-bit offset, which reaches past both ends of
    /// what B.2 says a table may encode, so the ones that do are expected to
    /// be refused rather than wrapped — that arm of the loop is as much the
    /// point as the equalities are.
    #[test]
    fn standard_tables_round_trip_the_ends_of_every_range() {
        for number in 1..=15u8 {
            let table = standard(number).expect("a standard table number");
            for (index, line) in table.lines.iter().enumerate() {
                if line.kind == Kind::Oob {
                    assert_eq!(round_trip(&table, index, 0), Ok(None), "B.{number} OOB");
                    continue;
                }
                let last = match line.range_len {
                    32 => u32::MAX,
                    n => (1u32 << n) - 1,
                };
                for offset in [0, last.min(1), last] {
                    let low = i64::from(line.range_low);
                    let want = match line.kind {
                        Kind::Lower => low - i64::from(offset),
                        Kind::Normal | Kind::Oob => low + i64::from(offset),
                    };
                    let got = round_trip(&table, index, offset);
                    let expected = match i32::try_from(want) {
                        Ok(value) => Ok(Some(value)),
                        Err(_) => Err(Jbig2Error::Malformed(
                            "Huffman value outside the codable range",
                        )),
                    };
                    assert_eq!(got, expected, "B.{number} line {index} offset {offset}");
                }
            }
        }
    }

    /// A selector that names no standard table is a mistake in the caller, and
    /// is reported rather than silently resolved to a neighbouring table.
    #[test]
    fn there_is_no_standard_table_outside_one_to_fifteen() {
        for number in [0u8, 16, 255] {
            assert_eq!(
                standard(number).map(|_| ()),
                Err(Jbig2Error::Malformed("no such standard Huffman table")),
            );
        }
    }

    /// Assembles a code table segment: the flags byte of B.2.1, the two signed
    /// bounds, and a body of already-packed table line bits.
    fn table_segment(
        htoob: bool,
        htps: u8,
        htrs: u8,
        htlow: i32,
        hthigh: i32,
        body: &[u8],
    ) -> Vec<u8> {
        let flags = u8::from(htoob) | ((htps - 1) << 1) | ((htrs - 1) << 4);
        let mut out = vec![flags];
        out.extend_from_slice(&(htlow as u32).to_be_bytes());
        out.extend_from_slice(&(hthigh as u32).to_be_bytes());
        out.extend_from_slice(body);
        out
    }

    /// The byte string B.4 prints as an example of an encoded table.
    const WORKED_EXAMPLE: [u8; 13] = [
        0x42, 0x00, 0x00, 0x00, 0x00, 0x00, 0x01, 0x01, 0x10, 0x49, 0x23, 0x81, 0x80,
    ];

    /// The mandatory golden vector for B.2: the worked example of B.4, decoded
    /// to the intermediate arrays the specification prints for it.
    ///
    /// Every number asserted here is on the page. Together they pin the flags
    /// decode, the two bounds, the line loop's termination test, the lower and
    /// upper range lines and B.3, against a vector this decoder had no hand in
    /// choosing.
    #[test]
    fn the_worked_example_of_b4_decodes_to_the_arrays_it_prints() {
        let mut budget = Budget::new();
        let table =
            parse_table_segment(&WORKED_EXAMPLE, &mut budget).expect("the encoding B.4 prints");

        // "After decoding these table lines, the value of NTEMP is 5."
        assert_eq!(table.lines.len(), 5);
        let pref_lens: Vec<u8> = table.lines.iter().map(|line| line.pref_len).collect();
        let range_lens: Vec<u8> = table.lines.iter().map(|line| line.range_len).collect();
        let range_lows: Vec<i32> = table.lines.iter().map(|line| line.range_low).collect();
        assert_eq!(pref_lens, vec![1, 2, 3, 0, 3]);
        assert_eq!(range_lens, vec![4, 8, 16, 32, 32]);
        assert_eq!(range_lows, vec![0, 16, 272, -1, 65808]);

        // "Applying the algorithm of B.3 to this yields the array of codes, in
        // binary, 0 10 110 X 111", the X being the lower range table line,
        // which a PREFLEN of 0 leaves without a code.
        let codes = assign_prefix_codes(&pref_lens).expect("a legal set of lengths");
        assert_eq!(bits_of(codes[0], 1), "0");
        assert_eq!(bits_of(codes[1], 2), "10");
        assert_eq!(bits_of(codes[2], 3), "110");
        assert_eq!(bits_of(codes[4], 3), "111");

        // The flags byte 0x42 sets HTOOB to 0.
        assert!(!table.has_oob());
    }

    /// B.4 introduces that byte string as "the encoding for Table B.1", so the
    /// custom table it decodes to and the transcribed standard table must be
    /// the same table. This checks one transcription against the other.
    #[test]
    fn the_worked_example_is_an_encoding_of_table_b1() {
        let mut budget = Budget::new();
        let custom =
            parse_table_segment(&WORKED_EXAMPLE, &mut budget).expect("the encoding B.4 prints");
        let standard_b1 = standard(1).expect("Table B.1");
        // The custom encoding carries its unused lower range line explicitly;
        // B.5 prints Table B.1 with that line omitted, which says the same
        // thing.
        let used: Vec<Line> = custom
            .lines
            .iter()
            .copied()
            .filter(|line| line.pref_len > 0)
            .collect();
        assert_eq!(used, standard_b1.lines);
    }

    /// HTOOB in the flags byte adds the out-of-band line of B.2 step 10, and
    /// the table then decodes all three of an ordinary value, the upper escape
    /// and OOB.
    #[test]
    fn a_custom_table_can_code_for_oob() {
        let mut body = BitWriter::default();
        // One ordinary line: PREFLEN 1, RANGELEN 2, covering 0 to 3.
        body.push(1, 4);
        body.push(2, 4);
        // The lower range line is unused; the upper and out-of-band lines take
        // the two codes of length 2.
        body.push(0, 4);
        body.push(2, 4);
        body.push(2, 4);
        let data = table_segment(true, 4, 4, 0, 4, &body.finish());

        let mut budget = Budget::new();
        let table = parse_table_segment(&data, &mut budget).expect("a legal table");
        assert!(table.has_oob());
        assert_eq!(table.lines.len(), 4);
        assert_eq!(round_trip(&table, 0, 3), Ok(Some(3)));
        assert_eq!(round_trip(&table, 2, 7), Ok(Some(11)));
        assert_eq!(round_trip(&table, 3, 0), Ok(None));
    }

    #[test]
    fn a_reserved_flag_bit_is_refused() {
        let data = table_segment(false, 1, 1, 0, 1, &[0x00]);
        let mut poisoned = data.clone();
        poisoned[0] |= 0x80;
        let mut budget = Budget::new();
        assert_eq!(
            parse_table_segment(&poisoned, &mut budget).map(|_| ()),
            Err(Jbig2Error::Malformed(
                "reserved bit set in the Huffman table flags"
            )),
        );
    }

    #[test]
    fn a_segment_shorter_than_its_header_is_truncated() {
        for len in 0..9 {
            let mut budget = Budget::new();
            assert_eq!(
                parse_table_segment(&WORKED_EXAMPLE[..len], &mut budget).map(|_| ()),
                Err(Jbig2Error::Truncated),
                "{len} bytes",
            );
        }
    }

    /// A segment that stops in the middle of a table line is truncated, not a
    /// table with fewer lines.
    #[test]
    fn a_segment_that_runs_out_mid_line_is_truncated() {
        // HTPS and HTRS of 8 make a line sixteen bits wide; one byte is half a
        // line.
        let data = table_segment(false, 8, 8, 0, 1_000, &[0x01]);
        let mut budget = Budget::new();
        assert_eq!(
            parse_table_segment(&data, &mut budget).map(|_| ()),
            Err(Jbig2Error::Truncated),
        );
        // And a body long enough for the lines but not for the two escape
        // prefixes that follow them.
        let data = table_segment(false, 8, 8, 0, 1, &[0x01, 0x00]);
        let mut budget = Budget::new();
        assert_eq!(
            parse_table_segment(&data, &mut budget).map(|_| ()),
            Err(Jbig2Error::Truncated),
        );
    }

    #[test]
    fn a_segment_declaring_an_over_long_range_is_refused() {
        let mut body = BitWriter::default();
        body.push(1, 1);
        body.push(33, 6);
        let data = table_segment(false, 1, 6, 0, 1, &body.finish());
        let mut budget = Budget::new();
        assert_eq!(
            parse_table_segment(&data, &mut budget).map(|_| ()),
            Err(Jbig2Error::Malformed("Huffman range longer than a value")),
        );
    }

    #[test]
    fn a_segment_whose_prefixes_over_subscribe_is_refused() {
        let mut body = BitWriter::default();
        // Two ordinary lines and both escape lines, all with PREFLEN 1: four
        // codes competing for the two of length one.
        body.push(1, 4);
        body.push(0, 1);
        body.push(1, 4);
        body.push(0, 1);
        body.push(1, 4);
        body.push(1, 4);
        let data = table_segment(false, 4, 1, 0, 2, &body.finish());
        let mut budget = Budget::new();
        assert_eq!(
            parse_table_segment(&data, &mut budget).map(|_| ()),
            Err(Jbig2Error::Malformed(
                "Huffman code lengths over-subscribe the code space"
            )),
        );
    }

    /// The work-budget hazard of B.2 step 5, from both directions.
    ///
    /// A header naming a span of four billion with a step of one asks for two
    /// billion lines. Whether the segment carries the bits for them or not,
    /// the answer must be a named error and it must arrive at once — the loop
    /// is bounded before it is entered, so neither the span nor the body
    /// length decides how long this takes.
    #[test]
    fn a_table_spanning_the_whole_range_is_refused_rather_than_iterated() {
        // Bits for far more lines than the cap allows: HTPS and HTRS of 1 make
        // each line two bits, and every line advances CURRANGELOW by one.
        let data = table_segment(false, 1, 1, i32::MIN, i32::MAX, &[0x00; 2048]);
        let mut budget = Budget::new();
        assert_eq!(
            parse_table_segment(&data, &mut budget).map(|_| ()),
            Err(Jbig2Error::Malformed("too many Huffman table lines")),
        );

        // The same header with almost no body: the lines run out before the
        // cap does, and that is a truncation.
        let data = table_segment(false, 1, 1, i32::MIN, i32::MAX, &[0x00; 4]);
        let mut budget = Budget::new();
        assert_eq!(
            parse_table_segment(&data, &mut budget).map(|_| ()),
            Err(Jbig2Error::Truncated),
        );
    }

    /// The cap is on the line count itself, not only on the span, so a table
    /// whose span is modest but whose lines are numerous is refused too.
    #[test]
    fn more_lines_than_the_cap_allows_are_refused() {
        let data = table_segment(false, 1, 1, 0, 100_000, &[0x00; 2048]);
        let mut budget = Budget::new();
        assert_eq!(
            parse_table_segment(&data, &mut budget).map(|_| ()),
            Err(Jbig2Error::Malformed("too many Huffman table lines")),
        );
    }

    /// A table is charged before it is built, from the header, so a stream
    /// that repeats one is stopped even though every individual table is
    /// small and well formed.
    #[test]
    fn tables_are_charged_against_the_stream_budget() {
        let mut budget = Budget::with_limit(0);
        assert_eq!(
            parse_table_segment(&WORKED_EXAMPLE, &mut budget).map(|_| ()),
            Err(Jbig2Error::WorkLimit),
        );

        let mut budget = Budget::new();
        let mut parsed = 0u32;
        loop {
            match parse_table_segment(&WORKED_EXAMPLE, &mut budget) {
                Ok(_) => parsed += 1,
                Err(Jbig2Error::WorkLimit) => break,
                Err(other) => panic!("unexpected {other:?}"),
            }
            assert!(parsed < 10_000_000, "the budget never ran out");
        }
        assert!(parsed > 0, "one table has to be affordable");
    }

    /// HTHIGH at or below HTLOW still yields one ordinary line: B.2 step 5
    /// decodes a line before it first tests, so the loop cannot run zero
    /// times.
    #[test]
    fn an_empty_span_still_yields_one_line() {
        let mut body = BitWriter::default();
        body.push(1, 4);
        body.push(0, 4);
        body.push(0, 4);
        body.push(2, 4);
        let data = table_segment(false, 4, 4, 5, 5, &body.finish());
        let mut budget = Budget::new();
        let table = parse_table_segment(&data, &mut budget).expect("a legal table");
        assert_eq!(table.lines.len(), 3);
        assert_eq!(round_trip(&table, 0, 0), Ok(Some(5)));
    }
}
