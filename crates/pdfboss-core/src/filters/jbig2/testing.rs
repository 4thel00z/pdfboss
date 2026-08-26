//! Fixture builders shared by the JBIG2 test modules. Test-only.
//!
//! A `#[cfg(test)] mod tests` is private to the module that declares it, so a
//! builder written inside one cannot be called from another's tests. The
//! segment-level fixtures are needed from several modules at once — a symbol
//! dictionary's bytes are built the same way whether the assertion is about the
//! dictionary decoder or about the page walk that dispatches to it — so they
//! live here instead, in one copy.
//!
//! These builders are miniature encoders. That is deliberate: the coded data of
//! a symbol dictionary is a braid of Annex A integers and generic-region pixel
//! decisions sharing a single arithmetic stream, and no hand-written byte string
//! is a realistic sample of it. Building the braid the way an encoder would and
//! reading it back is what pins the decoder to the standard's bit order rather
//! than to itself.

use super::arith_int::encoder::{encode_iaid, encode_int};
use super::arith_int::{IaidCtx, IntCtxSet};
use super::bitmap::{Bitmap, CombOp};
use super::generic::{context_at, GenericParams, GB_CONTEXT_LEN, NOMINAL_AT};
use super::huffman::encoder::{push_value, BitWriter};
use super::huffman::{from_code_lengths, standard, Table, Unused};
use super::mq::encoder::MqEncoder;
use super::mq::MqContext;
use super::reader::Reader;
use super::refinement::{
    encode_refinement_into, RefinementParams, GR_CONTEXT_LEN, NOMINAL_AT as REFINEMENT_NOMINAL_AT,
};
use super::segment::parse_header;
use super::text_region::sym_code_len;
use crate::filters::ccitt::testing::encode_g4;

/// A short-form segment header (T.88 7.2): number, flags, referred-to
/// segments, page association, data length.
///
/// Only the short form of the referred-to field (7.2.4) is emitted, which caps
/// a fixture at four referred-to segments, and each referred-to number is one
/// byte, which holds while the referring segment's own number is at most 256
/// (7.2.5). Both are true of every fixture here; the long forms have their own
/// tests in the segment parser.
pub(crate) fn header(number: u32, kind: u8, refs: &[u8], page: u8, len: u32) -> Vec<u8> {
    let mut out = number.to_be_bytes().to_vec();
    // Flags: the type in bits 0 to 5, with the page association size bit clear
    // so the association below is a single byte.
    out.push(kind);
    out.push((refs.len() as u8) << 5);
    out.extend_from_slice(refs);
    out.push(page);
    out.extend_from_slice(&len.to_be_bytes());
    out
}

/// A bitmap from rows of `'1'` and `'0'` characters.
pub(crate) fn glyph(rows: &[&str]) -> Bitmap {
    let height = rows.len() as u32;
    let width = rows[0].len() as u32;
    let mut bm = Bitmap::new(width, height).expect("glyph");
    for (y, row) in rows.iter().enumerate() {
        for (x, ch) in row.bytes().enumerate() {
            bm.set(x as u32, y as u32, u8::from(ch == b'1'));
        }
    }
    bm
}

/// Codes one bitmap's pixels into `enc` with the nominal template 0 of a
/// generic region, through the caller's context array `gb` (T.88 6.2.5.7).
///
/// The array is the caller's rather than this function's for the same reason
/// the decoder's is: a symbol dictionary codes every symbol through one array,
/// and the adaptation carried from one symbol to the next is what makes the
/// coding efficient. A fresh array per symbol would produce bytes the decoder
/// reads back as noise from the second symbol onward.
fn encode_pixels(enc: &mut MqEncoder, gb: &mut [MqContext], bm: &Bitmap) {
    let params = GenericParams::nominal(0);
    for y in 0..bm.height() {
        for x in 0..bm.width() {
            let ctx = usize::from(context_at(bm, x, y, &params));
            enc.encode(&mut gb[ctx], bm.get(i64::from(x), i64::from(y)));
        }
    }
}

/// Builds the data of an arithmetic symbol dictionary segment (T.88 7.4.2)
/// carrying `symbols`, all of them exported.
///
/// `symbols` must be ordered as an encoder orders them: ascending by height,
/// and ascending by width within each height, since the height classes of
/// 6.5.5 are formed by grouping runs of equal height.
///
/// `num_input` is how many symbols the referred-to dictionaries contributed.
/// The export runs skip exactly that many and then export every new symbol,
/// which is the common case; a fixture that needs an input symbol re-exported
/// builds its runs itself.
pub(crate) fn dictionary_segment(symbols: &[Bitmap], num_input: u32) -> Vec<u8> {
    let mut enc = MqEncoder::new();
    let mut ints = IntCtxSet::new();
    let mut gb = vec![MqContext::default(); GB_CONTEXT_LEN];

    let mut height = 0i32;
    let mut index = 0usize;
    while index < symbols.len() {
        let class_height = symbols[index].height() as i32;
        encode_int(&mut enc, &mut ints.iadh, Some(class_height - height));
        height = class_height;

        let mut width = 0i32;
        while index < symbols.len() && symbols[index].height() as i32 == height {
            let symbol = &symbols[index];
            encode_int(
                &mut enc,
                &mut ints.iadw,
                Some(symbol.width() as i32 - width),
            );
            width = symbol.width() as i32;
            encode_pixels(&mut enc, &mut gb, symbol);
            index += 1;
        }
        // OOB closes the height class (6.5.5 step 4(c)).
        encode_int(&mut enc, &mut ints.iadw, None);
    }

    // 6.5.10: a non-export run covering the input symbols, then an export run
    // covering the new ones.
    encode_int(&mut enc, &mut ints.iaex, Some(num_input as i32));
    encode_int(&mut enc, &mut ints.iaex, Some(symbols.len() as i32));

    let mut out = 0u16.to_be_bytes().to_vec(); // arithmetic, template 0
    out.extend_from_slice(&nominal_at_bytes());
    out.extend_from_slice(&(symbols.len() as u32).to_be_bytes()); // SDNUMEXSYMS
    out.extend_from_slice(&(symbols.len() as u32).to_be_bytes()); // SDNUMNEWSYMS
    out.extend_from_slice(&enc.finish());
    out
}

