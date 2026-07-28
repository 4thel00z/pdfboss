//! Text region segments (T.88 6.4, 7.4.3).
//!
//! A text region is the placement list a symbol dictionary exists to serve: a
//! sequence of (symbol, position) pairs, coded as small integers because every
//! position is expressed as a delta from the last one.
//!
//! The region is walked in horizontal **strips** of SBSTRIPS rows. `T` is the
//! coordinate across the strips and `S` the coordinate along them, and
//! TRANSPOSED swaps which axis each of the two indexes: with it clear `S` runs
//! across the page and `T` down it, with it set the other way about. Within a
//! strip the running coordinate CURS is advanced by the gap `IDS` plus
//! SBDSOFFSET — the offset applies to the gap between instances, never to the
//! coordinate itself — and an OOB from `IADS` closes the strip.
//!
//! The part of 6.4.5 worth stating plainly is the one that keeps those gaps
//! small. CURS is advanced past the symbol *before* the draw for the two
//! corners that name the far edge along the strip, and *after* it for the two
//! that name the near edge. The two conditions are exact complements, so the
//! invariant either way is that CURS finishes on the symbol's far edge and the
//! next `IDS` is the gap from there. Getting the split wrong does not fail: it
//! makes text drift by one pixel per symbol along every line.
//!
//! With SBHUFF set that walk does not change at all; only the six values it
//! reads come from somewhere else. The prefix codes of Annex B stand in for the
//! integer procedures of Annex A, the T coordinate within a strip becomes
//! `log2(SBSTRIPS)` bits read straight from the stream (6.4.9), and the symbol
//! ID becomes a prefix code from a table the segment carries in its own header
//! (7.4.3.1.7). That asymmetry is why [`Walk`] is generic over a [`Values`]
//! source rather than written once per coding: a placement walk that is wrong
//! by a pixel reads as a font problem rather than as a coding bug, and two
//! copies of it would each have to be got right.
//!
//! Instance refinement (REFINE) is refused by name rather than approximated.

use super::arith_int::{decode_iaid, decode_int, IaidCtx, IntCtxSet};
use super::bitmap::{Bitmap, CombOp};
use super::budget::Budget;
use super::huffman::{from_code_lengths, read_bits, standard, take_custom, Table};
use super::mq::MqDecoder;
use super::reader::Reader;
use super::segment::{parse_region_info, RegionInfo};
use super::Jbig2Error;
use crate::filters::ccitt::bits::BitReader;

/// The most symbol instances one text region may place.
///
/// T.88 gives SBNUMINSTANCES a 32-bit field and no ceiling. Four million
/// placements is far past any page a scanner produces — a dense A4 page of
/// small type holds a few thousand — and the cap is checked before the count
/// drives a loop. The work each placement then costs is charged separately,
/// against the stream's budget, so this is a sanity bound rather than the thing
/// that makes the region affordable.
pub(crate) const MAX_INSTANCES: u32 = 1 << 22;

/// What placing one symbol instance costs beyond the pixels it composites,
/// in the units [`Budget`] counts.
///
/// A placement is four or five arithmetic integer decodes and a symbol ID
/// whatever the symbol's size, so it is never free — and a symbol with no rows
/// composites nothing at all, which would otherwise let SBNUMINSTANCES buy
/// arithmetic decoding the budget never sees. The figure is not an exact
/// accounting of those decisions; it is a fixed price that ties the number of
/// placements a stream may make to the one allowance the stream has.
pub(crate) const INSTANCE_COST: u64 = 64;

/// What a text region is told when its Huffman table selectors and its
/// referred-to table segments do not account for one another
/// (T.88 7.4.3.1.6).
///
/// One message for both directions, because both are the same mistake seen from
/// opposite ends: a selector reading "user-supplied" with no table segment left
/// to bind, and a table segment nothing selected.
const TABLE_COUNT_DISAGREES: &str = "Huffman table count disagrees with the text region flags";

/// How many run codes the symbol ID table is coded with (T.88 7.4.3.1.7 step 1,
/// Table 29).
///
/// RUNCODE0 to RUNCODE31 name a symbol ID code length outright; the last three
/// repeat a length or a run of zeros.
const RUN_CODES: usize = 35;

/// Which corner of a symbol its coded coordinate names (T.88 7.4.3.1.1,
/// REFCORNER).
///
/// The discriminants are the field's own encoding, which is why the ordering
/// looks arbitrary: the bit pattern counts up through BOTTOMLEFT, TOPLEFT,
/// BOTTOMRIGHT, TOPRIGHT.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum RefCorner {
    /// The symbol's bottom-left pixel lies at the coded coordinate.
    BottomLeft,
    /// The symbol's top-left pixel lies at the coded coordinate.
    TopLeft,
    /// The symbol's bottom-right pixel lies at the coded coordinate.
    BottomRight,
    /// The symbol's top-right pixel lies at the coded coordinate.
    TopRight,
}

impl RefCorner {
    /// Decodes the two-bit REFCORNER field of T.88 7.4.3.1.1.
    ///
    /// All four values are defined, so there is nothing to reject; bits above
    /// the low two belong to other fields and are masked away.
    pub(crate) fn from_bits(bits: u8) -> RefCorner {
        match bits & 0x3 {
            0 => RefCorner::BottomLeft,
            1 => RefCorner::TopLeft,
            2 => RefCorner::BottomRight,
            _ => RefCorner::TopRight,
        }
    }
}

/// SBSYMCODELEN: the number of bits an arithmetic text region spends on a
/// symbol ID (T.88 7.4.3.2, Table 31).
///
/// It is the width of the largest id, `ceil(log2(SBNUMSYMS))`, which is 0 when
/// there is exactly one symbol — a region with a single symbol codes no id bits
/// at all. Computed from `leading_zeros` rather than a logarithm so that no
/// rounding decision is delegated to floating point.
pub(crate) fn sym_code_len(num_syms: u32) -> u32 {
    if num_syms > 1 {
        u32::BITS - (num_syms - 1).leading_zeros()
    } else {
        0
    }
}

/// Decodes a text region segment's data (T.88 7.4.3), returning where the
/// region goes and the pixels that go there.
///
/// `symbols` are the symbols exported by the referred-to dictionary segments,
/// concatenated in the order the referred-to list gives them (SBSYMS). Their
/// count is what sizes the symbol ID code, so an empty list is refused: it
/// would leave every id in the stream unanswerable.
///
/// `tables` are the Huffman tables the referred-to code table segments carry,
/// in the order the referred-to list names them, which is the order the
/// selectors of 7.4.3.1.6 bind them in. An arithmetic region selects none of
/// them, so for one of those the list must be empty.
///
/// `budget` is the embedded stream's remaining allowance of decoding work, the
/// same one the page's other regions draw on. The region is charged from the
/// dimensions its header declares before its bitmap is allocated, and each
/// placement is charged before it composites, so neither the declared size nor
/// the declared instance count can buy work the allowance never sees.
pub(crate) fn decode_text_region(
    data: &[u8],
    symbols: &[&Bitmap],
    tables: &[&Table],
    budget: &mut Budget,
) -> Result<(RegionInfo, Bitmap), Jbig2Error> {
    let mut r = Reader::new(data);
    let info = parse_region_info(&mut r)?;
    let params = parse_params(&mut r, tables)?;
    if symbols.is_empty() {
        return Err(Jbig2Error::Malformed("text region with no symbols"));
    }
    let num_syms = u32::try_from(symbols.len())
        .map_err(|_| Jbig2Error::Malformed("symbol count exceeds the limit"))?;

    budget.charge_region(info.width, info.height)?;
    let mut region = Bitmap::filled(info.width, info.height, params.def_pixel)?;

    match &params.coding {
        Coding::Arithmetic => Walk {
            values: Arithmetic {
                dec: MqDecoder::new(r.rest()),
                ints: IntCtxSet::new(),
                iaid: IaidCtx::new(sym_code_len(num_syms)),
            },
            region: &mut region,
            symbols,
            params: &params,
            budget,
        }
        .run()?,
        Coding::Huffman(tables) => {
            // 7.4.3.1.5: the last field of the segment's data header, and the
            // only one whose size depends on something outside the segment —
            // SBNUMSYMS is however many symbols the referred-to dictionaries
            // exported. It shares the cursor with the coded data that follows
            // it, which is what step 6's byte alignment exists to settle.
            let mut bits = BitReader::new(r.rest());
            let codes = decode_symbol_id_codes(&mut bits, num_syms)?;
            Walk {
                values: Huffman {
                    bits,
                    tables,
                    codes,
                    log_strips: params.log_strips,
                },
                region: &mut region,
                symbols,
                params: &params,
                budget,
            }
            .run()?;
        }
    }
    Ok((info, region))
}

