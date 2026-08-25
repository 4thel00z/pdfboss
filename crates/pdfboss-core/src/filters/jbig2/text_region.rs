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
//! Instance refinement (6.4.11) rides on that walk without changing it: after
//! the symbol ID, one more coded bit says whether the instance is the
//! dictionary symbol as it stands or that symbol refined into a bitmap of its
//! own, sized by four signed deltas and decoded against the symbol through the
//! procedure of 6.3. The arithmetic variant braids those decisions into the
//! region's one codeword; the Huffman variant carries each refinement as a
//! byte-counted arithmetic codeword of its own, byte-aligned within the
//! segment's bit stream.
//!
//! The walk serves one caller besides the text region segment: a
//! refinement/aggregate symbol dictionary decodes a many-instance symbol as a
//! text region nested in its own stream (6.5.8.2, Table 17), entering through
//! [`decode_aggregate_region`] with the dictionary's decoder and contexts
//! borrowed rather than fresh.

use super::arith_int::{decode_iaid, decode_int, IaidCtx, IntCtxSet};
use super::bitmap::{Bitmap, CombOp};
use super::budget::Budget;
use super::huffman::{from_code_lengths, read_bits, standard, take_custom, Table, Unused};
use super::mq::{MqContexts, MqDecoder};
use super::reader::Reader;
use super::refinement::{
    decode_refinement_region, parse_refinement_at, Reference, RefinementParams, GR_CONTEXT_LEN,
};
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

/// What one line of a region's symbol ID Huffman table costs, in the units
/// [`Budget`] counts (T.88 7.4.3.1.7).
///
/// 7.4.3.1.5 puts that table in the header of *every* text region segment with
/// SBHUFF set, and 7.4.3.1.7 gives it a line per symbol of SBSYMS — a count the
/// referred-to dictionaries fixed, which this segment merely names. So one
/// dictionary's exports can be made to buy the run-code decoding and the B.3
/// assignment over all of them again in each of any number of later segments,
/// none of which need be longer than a few hundred bytes. The charge the
/// dictionary made for the symbols themselves pays for the first such table and
/// for none of the rest.
///
/// The figure is a bound on what building a line costs rather than a
/// measurement of it. B.3's assignment and the by-length index the matcher
/// needs each make a pass over every line per prefix length, and prefixes run
/// to 32 bits here, so a line's share is on the order of thirty-two steps —
/// every one of them cheaper than the pixel decision the budget counts in.
pub(crate) const SYMBOL_CODE_COST: u64 = 32;

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
    let params = parse_params(&mut r, tables, budget)?;
    if symbols.is_empty() {
        return Err(Jbig2Error::Malformed("text region with no symbols"));
    }
    let num_syms = u32::try_from(symbols.len())
        .map_err(|_| Jbig2Error::Malformed("symbol count exceeds the limit"))?;

    budget.charge_region(info.width, info.height)?;
    let mut region = Bitmap::filled(info.width, info.height, params.walk.def_pixel)?;

    // The GR statistics adapt across every refinement of the region and are
    // allocated only when the region can code one; the placeholder entry a
    // refinement-free region carries instead is what `MqContexts` makes of a
    // zero length.
    let gr_len = if params.walk.refinement.is_some() {
        GR_CONTEXT_LEN
    } else {
        0
    };
    match &params.coding {
        Coding::Arithmetic => {
            let mut dec = MqDecoder::new(r.rest());
            let mut ints = IntCtxSet::new();
            let mut iaid = IaidCtx::new(sym_code_len(num_syms));
            let mut gr = MqContexts::new(gr_len);
            Walk {
                values: Arithmetic {
                    dec: &mut dec,
                    ints: &mut ints,
                    iaid: &mut iaid,
                    gr: &mut gr,
                },
                region: &mut region,
                symbols: Symbols::concat(symbols, &[]),
                params: &params.walk,
                budget,
            }
            .run()?
        }
        Coding::Huffman(tables) => {
            // 7.4.3.1.5: the last field of the segment's data header, and the
            // only one whose size depends on something outside the segment —
            // SBNUMSYMS is however many symbols the referred-to dictionaries
            // exported. It shares the cursor with the coded data that follows
            // it, which is what step 6's byte alignment exists to settle.
            let mut bits = BitReader::new(r.rest());
            let codes = decode_symbol_id_codes(&mut bits, num_syms, budget)?;
            let mut gr = MqContexts::new(gr_len);
            Walk {
                values: Huffman {
                    bits: &mut bits,
                    tables,
                    codes: SymbolCodes::Assigned(Box::new(codes)),
                    log_strips: params.log_strips,
                    gr: &mut gr,
                },
                region: &mut region,
                symbols: Symbols::concat(symbols, &[]),
                params: &params.walk,
                budget,
            }
            .run()?;
        }
    }
    Ok((info, region))
}

/// The decoding state a symbol dictionary shares with the nested text region
/// of T.88 6.5.8.2 step 2.
///
/// The nested region has no coded data of its own: its values continue in the
/// dictionary's stream, read with the statistics already adapted there —
/// 7.4.2.2 resets the integer contexts once per segment, E.3.7 the bitmap ones
/// — so everything here is borrowed from the enclosing decode rather than
/// created for the nested one. A fresh context set would decode the first
/// aggregate and desynchronise every value after it.
pub(crate) enum AggregateSource<'a, 'd> {
    /// SDHUFF = 0: the dictionary's one arithmetic decoder and its contexts.
    /// `iaid` is sized by 6.5.8.2.3's SBSYMCODELEN, which the dictionary fixed
    /// from its total symbol count so that it never changes mid-decode.
    Arithmetic {
        dec: &'a mut MqDecoder<'d>,
        ints: &'a mut IntCtxSet,
        iaid: &'a mut IaidCtx,
        gr: &'a mut MqContexts,
    },
    /// SDHUFF = 1: the dictionary's bit cursor, the shared GR statistics its
    /// refinement codewords adapt, and the equal-length symbol ID code width
    /// of 6.5.8.2.3.
    Huffman {
        bits: &'a mut BitReader<'d>,
        gr: &'a mut MqContexts,
        code_len: u32,
    },
}

/// Decodes one refinement/aggregate symbol bitmap as the nested text region of
/// T.88 6.5.8.2 step 2, with the parameters Table 17 fixes.
///
/// `symbols` is SBSYMS per 6.5.8.2.4 — the dictionary's input symbols followed
/// by the new ones decoded so far — and `instances` is REFAGGNINST, which step
/// 2 has already found to be greater than one. The bitmap is charged from its
/// declared dimensions before it is allocated, exactly as a stand-alone region
/// is; each placement then pays [`INSTANCE_COST`] and its own pixels as the
/// walk reaches it, so a large instance count buys nothing the budget does not
/// see.
pub(crate) fn decode_aggregate_region(
    source: AggregateSource<'_, '_>,
    symbols: Symbols<'_>,
    width: u32,
    height: u32,
    instances: u32,
    refinement: RefinementParams,
    budget: &mut Budget,
) -> Result<Bitmap, Jbig2Error> {
    if symbols.is_empty() {
        return Err(Jbig2Error::Malformed("text region with no symbols"));
    }
    if instances > MAX_INSTANCES {
        return Err(Jbig2Error::Malformed("instance count exceeds the limit"));
    }
    let params = WalkParams::aggregate(instances, refinement);
    budget.charge_region(width, height)?;
    let mut region = Bitmap::filled(width, height, params.def_pixel)?;
    match source {
        AggregateSource::Arithmetic {
            dec,
            ints,
            iaid,
            gr,
        } => Walk {
            values: Arithmetic {
                dec,
                ints,
                iaid,
                gr,
            },
            region: &mut region,
            symbols,
            params: &params,
            budget,
        }
        .run()?,
        AggregateSource::Huffman { bits, gr, code_len } => {
            let tables = aggregate_tables()?;
            Walk {
                values: Huffman {
                    bits,
                    tables: &tables,
                    codes: SymbolCodes::EqualLength(code_len),
                    log_strips: 0,
                    gr,
                },
                region: &mut region,
                symbols,
                params: &params,
                budget,
            }
            .run()?
        }
    }
    Ok(region)
}