/// Builds the data of a symbol dictionary segment (T.88 7.4.2) that codes no
/// symbols of its own and re-exports all `num_input` symbols its referred-to
/// dictionaries supplied (6.5.10).
///
/// Eighteen bytes of header and a handful of coded ones, whatever `num_input`
/// is — which is the point of it. A referred-to list may name the same
/// dictionary over and over, and every occurrence contributes that dictionary's
/// exports to the input list again, so this is the smallest segment that asks
/// for one decoded bitmap to be copied an arbitrary number of times.
pub(crate) fn reexport_segment(num_input: u32) -> Vec<u8> {
    let mut enc = MqEncoder::new();
    let mut ints = IntCtxSet::new();
    // A zero-length "not exported" run flips the flag without consuming an
    // index, so the single run after it covers the whole input list.
    encode_int(&mut enc, &mut ints.iaex, Some(0));
    encode_int(&mut enc, &mut ints.iaex, Some(num_input as i32));

    let mut out = 0u16.to_be_bytes().to_vec(); // arithmetic, template 0
    out.extend_from_slice(&nominal_at_bytes());
    out.extend_from_slice(&num_input.to_be_bytes()); // SDNUMEXSYMS
    out.extend_from_slice(&0u32.to_be_bytes()); // SDNUMNEWSYMS
    out.extend_from_slice(&enc.finish());
    out
}