/// The fields of a text region segment that precede its coded data
/// (T.88 7.4.3.1).
struct TextParams {
    /// SBHUFF, and with it whatever the chosen coding needs in order to read
    /// the values that follow.
    coding: Coding,
    /// LOGSBSTRIPS. Kept alongside [`TextParams::strips`], which is `1` shifted
    /// by it, because a Huffman region reads the T coordinate within a strip as
    /// exactly this many raw bits (6.4.9).
    log_strips: u8,
    /// SBSTRIPS, the number of rows one strip spans: `1 << LOGSBSTRIPS`, so 1,
    /// 2, 4 or 8.
    strips: i32,
    /// REFCORNER, which corner of a symbol its coordinate names.
    corner: RefCorner,
    /// TRANSPOSED, which swaps the axes S and T index.
    transposed: bool,
    /// SBCOMBOP, how a symbol's pixels combine with what is already there.
    comb_op: CombOp,
    /// SBDEFPIXEL, the value the region is filled with before any placement.
    def_pixel: u8,
    /// SBDSOFFSET, added to every gap after the first instance of a strip.
    ds_offset: i32,
    /// SBNUMINSTANCES, the number of placements the region carries.
    instances: u32,
}

/// How a text region's values are coded (T.88 7.4.3.1.1, bit 0).
///
/// The flag decides more than which decoder reads the integers: it decides
/// whether the header carries a Huffman flags word and a symbol ID table at
/// all. Holding the tables inside it is what keeps a region from being read
/// with half of each.
enum Coding {
    /// SBHUFF = 0. The integer procedures of Annex A, whose contexts the walk
    /// creates for itself.
    Arithmetic,
    /// SBHUFF = 1, with the tables 7.4.3.1.6 bound to the selectors. Boxed
    /// because three tables are a kilobyte and a half of lines and length
    /// slots, which every arithmetic region would otherwise carry around as the
    /// size of this enum.
    Huffman(Box<TextTables>),
}

/// The Huffman tables a text region decodes its coordinates with
/// (T.88 7.4.3.1.6).
///
/// The five refinement selectors are absent because nothing can select them:
/// 7.4.3.1.2 requires each of their fields to be 0 while SBREFINE is 0, and
/// SBREFINE = 1 is refused before the tables are bound at all. The symbol ID
/// table is not here either — it is not selected but carried, in the field
/// 7.4.3.1.5 puts after SBNUMINSTANCES.
struct TextTables {
    /// SBHUFFFS, the S coordinate of a strip's first instance (6.4.7).
    fs: Table,
    /// SBHUFFDS, the gap to a later instance, whose OOB closes the strip
    /// (6.4.8).
    ds: Table,
    /// SBHUFFDT, the strip offset, in strips rather than rows (6.4.6).
    dt: Table,
}

/// Parses the text region segment's data header down to the instance count
/// (T.88 7.4.3.1.1 to 7.4.3.1.4).
///
/// The field order is the whole reason this reads as it does. 7.4.3.1 puts the
/// Huffman flags between the ordinary flags and SBNUMINSTANCES, and makes them
/// present only when SBHUFF is 1, so a parser that reaches for the instance
/// count straight after the flags word reads two bytes of table selectors as
/// the top half of it.
///
/// The one coding mode this build does not implement is refused before any
/// further byte is read, for the same reason: a refining region carries the
/// SBRAT pixels here (7.4.3.1.3), so reading past them would leave the cursor
/// in the wrong field and turn an unsupported stream into a plausible wrong
/// answer.
///
/// Bit 15, SBRTEMPLATE, selects the template refinement uses; with REFINE
/// refused above it selects nothing, so it is not examined.
fn parse_params(r: &mut Reader<'_>, tables: &[&Table]) -> Result<TextParams, Jbig2Error> {
    let flags = r.u16()?;
    if flags & 0x0002 != 0 {
        return Err(Jbig2Error::Unimplemented("text region symbol refinement"));
    }
    let coding = if flags & 0x0001 == 0 {
        // 7.4.3.1.6: the number of selectors reading "user-supplied table" must
        // equal the number of table segments referred to, and an arithmetic
        // region has no selectors at all. A referred-to table is therefore
        // bound to nothing, which is a header describing a region other than
        // the one it carries.
        if !tables.is_empty() {
            return Err(Jbig2Error::Malformed(TABLE_COUNT_DISAGREES));
        }
        Coding::Arithmetic
    } else {
        Coding::Huffman(Box::new(bind_tables(r.u16()?, tables)?))
    };
    let log_strips = ((flags >> 2) & 0x3) as u8;
    let strips = 1i32 << log_strips;
    let corner = RefCorner::from_bits(((flags >> 4) & 0x3) as u8);
    let transposed = flags & 0x0040 != 0;
    // Two bits here rather than the three a region information field carries,
    // so REPLACE is unreachable — 7.4.3.1.1 does not offer it.
    let comb_op = CombOp::from_bits(((flags >> 7) & 0x3) as u8)?;
    let def_pixel = u8::from(flags & 0x0200 != 0);
    // SBDSOFFSET is five bits of two's complement, so it sign-extends from bit
    // 14 of the flags word — bit 4 of the extracted field — and not from bit
    // 15, which belongs to SBRTEMPLATE.
    let raw = i32::from((flags >> 10) & 0x1F);
    let ds_offset = if raw > 15 { raw - 32 } else { raw };

    let instances = r.u32()?;
    if instances > MAX_INSTANCES {
        return Err(Jbig2Error::Malformed("instance count exceeds the limit"));
    }
    Ok(TextParams {
        coding,
        log_strips,
        strips,
        corner,
        transposed,
        comb_op,
        def_pixel,
        ds_offset,
        instances,
    })
}

/// Resolves the Huffman table selectors of T.88 7.4.3.1.2 against the standard
/// tables and the referred-to code table segments (7.4.3.1.6).
///
/// The selectors are not uniform, and the one place a reader is likely to
/// assume they are is exactly where the specification says otherwise: SBHUFFDS
/// and SBHUFFDT each admit the value 2, naming Tables B.10 and B.13, where
/// SBHUFFFS and the four refinement selectors call 2 "not permitted".
///
/// Everything from bit 6 up must read 0 here. 7.4.3.1.2 requires each of the
/// five refinement selectors to be 0 while SBREFINE is 0, and SBREFINE = 1 is
/// refused, so a stream that sets one has named a table segment nothing in this
/// region would ever read. Saying so beats binding a table to a slot no value
/// comes out of, because the binding order is positional: a table consumed by a
/// dead selector is a table the live ones no longer receive.
///
/// The OOB requirement of 7.4.3.1.6 is checked for every table rather than only
/// for the custom ones, which costs nothing because the standard tables satisfy
/// it by construction. It is what catches two custom tables bound the wrong way
/// round: SBHUFFDS's OOB is the only thing that closes a strip, so a table
/// without one would run a strip until the segment ran out.
fn bind_tables(flags: u16, tables: &[&Table]) -> Result<TextTables, Jbig2Error> {
    // Bit 15.
    if flags & 0x8000 != 0 {
        return Err(Jbig2Error::Malformed(
            "reserved bit set in the text region Huffman flags",
        ));
    }
    // Bits 6 to 14: SBHUFFRDW, RDH, RDX, RDY and RSIZE.
    if flags & 0x7FC0 != 0 {
        return Err(Jbig2Error::Malformed(
            "refinement Huffman table selected without refinement",
        ));
    }

    let mut used = 0usize;
    // Bits 0 and 1: SBHUFFFS.
    let fs = match flags & 0x3 {
        0 => standard(6)?,
        1 => standard(7)?,
        3 => take_custom(tables, &mut used, TABLE_COUNT_DISAGREES)?,
        _ => return Err(Jbig2Error::Malformed("reserved SBHUFFFS selection")),
    };
    // Bits 2 and 3: SBHUFFDS.
    let ds = match (flags >> 2) & 0x3 {
        0 => standard(8)?,
        1 => standard(9)?,
        2 => standard(10)?,
        _ => take_custom(tables, &mut used, TABLE_COUNT_DISAGREES)?,
    };
    // Bits 4 and 5: SBHUFFDT.
    let dt = match (flags >> 4) & 0x3 {
        0 => standard(11)?,
        1 => standard(12)?,
        2 => standard(13)?,
        _ => take_custom(tables, &mut used, TABLE_COUNT_DISAGREES)?,
    };
    if used != tables.len() {
        return Err(Jbig2Error::Malformed(TABLE_COUNT_DISAGREES));
    }
    if !ds.has_oob() {
        return Err(Jbig2Error::Malformed("SBHUFFDS cannot code OOB"));
    }
    if fs.has_oob() || dt.has_oob() {
        return Err(Jbig2Error::Malformed("SBHUFFFS or SBHUFFDT codes OOB"));
    }
    Ok(TextTables { fs, ds, dt })
}

