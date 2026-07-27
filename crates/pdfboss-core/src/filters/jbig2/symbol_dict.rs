//! Symbol dictionary segments (T.88 6.5, 7.4.3).
//!
//! A scanned page of text is not coded as pixels. The encoder finds the
//! connected components on the scan, clusters the ones that look alike, and
//! codes each distinct shape once into a dictionary; the page then becomes a
//! list of (symbol, position) placements. A page carrying four thousand
//! instances of two hundred shapes stores two hundred bitmaps and four thousand
//! small integers, which is why this is the segment type a text-scanned
//! document is made of.
//!
//! The shape of the coded data is what this module exists to follow. Symbols
//! are grouped into **height classes**: `IADH` gives the delta on a running
//! height, `IADW` the deltas on a running width within the class, and an OOB
//! from `IADW` closes the class. The outer loop runs until the declared symbol
//! count is reached. Each symbol's bitmap is an ordinary generic region
//! (6.5.8.1), coded through the *same* arithmetic decoder and the *same*
//! context array as every other symbol in the dictionary — the adaptation
//! carried across symbols is most of what makes the coding compact, and a fresh
//! array per symbol would decode the first symbol correctly and then noise.
//!
//! Finally the dictionary says which symbols it passes on, as run lengths over
//! the input symbols followed by the new ones (6.5.10).
//!
//! Huffman-coded dictionaries (SDHUFF) and refinement/aggregate coding
//! (SDREFAGG) are refused by name rather than approximated.

use super::arith_int::{decode_int, IntCtxSet};
use super::bitmap::Bitmap;
use super::budget::Budget;
use super::generic::{decode_generic_region, GenericParams, GB_CONTEXT_LEN};
use super::mq::{MqContexts, MqDecoder};
use super::reader::Reader;
use super::Jbig2Error;

/// The most symbols one dictionary may hold, counting its inputs, its new
/// symbols and its exports separately.
///
/// T.88 gives each of those counts a 32-bit field and no ceiling. The figure
/// here is the one the symbol-ID code length is bounded by, since 65 536
/// symbols are exactly what a 16-bit ID addresses, and it is checked before any
/// of the three counts drives a loop or an allocation.
pub(crate) const MAX_SYMBOLS: u32 = 65_536;

/// What one symbol costs beyond the pixels it is made of, in the units
/// [`Budget`] counts.
///
/// Two things a symbol always costs are invisible to a charge computed from its
/// dimensions. It takes at least one arithmetic width decode to bring into
/// existence, and a symbol with no rows has no pixels for that charge to land
/// on — `height * (width + ROW_COST)` is zero when the height is — so without a
/// fixed price a dictionary of tens of thousands of rowless symbols is decoded
/// for nothing. And a symbol that is exported is *kept*: the page walk holds
/// every dictionary's exports until the last segment is read, so unlike a
/// region's pixels the space is never given back mid-stream.
///
/// The figure is not an accounting of either. It is the price that ties the
/// number of symbols a stream may bring into existence to the one allowance the
/// stream has: at this rate [`MAX_WORK`](super::budget::MAX_WORK) buys 524 288
/// of them, which is eight times what the symbol ID code can even address and a
/// few tens of megabytes of bookkeeping if a stream insists on all of them.
pub(crate) const SYMBOL_COST: u64 = 512;

/// How much slack the two coded-data loops are given over the smallest number
/// of iterations that could express the same dictionary.
///
/// Both loops can be made to iterate without advancing: an empty height class
/// codes no symbol, a zero-length export run fills no flag, and neither is
/// forbidden. Bounding each loop at this multiple of the count it is filling
/// keeps the end of the loop a property of the segment header rather than of
/// the coded data, while leaving an encoder that emits a degenerate iteration
/// here and there entirely alone.
const LOOP_SLACK: usize = 2;