/// Builds the data of a symbol dictionary segment (T.88 7.4.2) coding `count`
/// symbols one pixel wide and no pixels tall, none of them exported.
///
/// A symbol with no rows codes no pixel decisions whatever its width, so the
/// whole dictionary is one height class delta, `count` width deltas of nearly
/// no entropy, the OOB that closes the class and one export run. That fits tens
/// of thousands of symbols into a few dozen bytes, which makes it the cheapest
/// demand a dictionary can make and the fixture the per-symbol charge is
/// measured against.
pub(crate) fn rowless_dictionary_segment(count: u32) -> Vec<u8> {
    let mut enc = MqEncoder::new();
    let mut ints = IntCtxSet::new();
    // The running height starts at zero and stays there, so the class delta is
    // zero as well.
    encode_int(&mut enc, &mut ints.iadh, Some(0));
    // The first symbol widens the running width to one; the rest repeat it.
    for index in 0..count {
        encode_int(&mut enc, &mut ints.iadw, Some(i32::from(index == 0)));
    }
    encode_int(&mut enc, &mut ints.iadw, None);
    // One run over every symbol, with the flag still on "not exported".
    encode_int(&mut enc, &mut ints.iaex, Some(count as i32));

    let mut out = 0u16.to_be_bytes().to_vec(); // arithmetic, template 0
    out.extend_from_slice(&nominal_at_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // SDNUMEXSYMS
    out.extend_from_slice(&count.to_be_bytes()); // SDNUMNEWSYMS
    out.extend_from_slice(&enc.finish());
    out
}

/// How a Huffman dictionary fixture stores a height class collective bitmap
/// (T.88 6.5.9).
#[derive(Clone, Copy)]
pub(crate) enum Collective {
    /// BMSIZE 0: the rows are stored raw, each padded to a byte boundary.
    Uncompressed,
    /// BMSIZE nonzero: the class is MMR-coded, and BMSIZE counts its bytes.
    Mmr,
}

/// Builds the data of a Huffman-coded symbol dictionary segment (T.88 7.4.2)
/// carrying `symbols`, all of them exported.
///
/// `symbols` must be ordered as an encoder orders them, ascending by height and
/// by width within a height, and the deltas the standard tables can express add
/// one more requirement: Table B.4 codes a delta height of 1 upwards, so two
/// height classes may not have the same height, and Table B.2 codes a delta
/// width of 0 upwards.
///
/// `dh` overrides SDHUFFDH with a user-supplied table, which sets that selector
/// to 3 and makes the segment's referred-to list responsible for carrying the
/// code table segment it came from (7.4.2.1.6). `None` selects standard
/// Table B.4.
///
/// Figure 22 is the whole point of the builder: the delta widths of a class all
/// precede its pixels, and the pixels arrive as one bitmap holding every symbol
/// of the class side by side.
pub(crate) fn huffman_dictionary_segment(
    symbols: &[Bitmap],
    collective: Collective,
    dh: Option<&Table>,
) -> Vec<u8> {
    let standard_dh = standard(4).expect("Table B.4");
    let dh_table = dh.unwrap_or(&standard_dh);
    let dw = standard(2).expect("Table B.2");
    // Table B.1 serves twice over: as SDHUFFBMSIZE with that selector left at 0
    // (7.4.2.1.1), and as the table EXRUNLENGTH is always read with when SDHUFF
    // is 1 (6.5.10 step 2).
    let b1 = standard(1).expect("Table B.1");

    let mut w = BitWriter::default();
    let mut height = 0i32;
    let mut index = 0usize;
    while index < symbols.len() {
        let class_height = symbols[index].height() as i32;
        push_value(&mut w, dh_table, Some(class_height - height));
        height = class_height;

        let mut width = 0i32;
        let first = index;
        while index < symbols.len() && symbols[index].height() as i32 == height {
            push_value(&mut w, &dw, Some(symbols[index].width() as i32 - width));
            width = symbols[index].width() as i32;
            index += 1;
        }
        // OOB closes the height class (6.5.5 step 4 c) i)).
        push_value(&mut w, &dw, None);

        // 6.5.9: the size in bytes, then a byte boundary, then the bitmap, then
        // another byte boundary.
        let joined = side_by_side(&symbols[first..index]);
        match collective {
            Collective::Uncompressed => {
                push_value(&mut w, &b1, Some(0));
                w.align();
                for y in 0..joined.height() {
                    for x in 0..joined.width() {
                        w.push(u32::from(joined.get(i64::from(x), i64::from(y))), 1);
                    }
                    w.align();
                }
            }
            Collective::Mmr => {
                let coded = encode_g4(&joined);
                push_value(&mut w, &b1, Some(coded.len() as i32));
                w.align();
                w.push_bytes(&coded);
            }
        }
    }

    // 6.5.10: a zero-length "not exported" run, then one export run covering
    // every new symbol, both read with Table B.1.
    push_value(&mut w, &b1, Some(0));
    push_value(&mut w, &b1, Some(symbols.len() as i32));

    // 7.4.2.1.1: SDHUFF, and SDHUFFDH set to 3 when the caller supplied a
    // table. No AT flags follow, whatever the template bits would have said
    // (7.4.2.1.2).
    let flags = 0x0001u16 | if dh.is_some() { 3 << 2 } else { 0 };
    let mut out = flags.to_be_bytes().to_vec();
    out.extend_from_slice(&(symbols.len() as u32).to_be_bytes()); // SDNUMEXSYMS
    out.extend_from_slice(&(symbols.len() as u32).to_be_bytes()); // SDNUMNEWSYMS
    out.extend_from_slice(&w.finish());
    out
}

/// The bitmaps of one height class concatenated left to right with no gaps,
/// which is what a collective bitmap holds (T.88 6.5.9).
fn side_by_side(symbols: &[Bitmap]) -> Bitmap {
    let width = symbols.iter().map(|s| s.width()).sum();
    let height = symbols.first().map_or(0, |s| s.height());
    let mut out = Bitmap::new(width, height).expect("fixture bitmaps are small");
    let mut left = 0u32;
    for symbol in symbols {
        assert_eq!(symbol.height(), height, "a height class of two heights");
        for y in 0..height {
            for x in 0..symbol.width() {
                out.set(left + x, y, symbol.get(i64::from(x), i64::from(y)));
            }
        }
        left += symbol.width();
    }
    out
}

/// Builds the data of a code table segment (T.88 7.4.13, whose syntax is
/// Annex B.2) holding one ordinary range line that covers `low` and the fifteen
/// values above it behind a one-bit prefix.
///
/// The lower range table line is left unused — a PREFLEN of 0 says a line is
/// never used (B.3) — and the upper range line takes the other one-bit code, so
/// the two assigned codes fill the code space exactly. HTOOB is 0, which is
/// what the SDHUFFDH, SDHUFFBMSIZE and SDHUFFAGGINST selectors require of a
/// user-supplied table (7.4.2.1.6).
///
/// The table this decodes to is deliberately unlike any standard one: a value
/// in range costs a `0` bit and four more, where Table B.4 spends its `0` on
/// the single value 1. A fixture that binds this and is decoded with a standard
/// table instead does not merely read a different number, it desynchronises.
pub(crate) fn code_table_segment(low: i32) -> Vec<u8> {
    // B.2.1: HTOOB in bit 0, HTPS − 1 in bits 1 to 3, HTRS − 1 in bits 4 to 6.
    let htps = 3u8;
    let htrs = 5u8;
    let mut out = vec![((htps - 1) << 1) | ((htrs - 1) << 4)];
    // B.2.2 and B.2.3, both signed four-byte fields. HTHIGH is one past the
    // last value the ordinary lines cover.
    out.extend_from_slice(&(low as u32).to_be_bytes());
    out.extend_from_slice(&((low + 16) as u32).to_be_bytes());

    let mut w = BitWriter::default();
    w.push(1, htps); // B.2 step 5a: PREFLEN of the one ordinary line
    w.push(4, htrs); // B.2 step 5b: RANGELEN, so the line covers 16 values
    w.push(0, htps); // B.2 step 6: the lower range line, unused
    w.push(1, htps); // B.2 step 8: the upper range line
    out.extend_from_slice(&w.finish());
    out
}

/// [`code_table_segment`] with HTOOB set: one ordinary line covering `low`
/// and the fifteen values above it, an upper range line, and the out-of-band
/// line of B.2 step 10 holding the one-bit code.
///
/// This is the shape a table bound to SBHUFFDS wants and every other selector
/// refuses (7.4.3.1.6), so it is the fixture for testing that refusal.
pub(crate) fn oob_code_table_segment(low: i32) -> Vec<u8> {
    let htps = 3u8;
    let htrs = 5u8;
    // B.2.1: HTOOB in bit 0.
    let mut out = vec![1 | ((htps - 1) << 1) | ((htrs - 1) << 4)];
    out.extend_from_slice(&(low as u32).to_be_bytes());
    out.extend_from_slice(&((low + 16) as u32).to_be_bytes());

    let mut w = BitWriter::default();
    w.push(2, htps); // B.2 step 5a: PREFLEN of the one ordinary line
    w.push(4, htrs); // B.2 step 5b: RANGELEN, so the line covers 16 values
    w.push(0, htps); // B.2 step 6: the lower range line, unused
    w.push(2, htps); // B.2 step 8: the upper range line
    w.push(1, htps); // B.2 step 10: the out-of-band line
    out.extend_from_slice(&w.finish());
    out
}

/// Encodes `bitmaps` one after another through a single arithmetic coder and
/// a single GB context array.
///
/// That is the shape of every multi-bitmap coding within one segment: E.3.7
/// resets the statistics per segment, not per bitmap, so the bitplanes of a
/// gray-scale image (T.88 Annex C) share coder and contexts exactly as a
/// symbol dictionary's symbols do. `skip` marks pixels no decision is coded
/// for (6.2.5.7 USESKIP); a skipped pixel must hold 0 in its bitmap, because
/// 0 is what the decoder stores there and forms the following contexts from.
pub(crate) fn encode_generic_sequence(
    bitmaps: &[&Bitmap],
    params: &GenericParams,
    skip: Option<&Bitmap>,
) -> Vec<u8> {
    let mut enc = MqEncoder::new();
    let mut cx = vec![MqContext::default(); GB_CONTEXT_LEN];
    for bm in bitmaps {
        for y in 0..bm.height() {
            for x in 0..bm.width() {
                if skip.is_some_and(|s| s.get(i64::from(x), i64::from(y)) == 1) {
                    assert_eq!(
                        bm.get(i64::from(x), i64::from(y)),
                        0,
                        "a skipped pixel must be 0 in the fixture bitmap",
                    );
                    continue;
                }
                let ctx = usize::from(context_at(bm, x, y, params));
                enc.encode(&mut cx[ctx], bm.get(i64::from(x), i64::from(y)));
            }
        }
    }
    enc.finish()
}

/// A pattern dictionary segment's data (T.88 7.4.4): the header, then
/// `patterns` concatenated left to right into the collective bitmap and coded
/// as one region — MMR or arithmetic with the Table 27 parameters, whose A1
/// sits at (−HDPW, 0) rather than anywhere a segment header could put it.
pub(crate) fn pattern_dict_segment(patterns: &[Bitmap], mmr: bool) -> Vec<u8> {
    let hdpw = patterns[0].width();
    let hdph = patterns[0].height();
    let mut collective =
        Bitmap::new(hdpw * patterns.len() as u32, hdph).expect("fixture collectives are small");
    for (index, pattern) in patterns.iter().enumerate() {
        assert_eq!((pattern.width(), pattern.height()), (hdpw, hdph));
        collective.combine(pattern, (index as u32 * hdpw) as i32, 0, CombOp::Replace);
    }

    let mut out = vec![u8::from(mmr)]; // flags: HDTEMPLATE 0
    out.push(hdpw as u8);
    out.push(hdph as u8);
    out.extend_from_slice(&(patterns.len() as u32 - 1).to_be_bytes());
    if mmr {
        out.extend_from_slice(&encode_g4(&collective));
        return out;
    }
    let params = GenericParams {
        template: 0,
        at: [(-(hdpw as i16), 0), (-3, -1), (2, -2), (-2, -2)],
        tpgdon: false,
    };
    out.extend_from_slice(&encode_generic_sequence(&[&collective], &params, None));
    out
}

/// A halftone region segment's data (T.88 7.4.5): the region segment
/// information field — `size` at `at`, composited with `op` — then the
/// halftone flags byte exactly as Figure 42 lays it out, the grid as
/// (HGW, HGH, HGX, HGY, HRX, HRY), and the coded gray-plane bytes.
pub(crate) fn halftone_segment(
    size: (u32, u32),
    at: (u32, u32),
    op: u8,
    flags: u8,
    grid: (u32, u32, i32, i32, u16, u16),
    coded: &[u8],
) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(&size.0.to_be_bytes());
    out.extend_from_slice(&size.1.to_be_bytes());
    out.extend_from_slice(&at.0.to_be_bytes());
    out.extend_from_slice(&at.1.to_be_bytes());
    out.push(op);
    out.push(flags);
    out.extend_from_slice(&grid.0.to_be_bytes());
    out.extend_from_slice(&grid.1.to_be_bytes());
    out.extend_from_slice(&grid.2.to_be_bytes());
    out.extend_from_slice(&grid.3.to_be_bytes());
    out.extend_from_slice(&grid.4.to_be_bytes());
    out.extend_from_slice(&grid.5.to_be_bytes());
    out.extend_from_slice(coded);
    out
}