/// The Huffman tables Table 17 fixes for a nested text region: B.6, B.8 and
/// B.11 for the walk, B.15 for all four refinement deltas and B.1 for a
/// refinement codeword's byte count. Nothing selects here — a nested region
/// has no header to select with.
fn aggregate_tables() -> Result<TextTables, Jbig2Error> {
    Ok(TextTables {
        fs: standard(6)?,
        ds: standard(8)?,
        dt: standard(11)?,
        refine: Some(RefineTables {
            rdw: standard(15)?,
            rdh: standard(15)?,
            rdx: standard(15)?,
            rdy: standard(15)?,
            rsize: standard(1)?,
        }),
    })
}

/// The fields of a text region segment that precede its coded data
/// (T.88 7.4.3.1).
struct TextParams {
    /// SBHUFF, and with it whatever the chosen coding needs in order to read
    /// the values that follow.
    coding: Coding,
    /// LOGSBSTRIPS. Kept alongside [`WalkParams::strips`], which is `1` shifted
    /// by it, because a Huffman region reads the T coordinate within a strip as
    /// exactly this many raw bits (6.4.9).
    log_strips: u8,
    /// The parameters the strip walk of 6.4.5 reads.
    walk: WalkParams,
}

/// The parameters of the text region decoding procedure itself (T.88 6.4.2),
/// as the strip walk reads them.
///
/// Split from [`TextParams`] because the procedure has two callers with two
/// sources for these values: a text region segment reads them from its own
/// header (7.4.3.1), and the nested region of a refinement/aggregate symbol
/// dictionary is handed the fixed set of Table 17 with no header at all.
struct WalkParams {
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
    /// SBREFINE, and with it SBRTEMPLATE and the SBRAT pixels, folded into the
    /// parameters 6.4.11 hands to the refinement procedure. `None` when the
    /// region codes no refinement flag at all — Table 12 fixes TPGRON at 0.
    refinement: Option<RefinementParams>,
}

impl WalkParams {
    /// The values Table 17 fixes for the nested text region of 6.5.8.2 step 2:
    /// one-row strips, TOPLEFT corners, no transposition, OR compositing, a 0
    /// default pixel and no gap offset — and SBREFINE is 1, with the
    /// dictionary's own refinement template and AT pixels.
    fn aggregate(instances: u32, refinement: RefinementParams) -> WalkParams {
        WalkParams {
            strips: 1,
            corner: RefCorner::TopLeft,
            transposed: false,
            comb_op: CombOp::Or,
            def_pixel: 0,
            ds_offset: 0,
            instances,
            refinement: Some(refinement),
        }
    }
}

/// The symbols a text region's coded ids index — SBSYMS of T.88 6.4.2.
///
/// A stand-alone region gets them as one list, the concatenated exports of its
/// referred-to dictionaries. The nested region of 6.5.8.2 indexes the symbols
/// its dictionary knows *so far* — the input symbols and the new ones already
/// decoded (6.5.8.2.4) — and those cannot be one slice, because the second
/// list is still being built while the first is borrowed. Two slices behind
/// one lookup keep that concatenation a view rather than a copy made afresh
/// for every aggregate symbol.
#[derive(Clone, Copy)]
pub(crate) struct Symbols<'a> {
    /// The referred-to exports, or SDINSYMS for a nested region.
    inputs: &'a [&'a Bitmap],
    /// The enclosing dictionary's SDNEWSYMS decoded so far; empty for a
    /// stand-alone region.
    new: &'a [Bitmap],
}

impl<'a> Symbols<'a> {
    /// The two lists as one, ids running over `inputs` first.
    pub(crate) fn concat(inputs: &'a [&'a Bitmap], new: &'a [Bitmap]) -> Symbols<'a> {
        Symbols { inputs, new }
    }

    /// The symbol a coded id names, or `None` for an id past both lists.
    pub(crate) fn get(&self, id: usize) -> Option<&'a Bitmap> {
        if id < self.inputs.len() {
            return Some(self.inputs[id]);
        }
        self.new.get(id - self.inputs.len())
    }

    fn is_empty(&self) -> bool {
        self.inputs.is_empty() && self.new.is_empty()
    }
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
/// The five refinement tables live behind an `Option` because 7.4.3.1.2
/// requires each of their selectors to be 0 while SBREFINE is 0: a
/// refinement-free region cannot select them, and holding them this way keeps
/// that impossibility in the type rather than in five unread fields. The
/// symbol ID table is not here either — it is not selected but carried, in
/// the field 7.4.3.1.5 puts after SBNUMINSTANCES.
struct TextTables {
    /// SBHUFFFS, the S coordinate of a strip's first instance (6.4.7).
    fs: Table,
    /// SBHUFFDS, the gap to a later instance, whose OOB closes the strip
    /// (6.4.8).
    ds: Table,
    /// SBHUFFDT, the strip offset, in strips rather than rows (6.4.6).
    dt: Table,
    /// The tables of 6.4.11, present exactly when SBREFINE is 1.
    refine: Option<RefineTables>,
}

/// The Huffman tables a refined instance is decoded with (T.88 6.4.11,
/// selectors in 7.4.3.1.2 bits 6 to 14).
struct RefineTables {
    /// SBHUFFRDW, the refinement delta width (6.4.11.1).
    rdw: Table,
    /// SBHUFFRDH, the refinement delta height (6.4.11.2).
    rdh: Table,
    /// SBHUFFRDX, the refinement X offset (6.4.11.3).
    rdx: Table,
    /// SBHUFFRDY, the refinement Y offset (6.4.11.4).
    rdy: Table,
    /// SBHUFFRSIZE, the byte count of a refinement's coded data (6.4.11.5).
    rsize: Table,
}