/// Decodes the symbol ID Huffman decoding table of T.88 7.4.3.1.5, whose
/// coding is 7.4.3.1.7.
///
/// The field is a Huffman table describing a Huffman table. Thirty-five
/// four-bit lengths give the run codes their own prefix codes (steps 1 and 2),
/// those run codes then spell out one code length per symbol (steps 3 to 5),
/// and B.3 over *those* is SBSYMCODES (step 7). Between the two lies step 6's
/// byte alignment, which is what makes the region's coded data begin on a byte
/// boundary however many bits the run codes happened to take.
///
/// Nothing here is charged against the work budget. Both loops are bounded by
/// SBNUMSYMS, which is the length of the symbol list the referred-to
/// dictionaries produced, and every one of those symbols was charged for as it
/// was decoded or copied — so the size of this field is already paid for.
fn decode_symbol_id_codes(bits: &mut BitReader, num_syms: u32) -> Result<Table, Jbig2Error> {
    let runs = read_run_code_table(bits)?;
    let lengths = read_symbol_code_lengths(bits, &runs, num_syms)?;
    // Step 6.
    bits.align_to_byte();
    // Step 7.
    from_code_lengths(&lengths)
}

/// Reads the thirty-five run code lengths (T.88 7.4.3.1.7 step 1).
///
/// Four bits each, so a run code's own prefix is at most 15 bits and the
/// assignment of step 2 cannot be asked for a code wider than a word.
fn read_run_code_lengths(bits: &mut BitReader) -> Result<[u8; RUN_CODES], Jbig2Error> {
    let mut lengths = [0u8; RUN_CODES];
    for slot in &mut lengths {
        *slot = read_bits(bits, 4)? as u8;
    }
    Ok(lengths)
}

/// Reads the thirty-five run code lengths and assigns their codes
/// (T.88 7.4.3.1.7 steps 1 and 2).
fn read_run_code_table(bits: &mut BitReader) -> Result<Table, Jbig2Error> {
    from_code_lengths(&read_run_code_lengths(bits)?)
}

/// Run-length decodes the SBNUMSYMS symbol ID code lengths
/// (T.88 7.4.3.1.7 steps 3 to 5, Table 29).
///
/// Every run writes at least one length, so the loop advances on every pass and
/// SBNUMSYMS bounds it. A run that would write past the last symbol is refused
/// rather than truncated: the encoder and the header disagree about how many
/// symbols there are, and the codes assigned to the ones already read would be
/// wrong whichever way that disagreement were resolved.
fn read_symbol_code_lengths(
    bits: &mut BitReader,
    runs: &Table,
    num_syms: u32,
) -> Result<Vec<u8>, Jbig2Error> {
    let wanted = num_syms as usize;
    let mut lengths: Vec<u8> = Vec::new();
    while lengths.len() < wanted {
        let code = runs.decode(bits)?.ok_or(Jbig2Error::Malformed(
            "unexpected OOB decoding a symbol ID run code",
        ))?;
        // Table 29. The extra bits are read only after the run code that calls
        // for them, so a stream that never uses RUNCODE32 to RUNCODE34 spends
        // none.
        let (length, count) = match code {
            0..=31 => (code as u8, 1u32),
            32 => {
                let previous = *lengths.last().ok_or(Jbig2Error::Malformed(
                    "RUNCODE32 with no previous symbol ID code length",
                ))?;
                (previous, read_bits(bits, 2)? + 3)
            }
            33 => (0, read_bits(bits, 3)? + 3),
            34 => (0, read_bits(bits, 7)? + 11),
            // `runs` has one line per run code, so a decoded value outside the
            // table's own index range cannot arise; refusing rather than
            // indexing keeps that an assumption about this file.
            _ => return Err(Jbig2Error::Malformed("no such symbol ID run code")),
        };
        if count as usize > wanted - lengths.len() {
            return Err(Jbig2Error::Malformed("symbol ID run past the last symbol"));
        }
        lengths.resize(lengths.len() + count as usize, length);
    }
    Ok(lengths)
}

/// Where a text region's coded values come from (T.88 6.4.6 to 6.4.10).
///
/// Five of the six differ only in which decoder reads them. The sixth does not:
/// with SBHUFF set the T coordinate within a strip is not a coded value at all
/// but `log2(SBSTRIPS)` raw bits (6.4.9), which is why the source is a trait
/// over the reads rather than a pair of decoders the walk chooses between.
///
/// `Ok(None)` is OOB. Only 6.4.8's is meaningful — it is what closes a strip —
/// but every read is given the same shape so that the walk, rather than each
/// implementation, decides what an unexpected one means.
trait Values {
    /// 6.4.6, before the multiplication by SBSTRIPS the caller applies. Serves
    /// both the initial STRIPT of 6.4.5 step 2 and each strip's delta.
    fn delta_t(&mut self) -> Result<Option<i32>, Jbig2Error>;
    /// 6.4.7: the S coordinate of a strip's first instance, as a delta on
    /// FIRSTS.
    fn first_s(&mut self) -> Result<Option<i32>, Jbig2Error>;
    /// 6.4.8: the gap to a later instance of the strip. OOB closes the strip.
    fn delta_s(&mut self) -> Result<Option<i32>, Jbig2Error>;
    /// 6.4.9: an instance's T coordinate within its strip.
    fn curt(&mut self) -> Result<Option<i32>, Jbig2Error>;
    /// 6.4.10: an instance's symbol ID.
    fn symbol_id(&mut self) -> Result<u32, Jbig2Error>;
}

/// The arithmetic value source: the integer procedures of Annex A, all drawing
/// on one decoder and each adapting its own contexts across the whole region.
struct Arithmetic<'d> {
    /// The one arithmetic decoder every coded value of the region comes from.
    dec: MqDecoder<'d>,
    /// The integer procedures of Annex A, adapting across the whole region.
    ints: IntCtxSet,
    /// The symbol ID procedure of A.3, sized by SBSYMCODELEN.
    iaid: IaidCtx,
}

impl Values for Arithmetic<'_> {
    fn delta_t(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(&mut self.dec, &mut self.ints.iadt))
    }

    fn first_s(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(&mut self.dec, &mut self.ints.iafs))
    }

    fn delta_s(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(&mut self.dec, &mut self.ints.iads))
    }

    fn curt(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(&mut self.dec, &mut self.ints.iait))
    }

    fn symbol_id(&mut self) -> Result<u32, Jbig2Error> {
        Ok(decode_iaid(&mut self.dec, &mut self.iaid))
    }
}

/// The Huffman value source: three selected tables, the symbol ID table the
/// segment carried, and one bit cursor they all share.
///
/// Running out of bits is [`Jbig2Error::Truncated`] here, where the arithmetic
/// source above synthesises bits forever and settles into returning OOB (T.88
/// E.3.4). The walk is written for the latter — its strip loop treats an OOB as
/// the end of a strip — so the difference matters: a truncated Huffman region
/// fails instead of decoding to a plausible short one.
struct Huffman<'a, 'd> {
    /// The cursor over the region's coded data, positioned by 7.4.3.1.7 step 6
    /// at the byte boundary the walk begins on.
    bits: BitReader<'d>,
    /// SBHUFFFS, SBHUFFDS and SBHUFFDT, as 7.4.3.1.6 bound them.
    tables: &'a TextTables,
    /// SBSYMCODES, the symbol ID table of 7.4.3.1.7, whose lines decode to the
    /// index of the symbol they name.
    codes: Table,
    /// LOGSBSTRIPS: how many bits an instance's T coordinate occupies (6.4.9).
    log_strips: u8,
}