/// Decodes a symbol dictionary segment's data (T.88 7.4.3), returning the
/// symbols it exports.
///
/// `input_symbols` are the symbols exported by the referred-to dictionary
/// segments, concatenated in the order the referred-to list gives them
/// (SDINSYMS). They may be re-exported by this dictionary, so they take part in
/// the export runs of 6.5.10 ahead of the symbols coded here.
///
/// `budget` is the embedded stream's remaining allowance of decoding work, the
/// same one the page's regions draw on. Every symbol this dictionary yields is
/// charged against it — from the dimensions the coded data declared, before its
/// pixel loop is entered, plus [`SYMBOL_COST`] for existing at all. Both halves
/// are needed. A dictionary need not carry the bits it asks to have decoded, so
/// the cost cannot be bounded by the segment's length; and a symbol may cost no
/// pixels, either because its height class has no rows or because it was copied
/// from the input list rather than coded, so it cannot be bounded by pixels
/// alone.
pub(crate) fn decode_symbol_dict(
    data: &[u8],
    input_symbols: &[&Bitmap],
    budget: &mut Budget,
) -> Result<Vec<Bitmap>, Jbig2Error> {
    let mut r = Reader::new(data);
    let header = parse_header(&mut r)?;
    let num_input = u32::try_from(input_symbols.len())
        .map_err(|_| Jbig2Error::Malformed("symbol count exceeds the limit"))?;
    if num_input > MAX_SYMBOLS || num_input.saturating_add(header.num_new) > MAX_SYMBOLS {
        return Err(Jbig2Error::Malformed("symbol count exceeds the limit"));
    }

    let mut dec = MqDecoder::new(r.rest());
    let mut gb = MqContexts::new(GB_CONTEXT_LEN);
    let mut ints = IntCtxSet::new();
    let new_symbols = decode_height_classes(&mut dec, &mut gb, &mut ints, &header, budget)?;
    let flags = decode_export_flags(&mut dec, &mut ints, input_symbols.len() + new_symbols.len())?;

    // The flags run over the input symbols and then the new ones, in that
    // order, one flag each — so walking the two lists against one iterator is
    // the whole of 6.5.10's "exported set". An exported input symbol is copied
    // because the caller keeps its dictionary; an exported new symbol is moved,
    // since a run visits each index once and nothing else will want it.
    //
    // The copy is charged like a symbol that had just been decoded, and for the
    // same reason: this segment codes nothing to obtain it, so the price of a
    // bitmap here is whatever the caller's referred-to list decided. Naming one
    // dictionary again and again is a legal way to write that list, and each
    // occurrence contributes its exports afresh, so an uncharged copy would let
    // four bytes of segment number duplicate an entire decoded symbol.
    let mut exported: Vec<Bitmap> = Vec::new();
    let mut flags = flags.into_iter();
    for symbol in input_symbols {
        if flags.next().unwrap_or(false) {
            budget.charge(SYMBOL_COST)?;
            budget.charge_region(symbol.width(), symbol.height())?;
            exported.push((*symbol).clone());
        }
    }
    for symbol in new_symbols {
        if flags.next().unwrap_or(false) {
            exported.push(symbol);
        }
    }
    if exported.len() != header.num_ex as usize {
        return Err(Jbig2Error::Malformed(
            "exported symbol count disagrees with the header",
        ));
    }
    Ok(exported)
}

/// The fields of a symbol dictionary segment that precede its coded data
/// (T.88 7.4.3.1).
struct DictHeader {
    /// The generic region parameters every symbol bitmap is coded with:
    /// SDTEMPLATE and SDAT, with typical prediction off (6.5.8.1).
    params: GenericParams,
    /// SDNUMEXSYMS, the number of symbols the dictionary exports.
    num_ex: u32,
    /// SDNUMNEWSYMS, the number of symbols coded in this segment.
    num_new: u32,
}