/// The four AT pixel pairs of template 0 at their nominal offsets, as the
/// eight signed bytes a segment header carries them in (T.88 7.4.2.1.2).
pub(crate) fn nominal_at_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    for (dx, dy) in NOMINAL_AT[0] {
        out.push(dx as u8);
        out.push(dy as u8);
    }
    out
}

/// Three symbols in two height classes: two of height 4 with widths 3 and 5,
/// then one of height 5. Ordered as an encoder would order them.
pub(crate) fn sample_symbols() -> Vec<Bitmap> {
    vec![
        glyph(&["101", "010", "101", "010"]),
        glyph(&["11111", "10001", "10001", "11111"]),
        glyph(&["1100", "0110", "0011", "1001", "1111"]),
    ]
}

/// Two symbols of the same height and different widths, the shape a text
/// region fixture wants: a placement bug that loses a symbol's width shows up
/// as a misplaced neighbour rather than as a symbol drawn in the right place.
pub(crate) fn two_symbols() -> Vec<Bitmap> {
    vec![
        glyph(&["101", "010", "101", "010"]),
        glyph(&["11111", "10001", "10001", "11111"]),
    ]
}

/// The text region segment flags a fixture sets (T.88 7.4.3.1.1).
///
/// The defaults are the combination a plain line of text uses: one row per
/// strip, TOPLEFT corners, untransposed, OR composition, a clear background and
/// no offset on the gaps.
#[derive(Clone, Copy)]
pub(crate) struct Shape {
    /// LOGSBSTRIPS, so SBSTRIPS is `1 << log_strips`.
    pub(crate) log_strips: u8,
    /// REFCORNER: 0 BOTTOMLEFT, 1 TOPLEFT, 2 BOTTOMRIGHT, 3 TOPRIGHT.
    pub(crate) corner: u8,
    /// TRANSPOSED, which swaps the axes S and T index.
    pub(crate) transposed: bool,
    /// SBCOMBOP, two bits here: 0 OR, 1 AND, 2 XOR, 3 XNOR.
    pub(crate) combop: u8,
    /// SBDEFPIXEL, the value the region is filled with before any placement.
    pub(crate) defpixel: bool,
    /// SBDSOFFSET, added to every gap after the first instance of a strip.
    pub(crate) dsoffset: i32,
    /// SBRTEMPLATE, the refinement template. Read only by the refined
    /// builders, which set SBREFINE themselves.
    pub(crate) rtemplate: u8,
}

impl Default for Shape {
    fn default() -> Self {
        Shape {
            log_strips: 0,
            corner: 1, // TOPLEFT
            transposed: false,
            combop: 0, // OR
            defpixel: false,
            dsoffset: 0,
            rtemplate: 0,
        }
    }
}