impl Values for Huffman<'_, '_> {
    fn delta_t(&mut self) -> Result<Option<i32>, Jbig2Error> {
        self.tables.dt.decode(&mut self.bits)
    }

    fn first_s(&mut self) -> Result<Option<i32>, Jbig2Error> {
        self.tables.fs.decode(&mut self.bits)
    }

    fn delta_s(&mut self) -> Result<Option<i32>, Jbig2Error> {
        self.tables.ds.decode(&mut self.bits)
    }

    fn curt(&mut self) -> Result<Option<i32>, Jbig2Error> {
        // 6.4.9: read directly from the bitstream, through no table at all.
        // LOGSBSTRIPS is at most 3, so the value is at most 7.
        let value = read_bits(&mut self.bits, self.log_strips)?;
        Ok(Some(value as i32))
    }

    fn symbol_id(&mut self) -> Result<u32, Jbig2Error> {
        // 6.4.10: bits are read until they spell one of the entries of
        // SBSYMCODES, and the value is that entry's index. `codes` carries the
        // index as the line's value, so the matcher of B.4 answers directly.
        let id = self
            .codes
            .decode(&mut self.bits)?
            .ok_or(Jbig2Error::Malformed("unexpected OOB decoding a symbol id"))?;
        u32::try_from(id).map_err(|_| Jbig2Error::Malformed("symbol id out of range"))
    }
}

/// The state a text region's strip walk carries (T.88 6.4.5).
///
/// The value source is owned rather than borrowed because it is the walk's
/// alone: whichever coding is in force, everything the region reads is read
/// through it, in order, and nothing else in the segment shares the cursor it
/// sits on.
struct Walk<'a, V> {
    /// Where the coded values come from, which is the only thing SBHUFF
    /// changes about this procedure.
    values: V,
    /// SBREGBITMAP, the region being painted.
    region: &'a mut Bitmap,
    /// SBSYMS, the symbols the coded ids index.
    symbols: &'a [&'a Bitmap],
    /// The parameters the segment header fixed.
    params: &'a TextParams,
    /// The embedded stream's remaining allowance of decoding work.
    budget: &'a mut Budget,
}

impl<V: Values> Walk<'_, V> {
    /// Walks the strips, compositing every symbol instance the region declares
    /// (T.88 6.4.5).
    ///
    /// Both loops end on something the coded data cannot extend. Every pass of
    /// the inner loop either places an instance or reads the OOB that closes
    /// the strip, and an exhausted arithmetic decoder reads as OOB (T.88
    /// E.3.4) where an exhausted Huffman one errors; every pass of the outer
    /// loop enters the inner one with at least one placement still owed, and
    /// the inner loop's first pass always takes it. So the total number of
    /// passes is bounded by SBNUMINSTANCES, which the segment header fixed
    /// before any of this was read.
    fn run(&mut self) -> Result<(), Jbig2Error> {
        let strips = i64::from(self.params.strips);
        // 6.4.5 step 2: the leading strip offset is negated, so a region whose
        // first strip starts above its own top edge says so with a positive
        // value here.
        let initial = self.values.delta_t()?.ok_or(Jbig2Error::Malformed(
            "unexpected OOB decoding the leading strip offset",
        ))?;
        let mut strip_t = i64::from(initial).saturating_mul(strips).saturating_neg();
        // FIRSTS runs across the whole region rather than resetting per strip:
        // each strip's first instance is a delta on the previous strip's.
        let mut first_s: i64 = 0;
        let mut placed: u32 = 0;

        while placed < self.params.instances {
            let delta = self.values.delta_t()?.ok_or(Jbig2Error::Malformed(
                "unexpected OOB decoding a strip offset",
            ))?;
            // The delta counts strips, not rows (6.4.5 step 3(b)).
            strip_t = strip_t.saturating_add(i64::from(delta).saturating_mul(strips));

            // 6.4.5 step 3(c)(i): a strip's first instance gives its S
            // coordinate as a delta on FIRSTS.
            let dfs = self.values.first_s()?.ok_or(Jbig2Error::Malformed(
                "unexpected OOB decoding a first S coordinate",
            ))?;
            first_s = first_s.saturating_add(i64::from(dfs));
            let mut curs = first_s;

            loop {
                if placed >= self.params.instances {
                    break;
                }
                curs = self.place_one(curs, strip_t)?;
                placed += 1;

                // Every later instance of the strip gives the gap from the far
                // edge of the one just placed, offset by SBDSOFFSET; an OOB
                // closes the strip.
                let Some(ids) = self.values.delta_s()? else {
                    break;
                };
                curs = curs
                    .saturating_add(i64::from(ids))
                    .saturating_add(i64::from(self.params.ds_offset));
            }
        }
        Ok(())
    }

    /// Decodes and composites one symbol instance, returning the value CURS
    /// takes after it (T.88 6.4.5 step 3(c)).
    fn place_one(&mut self, curs: i64, strip_t: i64) -> Result<i64, Jbig2Error> {
        // 6.4.5 step 3(c)(iii): a one-row strip has nowhere to offset within,
        // and 6.4.9 codes nothing at all for it — no IAIT value in the
        // arithmetic variant and no bits in the Huffman one.
        let curt = if self.params.strips == 1 {
            0
        } else {
            self.values.curt()?.ok_or(Jbig2Error::Malformed(
                "unexpected OOB decoding a T coordinate",
            ))?
        };
        let ti = strip_t.saturating_add(i64::from(curt));

        let id = self.values.symbol_id()?;
        // The code length is the bit width of the largest id, so a symbol count
        // that is not a power of two leaves ids the code can express and the
        // list cannot answer. Refusing those keeps the lookup in bounds.
        let symbol = *self
            .symbols
            .get(id as usize)
            .ok_or(Jbig2Error::Malformed("symbol id out of range"))?;

        self.budget.charge(INSTANCE_COST)?;
        self.budget.charge_region(symbol.width(), symbol.height())?;

        // 6.4.5 steps 3(c)(vi) and (x): CURS always finishes on the symbol's
        // far edge along the strip. Which end of the symbol that is depends on
        // the corner, so the advance happens either before the draw or after it
        // — never both, never neither. The two conditions are complements,
        // which is why one boolean drives them.
        let w = i64::from(symbol.width());
        let h = i64::from(symbol.height());
        let extent = if self.params.transposed { h } else { w } - 1;
        let advance_first = if self.params.transposed {
            matches!(
                self.params.corner,
                RefCorner::BottomLeft | RefCorner::BottomRight
            )
        } else {
            matches!(
                self.params.corner,
                RefCorner::TopRight | RefCorner::BottomRight
            )
        };

        let si = if advance_first {
            curs.saturating_add(extent)
        } else {
            curs
        };
        let (x, y) = top_left(si, ti, w, h, self.params.transposed, self.params.corner);
        self.region.combine(
            symbol,
            clamp_offset(x),
            clamp_offset(y),
            self.params.comb_op,
        );
        Ok(if advance_first {
            si
        } else {
            si.saturating_add(extent)
        })
    }
}

/// Where a symbol's top-left pixel lands, given that its REFCORNER lies at
/// `(s, t)` (T.88 6.4.5 step 3(c)(viii)).
///
/// TRANSPOSED does not rotate the symbol; it swaps which axis each of the two
/// coordinates indexes. So the untransposed cases read `(s, t)` as
/// `(column, row)` and the transposed ones read `(t, s)`, while the corner
/// adjustments stay attached to the symbol's own width and height.
fn top_left(s: i64, t: i64, w: i64, h: i64, transposed: bool, corner: RefCorner) -> (i64, i64) {
    let from_right = |v: i64| v.saturating_sub(w).saturating_add(1);
    let from_bottom = |v: i64| v.saturating_sub(h).saturating_add(1);
    if transposed {
        match corner {
            RefCorner::TopLeft => (t, s),
            RefCorner::TopRight => (from_right(t), s),
            RefCorner::BottomLeft => (t, from_bottom(s)),
            RefCorner::BottomRight => (from_right(t), from_bottom(s)),
        }
    } else {
        match corner {
            RefCorner::TopLeft => (s, t),
            RefCorner::TopRight => (from_right(s), t),
            RefCorner::BottomLeft => (s, from_bottom(t)),
            RefCorner::BottomRight => (from_right(s), from_bottom(t)),
        }
    }
}