/// Parses the symbol dictionary flags and the fields that follow them
/// (T.88 7.4.3.1.1 to 7.4.3.1.4).
///
/// The two coding modes this build does not implement are refused before a
/// single further byte is read, because the layout of everything after the
/// flags depends on them: a Huffman dictionary carries no AT pixels, so reading
/// them would leave the cursor eight bytes into the wrong field and turn an
/// unsupported stream into a plausible-looking wrong answer.
///
/// Bits 2 to 7 select Huffman tables and are meaningless with SDHUFF clear, so
/// they are not examined at all. Bits 8 and 9 — "bitmap coding context used"
/// and "retained" — ask for the arithmetic context array to be carried in from,
/// or handed on to, another dictionary segment. Both are accepted and ignored:
/// they change nothing for a dictionary that codes its symbols within one
/// segment, which is every dictionary that does not deliberately split itself,
/// and honouring them would mean keeping a context array alive across the
/// segment walk for a case no encoder in practice emits. Bits 13 to 15 are
/// reserved; they select no field, so a stream that sets one still describes a
/// dictionary that can be read.
fn parse_header(r: &mut Reader<'_>) -> Result<DictHeader, Jbig2Error> {
    let flags = r.u16()?;
    if flags & 0x0001 != 0 {
        return Err(Jbig2Error::Unimplemented("Huffman-coded symbol dictionary"));
    }
    if flags & 0x0002 != 0 {
        return Err(Jbig2Error::Unimplemented(
            "refinement/aggregate symbol coding",
        ));
    }
    let template = ((flags >> 10) & 0x3) as u8;

    // 7.4.3.1.2: eight AT bytes for template 0, two for the rest. The slots a
    // template does not use keep their nominal offsets, so the parameters
    // always describe a complete neighbourhood.
    let mut params = GenericParams::nominal(template);
    let at_pairs = if template == 0 { 4 } else { 1 };
    for slot in params.at.iter_mut().take(at_pairs) {
        let dx = r.i8()?;
        let dy = r.i8()?;
        *slot = (dx, dy);
    }

    let num_ex = r.u32()?;
    let num_new = r.u32()?;
    if num_ex > MAX_SYMBOLS || num_new > MAX_SYMBOLS {
        return Err(Jbig2Error::Malformed("symbol count exceeds the limit"));
    }
    Ok(DictHeader {
        params,
        num_ex,
        num_new,
    })
}

/// Decodes the new symbols of a dictionary, height class by height class
/// (T.88 6.5.5).
///
/// `dec`, `gb` and `ints` are shared across every symbol by design; see the
/// module documentation for why the context array in particular must be.
///
/// Both loops end on something the input cannot extend indefinitely. The inner
/// one either takes a symbol — and there are at most SDNUMNEWSYMS of those
/// before the count is exceeded and the stream refused — or reads the OOB that
/// closes the class, which is also what an exhausted arithmetic decoder returns
/// (T.88 E.3.4). The outer one is capped from SDNUMNEWSYMS, because a height
/// class that codes no symbol advances nothing and a stream of those would
/// otherwise be a loop the coded data decides the length of.
fn decode_height_classes(
    dec: &mut MqDecoder<'_>,
    gb: &mut MqContexts,
    ints: &mut IntCtxSet,
    header: &DictHeader,
    budget: &mut Budget,
) -> Result<Vec<Bitmap>, Jbig2Error> {
    let mut new_symbols: Vec<Bitmap> = Vec::new();
    // The running height, and the running width inside each class below, both
    // accumulate signed deltas and are therefore free to go negative on a
    // malformed stream. Each is held wider than the dimension it becomes so
    // that the check is a comparison rather than a cast that has already lost
    // the sign.
    let mut height: i64 = 0;
    // One class per symbol is the most a dictionary needs, since a class holds
    // at least one symbol unless it is empty; the slack covers the empty ones.
    let max_classes = (header.num_new as usize)
        .saturating_mul(LOOP_SLACK)
        .saturating_add(LOOP_SLACK);
    let mut classes = 0usize;

    while (new_symbols.len() as u32) < header.num_new {
        classes += 1;
        if classes > max_classes {
            return Err(Jbig2Error::Malformed("too many symbol height classes"));
        }
        let delta = decode_int(dec, &mut ints.iadh).ok_or(Jbig2Error::Malformed(
            "unexpected OOB decoding a height class",
        ))?;
        height += i64::from(delta);
        if height < 0 {
            return Err(Jbig2Error::Malformed("negative symbol height class"));
        }
        let class_height =
            u32::try_from(height).map_err(|_| Jbig2Error::Malformed("symbol too tall"))?;

        // OOB closes the height class, and an exhausted decoder reads as OOB,
        // so a truncated segment ends the class rather than looping on
        // synthesized bits.
        let mut width: i64 = 0;
        while let Some(delta) = decode_int(dec, &mut ints.iadw) {
            width += i64::from(delta);
            if width < 0 {
                return Err(Jbig2Error::Malformed("negative symbol width"));
            }
            let symbol_width =
                u32::try_from(width).map_err(|_| Jbig2Error::Malformed("symbol too wide"))?;
            if (new_symbols.len() as u32) >= header.num_new {
                return Err(Jbig2Error::Malformed("more symbols coded than declared"));
            }
            // The generic region decoder charges for the symbol's pixels, which
            // is nothing at all when the height class has no rows — and a
            // rowless symbol still costs the width decode that produced it and
            // a bitmap the caller may keep for the rest of the stream. Hence
            // the fixed price here, before the region charge and before any of
            // its pixels are read.
            budget.charge(SYMBOL_COST)?;
            new_symbols.push(decode_generic_region(
                dec,
                gb,
                budget,
                symbol_width,
                class_height,
                &header.params,
                None,
            )?);
        }
    }
    Ok(new_symbols)
}