/// Packs a [`Shape`] into the two-byte text region segment flags field
/// (T.88 7.4.3.1.1), with SBHUFF and REFINE clear.
///
/// SBDSOFFSET occupies bits 10 to 14 as a five-bit two's complement number, so
/// a negative offset is masked to five bits rather than sign-extended into the
/// SBRTEMPLATE bit above it.
fn flags_of(shape: Shape) -> u16 {
    u16::from(shape.log_strips) << 2
        | u16::from(shape.corner) << 4
        | u16::from(shape.transposed) << 6
        | u16::from(shape.combop) << 7
        | u16::from(shape.defpixel) << 9
        | ((shape.dsoffset as u16) & 0x1F) << 10
        | u16::from(shape.rtemplate & 1) << 15
}

/// One instruction in a text region fixture, in the order T.88 6.4.5 decodes
/// them.
pub(crate) enum Op {
    /// Open a strip: the delta on STRIPT, counted in strips rather than rows.
    Strip(i32),
    /// The first instance of the current strip: the delta on FIRSTS, then the
    /// symbol id.
    First(i32, u32),
    /// A later instance of the current strip: the gap from the previous
    /// instance's far edge, then the symbol id.
    Next(i32, u32),
    /// Close the current strip, which an OOB from `IADS` is what does.
    EndStrip,
}

/// The seventeen-byte region segment information field every region segment
/// opens with (T.88 7.4.1), for a region placed at the page origin and
/// composited with OR.
fn region_info_bytes(width: u32, height: u32) -> Vec<u8> {
    let mut out = width.to_be_bytes().to_vec();
    out.extend_from_slice(&height.to_be_bytes());
    out.extend_from_slice(&0u32.to_be_bytes()); // region X
    out.extend_from_slice(&0u32.to_be_bytes()); // region Y
    out.push(0); // external combination operator: OR
    out
}

/// Builds the data of an arithmetic text region segment (T.88 7.4.3) placing
/// the instructions in `ops`.
///
/// `initial_dt` is the value of step 2's leading `IADT`, which the procedure
/// negates: STRIPT starts at `-initial_dt * SBSTRIPS`.
///
/// `num_syms` sizes the symbol ID code, and is a parameter rather than being
/// taken from the symbols themselves so that a fixture can disagree with the
/// decoder on purpose. Every instance here carries its id immediately after its
/// S coordinate, which is the layout when SBSTRIPS is 1 and no `IAIT` value
/// comes between them; [`text_segment_with_curt`] is the builder for the other
/// case.
pub(crate) fn text_segment(
    region: (u32, u32),
    shape: Shape,
    instances: u32,
    num_syms: u32,
    initial_dt: i32,
    ops: &[Op],
) -> Vec<u8> {
    let mut enc = MqEncoder::new();
    let mut ints = IntCtxSet::new();
    let mut iaid = IaidCtx::new(sym_code_len(num_syms));

    encode_int(&mut enc, &mut ints.iadt, Some(initial_dt));
    for op in ops {
        match op {
            Op::Strip(dt) => encode_int(&mut enc, &mut ints.iadt, Some(*dt)),
            Op::First(dfs, id) => {
                encode_int(&mut enc, &mut ints.iafs, Some(*dfs));
                encode_iaid(&mut enc, &mut iaid, *id);
            }
            Op::Next(ids, id) => {
                encode_int(&mut enc, &mut ints.iads, Some(*ids));
                encode_iaid(&mut enc, &mut iaid, *id);
            }
            Op::EndStrip => encode_int(&mut enc, &mut ints.iads, None),
        }
    }

    let mut out = region_info_bytes(region.0, region.1);
    out.extend_from_slice(&flags_of(shape).to_be_bytes());
    out.extend_from_slice(&instances.to_be_bytes()); // SBNUMINSTANCES
    out.extend_from_slice(&enc.finish());
    out
}

/// One placement in a [`text_segment_with_curt`] strip: the delta on S, the T
/// offset within the strip, and the symbol id.
pub(crate) type Placement = (i32, i32, u32);

/// One strip of a [`text_segment_with_curt`] fixture: the delta on STRIPT,
/// counted in strips, and the placements the strip holds.
pub(crate) type StripOf<'a> = (i32, &'a [Placement]);

/// [`text_segment`] for a region whose SBSTRIPS is greater than one, where each
/// instance carries its own T offset within the strip.
///
/// The strips are given explicitly because the `IAIT` value falls between the S
/// coordinate and the symbol id, which the flat instruction list of [`Op`]
/// folds together. Two builders that each say plainly what they emit read
/// better than one with an optional value in the middle of its stream.
pub(crate) fn text_segment_with_curt(
    region: (u32, u32),
    shape: Shape,
    instances: u32,
    num_syms: u32,
    initial_dt: i32,
    strips: &[StripOf<'_>],
) -> Vec<u8> {
    let mut enc = MqEncoder::new();
    let mut ints = IntCtxSet::new();
    let mut iaid = IaidCtx::new(sym_code_len(num_syms));

    encode_int(&mut enc, &mut ints.iadt, Some(initial_dt));
    for (dt, placements) in strips {
        encode_int(&mut enc, &mut ints.iadt, Some(*dt));
        for (index, (ds, curt, id)) in placements.iter().enumerate() {
            if index == 0 {
                encode_int(&mut enc, &mut ints.iafs, Some(*ds));
            } else {
                encode_int(&mut enc, &mut ints.iads, Some(*ds));
            }
            encode_int(&mut enc, &mut ints.iait, Some(*curt));
            encode_iaid(&mut enc, &mut iaid, *id);
        }
        encode_int(&mut enc, &mut ints.iads, None);
    }

    let mut out = region_info_bytes(region.0, region.1);
    out.extend_from_slice(&flags_of(shape).to_be_bytes());
    out.extend_from_slice(&instances.to_be_bytes()); // SBNUMINSTANCES
    out.extend_from_slice(&enc.finish());
    out
}

/// One symbol instance of a refined text region fixture: the delta on S, the
/// T offset within the strip, the symbol id, and — when RI is 1 — the
/// refinement 6.4.11 codes for it.
pub(crate) struct RefinedPlacement<'a> {
    /// The delta on S: `IAFS` for a strip's first instance, `IADS` after.
    pub(crate) ds: i32,
    /// The T offset within the strip, coded only when SBSTRIPS is above 1.
    pub(crate) curt: i32,
    /// The symbol id.
    pub(crate) id: u32,
    /// `None` codes RI as 0; the instance is its symbol as it stands.
    pub(crate) refine: Option<Refine<'a>>,
}