/// Parses the text region segment's data header down to the instance count
/// (T.88 7.4.3.1.1 to 7.4.3.1.4).
///
/// The field order is the whole reason this reads as it does. 7.4.3.1 puts the
/// Huffman flags between the ordinary flags and SBNUMINSTANCES, and makes them
/// present only when SBHUFF is 1, so a parser that reaches for the instance
/// count straight after the flags word reads two bytes of table selectors as
/// the top half of it. The refinement AT pixels sit between the two
/// (7.4.3.1.3), present only when SBREFINE is 1 *and* SBRTEMPLATE is 0 —
/// template 1 has no adaptive pixels, so a refining region that selects it
/// carries no such field.
///
/// Bit 15, SBRTEMPLATE, selects the template refinement uses; with SBREFINE
/// clear it selects nothing, so it is not examined then.
fn parse_params(
    r: &mut Reader<'_>,
    tables: &[&Table],
    budget: &mut Budget,
) -> Result<TextParams, Jbig2Error> {
    let flags = r.u16()?;
    let refine = flags & 0x0002 != 0;
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
        Coding::Huffman(Box::new(bind_tables(r.u16()?, refine, tables, budget)?))
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
    // 7.4.3.1.3: the refinement AT pixels, present only when SBREFINE is 1 and
    // SBRTEMPLATE — bit 15 — is 0.
    let refinement = if refine {
        Some(parse_refinement_at(r, flags & 0x8000 != 0)?)
    } else {
        None
    };

    let instances = r.u32()?;
    if instances > MAX_INSTANCES {
        return Err(Jbig2Error::Malformed("instance count exceeds the limit"));
    }
    Ok(TextParams {
        coding,
        log_strips,
        walk: WalkParams {
            strips,
            corner,
            transposed,
            comb_op,
            def_pixel,
            ds_offset,
            instances,
            refinement,
        },
    })
}

/// Resolves the Huffman table selectors of T.88 7.4.3.1.2 against the standard
/// tables and the referred-to code table segments (7.4.3.1.6).
///
/// The selectors are not uniform, and the one place a reader is likely to
/// assume they are is exactly where the specification says otherwise: SBHUFFDS
/// and SBHUFFDT each admit the value 2, naming Tables B.10 and B.13, where
/// SBHUFFFS and the four refinement delta selectors call 2 "not permitted".
///
/// With SBREFINE clear, everything from bit 6 up must read 0 here — 7.4.3.1.2
/// requires each of the five refinement selectors to be 0 then, so a stream
/// that sets one has named a table segment nothing in this region would ever
/// read. Saying so beats binding a table to a slot no value comes out of,
/// because the binding order is positional: a table consumed by a dead
/// selector is a table the live ones no longer receive. With SBREFINE set the
/// five bind after SBHUFFDT, in the order 7.4.3.1.6 fixes.
///
/// The OOB requirement of 7.4.3.1.6 is checked for every table rather than only
/// for the custom ones, which costs nothing because the standard tables satisfy
/// it by construction. It is what catches two custom tables bound the wrong way
/// round: SBHUFFDS's OOB is the only thing that closes a strip, so a table
/// without one would run a strip until the segment ran out — and none of the
/// other seven may code OOB at all.
fn bind_tables(
    flags: u16,
    refine: bool,
    tables: &[&Table],
    budget: &mut Budget,
) -> Result<TextTables, Jbig2Error> {
    // Bit 15.
    if flags & 0x8000 != 0 {
        return Err(Jbig2Error::Malformed(
            "reserved bit set in the text region Huffman flags",
        ));
    }
    // Bits 6 to 14: SBHUFFRDW, RDH, RDX, RDY and RSIZE.
    if !refine && flags & 0x7FC0 != 0 {
        return Err(Jbig2Error::Malformed(
            "refinement Huffman table selected without refinement",
        ));
    }

    let mut used = 0usize;
    // Bits 0 and 1: SBHUFFFS.
    let fs = match flags & 0x3 {
        0 => standard(6)?,
        1 => standard(7)?,
        3 => take_custom(tables, &mut used, TABLE_COUNT_DISAGREES, budget)?,
        _ => return Err(Jbig2Error::Malformed("reserved SBHUFFFS selection")),
    };
    // Bits 2 and 3: SBHUFFDS.
    let ds = match (flags >> 2) & 0x3 {
        0 => standard(8)?,
        1 => standard(9)?,
        2 => standard(10)?,
        _ => take_custom(tables, &mut used, TABLE_COUNT_DISAGREES, budget)?,
    };
    // Bits 4 and 5: SBHUFFDT.
    let dt = match (flags >> 4) & 0x3 {
        0 => standard(11)?,
        1 => standard(12)?,
        2 => standard(13)?,
        _ => take_custom(tables, &mut used, TABLE_COUNT_DISAGREES, budget)?,
    };
    let refine_tables = if refine {
        Some(bind_refine_tables(flags, tables, &mut used, budget)?)
    } else {
        None
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
    Ok(TextTables {
        fs,
        ds,
        dt,
        refine: refine_tables,
    })
}

/// Resolves the five refinement selectors of T.88 7.4.3.1.2, bits 6 to 14
/// (SBHUFFRDW, RDH, RDX, RDY, RSIZE).
///
/// The four delta selectors share one shape — 0 names Table B.14, 1 names
/// B.15, 2 is not permitted — and RSIZE is a single bit choosing between
/// Table B.1 and a user-supplied table. 7.4.3.1.6 forbids all five to code
/// OOB: nothing in 6.4.11 could read one, and the checks are the mirror of
/// SBHUFFDS's, whose OOB is required.
fn bind_refine_tables(
    flags: u16,
    tables: &[&Table],
    used: &mut usize,
    budget: &mut Budget,
) -> Result<RefineTables, Jbig2Error> {
    let mut delta = |selector: u16| -> Result<Table, Jbig2Error> {
        match selector {
            0 => standard(14),
            1 => standard(15),
            3 => take_custom(tables, used, TABLE_COUNT_DISAGREES, budget),
            _ => Err(Jbig2Error::Malformed(
                "reserved refinement Huffman table selection",
            )),
        }
    };
    // Bits 6 to 13: SBHUFFRDW, RDH, RDX, RDY, two bits each.
    let rdw = delta((flags >> 6) & 0x3)?;
    let rdh = delta((flags >> 8) & 0x3)?;
    let rdx = delta((flags >> 10) & 0x3)?;
    let rdy = delta((flags >> 12) & 0x3)?;
    // Bit 14: SBHUFFRSIZE.
    let rsize = if flags & 0x4000 == 0 {
        standard(1)?
    } else {
        take_custom(tables, used, TABLE_COUNT_DISAGREES, budget)?
    };
    for table in [&rdw, &rdh, &rdx, &rdy, &rsize] {
        if table.has_oob() {
            return Err(Jbig2Error::Malformed(
                "a refinement Huffman table codes OOB",
            ));
        }
    }
    Ok(RefineTables {
        rdw,
        rdh,
        rdx,
        rdy,
        rsize,
    })
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
/// The field is charged for by the line, from SBNUMSYMS, before any of it is
/// read. It is tempting to call it already paid for, on the grounds that every
/// symbol it names was charged for when a dictionary decoded or copied it; that
/// argument holds for the symbols and not for this table. The symbols are
/// charged once, where they are made. The table over them is built again by
/// every segment that refers to that dictionary, and Annex D.3 puts no limit on
/// how many segments an embedded stream holds — so the number of times B.3 is
/// run over SBNUMSYMS lines is a function of the stream's length, not of
/// anything the dictionary paid for.
fn decode_symbol_id_codes(
    bits: &mut BitReader,
    num_syms: u32,
    budget: &mut Budget,
) -> Result<Table, Jbig2Error> {
    budget.charge(SYMBOL_CODE_COST.saturating_mul(u64::from(num_syms)))?;
    let runs = read_run_code_table(bits)?;
    let lengths = read_symbol_code_lengths(bits, &runs, num_syms)?;
    // Step 6.
    bits.align_to_byte();
    // Step 7. A list in which every length is 0 assigns no code and is
    // conforming: SBSYMCODES is read only by 6.4.10, which a region whose
    // SBNUMINSTANCES is 0 never reaches, so refusing it would refuse a region
    // that legitimately paints nothing but SBDEFPIXEL.
    from_code_lengths(&lengths, Unused::Permitted)
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
///
/// Unlike the symbol ID table these lengths go on to describe, this one is
/// always read from: step 3 decodes a run code before step 5 can decide there
/// are none left to read, and SBNUMSYMS is at least 1. So a set of lengths that
/// assigns no code at all is refused here.
fn read_run_code_table(bits: &mut BitReader) -> Result<Table, Jbig2Error> {
    from_code_lengths(&read_run_code_lengths(bits)?, Unused::Refused)
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
    /// 6.4.11: RI, whether the instance refines its symbol. Read for every
    /// instance when SBREFINE is 1 and never otherwise — a raw bit with SBHUFF
    /// set, a value through `IARI` without.
    fn refine_flag(&mut self) -> Result<Option<i32>, Jbig2Error>;
    /// 6.4.11.1: RDW, the refinement delta width. Signed — a refined instance
    /// may shrink.
    fn rdw(&mut self) -> Result<Option<i32>, Jbig2Error>;
    /// 6.4.11.2: RDH, the refinement delta height.
    fn rdh(&mut self) -> Result<Option<i32>, Jbig2Error>;
    /// 6.4.11.3: RDX, the refinement X offset.
    fn rdx(&mut self) -> Result<Option<i32>, Jbig2Error>;
    /// 6.4.11.4: RDY, the refinement Y offset.
    fn rdy(&mut self) -> Result<Option<i32>, Jbig2Error>;
    /// 6.4.11 steps 5 to 7: the refined bitmap itself, decoded through 6.3
    /// against the instance's dictionary symbol.
    ///
    /// This is a whole procedure rather than a value because the two codings
    /// disagree about where the coded pixels live: the arithmetic variant
    /// braids them into the decoder every other value comes from, while the
    /// Huffman variant carries a byte count through SBHUFFRSIZE, aligns to a
    /// byte boundary, and gives the refinement a codeword of its own. Both
    /// adapt one set of GR statistics across the region's refinements — E.3.7
    /// resets statistics per segment, not per bitmap.
    fn refine(
        &mut self,
        width: u32,
        height: u32,
        reference: Reference<'_>,
        params: &RefinementParams,
        budget: &mut Budget,
    ) -> Result<Bitmap, Jbig2Error>;
}

/// The arithmetic value source: the integer procedures of Annex A, all drawing
/// on one decoder and each adapting its own contexts across the whole region.
///
/// Everything here is borrowed rather than owned because the region is not
/// always the whole segment: a stand-alone text region makes these for itself,
/// but the nested region of 6.5.8.2 reads the enclosing symbol dictionary's
/// decoder and contexts, mid-stream and mid-adaptation.
struct Arithmetic<'a, 'd> {
    /// The one arithmetic decoder every coded value of the region comes from.
    dec: &'a mut MqDecoder<'d>,
    /// The integer procedures of Annex A, adapting across the whole segment.
    ints: &'a mut IntCtxSet,
    /// The symbol ID procedure of A.3, sized by SBSYMCODELEN.
    iaid: &'a mut IaidCtx,
    /// The GR statistics every refinement of the segment adapts.
    gr: &'a mut MqContexts,
}

impl Values for Arithmetic<'_, '_> {
    fn delta_t(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(self.dec, &mut self.ints.iadt))
    }

    fn first_s(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(self.dec, &mut self.ints.iafs))
    }

    fn delta_s(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(self.dec, &mut self.ints.iads))
    }

    fn curt(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(self.dec, &mut self.ints.iait))
    }

    fn symbol_id(&mut self) -> Result<u32, Jbig2Error> {
        Ok(decode_iaid(self.dec, self.iaid))
    }

    fn refine_flag(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(self.dec, &mut self.ints.iari))
    }

    fn rdw(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(self.dec, &mut self.ints.iardw))
    }

    fn rdh(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(self.dec, &mut self.ints.iardh))
    }

    fn rdx(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(self.dec, &mut self.ints.iardx))
    }

    fn rdy(&mut self) -> Result<Option<i32>, Jbig2Error> {
        Ok(decode_int(self.dec, &mut self.ints.iardy))
    }

    fn refine(
        &mut self,
        width: u32,
        height: u32,
        reference: Reference<'_>,
        params: &RefinementParams,
        budget: &mut Budget,
    ) -> Result<Bitmap, Jbig2Error> {
        // 6.4.11 step 6: the pixel decisions follow the deltas in the
        // region's one codeword — nothing marks where they begin or end.
        decode_refinement_region(self.dec, self.gr, budget, width, height, reference, params)
    }
}