/// Decodes the export flags of a dictionary (T.88 6.5.10), one per symbol over
/// the input symbols followed by the new ones.
///
/// The flags are run lengths, alternating between "not exported" and
/// "exported" and starting with the former. A run that would carry the index
/// past the end of the list is a malformed stream, not a place to stop early:
/// the runs describe a partition of a list whose length both sides already
/// agree on.
///
/// A zero-length run is legal and is how a dictionary starts with "exported" —
/// it flips the flag without consuming an index. That is also the reason for
/// the count: a partition of `total` entries never needs more than one run per
/// entry plus a leading empty one, so a stream offering more than that is
/// spending runs that fill nothing.
fn decode_export_flags(
    dec: &mut MqDecoder<'_>,
    ints: &mut IntCtxSet,
    total: usize,
) -> Result<Vec<bool>, Jbig2Error> {
    let mut flags = vec![false; total];
    let max_runs = total.saturating_mul(LOOP_SLACK).saturating_add(LOOP_SLACK);
    let mut index = 0usize;
    let mut exporting = false;
    let mut runs = 0usize;
    while index < total {
        runs += 1;
        if runs > max_runs {
            return Err(Jbig2Error::Malformed("too many symbol export runs"));
        }
        let run = decode_int(dec, &mut ints.iaex).ok_or(Jbig2Error::Malformed(
            "unexpected OOB decoding export flags",
        ))?;
        let run = usize::try_from(run).map_err(|_| Jbig2Error::Malformed("negative export run"))?;
        if run > total - index {
            return Err(Jbig2Error::Malformed(
                "export run runs past the symbol list",
            ));
        }
        if exporting {
            for flag in flags.iter_mut().skip(index).take(run) {
                *flag = true;
            }
        }
        index += run;
        exporting = !exporting;
    }
    Ok(flags)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::jbig2::arith_int::encoder::encode_int;
    use crate::filters::jbig2::budget::ROW_COST;
    use crate::filters::jbig2::generic::context_at;
    use crate::filters::jbig2::mq::encoder::MqEncoder;
    use crate::filters::jbig2::mq::MqContext;
    use crate::filters::jbig2::testing::{
        dictionary_segment, glyph, nominal_at_bytes, reexport_segment, rowless_dictionary_segment,
        sample_symbols,
    };

    /// What one 4 x 4 symbol costs: the fixed per-symbol price and its rows.
    const FOUR_BY_FOUR: u64 = SYMBOL_COST + (4 + ROW_COST) * 4;

    /// Decodes with the allowance a real embedded stream gets.
    fn decode(data: &[u8], inputs: &[&Bitmap]) -> Result<Vec<Bitmap>, Jbig2Error> {
        decode_symbol_dict(data, inputs, &mut Budget::new())
    }

    fn assert_same(got: &Bitmap, want: &Bitmap, which: usize) {
        assert_eq!(
            (got.width(), got.height()),
            (want.width(), want.height()),
            "symbol {which}",
        );
        for y in 0..want.height() {
            assert_eq!(got.row(y), want.row(y), "symbol {which}, row {y}");
        }
    }

    #[test]
    fn decodes_symbols_across_height_classes() {
        let want = sample_symbols();
        let segment = dictionary_segment(&want, 0);
        let got = decode(&segment, &[]).expect("dictionary");
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_same(g, w, i);
        }
    }

    /// A single symbol is the degenerate case: one height class, one width, one
    /// export run.
    #[test]
    fn decodes_a_single_symbol_dictionary() {
        let want = vec![glyph(&["1"])];
        let segment = dictionary_segment(&want, 0);
        let got = decode(&segment, &[]).expect("dictionary");
        assert_eq!(got.len(), 1);
        assert_eq!(got[0].get(0, 0), 1);
    }

    /// One arithmetic decoder and one context array serve the whole dictionary,
    /// so a symbol decoded second depends on the adaptation left by the first.
    ///
    /// Sixteen symbols of the same shape in one height class is the case that
    /// separates a shared array from a per-symbol one: with the array shared,
    /// the repeats cost almost nothing and decode back exactly; with a fresh
    /// array per symbol the first symbol still decodes and the rest turn to
    /// noise, which is a failure that looks like a placement bug rather than a
    /// coding one.
    #[test]
    fn adaptation_carries_from_one_symbol_to_the_next() {
        let want: Vec<Bitmap> = (0..16).map(|_| glyph(&["1101", "0110", "1011"])).collect();
        let segment = dictionary_segment(&want, 0);
        let got = decode(&segment, &[]).expect("dictionary");
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_same(g, w, i);
        }
    }

    /// Input symbols are re-exportable: the export runs index the input symbols
    /// first and the new ones after (6.5.10).
    #[test]
    fn input_symbols_can_be_re_exported() {
        let inputs = [glyph(&["11", "11"])];
        let new = glyph(&["10", "01"]);

        // A zero-length "not exported" run flips the flag without consuming an
        // index, so the single run that follows exports both symbols.
        let params = GenericParams::nominal(0);
        let mut enc = MqEncoder::new();
        let mut ints = IntCtxSet::new();
        let mut gb = vec![MqContext::default(); GB_CONTEXT_LEN];
        encode_int(&mut enc, &mut ints.iadh, Some(2));
        encode_int(&mut enc, &mut ints.iadw, Some(2));
        for y in 0..2u32 {
            for x in 0..2u32 {
                let ctx = usize::from(context_at(&new, x, y, &params));
                enc.encode(&mut gb[ctx], new.get(i64::from(x), i64::from(y)));
            }
        }
        encode_int(&mut enc, &mut ints.iadw, None);
        encode_int(&mut enc, &mut ints.iaex, Some(0));
        encode_int(&mut enc, &mut ints.iaex, Some(2));

        let mut segment = 0u16.to_be_bytes().to_vec();
        segment.extend_from_slice(&nominal_at_bytes());
        segment.extend_from_slice(&2u32.to_be_bytes()); // SDNUMEXSYMS
        segment.extend_from_slice(&1u32.to_be_bytes()); // SDNUMNEWSYMS
        segment.extend_from_slice(&enc.finish());

        let refs: Vec<&Bitmap> = inputs.iter().collect();
        let got = decode(&segment, &refs).expect("dictionary");
        assert_eq!(got.len(), 2);
        assert_same(&got[0], &inputs[0], 0);
        assert_same(&got[1], &new, 1);
    }

    /// A dictionary that exports none of its symbols is legal and yields
    /// nothing.
    #[test]
    fn a_dictionary_can_export_nothing() {
        let new = glyph(&["1"]);
        let mut enc = MqEncoder::new();
        let mut ints = IntCtxSet::new();
        let mut gb = vec![MqContext::default(); GB_CONTEXT_LEN];
        encode_int(&mut enc, &mut ints.iadh, Some(1));
        encode_int(&mut enc, &mut ints.iadw, Some(1));
        let ctx = usize::from(context_at(&new, 0, 0, &GenericParams::nominal(0)));
        enc.encode(&mut gb[ctx], 1);
        encode_int(&mut enc, &mut ints.iadw, None);
        encode_int(&mut enc, &mut ints.iaex, Some(1)); // one symbol, not exported

        let mut segment = 0u16.to_be_bytes().to_vec();
        segment.extend_from_slice(&nominal_at_bytes());
        segment.extend_from_slice(&0u32.to_be_bytes()); // SDNUMEXSYMS
        segment.extend_from_slice(&1u32.to_be_bytes()); // SDNUMNEWSYMS
        segment.extend_from_slice(&enc.finish());

        assert_eq!(decode(&segment, &[]), Ok(Vec::new()));
    }

    /// A header that promises more exports than the runs deliver is refused,
    /// rather than returning a short list a text region would then index past.
    #[test]
    fn an_export_count_disagreeing_with_the_runs_is_rejected() {
        let mut segment = dictionary_segment(&sample_symbols(), 0);
        segment[10..14].copy_from_slice(&2u32.to_be_bytes()); // SDNUMEXSYMS: 3 -> 2
        assert_eq!(
            decode(&segment, &[]),
            Err(Jbig2Error::Malformed(
                "exported symbol count disagrees with the header"
            )),
        );
    }

    #[test]
    fn huffman_and_refagg_report_themselves() {
        for (flags, want) in [
            (0x0001u16, "Huffman-coded symbol dictionary"),
            (0x0002, "refinement/aggregate symbol coding"),
        ] {
            let mut segment = flags.to_be_bytes().to_vec();
            segment.extend_from_slice(&[0u8; 16]);
            assert_eq!(
                decode(&segment, &[]),
                Err(Jbig2Error::Unimplemented(want)),
                "flags {flags:#06x}",
            );
        }
    }

    #[test]
    fn a_negative_height_class_is_rejected() {
        let mut enc = MqEncoder::new();
        let mut ints = IntCtxSet::new();
        encode_int(&mut enc, &mut ints.iadh, Some(-1));
        let mut segment = 0u16.to_be_bytes().to_vec();
        segment.extend_from_slice(&nominal_at_bytes());
        segment.extend_from_slice(&1u32.to_be_bytes());
        segment.extend_from_slice(&1u32.to_be_bytes());
        segment.extend_from_slice(&enc.finish());
        assert_eq!(
            decode(&segment, &[]),
            Err(Jbig2Error::Malformed("negative symbol height class")),
        );
    }

    #[test]
    fn a_negative_symbol_width_is_rejected() {
        let mut enc = MqEncoder::new();
        let mut ints = IntCtxSet::new();
        encode_int(&mut enc, &mut ints.iadh, Some(4));
        encode_int(&mut enc, &mut ints.iadw, Some(-1));
        let mut segment = 0u16.to_be_bytes().to_vec();
        segment.extend_from_slice(&nominal_at_bytes());
        segment.extend_from_slice(&1u32.to_be_bytes());
        segment.extend_from_slice(&1u32.to_be_bytes());
        segment.extend_from_slice(&enc.finish());
        assert_eq!(
            decode(&segment, &[]),
            Err(Jbig2Error::Malformed("negative symbol width")),
        );
    }

    /// A height class that keeps coding symbols after the declared count is
    /// exhausted is refused rather than silently truncated.
    #[test]
    fn more_symbols_than_declared_is_rejected() {
        let mut segment = dictionary_segment(&sample_symbols(), 0);
        segment[14..18].copy_from_slice(&1u32.to_be_bytes()); // SDNUMNEWSYMS: 3 -> 1
        assert_eq!(
            decode(&segment, &[]),
            Err(Jbig2Error::Malformed("more symbols coded than declared")),
        );
    }

    #[test]
    fn an_absurd_symbol_count_is_refused_before_allocating() {
        let mut segment = 0u16.to_be_bytes().to_vec();
        segment.extend_from_slice(&nominal_at_bytes());
        segment.extend_from_slice(&u32::MAX.to_be_bytes()); // SDNUMEXSYMS
        segment.extend_from_slice(&u32::MAX.to_be_bytes()); // SDNUMNEWSYMS
        assert_eq!(
            decode(&segment, &[]),
            Err(Jbig2Error::Malformed("symbol count exceeds the limit")),
        );
    }

    /// A height class that codes no symbol is well formed and advances
    /// nothing, so a stream of them is refused rather than looped on.
    ///
    /// The fixture codes far more empty classes than the declared symbol count
    /// can justify and never codes the symbol it promised. Without the cap the
    /// loop would run until the arithmetic decoder ran out of data, which is a
    /// length the stream picks — and a stream is free to pick a long one.
    #[test]
    fn a_stream_of_empty_height_classes_is_refused() {
        let mut enc = MqEncoder::new();
        let mut ints = IntCtxSet::new();
        for _ in 0..64 {
            encode_int(&mut enc, &mut ints.iadh, Some(0));
            encode_int(&mut enc, &mut ints.iadw, None);
        }
        let mut segment = 0u16.to_be_bytes().to_vec();
        segment.extend_from_slice(&nominal_at_bytes());
        segment.extend_from_slice(&1u32.to_be_bytes()); // SDNUMEXSYMS
        segment.extend_from_slice(&1u32.to_be_bytes()); // SDNUMNEWSYMS
        segment.extend_from_slice(&enc.finish());
        assert_eq!(
            decode(&segment, &[]),
            Err(Jbig2Error::Malformed("too many symbol height classes")),
        );
    }

    /// The same hazard one level up: a zero-length export run is legal and
    /// fills no flag, so a stream of them is refused rather than looped on.
    #[test]
    fn a_stream_of_empty_export_runs_is_refused() {
        let new = glyph(&["1"]);
        let mut enc = MqEncoder::new();
        let mut ints = IntCtxSet::new();
        let mut gb = vec![MqContext::default(); GB_CONTEXT_LEN];
        encode_int(&mut enc, &mut ints.iadh, Some(1));
        encode_int(&mut enc, &mut ints.iadw, Some(1));
        let ctx = usize::from(context_at(&new, 0, 0, &GenericParams::nominal(0)));
        enc.encode(&mut gb[ctx], 1);
        encode_int(&mut enc, &mut ints.iadw, None);
        for _ in 0..64 {
            encode_int(&mut enc, &mut ints.iaex, Some(0));
        }
        let mut segment = 0u16.to_be_bytes().to_vec();
        segment.extend_from_slice(&nominal_at_bytes());
        segment.extend_from_slice(&1u32.to_be_bytes()); // SDNUMEXSYMS
        segment.extend_from_slice(&1u32.to_be_bytes()); // SDNUMNEWSYMS
        segment.extend_from_slice(&enc.finish());
        assert_eq!(
            decode(&segment, &[]),
            Err(Jbig2Error::Malformed("too many symbol export runs")),
        );
    }

    /// An export run reaching past the end of the symbol list is malformed
    /// input, and must be caught before it indexes anything.
    #[test]
    fn an_export_run_past_the_end_is_rejected() {
        let new = glyph(&["1"]);
        let mut enc = MqEncoder::new();
        let mut ints = IntCtxSet::new();
        let mut gb = vec![MqContext::default(); GB_CONTEXT_LEN];
        encode_int(&mut enc, &mut ints.iadh, Some(1));
        encode_int(&mut enc, &mut ints.iadw, Some(1));
        let ctx = usize::from(context_at(&new, 0, 0, &GenericParams::nominal(0)));
        enc.encode(&mut gb[ctx], 1);
        encode_int(&mut enc, &mut ints.iadw, None);
        encode_int(&mut enc, &mut ints.iaex, Some(0));
        encode_int(&mut enc, &mut ints.iaex, Some(9_000)); // one symbol exists

        let mut segment = 0u16.to_be_bytes().to_vec();
        segment.extend_from_slice(&nominal_at_bytes());
        segment.extend_from_slice(&1u32.to_be_bytes());
        segment.extend_from_slice(&1u32.to_be_bytes());
        segment.extend_from_slice(&enc.finish());
        assert_eq!(
            decode(&segment, &[]),
            Err(Jbig2Error::Malformed(
                "export run runs past the symbol list"
            )),
        );
    }

    /// A negative export run has no meaning and must not be cast into a large
    /// positive one.
    #[test]
    fn a_negative_export_run_is_rejected() {
        let new = glyph(&["1"]);
        let mut enc = MqEncoder::new();
        let mut ints = IntCtxSet::new();
        let mut gb = vec![MqContext::default(); GB_CONTEXT_LEN];
        encode_int(&mut enc, &mut ints.iadh, Some(1));
        encode_int(&mut enc, &mut ints.iadw, Some(1));
        let ctx = usize::from(context_at(&new, 0, 0, &GenericParams::nominal(0)));
        enc.encode(&mut gb[ctx], 1);
        encode_int(&mut enc, &mut ints.iadw, None);
        encode_int(&mut enc, &mut ints.iaex, Some(-1));

        let mut segment = 0u16.to_be_bytes().to_vec();
        segment.extend_from_slice(&nominal_at_bytes());
        segment.extend_from_slice(&1u32.to_be_bytes());
        segment.extend_from_slice(&1u32.to_be_bytes());
        segment.extend_from_slice(&enc.finish());
        assert_eq!(
            decode(&segment, &[]),
            Err(Jbig2Error::Malformed("negative export run")),
        );
    }

    /// A dictionary declaring a symbol far larger than the stream's remaining
    /// allowance is refused from the declared dimensions, before its pixel loop
    /// is entered.
    #[test]
    fn an_enormous_symbol_is_refused_by_the_budget() {
        let mut enc = MqEncoder::new();
        let mut ints = IntCtxSet::new();
        encode_int(&mut enc, &mut ints.iadh, Some(20_000));
        encode_int(&mut enc, &mut ints.iadw, Some(20_000));
        let mut segment = 0u16.to_be_bytes().to_vec();
        segment.extend_from_slice(&nominal_at_bytes());
        segment.extend_from_slice(&1u32.to_be_bytes());
        segment.extend_from_slice(&1u32.to_be_bytes());
        segment.extend_from_slice(&enc.finish());
        assert!(segment.len() < 64, "the demand is {} bytes", segment.len());
        assert_eq!(
            decode_symbol_dict(&segment, &[], &mut Budget::with_limit(1 << 20)),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// A symbol with no rows decodes no pixels, so a charge computed from its
    /// dimensions comes to nothing — and a dictionary can declare a great many
    /// of them in a few dozen bytes, each costing an arithmetic width decode
    /// and a bitmap that outlives the segment. The fixed per-symbol price is
    /// what stops that being free.
    #[test]
    fn a_symbol_with_no_rows_is_still_charged() {
        let segment = rowless_dictionary_segment(64);
        assert!(segment.len() < 96, "the demand is {} bytes", segment.len());

        let mut budget = Budget::with_limit(SYMBOL_COST * 64);
        assert_eq!(
            decode_symbol_dict(&segment, &[], &mut budget),
            Ok(Vec::new())
        );

        let mut budget = Budget::with_limit(SYMBOL_COST * 64 - 1);
        assert_eq!(
            decode_symbol_dict(&segment, &[], &mut budget),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// Re-exporting an input symbol copies it, and the copy costs what the
    /// original did.
    ///
    /// Nothing else bounds those copies. A dictionary codes no data at all to
    /// make one — the export runs are the whole segment — and the caller's
    /// referred-to list decides how many input symbols there are to copy, so an
    /// uncharged copy is a bitmap conjured out of a four-byte segment number.
    #[test]
    fn re_exporting_an_input_symbol_is_charged_for_the_copy() {
        let inputs = [glyph(&["1010", "0101", "1010", "0101"])];
        let refs: Vec<&Bitmap> = inputs.iter().collect();
        let segment = reexport_segment(1);

        let mut budget = Budget::with_limit(FOUR_BY_FOUR);
        let got = decode_symbol_dict(&segment, &refs, &mut budget).expect("dictionary");
        assert_same(&got[0], &inputs[0], 0);

        let mut budget = Budget::with_limit(FOUR_BY_FOUR - 1);
        assert_eq!(
            decode_symbol_dict(&segment, &refs, &mut budget),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// Every symbol draws on the one budget the stream was given, so a
    /// dictionary cannot buy unbounded decoding by splitting the demand across
    /// many small symbols.
    #[test]
    fn symbols_across_a_dictionary_draw_on_one_budget() {
        let symbols: Vec<Bitmap> = (0..8).map(|_| glyph(&["11", "11"])).collect();
        let segment = dictionary_segment(&symbols, 0);
        // Each 2 x 2 symbol costs the per-symbol price and (2 + ROW_COST) * 2.
        let each = SYMBOL_COST + (2 + ROW_COST) * 2;

        let mut budget = Budget::with_limit(each * 8);
        assert!(decode_symbol_dict(&segment, &[], &mut budget).is_ok());

        let mut budget = Budget::with_limit(each * 8 - 1);
        assert_eq!(
            decode_symbol_dict(&segment, &[], &mut budget),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// No byte string, however malformed, may panic, hang or read out of
    /// bounds. The budget is small so that a sweep of this size stays cheap:
    /// the dimensions of a symbol come from the coded data, so random bytes can
    /// and do ask for large ones, and paying for them is the behaviour under
    /// test rather than something to sit through.
    #[test]
    fn arbitrary_bytes_error_rather_than_panicking() {
        let mut state: u32 = 0x051D_2A17;
        for _ in 0..2_000 {
            let len = (state % 193) as usize;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            let _ = decode_symbol_dict(&data, &[], &mut Budget::with_limit(1 << 16));
        }
    }

    #[test]
    fn every_truncation_of_a_valid_segment_errors_cleanly() {
        let segment = dictionary_segment(&sample_symbols(), 0);
        for cut in 0..segment.len() {
            let _ = decode_symbol_dict(&segment[..cut], &[], &mut Budget::with_limit(1 << 16));
        }
    }
}
