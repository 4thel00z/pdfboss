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
use super::bitmap::Bitmap;
use super::generic::{context_at, GenericParams, GB_CONTEXT_LEN, NOMINAL_AT};
use super::mq::encoder::MqEncoder;
use super::mq::MqContext;
use super::text_region::sym_code_len;

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

/// Codes one symbol's pixels into `enc` through the shared generic-region
/// context array `gb` (T.88 6.2.5.7).
///
/// The array is the caller's rather than this function's for the same reason
/// the decoder's is: a symbol dictionary codes every symbol through one array,
/// and the adaptation carried from one symbol to the next is what makes the
/// coding efficient. A fresh array per symbol would produce bytes the decoder
/// reads back as noise from the second symbol onward.
fn encode_symbol(enc: &mut MqEncoder, gb: &mut [MqContext], symbol: &Bitmap) {
    let params = GenericParams::nominal(0);
    for y in 0..symbol.height() {
        for x in 0..symbol.width() {
            let ctx = usize::from(context_at(symbol, x, y, &params));
            enc.encode(&mut gb[ctx], symbol.get(i64::from(x), i64::from(y)));
        }
    }
}

/// Builds the data of an arithmetic symbol dictionary segment (T.88 7.4.3)
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
            encode_symbol(&mut enc, &mut gb, symbol);
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

/// The four AT pixel pairs of template 0 at their nominal offsets, as the
/// eight signed bytes a segment header carries them in (T.88 7.4.3.1.2).
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

/// The text region segment flags a fixture sets (T.88 7.4.4.1.1).
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
        }
    }
}

/// Packs a [`Shape`] into the two-byte text region segment flags field
/// (T.88 7.4.4.1.1), with SBHUFF, REFINE and SBRTEMPLATE all clear.
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

/// Builds the data of an arithmetic text region segment (T.88 7.4.4) placing
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

/// Asserts that `symbol` was drawn into `region` with its top-left pixel at
/// `(x, y)`, pixel for pixel.
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