/// How a Huffman text region's symbol ids are coded (T.88 6.4.10).
enum SymbolCodes {
    /// SBSYMCODES as a stand-alone segment carries it (7.4.3.1.7): a table
    /// whose lines decode to the index of the symbol they name. Boxed for the
    /// reason the segment's other tables are — a table is most of a kilobyte,
    /// which the nested variant below would otherwise carry as its size.
    Assigned(Box<Table>),
    /// SBSYMCODES as 6.5.8.2.3 fixes it for a nested region: every symbol's
    /// code is its own index in this many bits, so reading them *is* the
    /// decode and no table need be built over a list that grows with every
    /// symbol the dictionary finishes.
    EqualLength(u32),
}

/// The Huffman value source: three selected tables, the symbol ID coding, and
/// one bit cursor they all share.
///
/// Running out of bits is [`Jbig2Error::Truncated`] here, where the arithmetic
/// source above synthesises bits forever and settles into returning OOB (T.88
/// E.3.4). The walk is written for the latter — its strip loop treats an OOB as
/// the end of a strip — so the difference matters: a truncated Huffman region
/// fails instead of decoding to a plausible short one.
///
/// The cursor and the GR statistics are borrowed for the reason the arithmetic
/// source's are: a nested region continues in its dictionary's bit stream and
/// adapts the refinement statistics the dictionary's other symbols share.
struct Huffman<'a, 'd> {
    /// The cursor over the region's coded data, positioned by 7.4.3.1.7 step 6
    /// at the byte boundary the walk begins on.
    bits: &'a mut BitReader<'d>,
    /// SBHUFFFS, SBHUFFDS and SBHUFFDT, as 7.4.3.1.6 bound them.
    tables: &'a TextTables,
    /// SBSYMCODES, however this region came by it.
    codes: SymbolCodes,
    /// LOGSBSTRIPS: how many bits an instance's T coordinate occupies (6.4.9).
    log_strips: u8,
    /// The GR statistics every refinement of the segment adapts — shared
    /// across the refinements' separate codewords, since E.3.7 resets
    /// statistics per segment.
    gr: &'a mut MqContexts,
}

impl Values for Huffman<'_, '_> {
    fn delta_t(&mut self) -> Result<Option<i32>, Jbig2Error> {
        self.tables.dt.decode(self.bits)
    }

    fn first_s(&mut self) -> Result<Option<i32>, Jbig2Error> {
        self.tables.fs.decode(self.bits)
    }

    fn delta_s(&mut self) -> Result<Option<i32>, Jbig2Error> {
        self.tables.ds.decode(self.bits)
    }

    fn curt(&mut self) -> Result<Option<i32>, Jbig2Error> {
        // 6.4.9: read directly from the bitstream, through no table at all.
        // LOGSBSTRIPS is at most 3, so the value is at most 7.
        let value = read_bits(self.bits, self.log_strips)?;
        Ok(Some(value as i32))
    }

