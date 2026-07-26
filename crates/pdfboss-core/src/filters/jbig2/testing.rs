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

use super::arith_int::encoder::encode_int;
use super::arith_int::IntCtxSet;
use super::bitmap::Bitmap;
use super::generic::{context_at, GenericParams, GB_CONTEXT_LEN, NOMINAL_AT};
use super::mq::encoder::MqEncoder;
use super::mq::MqContext;

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