/// The refinement of one placement: the bitmap the instance decodes to, the
/// four deltas of 6.4.11.1 to 6.4.11.4, and the reference offset the encoder
/// codes with.
///
/// `dx` and `dy` are stated by the caller rather than derived from the deltas
/// so that a test pins Table 12's `⌊RDW/2⌋ + RDX` by hand: the encoder codes
/// the target's pixels against the reference at the literal offset, the
/// decoder derives its own offset from the coded RDW and RDX, and the decoded
/// pixels match the target only when the derivation is the table's.
pub(crate) struct Refine<'a> {
    /// The bitmap the refined instance decodes to, of size
    /// `(symbol width + rdw, symbol height + rdh)`.
    pub(crate) target: &'a Bitmap,
    /// RDW, the signed refinement delta width.
    pub(crate) rdw: i32,
    /// RDH, the signed refinement delta height.
    pub(crate) rdh: i32,
    /// RDX, the refinement X offset.
    pub(crate) rdx: i32,
    /// RDY, the refinement Y offset.
    pub(crate) rdy: i32,
    /// GRREFERENCEDX, hand-derived from Table 12 by the test.
    pub(crate) dx: i32,
    /// GRREFERENCEDY, likewise.
    pub(crate) dy: i32,
}

/// One strip of a refined text region fixture: the delta on STRIPT, counted in
/// strips, and the placements the strip holds.
pub(crate) type RefinedStrip<'a> = (i32, &'a [RefinedPlacement<'a>]);

/// The two refinement AT pixel pairs at their nominal offsets, as the four
/// signed bytes the text region refinement AT flags field carries them in
/// (T.88 7.4.3.1.3).
fn refinement_at_bytes() -> Vec<u8> {
    let mut out = Vec::new();
    for (dx, dy) in REFINEMENT_NOMINAL_AT {
        out.push(dx as u8);
        out.push(dy as u8);
    }
    out
}

/// Builds the data of an arithmetic text region segment with SBREFINE set
/// (T.88 7.4.3, 6.4.11), placing the instances of `strips`.
///
/// Every value — the walk's integers, each instance's RI, a refined
/// instance's four deltas and its pixel decisions — is coded into the one
/// arithmetic codeword the region has, and the GR statistics adapt across the
/// refinements exactly as the walk's integer contexts do across its values.
/// The template is `shape.rtemplate`; with template 0 the header carries the
/// nominal SBRAT pixels, with template 1 no such field at all (7.4.3.1.3).
pub(crate) fn refined_text_segment(
    region: (u32, u32),
    shape: Shape,
    instances: u32,
    symbols: &[Bitmap],
    initial_dt: i32,
    strips: &[RefinedStrip<'_>],
) -> Vec<u8> {
    let mut enc = MqEncoder::new();
    let mut ints = IntCtxSet::new();
    let mut iaid = IaidCtx::new(sym_code_len(symbols.len() as u32));
    let mut gr = vec![MqContext::default(); GR_CONTEXT_LEN];
    let params = RefinementParams {
        template: shape.rtemplate,
        at: REFINEMENT_NOMINAL_AT,
        tpgron: false,
    };

    encode_int(&mut enc, &mut ints.iadt, Some(initial_dt));
    for (dt, placements) in strips {
        encode_int(&mut enc, &mut ints.iadt, Some(*dt));
        for (index, p) in placements.iter().enumerate() {
            if index == 0 {
                encode_int(&mut enc, &mut ints.iafs, Some(p.ds));
            } else {
                encode_int(&mut enc, &mut ints.iads, Some(p.ds));
            }
            if shape.log_strips > 0 {
                encode_int(&mut enc, &mut ints.iait, Some(p.curt));
            }
            encode_iaid(&mut enc, &mut iaid, p.id);
            let ri = i32::from(p.refine.is_some());
            encode_int(&mut enc, &mut ints.iari, Some(ri));
            let Some(r) = &p.refine else {
                continue;
            };
            encode_int(&mut enc, &mut ints.iardw, Some(r.rdw));
            encode_int(&mut enc, &mut ints.iardh, Some(r.rdh));
            encode_int(&mut enc, &mut ints.iardx, Some(r.rdx));
            encode_int(&mut enc, &mut ints.iardy, Some(r.rdy));
            encode_refinement_into(
                &mut enc,
                &mut gr,
                r.target,
                &symbols[p.id as usize],
                &params,
                r.dx,
                r.dy,
            );
        }
        encode_int(&mut enc, &mut ints.iads, None);
    }

    let mut out = region_info_bytes(region.0, region.1);
    out.extend_from_slice(&(flags_of(shape) | 0x0002).to_be_bytes());
    if shape.rtemplate == 0 {
        out.extend_from_slice(&refinement_at_bytes());
    }
    out.extend_from_slice(&instances.to_be_bytes()); // SBNUMINSTANCES
    out.extend_from_slice(&enc.finish());
    out
}

/// Builds the data of a Huffman text region segment with SBREFINE set
/// (T.88 7.4.3, 6.4.11), placing the instances of `strips`.
///
/// The selectors this builder writes exercise both refinement defaults: RDW,
/// RDX and RDY read standard Table B.14 and RDH reads B.15, with BMSIZE
/// through Table B.1. `rdw` overrides SBHUFFRDW with a user-supplied table,
/// which sets that selector to 3 and makes the segment's referred-to list
/// responsible for carrying the code table segment it came from (7.4.3.1.6).
///
/// Each instance spends one raw bit on RI, and each refinement is its own
/// arithmetic codeword: its byte count through SBHUFFRSIZE, a byte boundary,
/// then exactly that many bytes — while the GR statistics carry over from one
/// refinement to the next, since E.3.7 resets them per segment.
pub(crate) fn huffman_refined_text_segment(
    region: (u32, u32),
    shape: Shape,
    instances: u32,
    symbols: &[Bitmap],
    initial_dt: i32,
    strips: &[RefinedStrip<'_>],
    rdw: Option<&Table>,
) -> Vec<u8> {
    let standard_rdw = standard(14).expect("Table B.14");
    let rdw_table = rdw.unwrap_or(&standard_rdw);
    let rdh = standard(15).expect("Table B.15");
    let rd = standard(14).expect("Table B.14");
    let fs = standard(6).expect("Table B.6");
    let ds = standard(8).expect("Table B.8");
    let dt = standard(11).expect("Table B.11");
    let rsize = standard(1).expect("Table B.1");
    let lengths = symbol_code_lengths(symbols.len() as u32);
    let codes = from_code_lengths(&lengths, Unused::Refused).expect("symbol ID codes");
    let mut gr = vec![MqContext::default(); GR_CONTEXT_LEN];
    let params = RefinementParams {
        template: shape.rtemplate,
        at: REFINEMENT_NOMINAL_AT,
        tpgron: false,
    };

    let mut w = BitWriter::default();
    push_symbol_id_table(&mut w, &lengths);

    push_value(&mut w, &dt, Some(initial_dt));
    for (delta, placements) in strips {
        push_value(&mut w, &dt, Some(*delta));
        for (index, p) in placements.iter().enumerate() {
            if index == 0 {
                push_value(&mut w, &fs, Some(p.ds));
            } else {
                push_value(&mut w, &ds, Some(p.ds));
            }
            if shape.log_strips > 0 {
                w.push(p.curt as u32, shape.log_strips);
            }
            push_value(&mut w, &codes, Some(p.id as i32));
            // 6.4.11: RI is one bit read directly from the bitstream.
            w.push(u32::from(p.refine.is_some()), 1);
            let Some(r) = &p.refine else {
                continue;
            };
            push_value(&mut w, rdw_table, Some(r.rdw));
            push_value(&mut w, &rdh, Some(r.rdh));
            push_value(&mut w, &rd, Some(r.rdx));
            push_value(&mut w, &rd, Some(r.rdy));
            let mut chunk = MqEncoder::new();
            encode_refinement_into(
                &mut chunk,
                &mut gr,
                r.target,
                &symbols[p.id as usize],
                &params,
                r.dx,
                r.dy,
            );
            let coded = chunk.finish();
            push_value(&mut w, &rsize, Some(coded.len() as i32));
            w.align();
            w.push_bytes(&coded);
        }
        push_value(&mut w, &ds, None);
    }

    let mut out = region_info_bytes(region.0, region.1);
    out.extend_from_slice(&(flags_of(shape) | 0x0003).to_be_bytes());
    // 7.4.3.1.2: all-standard walk tables, RDH selecting B.15 and RDW
    // whichever the caller chose.
    let huffman_flags: u16 = (1 << 8) | if rdw.is_some() { 3 << 6 } else { 0 };
    out.extend_from_slice(&huffman_flags.to_be_bytes());
    if shape.rtemplate == 0 {
        out.extend_from_slice(&refinement_at_bytes());
    }
    out.extend_from_slice(&instances.to_be_bytes()); // SBNUMINSTANCES
    out.extend_from_slice(&w.finish());
    out
}

/// The symbol ID code lengths a Huffman text region fixture uses: the same
/// length for every symbol, wide enough to tell them apart
/// (T.88 7.4.3.1.7 step 7).
///
/// With `n` equal lengths of `ceil(log2 n)` bits, B.3 hands symbol *i* the code
/// *i*, so a fixture's symbol IDs are the numbers a reader expects to see in
/// the bits. One bit is the floor: B.3 assigns no zero-length code, so a region
/// with a single symbol still spends a bit naming it — unlike the arithmetic
/// variant, where [`sym_code_len`] of one symbol is 0 and no bits are coded.
pub(crate) fn symbol_code_lengths(num_syms: u32) -> Vec<u8> {
    let len = sym_code_len(num_syms).max(1) as u8;
    vec![len; num_syms as usize]
}

/// Writes the symbol ID Huffman decoding table of T.88 7.4.3.1.7 for
/// `lengths`, ending on the byte boundary step 6 asks for.
///
/// Every one of the thirty-five run codes is given a six-bit length, which is
/// legal — 35 codes fit in 64 — and makes B.3 assign RUNCODE*n* the six-bit
/// binary of *n*. So the fixture writes one run code per symbol, as a plain
/// six-bit number, and needs no Huffman tree of its own to state what an
/// encoder would have emitted. RUNCODE32 to RUNCODE34, the three that compress
/// runs, are deliberately never used here: a fixture that leaned on them would
/// be asserting the decoder's own reading of Table 29 rather than a placement.
fn push_symbol_id_table(w: &mut BitWriter, lengths: &[u8]) {
    // Step 1: thirty-five four-bit run code lengths.
    for _ in 0..35 {
        w.push(6, 4);
    }
    // Steps 3 and 4: RUNCODE<len> says "the next symbol ID code length is len".
    for &len in lengths {
        assert!(len < 32, "a run code names lengths 0 to 31");
        w.push(u32::from(len), 6);
    }
    // Step 6.
    w.align();
}

/// Builds the data of a Huffman-coded text region segment (T.88 7.4.3),
/// placing the instances of `strips`.
///
/// The header is the one 7.4.3.1 lays out with SBHUFF set: the ordinary flags,
/// then a Huffman flags word, then SBNUMINSTANCES, then the symbol ID table —
/// which is where a parser that reads the instance count straight after the
/// flags goes wrong, since it takes the table selectors for the top half of it.
///
/// The standard tables this selects put one requirement on the caller. Tables
/// B.11, B.12 and B.13, the three SBHUFFDT may name, code no value below 1, so
/// `initial_dt` and every strip's delta must be positive; STRIPT reaches 0 by
/// starting at −1 × SBSTRIPS and being advanced by one strip, which is what an
/// encoder using these tables does.
///
/// `fs` overrides SBHUFFFS with a user-supplied table, which sets that selector
/// to 3 and makes the segment's referred-to list responsible for carrying the
/// code table segment it came from (7.4.3.1.6). `None` selects standard
/// Table B.6.
pub(crate) fn huffman_text_segment(
    region: (u32, u32),
    shape: Shape,
    instances: u32,
    num_syms: u32,
    initial_dt: i32,
    strips: &[StripOf<'_>],
    fs: Option<&Table>,
) -> Vec<u8> {
    let standard_fs = standard(6).expect("Table B.6");
    let fs_table = fs.unwrap_or(&standard_fs);
    let ds = standard(8).expect("Table B.8");
    let dt = standard(11).expect("Table B.11");
    let lengths = symbol_code_lengths(num_syms);
    let codes = from_code_lengths(&lengths, Unused::Refused).expect("symbol ID codes");

    let mut w = BitWriter::default();
    push_symbol_id_table(&mut w, &lengths);

    // 6.4.5 step 2, then the strips.
    push_value(&mut w, &dt, Some(initial_dt));
    for (delta, placements) in strips {
        push_value(&mut w, &dt, Some(*delta));
        for (index, (ds_value, curt, id)) in placements.iter().enumerate() {
            if index == 0 {
                push_value(&mut w, fs_table, Some(*ds_value));
            } else {
                push_value(&mut w, &ds, Some(*ds_value));
            }
            // 6.4.9: nothing at all when SBSTRIPS is 1.
            if shape.log_strips > 0 {
                w.push(*curt as u32, shape.log_strips);
            }
            push_value(&mut w, &codes, Some(*id as i32));
        }
        // OOB closes the strip (6.4.8).
        push_value(&mut w, &ds, None);
    }

    let mut out = region_info_bytes(region.0, region.1);
    out.extend_from_slice(&(flags_of(shape) | 0x0001).to_be_bytes());
    // 7.4.3.1.2, with every refinement selector 0 as SBREFINE requires.
    let huffman_flags: u16 = if fs.is_some() { 3 } else { 0 };
    out.extend_from_slice(&huffman_flags.to_be_bytes());
    out.extend_from_slice(&instances.to_be_bytes()); // SBNUMINSTANCES
    out.extend_from_slice(&w.finish());
    out
}

/// [`text_segment`] for the common page-level fixture: the region covers the
/// whole page, the flags are the defaults, and SBNUMINSTANCES is counted from
/// the instructions rather than stated, so a fixture cannot disagree with
/// itself about how many symbols it places.
pub(crate) fn text_segment_for_page(page: (u32, u32), num_syms: u32, ops: &[Op]) -> Vec<u8> {
    let instances = ops
        .iter()
        .filter(|op| matches!(op, Op::First(..) | Op::Next(..)))
        .count() as u32;
    text_segment(page, Shape::default(), instances, num_syms, 0, ops)
}

/// The offset just past the segment numbered `number` in an embedded stream.
///
/// Splitting a fixture there yields a `/JBIG2Globals` stream and a page stream
/// that between them hold the same segments in the same order, which is how a
/// test puts a dictionary in the globals without building it twice. The walk
/// mirrors the segment splitter's: header, then the data length the header
/// declared.
pub(crate) fn split_after_segment(stream: &[u8], number: u32) -> usize {
    let mut r = Reader::new(stream);
    while !r.is_empty() {
        let header = parse_header(&mut r).expect("header");
        let len = header.data_len.expect("declared data length") as usize;
        r.take(len).expect("segment data");
        if header.number == number {
            return r.pos();
        }
    }
    panic!("no segment numbered {number}");
}

/// An embedded stream (T.88 Annex D.3) holding one immediate generic region
/// that covers the page, with the page's width and height.
///
/// The pixels are fixed so that a test can state the sample bytes by hand:
/// eight columns and two rows, the first row `10000000` and the second
/// `01010101`. Packed as JBIG2 codes them that is `80 55`; inverted for
/// `/DeviceGray` it is `7F AA`. None of those four bytes is a palindrome or
/// the complement of its neighbour, so neither a dropped inversion nor a
/// reversed bit order can produce them by accident.
pub(crate) fn generic_region_stream() -> (Vec<u8>, u32, u32) {
    let bm = glyph(&["10000000", "01010101"]);

    let mut enc = MqEncoder::new();
    let mut gb = vec![MqContext::default(); GB_CONTEXT_LEN];
    encode_pixels(&mut enc, &mut gb, &bm);

    let mut region = region_info_bytes(bm.width(), bm.height());
    region.push(0); // MMR 0, template 0, TPGDON 0
    region.extend_from_slice(&nominal_at_bytes());
    region.extend_from_slice(&enc.finish());

    let mut out = header(0, 38, &[], 1, region.len() as u32);
    out.extend_from_slice(&region);
    out.extend_from_slice(&header(1, 49, &[], 1, 0)); // end of page
    (out, bm.width(), bm.height())
}

/// Asserts that `symbol` was drawn into `region` with its top-left pixel at
///
/// Reading through [`Bitmap::get`] means a placement that hangs off an edge
/// compares against the zeros outside the region rather than indexing out of
/// it, so a symbol expected off-page is checked rather than skipped.
pub(crate) fn expect_at(region: &Bitmap, symbol: &Bitmap, x: i64, y: i64) {
    for sy in 0..symbol.height() {
        for sx in 0..symbol.width() {
            let want = symbol.get(i64::from(sx), i64::from(sy));
            let got = region.get(x + i64::from(sx), y + i64::from(sy));
            assert_eq!(got, want, "symbol pixel ({sx}, {sy}) at region ({x}, {y})");
        }
    }
}