    fn symbol_id(&mut self) -> Result<u32, Jbig2Error> {
        match &self.codes {
            // 6.4.10: bits are read until they spell one of the entries of
            // SBSYMCODES, and the value is that entry's index. The table
            // carries the index as the line's value, so the matcher of B.4
            // answers directly.
            SymbolCodes::Assigned(codes) => {
                let id = codes
                    .decode(self.bits)?
                    .ok_or(Jbig2Error::Malformed("unexpected OOB decoding a symbol id"))?;
                u32::try_from(id).map_err(|_| Jbig2Error::Malformed("symbol id out of range"))
            }
            // 6.5.8.2.3: SBSYMCODES[I] is I, so the bits are the id. An id
            // the code can express but the symbol list cannot answer is the
            // walk's to refuse, exactly as it is for the arithmetic coding.
            SymbolCodes::EqualLength(len) => read_bits(self.bits, *len as u8),
        }
    }

    fn refine_flag(&mut self) -> Result<Option<i32>, Jbig2Error> {
        // 6.4.11: one bit read directly from the bitstream, through no table.
        let bit = read_bits(self.bits, 1)?;
        Ok(Some(bit as i32))
    }

    fn rdw(&mut self) -> Result<Option<i32>, Jbig2Error> {
        self.refine_tables()?.rdw.decode(self.bits)
    }

    fn rdh(&mut self) -> Result<Option<i32>, Jbig2Error> {
        self.refine_tables()?.rdh.decode(self.bits)
    }

    fn rdx(&mut self) -> Result<Option<i32>, Jbig2Error> {
        self.refine_tables()?.rdx.decode(self.bits)
    }

    fn rdy(&mut self) -> Result<Option<i32>, Jbig2Error> {
        self.refine_tables()?.rdy.decode(self.bits)
    }

    fn refine(
        &mut self,
        width: u32,
        height: u32,
        reference: Reference<'_>,
        params: &RefinementParams,
        budget: &mut Budget,
    ) -> Result<Bitmap, Jbig2Error> {
        // 6.4.11 step 5 a): BMSIZE, the refinement's coded size in bytes.
        let size = self
            .refine_tables()?
            .rsize
            .decode(self.bits)?
            .ok_or(Jbig2Error::Malformed(
                "unexpected OOB decoding a refinement data size",
            ))?;
        let size = usize::try_from(size)
            .map_err(|_| Jbig2Error::Malformed("negative refinement data size"))?;
        // Step 5 b): the refinement's codeword begins on a byte boundary.
        self.bits.align_to_byte();
        let data = self
            .bits
            .take_aligned_bytes(size)
            .ok_or(Jbig2Error::Truncated)?;
        // Steps 6 and 7: a decoder of its own over exactly those bytes. The
        // size field, not the arithmetic decoder, says where the codeword
        // ends — taking the chunk has already positioned the cursor after it,
        // which is also the byte boundary step 7 asks for.
        let mut dec = MqDecoder::new(data);
        decode_refinement_region(&mut dec, self.gr, budget, width, height, reference, params)
    }
}