/// Narrows a placement coordinate to the offset [`Bitmap::combine`] takes.
///
/// The coordinates are accumulated in `i64` because the deltas that build them
/// are signed 32-bit values a stream may repeat, so they can leave the range an
/// offset can express. Saturating rather than wrapping is what makes that
/// harmless: a coordinate that far outside the region clips entirely away,
/// which is the right outcome, whereas a wrap would paint it over the opposite
/// corner.
fn clamp_offset(v: i64) -> i32 {
    i32::try_from(v).unwrap_or(if v < 0 { i32::MIN } else { i32::MAX })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::jbig2::bitmap::Bitmap;
    use crate::filters::jbig2::budget::{Budget, ROW_COST};
    use crate::filters::jbig2::huffman::encoder::BitWriter;
    use crate::filters::jbig2::huffman::parse_table_segment;
    use crate::filters::jbig2::testing::{
        code_table_segment, expect_at, glyph, huffman_text_segment, text_segment,
        text_segment_with_curt, two_symbols, Op, Placement, Shape,
    };
    use crate::filters::jbig2::Jbig2Error;

    /// Decodes with the allowance a real embedded stream gets, and no
    /// referred-to code table segments.
    fn decode(data: &[u8], symbols: &[&Bitmap]) -> Result<(RegionInfo, Bitmap), Jbig2Error> {
        decode_with(data, symbols, &[])
    }

    /// [`decode`] for a region whose referred-to list carries code table
    /// segments.
    fn decode_with(
        data: &[u8],
        symbols: &[&Bitmap],
        tables: &[&Table],
    ) -> Result<(RegionInfo, Bitmap), Jbig2Error> {
        decode_text_region(data, symbols, tables, &mut Budget::new())
    }

    /// Three instances across two strips, with the coordinates walked by hand
    /// from 6.4.5.
    ///
    /// STRIPT starts at `-0`, the first strip's `IADT` of 2 puts it at row 2,
    /// and FIRSTS of 1 puts the first symbol's left edge at column 1. CURS then
    /// ends on that symbol's far edge, column 3, so the gap of 2 lands the
    /// second symbol at column 5 rather than at column 3. The second strip's
    /// `IADT` of 5 puts it at row 7, and its `IAFS` of -1 carries FIRSTS back
    /// from 1 to 0 — FIRSTS runs across strips, unlike CURS.
    #[test]
    fn places_instances_across_strips() {
        let syms = two_symbols();
        let refs: Vec<&Bitmap> = syms.iter().collect();
        let data = text_segment(
            (20, 16),
            Shape::default(),
            3,
            2,
            0,
            &[
                Op::Strip(2),
                Op::First(1, 0),
                Op::Next(2, 1),
                Op::EndStrip,
                Op::Strip(5),
                Op::First(-1, 0),
                Op::EndStrip,
            ],
        );
        let (info, region) = decode(&data, &refs).expect("text region");
        assert_eq!((info.width, info.height), (20, 16));
        expect_at(&region, &syms[0], 1, 2);
        expect_at(&region, &syms[1], 5, 2);
        expect_at(&region, &syms[0], 0, 7);
        // Nothing was painted outside those three placements.
        assert_eq!(region.get(19, 15), 0);
    }

    /// All eight TRANSPOSED by REFCORNER combinations, against coordinates
    /// derived by hand from 6.4.5 steps 3(c)(vi), (viii) and (x).
    ///
    /// One instance of a 3 by 2 symbol, FIRSTS = 4 and STRIPT = 5. Where two
    /// rows agree it is not a coincidence: the advance of step (vi) and the
    /// corner offset of step (viii) are designed to cancel, so a symbol
    /// occupies the same cells whichever end of itself it is placed by.
    #[test]
    fn honours_every_refcorner_and_transposition() {
        let symbol = glyph(&["101", "010"]);
        let syms = [&symbol];
        // (transposed, corner, expected x, expected y)
        let cases: [(bool, u8, i64, i64); 8] = [
            (false, 1, 4, 5), // TOPLEFT
            (false, 3, 4, 5), // TOPRIGHT
            (false, 0, 4, 4), // BOTTOMLEFT
            (false, 2, 4, 4), // BOTTOMRIGHT
            (true, 1, 5, 4),  // TOPLEFT
            (true, 3, 3, 4),  // TOPRIGHT
            (true, 0, 5, 4),  // BOTTOMLEFT
            (true, 2, 3, 4),  // BOTTOMRIGHT
        ];
        for (transposed, corner, x, y) in cases {
            let shape = Shape {
                corner,
                transposed,
                ..Shape::default()
            };
            let data = text_segment(
                (16, 16),
                shape,
                1,
                1,
                0,
                &[Op::Strip(5), Op::First(4, 0), Op::EndStrip],
            );
            let (_, region) = decode(&data, &syms).expect("text region");
            expect_at(&region, &symbol, x, y);
        }
    }

    /// The invariant behind the pre/post split of steps 3(c)(vi) and (ix):
    /// after any placement CURS sits on the symbol's far edge along the strip,
    /// in all eight combinations.
    ///
    /// A single placement cannot see this, because the corner offset of step
    /// (viii) hides where CURS ended. A second instance can: `IDS` is measured
    /// from the first symbol's far edge, so the distance between the two
    /// placements is `extent + IDS` exactly when the invariant holds. Get it
    /// wrong in one direction and every line drifts wider as it runs; wrong in
    /// the other and the symbols pile up. Neither fails outright, which is why
    /// the gap is asserted rather than the first placement alone.
    #[test]
    fn curs_ends_on_the_symbol_far_edge_for_every_corner() {
        let symbol = glyph(&["101", "010"]);
        let syms = [&symbol];
        // transposed, corner, then the two placements as x, y and x, y.
        let cases: [(bool, u8, i64, i64, i64, i64); 8] = [
            (false, 1, 4, 3, 8, 3), // TOPLEFT
            (false, 3, 4, 3, 8, 3), // TOPRIGHT
            (false, 0, 4, 2, 8, 2), // BOTTOMLEFT
            (false, 2, 4, 2, 8, 2), // BOTTOMRIGHT
            (true, 1, 3, 4, 3, 7),  // TOPLEFT
            (true, 3, 1, 4, 1, 7),  // TOPRIGHT
            (true, 0, 3, 4, 3, 7),  // BOTTOMLEFT
            (true, 2, 1, 4, 1, 7),  // BOTTOMRIGHT
        ];
        for (transposed, corner, first_x, first_y, second_x, second_y) in cases {
            let shape = Shape {
                corner,
                transposed,
                ..Shape::default()
            };
            let data = text_segment(
                (24, 24),
                shape,
                2,
                1,
                0,
                &[Op::Strip(3), Op::First(4, 0), Op::Next(2, 0), Op::EndStrip],
            );
            let (_, region) = decode(&data, &syms).expect("text region");
            expect_at(&region, &symbol, first_x, first_y);
            expect_at(&region, &symbol, second_x, second_y);
        }
    }

    /// SBDSOFFSET is a five-bit signed field applied to every gap after the
    /// first instance of a strip, not to the coordinate itself.
    #[test]
    fn applies_the_signed_ds_offset() {
        let symbol = glyph(&["11", "11"]);
        let syms = [&symbol];
        for offset in [-16i32, -1, 0, 1, 15] {
            let shape = Shape {
                dsoffset: offset,
                ..Shape::default()
            };
            let data = text_segment(
                (48, 8),
                shape,
                2,
                1,
                0,
                &[Op::Strip(0), Op::First(4, 0), Op::Next(20, 0), Op::EndStrip],
            );
            let (_, region) = decode(&data, &syms).expect("text region");
            // The first instance sits at S = 4 and leaves CURS on its far edge,
            // 4 + 2 - 1 = 5, so the second sits at 5 + 20 + offset. The gap is
            // 20 rather than something smaller so that even an offset of -16
            // leaves the second symbol on the region instead of clipping away
            // and passing vacuously.
            expect_at(&region, &symbol, 4, 0);
            expect_at(&region, &symbol, i64::from(25 + offset), 0);
        }
    }

    /// With SBSTRIPS greater than one each instance carries its own T offset
    /// within the strip, decoded through `IAIT`.
    #[test]
    fn decodes_the_within_strip_t_offset() {
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        let shape = Shape {
            log_strips: 2, // SBSTRIPS = 4
            ..Shape::default()
        };
        // STRIPT starts at -0 * 4 and the strip's delta of 1 puts it at row 4.
        // The strip holds two instances, at CURT 0 and CURT 3 within it.
        let data =
            text_segment_with_curt((8, 16), shape, 2, 1, 0, &[(1, &[(2, 0, 0), (4, 3, 0)][..])]);
        let (_, region) = decode(&data, &syms).expect("text region");
        assert_eq!(region.get(2, 4), 1, "first at S = 2, T = 4 + 0");
        // A 1 by 1 symbol leaves CURS where it started, so the gap of 4 puts
        // the second instance at S = 6.
        assert_eq!(region.get(6, 7), 1, "second at S = 6, T = 4 + 3");
    }

    /// The delta on STRIPT is counted in strips, so with SBSTRIPS at 4 a delta
    /// of 1 moves four rows down rather than one.
    #[test]
    fn the_strip_delta_is_scaled_by_the_strip_height() {
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        let shape = Shape {
            log_strips: 2, // SBSTRIPS = 4
            ..Shape::default()
        };
        let data = text_segment_with_curt(
            (8, 32),
            shape,
            2,
            1,
            0,
            &[(1, &[(0, 0, 0)][..]), (3, &[(1, 0, 0)][..])],
        );
        let (_, region) = decode(&data, &syms).expect("text region");
        assert_eq!(region.get(0, 4), 1, "first strip at row 1 * 4");
        assert_eq!(region.get(1, 16), 1, "second strip at row (1 + 3) * 4");
    }

    /// Step 2 negates the leading `IADT`, so a positive value starts the
    /// region above its own top edge.
    #[test]
    fn the_leading_strip_offset_is_negated() {
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        // STRIPT = -3, then + 5 = 2.
        let data = text_segment(
            (8, 8),
            Shape::default(),
            1,
            1,
            3,
            &[Op::Strip(5), Op::First(0, 0), Op::EndStrip],
        );
        let (_, region) = decode(&data, &syms).expect("text region");
        assert_eq!(region.get(0, 2), 1, "STRIPT = -3 + 5");
    }

    #[test]
    fn honours_the_default_pixel_value() {
        let symbol = glyph(&["0"]);
        let syms = [&symbol];
        let shape = Shape {
            defpixel: true,
            combop: 3, // XNOR, so a 0 symbol pixel over a 1 ground gives 0
            ..Shape::default()
        };
        let data = text_segment(
            (4, 4),
            shape,
            1,
            1,
            0,
            &[Op::Strip(0), Op::First(0, 0), Op::EndStrip],
        );
        let (_, region) = decode(&data, &syms).expect("text region");
        assert_eq!(region.get(3, 3), 1, "untouched cells keep SBDEFPIXEL");
        assert_eq!(region.get(0, 0), 0, "XNOR of 1 and 0 is 0");
    }

    #[test]
    fn sym_code_len_is_the_bit_width_of_the_largest_id() {
        assert_eq!(sym_code_len(1), 0);
        assert_eq!(sym_code_len(2), 1);
        assert_eq!(sym_code_len(3), 2);
        assert_eq!(sym_code_len(4), 2);
        assert_eq!(sym_code_len(5), 3);
        assert_eq!(sym_code_len(256), 8);
        assert_eq!(sym_code_len(257), 9);
    }

    /// A region with no symbols at all is degenerate rather than illegal for
    /// this helper, and must not underflow on `SBNUMSYMS - 1`.
    #[test]
    fn sym_code_len_of_no_symbols_is_zero() {
        assert_eq!(sym_code_len(0), 0);
    }

    /// Refinement is the one coding mode a text region can still name that this
    /// build does not decode. SBHUFF used to be refused beside it, and is now
    /// the path the Huffman tests below take, so what remains to pin is that
    /// the two flags are told apart: a Huffman region whose SBREFINE bit is
    /// also set is refused, and a Huffman region on its own is not.
    #[test]
    fn refinement_reports_itself_and_huffman_no_longer_does() {
        for flags in [0x0002u16, 0x0003] {
            let mut data = vec![0u8; 17];
            data.extend_from_slice(&flags.to_be_bytes());
            data.extend_from_slice(&0u32.to_be_bytes());
            assert_eq!(
                decode(&data, &[]),
                Err(Jbig2Error::Unimplemented("text region symbol refinement")),
                "flags {flags:#06x}",
            );
        }
    }

    /// The worked example of T.88 7.4.3.1.7, which is the only Huffman fixture
    /// in this file nobody here wrote: twenty-seven bytes the specification
    /// prints, the thirty-five run code lengths it says they carry, and the
    /// thirty-two symbol ID code lengths it says those decode to.
    ///
    /// It pins the whole field at once — the four-bit lengths, B.3 over them,
    /// the run codes of Table 29, and the byte alignment of step 6, which the
    /// example accounts for as "four bits of padding to fill the last byte".
    #[test]
    fn the_symbol_id_table_reproduces_the_worked_example() {
        #[rustfmt::skip]
        const EXAMPLE: [u8; 27] = [
            0x50, 0x03, 0x35, 0x32, 0x53, 0x00, 0x00, 0x00, 0x00,
            0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x00, 0x35, 0x0F,
            0x8B, 0x30, 0x9E, 0xB8, 0x5F, 0x1D, 0xD2, 0x83, 0x00,
        ];
        // RUNCODE0 to RUNCODE34, as the example lists them. RUNCODE34 is 0,
        // which is to say the example never uses it.
        #[rustfmt::skip]
        const RUN_LENGTHS: [u8; RUN_CODES] = [
            5, 0, 0, 3, 3, 5, 3, 2, 5, 3,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            0, 0, 3, 5, 0,
        ];
        #[rustfmt::skip]
        const SYMBOL_LENGTHS: [u8; 32] = [
            0, 0, 0, 9, 6, 6, 6, 6, 3, 4, 4, 4, 4, 4, 4, 0,
            7, 9, 8, 7, 5, 5, 5, 5, 5, 5, 3, 6, 7, 4, 7, 7,
        ];

        let mut bits = BitReader::new(&EXAMPLE);
        let run_lengths = read_run_code_lengths(&mut bits).expect("run code lengths");
        assert_eq!(run_lengths, RUN_LENGTHS);

        let runs = from_code_lengths(&run_lengths).expect("run code table");
        let lengths = read_symbol_code_lengths(&mut bits, &runs, 32).expect("symbol code lengths");
        assert_eq!(lengths, SYMBOL_LENGTHS);

        // Step 6: the padding the example names, and nothing after it.
        assert_eq!(bits.remaining(), 4);
        bits.align_to_byte();
        assert!(bits.is_exhausted());

        // Step 7, whose codes the example does not print. These three are
        // worked out from B.3 by hand: the two three-bit lengths take the codes
        // 000 and 001, in table order, so they go to symbols 8 and 26, and the
        // seven four-bit lengths follow from 0100, the first of which is
        // symbol 9. Reading the field whole must reach the same table and stop
        // on the same byte boundary.
        let mut whole = BitReader::new(&EXAMPLE);
        let codes = decode_symbol_id_codes(&mut whole, 32).expect("symbol ID codes");
        assert!(whole.is_exhausted());
        let mut ids = BitReader::new(&[0b0000_0101, 0b0000_0000]);
        assert_eq!(codes.decode(&mut ids), Ok(Some(8)));
        assert_eq!(codes.decode(&mut ids), Ok(Some(26)));
        assert_eq!(codes.decode(&mut ids), Ok(Some(9)));
    }

    /// The three run codes the worked example leaves unused, since it assigns
    /// RUNCODE34 a length of 0 and never reaches for RUNCODE33's longer runs.
    ///
    /// Every run code is given a six-bit length here, so B.3 assigns RUNCODE*n*
    /// the six-bit binary of *n* and the fixture can state the run codes
    /// directly. The three lengths and their extra-bit widths are what Table 29
    /// says: 32 copies the previous length two-bits-plus-three times, 33 writes
    /// three-bits-plus-three zeros, 34 writes seven-bits-plus-eleven of them.
    #[test]
    fn the_repeating_run_codes_expand_as_table_29_says() {
        let mut w = BitWriter::default();
        for _ in 0..RUN_CODES {
            w.push(6, 4);
        }
        w.push(5, 6); // RUNCODE5: one length of 5
        w.push(32, 6);
        w.push(1, 2); // four more 5s
        w.push(33, 6);
        w.push(7, 3); // ten zeros
        w.push(34, 6);
        w.push(0, 7); // eleven zeros
        w.push(1, 6); // RUNCODE1: one length of 1
        let data = w.finish();

        let mut bits = BitReader::new(&data);
        let runs = read_run_code_table(&mut bits).expect("run code table");
        let lengths = read_symbol_code_lengths(&mut bits, &runs, 27).expect("symbol code lengths");

        let mut want = vec![5u8; 5];
        want.extend(std::iter::repeat_n(0, 21));
        want.push(1);
        assert_eq!(lengths, want);
    }

    /// A run that would write past the last symbol is a disagreement between
    /// the encoder and SBNUMSYMS, not a place to stop early.
    #[test]
    fn a_symbol_id_run_past_the_last_symbol_is_refused() {
        let mut w = BitWriter::default();
        for _ in 0..RUN_CODES {
            w.push(6, 4);
        }
        w.push(33, 6);
        w.push(7, 3); // ten zeros, where the region declares five symbols
        let data = w.finish();

        let mut bits = BitReader::new(&data);
        let runs = read_run_code_table(&mut bits).expect("run code table");
        assert_eq!(
            read_symbol_code_lengths(&mut bits, &runs, 5),
            Err(Jbig2Error::Malformed("symbol ID run past the last symbol")),
        );
    }

    /// RUNCODE32 copies the previous symbol ID code length, and there is no
    /// such thing before the first one.
    #[test]
    fn a_leading_runcode32_is_refused() {
        let mut w = BitWriter::default();
        for _ in 0..RUN_CODES {
            w.push(6, 4);
        }
        w.push(32, 6);
        w.push(0, 2);
        let data = w.finish();

        let mut bits = BitReader::new(&data);
        let runs = read_run_code_table(&mut bits).expect("run code table");
        assert_eq!(
            read_symbol_code_lengths(&mut bits, &runs, 8),
            Err(Jbig2Error::Malformed(
                "RUNCODE32 with no previous symbol ID code length"
            )),
        );
    }

    /// The Huffman counterpart of `places_instances_across_strips`, against the
    /// same three placements and the same expected pixels.
    ///
    /// STRIPT cannot start at 0 here: Tables B.11 to B.13, the three SBHUFFDT
    /// may name, code no value below 1. So the leading value is 1, which step 2
    /// negates to −1, and the first strip's delta of 3 carries STRIPT to 2 —
    /// the row the arithmetic fixture reaches with 0 and 2.
    #[test]
    fn a_huffman_region_places_instances_across_strips() {
        let syms = two_symbols();
        let refs: Vec<&Bitmap> = syms.iter().collect();
        let first: [Placement; 2] = [(1, 0, 0), (2, 0, 1)];
        let second: [Placement; 1] = [(-1, 0, 0)];
        let data = huffman_text_segment(
            (20, 16),
            Shape::default(),
            3,
            2,
            1,
            &[(3, &first[..]), (5, &second[..])],
            None,
        );
        let (info, region) = decode(&data, &refs).expect("text region");
        assert_eq!((info.width, info.height), (20, 16));
        expect_at(&region, &syms[0], 1, 2);
        expect_at(&region, &syms[1], 5, 2);
        expect_at(&region, &syms[0], 0, 7);
        assert_eq!(region.get(19, 15), 0);
    }

    /// All eight TRANSPOSED by REFCORNER combinations again, with the values
    /// read through Annex B instead of Annex A.
    ///
    /// The expected coordinates are the arithmetic test's, because none of this
    /// depends on the coding: one instance of a 3 by 2 symbol, FIRSTS = 4 and
    /// STRIPT = 5, reached here as −1 + 6.
    #[test]
    fn a_huffman_region_honours_every_refcorner_and_transposition() {
        let symbol = glyph(&["101", "010"]);
        let syms = [&symbol];
        let cases: [(bool, u8, i64, i64); 8] = [
            (false, 1, 4, 5), // TOPLEFT
            (false, 3, 4, 5), // TOPRIGHT
            (false, 0, 4, 4), // BOTTOMLEFT
            (false, 2, 4, 4), // BOTTOMRIGHT
            (true, 1, 5, 4),  // TOPLEFT
            (true, 3, 3, 4),  // TOPRIGHT
            (true, 0, 5, 4),  // BOTTOMLEFT
            (true, 2, 3, 4),  // BOTTOMRIGHT
        ];
        for (transposed, corner, x, y) in cases {
            let shape = Shape {
                corner,
                transposed,
                ..Shape::default()
            };
            let only: [Placement; 1] = [(4, 0, 0)];
            let data = huffman_text_segment((16, 16), shape, 1, 1, 1, &[(6, &only[..])], None);
            let (_, region) = decode(&data, &syms).expect("text region");
            expect_at(&region, &symbol, x, y);
        }
    }

    /// With SBSTRIPS greater than one, 6.4.9 reads the T coordinate as
    /// LOGSBSTRIPS bits straight from the stream — no table, no arithmetic
    /// decoder.
    ///
    /// Two instances in one strip, at CURT 0 and CURT 3 of a four-row strip.
    /// A decoder that read those two bits through SBHUFFDT instead, or skipped
    /// them, would desynchronise on the symbol ID that follows rather than
    /// merely misplace a row, so this fails loudly either way.
    #[test]
    fn a_huffman_region_reads_the_within_strip_t_offset_as_raw_bits() {
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        let shape = Shape {
            log_strips: 2, // SBSTRIPS = 4
            ..Shape::default()
        };
        // STRIPT starts at −1 × 4 and the strip's delta of 2 carries it to 4.
        let strip: [Placement; 2] = [(2, 0, 0), (4, 3, 0)];
        let data = huffman_text_segment((8, 16), shape, 2, 1, 1, &[(2, &strip[..])], None);
        let (_, region) = decode(&data, &syms).expect("text region");
        assert_eq!(region.get(2, 4), 1, "first at S = 2, T = 4 + 0");
        assert_eq!(region.get(6, 7), 1, "second at S = 6, T = 4 + 3");
    }

    /// A user-supplied table reaches the selector it was bound to
    /// (T.88 7.4.3.1.6).
    ///
    /// The custom table spends a single `0` bit on the values 0 to 15, where
    /// standard Table B.6 spends `00` and seven more on the same range. So a
    /// region that binds it to SBHUFFFS and is read with B.6 instead does not
    /// merely place its first symbol elsewhere — it desynchronises, and the
    /// symbol ID that follows is read out of the wrong bits.
    #[test]
    fn a_custom_table_binds_to_the_selector_that_named_it() {
        let table = parse_table_segment(&code_table_segment(0), &mut Budget::new())
            .expect("code table segment");
        let syms = two_symbols();
        let refs: Vec<&Bitmap> = syms.iter().collect();
        let strip: [Placement; 2] = [(1, 0, 0), (2, 0, 1)];
        let data = huffman_text_segment(
            (20, 16),
            Shape::default(),
            2,
            2,
            1,
            &[(3, &strip[..])],
            Some(&table),
        );
        let (_, region) = decode_with(&data, &refs, &[&table]).expect("text region");
        expect_at(&region, &syms[0], 1, 2);
        expect_at(&region, &syms[1], 5, 2);
    }

    /// The seventeen-byte region information field, the flags with SBHUFF set,
    /// a Huffman flags word and an instance count: the shortest segment that
    /// reaches the table binding of 7.4.3.1.6.
    fn huffman_header(huffman_flags: u16) -> Vec<u8> {
        let mut data = vec![0u8; 17];
        data[3] = 8; // width 8
        data[7] = 8; // height 8
        data.extend_from_slice(&0x0001u16.to_be_bytes());
        data.extend_from_slice(&huffman_flags.to_be_bytes());
        data.extend_from_slice(&1u32.to_be_bytes());
        data
    }

    /// Every way the Huffman flags word of 7.4.3.1.2 can describe a region this
    /// decoder must not read.
    #[test]
    fn the_huffman_flags_are_validated() {
        const RESERVED: &str = "reserved bit set in the text region Huffman flags";
        const NO_REFINEMENT: &str = "refinement Huffman table selected without refinement";
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        for (flags, want) in [
            (0x8000u16, RESERVED),
            // SBHUFFRDW, and then SBHUFFRSIZE, with SBREFINE clear.
            (0x0040, NO_REFINEMENT),
            (0x4000, NO_REFINEMENT),
            (0x0002, "reserved SBHUFFFS selection"),
            // A selector reading "user-supplied" with no table segment to bind.
            (0x0003, TABLE_COUNT_DISAGREES),
        ] {
            assert_eq!(
                decode(&huffman_header(flags), &syms),
                Err(Jbig2Error::Malformed(want)),
                "flags {flags:#06x}",
            );
        }
    }

    /// SBHUFFDS's OOB is the only thing that closes a strip, and SBHUFFFS and
    /// SBHUFFDT must not code one at all (T.88 7.4.3.1.6). A custom table bound
    /// to the wrong one of the three is caught before it decodes anything.
    #[test]
    fn a_custom_table_is_checked_against_its_selector() {
        let table = parse_table_segment(&code_table_segment(0), &mut Budget::new())
            .expect("code table segment");
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        // The fixture's table has HTOOB 0, so it may serve FS or DT but not DS.
        assert_eq!(
            decode_with(&huffman_header(0x000C), &syms, &[&table]),
            Err(Jbig2Error::Malformed("SBHUFFDS cannot code OOB")),
        );
        // Bound to SBHUFFFS it is acceptable, and the region then runs out of
        // coded data rather than being refused by its header.
        assert_eq!(
            decode_with(&huffman_header(0x0003), &syms, &[&table]),
            Err(Jbig2Error::Truncated),
        );
    }

    /// An arithmetic region selects no user-supplied table at all, so a
    /// referred-to code table segment is bound to nothing (T.88 7.4.3.1.6).
    #[test]
    fn an_arithmetic_region_may_not_refer_to_a_table_segment() {
        let table = parse_table_segment(&code_table_segment(0), &mut Budget::new())
            .expect("code table segment");
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        let data = text_segment(
            (8, 8),
            Shape::default(),
            1,
            1,
            0,
            &[Op::Strip(0), Op::First(0, 0), Op::EndStrip],
        );
        assert_eq!(
            decode_with(&data, &syms, &[&table]),
            Err(Jbig2Error::Malformed(TABLE_COUNT_DISAGREES)),
        );
    }

    /// A symbol count that is not a power of two leaves ids the code can
    /// express but the symbol list cannot answer, and those must be refused
    /// rather than indexed.
    #[test]
    fn an_out_of_range_symbol_id_is_rejected() {
        let symbols = [glyph(&["1"]), glyph(&["1"]), glyph(&["1"])];
        let refs: Vec<&Bitmap> = symbols.iter().collect();
        // Three symbols need a two-bit code, which can also carry the id 3.
        let data = text_segment(
            (8, 8),
            Shape::default(),
            1,
            3,
            0,
            &[Op::Strip(0), Op::First(0, 3), Op::EndStrip],
        );
        assert_eq!(
            decode(&data, &refs),
            Err(Jbig2Error::Malformed("symbol id out of range")),
        );
    }

    #[test]
    fn a_text_region_with_no_symbols_is_rejected() {
        let mut data = vec![0u8; 17];
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&1u32.to_be_bytes());
        assert_eq!(
            decode(&data, &[]),
            Err(Jbig2Error::Malformed("text region with no symbols")),
        );
    }

    #[test]
    fn an_absurd_instance_count_is_refused() {
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        let mut data = vec![0u8; 17];
        data[3] = 8; // width 8
        data[7] = 8; // height 8
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&u32::MAX.to_be_bytes());
        assert_eq!(
            decode(&data, &syms),
            Err(Jbig2Error::Malformed("instance count exceeds the limit")),
        );
    }

    /// A stream that never closes its strip is stopped by the declared
    /// instance count rather than placing symbols past it.
    #[test]
    fn a_strip_that_never_ends_stops_at_the_declared_count() {
        let symbol = glyph(&["11", "11"]);
        let syms = [&symbol];
        let data = text_segment(
            (32, 8),
            Shape::default(),
            1, // one instance declared, three coded
            1,
            0,
            &[
                Op::Strip(0),
                Op::First(0, 0),
                Op::Next(4, 0),
                Op::Next(4, 0),
                Op::EndStrip,
            ],
        );
        let (_, region) = decode(&data, &syms).expect("text region");
        expect_at(&region, &symbol, 0, 0);
        assert_eq!(region.get(5, 0), 0, "the second instance was not placed");
    }

    /// A region declaring dimensions far beyond the stream's remaining
    /// allowance is refused from the header, before a pixel is decoded.
    #[test]
    fn an_enormous_region_is_refused_by_the_budget() {
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        let mut data = 8_000u32.to_be_bytes().to_vec();
        data.extend_from_slice(&8_000u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.extend_from_slice(&0u32.to_be_bytes());
        data.push(0); // OR
        data.extend_from_slice(&0u16.to_be_bytes());
        data.extend_from_slice(&1u32.to_be_bytes());
        assert_eq!(
            decode_text_region(&data, &syms, &[], &mut Budget::with_limit(1 << 20)),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// The instances of a region draw on the same allowance the region itself
    /// does, so a small region cannot buy unbounded composition by declaring a
    /// great many placements.
    #[test]
    fn instances_draw_on_the_stream_budget() {
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        let data = text_segment(
            (8, 8),
            Shape::default(),
            2,
            1,
            0,
            &[Op::Strip(0), Op::First(0, 0), Op::Next(2, 0), Op::EndStrip],
        );
        // The 8 by 8 region itself, then a fixed price plus the composited
        // area for each of the two placements of a 1 by 1 symbol.
        let region_cost = 8 * (8 + ROW_COST);
        let instance_cost = INSTANCE_COST + (1 + ROW_COST);
        let total = region_cost + 2 * instance_cost;

        let mut budget = Budget::with_limit(total);
        assert!(decode_text_region(&data, &syms, &[], &mut budget).is_ok());

        let mut budget = Budget::with_limit(total - 1);
        assert_eq!(
            decode_text_region(&data, &syms, &[], &mut budget),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// No byte string, however malformed, may panic, hang or read out of
    /// bounds.
    #[test]
    fn arbitrary_bytes_error_rather_than_panicking() {
        let symbol = glyph(&["1", "1"]);
        let syms = [&symbol];
        let mut state: u32 = 0x7E57_10AD;
        for _ in 0..2_000 {
            let len = (state % 129) as usize;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            let _ = decode_text_region(&data, &syms, &[], &mut Budget::with_limit(1 << 16));
        }
    }

    #[test]
    fn every_truncation_of_a_valid_segment_errors_cleanly() {
        let syms = two_symbols();
        let refs: Vec<&Bitmap> = syms.iter().collect();
        let segment = text_segment(
            (20, 16),
            Shape::default(),
            3,
            2,
            0,
            &[
                Op::Strip(2),
                Op::First(1, 0),
                Op::Next(2, 1),
                Op::EndStrip,
                Op::Strip(5),
                Op::First(-1, 0),
                Op::EndStrip,
            ],
        );
        for cut in 0..segment.len() {
            let _ = decode_text_region(
                &segment[..cut],
                &refs,
                &[],
                &mut Budget::with_limit(1 << 16),
            );
        }
    }
}