impl<'a> Huffman<'a, '_> {
    /// The refinement tables of 7.4.3.1.6, which a call implies were bound:
    /// every caller decodes a value 6.4.11 asked for, and 6.4.11 is reached
    /// only when SBREFINE is 1, the same flag that binds the tables. Refusing
    /// rather than unwrapping keeps that an assumption about this file.
    ///
    /// The result borrows the segment's tables rather than `self`, so a
    /// caller can hold it across reads of the bit cursor beside it.
    fn refine_tables(&self) -> Result<&'a RefineTables, Jbig2Error> {
        self.tables.refine.as_ref().ok_or(Jbig2Error::Malformed(
            "refinement value without refinement tables",
        ))
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
    symbols: Symbols<'a>,
    /// The parameters the caller fixed — from a segment header, or from
    /// Table 17 for a nested region.
    params: &'a WalkParams,
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
        let symbol = self
            .symbols
            .get(id as usize)
            .ok_or(Jbig2Error::Malformed("symbol id out of range"))?;

        self.budget.charge(INSTANCE_COST)?;
        // 6.4.5 step 3(c)(v): the instance's bitmap IBI — the symbol itself,
        // or that symbol refined into a bitmap of its own. Everything below
        // measures and draws IBI, never the symbol: a refined instance
        // advances CURS by its own extent, not its reference's.
        let refined = self.refine_instance(symbol)?;
        let instance = refined.as_ref().unwrap_or(symbol);
        self.budget
            .charge_region(instance.width(), instance.height())?;

        // 6.4.5 steps 3(c)(vi) and (x): CURS always finishes on the symbol's
        // far edge along the strip. Which end of the symbol that is depends on
        // the corner, so the advance happens either before the draw or after it
        // — never both, never neither. The two conditions are complements,
        // which is why one boolean drives them.
        let w = i64::from(instance.width());
        let h = i64::from(instance.height());
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
            instance,
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

    /// Decodes 6.4.11's refinement of one instance, or `None` when the
    /// instance is its dictionary symbol as it stands — because the region
    /// codes no refinements at all, or because this instance's RI bit says so.
    fn refine_instance(&mut self, symbol: &Bitmap) -> Result<Option<Bitmap>, Jbig2Error> {
        let Some(params) = &self.params.refinement else {
            return Ok(None);
        };
        let ri = self.values.refine_flag()?.ok_or(Jbig2Error::Malformed(
            "unexpected OOB decoding a refinement flag",
        ))?;
        if ri == 0 {
            return Ok(None);
        }
        const DELTA_OOB: &str = "unexpected OOB decoding a refinement delta";
        let rdw = self.values.rdw()?.ok_or(Jbig2Error::Malformed(DELTA_OOB))?;
        let rdh = self.values.rdh()?.ok_or(Jbig2Error::Malformed(DELTA_OOB))?;
        let rdx = self.values.rdx()?.ok_or(Jbig2Error::Malformed(DELTA_OOB))?;
        let rdy = self.values.rdy()?.ok_or(Jbig2Error::Malformed(DELTA_OOB))?;

        // Table 12: the refined size is the symbol's plus the signed deltas —
        // 6.4.11.1 says in as many words that a refinement may shrink, so only
        // a size below zero is a contradiction rather than a small bitmap.
        let width = u32::try_from(i64::from(symbol.width()) + i64::from(rdw))
            .map_err(|_| Jbig2Error::Malformed("refined instance size out of range"))?;
        let height = u32::try_from(i64::from(symbol.height()) + i64::from(rdh))
            .map_err(|_| Jbig2Error::Malformed("refined instance size out of range"))?;
        // Table 12: GRREFERENCEDX is ⌊RDW/2⌋ + RDX, and the floor matters —
        // an odd negative delta rounds away from zero, where `/` would round
        // toward it.
        let reference = Reference {
            bitmap: symbol,
            dx: rdw.div_euclid(2).saturating_add(rdx),
            dy: rdh.div_euclid(2).saturating_add(rdy),
        };
        self.values
            .refine(width, height, reference, params, self.budget)
            .map(Some)
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
    use crate::filters::jbig2::huffman::encoder::{push_value, BitWriter};
    use crate::filters::jbig2::huffman::parse_table_segment;
    use crate::filters::jbig2::testing::{
        code_table_segment, expect_at, glyph, huffman_refined_text_segment, huffman_text_segment,
        oob_code_table_segment, refined_text_segment, text_segment, text_segment_with_curt,
        two_symbols, Op, Placement, Refine, RefinedPlacement, Shape,
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

    /// One refined instance between two plain ones, against pixels and
    /// coordinates walked by hand from 6.4.5 and 6.4.11.
    ///
    /// The refined instance's deltas are RDW = 2, RDH = −1, RDX = 0, RDY = 1,
    /// so by Table 12 its size is 5 + 2 by 4 − 1 and the reference offsets are
    /// GRREFERENCEDX = ⌊2/2⌋ + 0 = 1 and GRREFERENCEDY = ⌊−1/2⌋ + 1 = 0 — the
    /// fixture encodes the target's pixels at exactly those literals, so the
    /// decoder reproduces the target only by deriving the same two numbers
    /// from the coded deltas.
    ///
    /// The third instance is the load-bearing one for step 3 c) x): its gap of
    /// 3 is measured from the *refined* bitmap's far edge, column 11, so it
    /// lands at 14. A decoder that advanced CURS by the dictionary symbol's
    /// width instead would put it at 12 — two columns of drift from a single
    /// refinement.
    #[test]
    fn a_refined_instance_decodes_and_advances_by_its_own_width() {
        let syms = two_symbols();
        let target = glyph(&["1111111", "1000001", "1111111"]);
        let strip = [
            RefinedPlacement {
                ds: 1,
                curt: 0,
                id: 0,
                refine: None,
            },
            RefinedPlacement {
                ds: 2,
                curt: 0,
                id: 1,
                refine: Some(Refine {
                    target: &target,
                    rdw: 2,
                    rdh: -1,
                    rdx: 0,
                    rdy: 1,
                    dx: 1, // Table 12: ⌊2/2⌋ + 0
                    dy: 0, // Table 12: ⌊−1/2⌋ + 1
                }),
            },
            RefinedPlacement {
                ds: 3,
                curt: 0,
                id: 0,
                refine: None,
            },
        ];
        let data = refined_text_segment((20, 10), Shape::default(), 3, &syms, 0, &[(2, &strip)]);
        let refs: Vec<&Bitmap> = syms.iter().collect();
        let (info, region) = decode(&data, &refs).expect("text region");
        assert_eq!((info.width, info.height), (20, 10));
        expect_at(&region, &syms[0], 1, 2);
        expect_at(&region, &target, 5, 2);
        expect_at(&region, &syms[0], 14, 2);
        assert_eq!(region.get(19, 9), 0);
    }

    /// A shrinking refinement: RDW and RDH are −1, so the instance is smaller
    /// than its reference, and both offsets exercise the floor of Table 12
    /// where it differs from truncation.
    ///
    /// GRREFERENCEDX = ⌊−1/2⌋ + 1 = 0 and GRREFERENCEDY = ⌊−1/2⌋ + 0 = −1.
    /// A decoder that divided toward zero would derive 1 and 0 instead,
    /// reading the reference one pixel askew on each axis.
    ///
    /// The bitmaps are deliberately not small. Every arithmetic context starts
    /// from the same state, so over a handful of pixels a decoder reading the
    /// reference askew still mirrors the encoder bit for bit — the wrong
    /// offset only shows once the statistics have adapted apart, which takes
    /// dozens of decisions. A fixture the size of a symbol would pass with
    /// truncation and pin nothing.
    #[test]
    fn a_shrinking_refinement_floors_its_reference_offsets() {
        let reference = glyph(&[
            "1111111111",
            "1000110001",
            "1011001101",
            "1010110101",
            "1001100011",
            "1111111111",
        ]);
        let syms = [reference];
        let target = glyph(&[
            "111111111",
            "100101001",
            "101100110",
            "101011010",
            "111111111",
        ]);
        let strip = [RefinedPlacement {
            ds: 2,
            curt: 0,
            id: 0,
            refine: Some(Refine {
                target: &target,
                rdw: -1,
                rdh: -1,
                rdx: 1,
                rdy: 0,
                dx: 0,  // Table 12: ⌊−1/2⌋ + 1
                dy: -1, // Table 12: ⌊−1/2⌋ + 0
            }),
        }];
        let data = refined_text_segment((16, 8), Shape::default(), 1, &syms, 0, &[(1, &strip)]);
        let refs: Vec<&Bitmap> = syms.iter().collect();
        let (_, region) = decode(&data, &refs).expect("text region");
        expect_at(&region, &target, 2, 1);
    }

    /// SBRTEMPLATE = 1 selects the ten-pixel refinement template, and with it
    /// 7.4.3.1.3 removes the AT field from the header entirely. A decoder that
    /// read four AT bytes anyway would take the instance count out of the
    /// wrong field, so this decoding at all pins the field's absence.
    #[test]
    fn a_template_1_refinement_carries_no_at_field() {
        let syms = two_symbols();
        let target = glyph(&["11111", "10011", "11001", "11111"]);
        let shape = Shape {
            rtemplate: 1,
            ..Shape::default()
        };
        let strip = [RefinedPlacement {
            ds: 1,
            curt: 0,
            id: 1,
            refine: Some(Refine {
                target: &target,
                rdw: 0,
                rdh: 0,
                rdx: 0,
                rdy: 0,
                dx: 0,
                dy: 0,
            }),
        }];
        let data = refined_text_segment((10, 8), shape, 1, &syms, 0, &[(1, &strip)]);
        let refs: Vec<&Bitmap> = syms.iter().collect();
        let (_, region) = decode(&data, &refs).expect("text region");
        expect_at(&region, &target, 1, 1);
    }

    /// The Huffman variant of 6.4.11: RI is a raw bit per instance, the deltas
    /// come through the selected tables — B.14 for RDW, RDX and RDY here, B.15
    /// for RDH — and each refinement is a byte-counted arithmetic codeword of
    /// its own behind a BMSIZE from Table B.1.
    ///
    /// Two instances are refined so that the second's codeword is decoded with
    /// the GR statistics the first left behind: E.3.7 resets statistics per
    /// segment, so a decoder that started each codeword from fresh contexts
    /// would read the second refinement as noise. Both refinements walk the
    /// same large reference for the reason the shrinking test's fixture is
    /// large: the statistics only tell adapted from fresh once dozens of
    /// decisions have revisited the same contexts. The trailing plain instance
    /// pins the cursor: it is read correctly only if each refinement consumed
    /// exactly BMSIZE bytes, no more and no fewer.
    #[test]
    fn a_huffman_region_decodes_refined_instances() {
        let big = glyph(&[
            "1111111111",
            "1000110001",
            "1011001101",
            "1010110101",
            "1001100011",
            "1111111111",
        ]);
        let syms = [glyph(&["101", "010", "101", "010"]), big];
        let first = glyph(&["10110011011", "11001100110", "10101010101", "11110000111"]);
        let second = glyph(&[
            "1111111111",
            "1001100101",
            "1010011001",
            "1011010101",
            "1000101011",
            "1111111111",
        ]);
        let strip = [
            RefinedPlacement {
                ds: 2,
                curt: 0,
                id: 1,
                refine: Some(Refine {
                    target: &first,
                    rdw: 1,
                    rdh: -2,
                    rdx: -1,
                    rdy: 0,
                    dx: -1, // Table 12: ⌊1/2⌋ − 1
                    dy: -1, // Table 12: ⌊−2/2⌋ + 0
                }),
            },
            RefinedPlacement {
                ds: 2,
                curt: 0,
                id: 1,
                refine: Some(Refine {
                    target: &second,
                    rdw: 0,
                    rdh: 0,
                    rdx: 0,
                    rdy: 0,
                    dx: 0,
                    dy: 0,
                }),
            },
            RefinedPlacement {
                ds: 3,
                curt: 0,
                id: 0,
                refine: None,
            },
        ];
        let data = huffman_refined_text_segment(
            (30, 8),
            Shape::default(),
            3,
            &syms,
            1,
            &[(2, &strip)],
            None,
        );
        let refs: Vec<&Bitmap> = syms.iter().collect();
        let (_, region) = decode(&data, &refs).expect("text region");
        // STRIPT = −1 + 2; FIRSTS = 2; the refined widths 11 and 10 leave CURS
        // at 12 and 23, so the gaps of 2 and 3 land the instances at 14 and 26.
        expect_at(&region, &first, 2, 1);
        expect_at(&region, &second, 14, 1);
        expect_at(&region, &syms[0], 26, 1);
    }

    /// A user-supplied table reaches the refinement selector that named it, in
    /// the binding order of 7.4.3.1.6 — after SBHUFFFS, SBHUFFDS and SBHUFFDT.
    ///
    /// The custom table spends a `0` bit and four more on the values 0 to 15,
    /// where standard Table B.14 spends three bits on 2; a region bound to it
    /// and read with B.14 instead desynchronises inside the refinement fields.
    #[test]
    fn a_custom_table_binds_to_a_refinement_selector() {
        let table = parse_table_segment(&code_table_segment(0), &mut Budget::new())
            .expect("code table segment");
        let syms = two_symbols();
        let target = glyph(&["1111111", "1000001", "1000001", "1111111"]);
        let strip = [RefinedPlacement {
            ds: 1,
            curt: 0,
            id: 1,
            refine: Some(Refine {
                target: &target,
                rdw: 2,
                rdh: 0,
                rdx: 0,
                rdy: 0,
                dx: 1, // Table 12: ⌊2/2⌋ + 0
                dy: 0,
            }),
        }];
        let data = huffman_refined_text_segment(
            (16, 8),
            Shape::default(),
            1,
            &syms,
            1,
            &[(2, &strip)],
            Some(&table),
        );
        let refs: Vec<&Bitmap> = syms.iter().collect();
        let (_, region) = decode_with(&data, &refs, &[&table]).expect("text region");
        expect_at(&region, &target, 1, 1);
    }

    /// Every refinement selector combination the Huffman flags may not name
    /// once SBREFINE is set: the reserved delta selection, a user-supplied
    /// RSIZE with no table segment to bind, and a bound table that codes OOB —
    /// which 7.4.3.1.6 forbids for all five refinement tables.
    #[test]
    fn the_refinement_huffman_flags_are_validated() {
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        // Region information, text flags with SBHUFF and SBREFINE, and the
        // Huffman flags under test; parsing fails inside the table binding,
        // before any later field is read.
        let header = |huffman_flags: u16| {
            let mut data = vec![0u8; 17];
            data[3] = 8; // width 8
            data[7] = 8; // height 8
            data.extend_from_slice(&0x0003u16.to_be_bytes());
            data.extend_from_slice(&huffman_flags.to_be_bytes());
            data
        };
        // Bits 6-7: SBHUFFRDW = 2 is not permitted (7.4.3.1.2).
        assert_eq!(
            decode(&header(2 << 6), &syms),
            Err(Jbig2Error::Malformed(
                "reserved refinement Huffman table selection"
            )),
        );
        // Bit 14: SBHUFFRSIZE reading "user-supplied" with nothing to bind.
        assert_eq!(
            decode(&header(1 << 14), &syms),
            Err(Jbig2Error::Malformed(TABLE_COUNT_DISAGREES)),
        );
        // A custom table that codes OOB, bound to SBHUFFRDW.
        let oob = parse_table_segment(&oob_code_table_segment(0), &mut Budget::new())
            .expect("code table segment");
        assert_eq!(
            decode_with(&header(3 << 6), &syms, &[&oob]),
            Err(Jbig2Error::Malformed(
                "a refinement Huffman table codes OOB"
            )),
        );
    }

    /// A refined instance decodes a fresh bitmap, and both that decode and its
    /// composite draw on the stream's one allowance — pinned at the exact
    /// boundary, like the plain-instance charge above.
    #[test]
    fn a_refined_instance_draws_on_the_stream_budget() {
        let symbol = glyph(&["1"]);
        let syms = [symbol.clone()];
        let refs: Vec<&Bitmap> = syms.iter().collect();
        let target = glyph(&["11", "10"]);
        let strip = [RefinedPlacement {
            ds: 0,
            curt: 0,
            id: 0,
            refine: Some(Refine {
                target: &target,
                rdw: 1,
                rdh: 1,
                rdx: 0,
                rdy: 0,
                dx: 0, // Table 12: ⌊1/2⌋ + 0
                dy: 0,
            }),
        }];
        let data = refined_text_segment((8, 8), Shape::default(), 1, &syms, 0, &[(0, &strip)]);
        // The 8 by 8 region, the fixed price of the placement, then the 2 by 2
        // refined bitmap twice over: once decoded, once composited.
        let total = 8 * (8 + ROW_COST) + INSTANCE_COST + 2 * (2 * (2 + ROW_COST));

        let mut budget = Budget::with_limit(total);
        assert!(decode_text_region(&data, &refs, &[], &mut budget).is_ok());

        let mut budget = Budget::with_limit(total - 1);
        assert_eq!(
            decode_text_region(&data, &refs, &[], &mut budget),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// A refinement whose declared deltas grow the instance far past the
    /// stream's allowance is refused from those deltas, before a pixel of it
    /// is decoded — the segment carries no bits to back the size up.
    #[test]
    fn an_enormous_refinement_is_refused_by_the_budget() {
        let syms = two_symbols();
        let target = glyph(&["11"]);
        let strip = [RefinedPlacement {
            ds: 0,
            curt: 0,
            id: 1,
            refine: Some(Refine {
                target: &target,
                rdw: 1 << 20,
                rdh: 1 << 20,
                rdx: 0,
                rdy: 0,
                dx: 0,
                dy: 0,
            }),
        }];
        let data = refined_text_segment((8, 8), Shape::default(), 1, &syms, 0, &[(0, &strip)]);
        let refs: Vec<&Bitmap> = syms.iter().collect();
        assert_eq!(
            decode_text_region(&data, &refs, &[], &mut Budget::with_limit(1 << 16)),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// A refinement may shrink an instance but not below nothing: deltas that
    /// leave a negative size contradict Table 12 rather than describe a small
    /// bitmap.
    #[test]
    fn a_refinement_below_zero_size_is_refused() {
        let syms = two_symbols();
        let target = glyph(&["1"]);
        let strip = [RefinedPlacement {
            ds: 0,
            curt: 0,
            id: 1,
            refine: Some(Refine {
                target: &target,
                rdw: -6, // the symbol is 5 wide
                rdh: 0,
                rdx: 0,
                rdy: 0,
                dx: 0,
                dy: 0,
            }),
        }];
        let data = refined_text_segment((8, 8), Shape::default(), 1, &syms, 0, &[(0, &strip)]);
        let refs: Vec<&Bitmap> = syms.iter().collect();
        assert_eq!(
            decode(&data, &refs),
            Err(Jbig2Error::Malformed("refined instance size out of range")),
        );
    }

    /// No truncation of a refined segment may panic, hang or read out of
    /// bounds — the refinement path adds header fields (the SBRAT pixels) and
    /// coded fields the plain sweep below never reaches.
    #[test]
    fn every_truncation_of_a_refined_segment_errors_cleanly() {
        let syms = two_symbols();
        let target = glyph(&["1111111", "1000001", "1111111"]);
        let strip = [RefinedPlacement {
            ds: 1,
            curt: 0,
            id: 1,
            refine: Some(Refine {
                target: &target,
                rdw: 2,
                rdh: -1,
                rdx: 0,
                rdy: 1,
                dx: 1,
                dy: 0,
            }),
        }];
        let segment = refined_text_segment((20, 10), Shape::default(), 1, &syms, 0, &[(2, &strip)]);
        let refs: Vec<&Bitmap> = syms.iter().collect();
        for cut in 0..segment.len() {
            let _ = decode_text_region(&segment[..cut], &refs, &[], &mut Budget::new());
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

        let runs = from_code_lengths(&run_lengths, Unused::Refused).expect("run code table");
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
        let codes =
            decode_symbol_id_codes(&mut whole, 32, &mut Budget::new()).expect("symbol ID codes");
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

    /// A symbol ID code length list in which every length is zero assigns no
    /// code at all, and that is a table B.3 defines rather than one it refuses:
    /// "the PREFLEN value 0 indicates that the table line is never used", so
    /// LENMAX in step 2 is 0 and step 3's loop never runs. 7.4.3.1.7 step 7
    /// asks for B.3 over the lengths just decoded and imposes nothing further.
    ///
    /// Such a list is conforming precisely when nothing reads the table.
    /// SBSYMCODES has one consumer, 6.4.10, and 6.4.5 step 3 a) ends the walk
    /// before the first strip when SBNUMINSTANCES is 0 — while step 2's leading
    /// STRIPT is still read. Example 2's encoder derives the lengths from
    /// per-symbol usage counts, so a region that uses no symbol is exactly the
    /// case that produces this. What the region decodes to is step 1's bitmap,
    /// left at SBDEFPIXEL.
    #[test]
    fn a_region_that_places_nothing_may_assign_no_symbol_codes() {
        let symbols = two_symbols();
        let refs: Vec<&Bitmap> = symbols.iter().collect();

        let mut w = BitWriter::default();
        // Step 1: RUNCODE0 alone is given a length, so B.3 hands it the one-bit
        // code 0 and the other thirty-four run codes none.
        for code in 0..RUN_CODES {
            w.push(u32::from(code == 0), 4);
        }
        // Steps 3 and 4: RUNCODE0 once per symbol, each saying "the symbol ID
        // code length is 0".
        w.push(0, 1);
        w.push(0, 1);
        // Step 6.
        w.align();
        // 6.4.5 step 2's leading STRIPT, read whatever SBNUMINSTANCES says.
        push_value(&mut w, &standard(11).expect("Table B.11"), Some(1));

        let mut data = vec![0u8; 17];
        data[3] = 8; // width 8
        data[7] = 8; // height 8
                     // SBHUFF, and SBDEFPIXEL 1 so the rectangle it fills shows.
        data.extend_from_slice(&0x0201u16.to_be_bytes());
        data.extend_from_slice(&0u16.to_be_bytes()); // all-standard tables
        data.extend_from_slice(&0u32.to_be_bytes()); // SBNUMINSTANCES
        data.extend_from_slice(&w.finish());

        let (info, region) = decode(&data, &refs).expect("text region");
        assert_eq!((info.width, info.height), (8, 8));
        for y in 0..8 {
            for x in 0..8 {
                assert_eq!(region.get(x, y), 1, "({x}, {y}) is not SBDEFPIXEL");
            }
        }
    }

    /// A custom table is copied into the region that binds it, and the region
    /// names it with a referred-to number rather than carrying it — so the copy
    /// is as large as the table however short the segment is, 7.4.3.1.6 lets
    /// one segment ask for three of them, and every later segment naming the
    /// same table segment asks again. The copy is charged by the line.
    ///
    /// The three charges a region makes before it reads a bit of coded data are
    /// pinned together here, in the order they happen: the table copy from
    /// 7.4.3.1.6, then the region from its declared size, then the symbol ID
    /// table of 7.4.3.1.7.
    #[test]
    fn binding_a_custom_table_is_charged_by_the_line() {
        let table = parse_table_segment(&code_table_segment(0), &mut Budget::new())
            .expect("code table segment");
        let symbol = glyph(&["1"]);
        let syms = [&symbol];
        // SBHUFFFS reading "user-supplied", against the fixture's three-line
        // table: one ordinary line and the two escape lines of B.2 steps 6 to 9.
        let data = huffman_header(0x0003);

        let mut budget = Budget::with_limit(2);
        assert_eq!(
            decode_text_region(&data, &syms, &[&table], &mut budget),
            Err(Jbig2Error::WorkLimit),
        );

        let total = 3 + 8 * (8 + ROW_COST) + SYMBOL_CODE_COST;
        let mut budget = Budget::with_limit(total - 1);
        assert_eq!(
            decode_text_region(&data, &syms, &[&table], &mut budget),
            Err(Jbig2Error::WorkLimit),
        );
        // Paid for in full, the header runs on into coded data that is not
        // there rather than being refused.
        let mut budget = Budget::with_limit(total);
        assert_eq!(
            decode_text_region(&data, &syms, &[&table], &mut budget),
            Err(Jbig2Error::Truncated),
        );
    }

    /// The symbol ID table of 7.4.3.1.7 is a line per symbol, and 7.4.3.1.5
    /// puts one in every Huffman text region segment, so what pays for it
    /// cannot be the charge the dictionary made when it decoded the symbols.
    ///
    /// The region here declares no pixels and places nothing, so
    /// [`Budget::charge_region`] and [`INSTANCE_COST`] both come to zero: the
    /// table is the only thing left that can spend anything.
    #[test]
    fn the_symbol_id_table_draws_on_the_stream_budget() {
        let symbols = vec![glyph(&["1"]); 64];
        let refs: Vec<&Bitmap> = symbols.iter().collect();
        let data = huffman_text_segment((0, 0), Shape::default(), 0, 64, 1, &[], None);
        let table_cost = SYMBOL_CODE_COST * 64;

        let mut budget = Budget::with_limit(table_cost);
        assert!(decode_text_region(&data, &refs, &[], &mut budget).is_ok());
        let mut budget = Budget::with_limit(table_cost - 1);
        assert_eq!(
            decode_text_region(&data, &refs, &[], &mut budget),
            Err(Jbig2Error::WorkLimit),
        );

        // The defect this guards: a second region naming the same dictionary
        // builds the same table over again, and has to pay over again.
        let mut budget = Budget::with_limit(2 * table_cost);
        assert!(decode_text_region(&data, &refs, &[], &mut budget).is_ok());
        assert!(decode_text_region(&data, &refs, &[], &mut budget).is_ok());
        assert_eq!(
            decode_text_region(&data, &refs, &[], &mut budget),
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
