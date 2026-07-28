//! Symbol dictionary segments (T.88 6.5, 7.4.2).
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
//! With SDHUFF set the same structure is coded with the prefix codes of
//! Annex B instead — `SDHUFFDH` for the height deltas, `SDHUFFDW` for the width
//! deltas, Table B.1 for the export runs — and one thing changes shape. A
//! height class no longer interleaves widths with bitmaps: the widths all come
//! first, and the symbols of the class arrive together in a single **collective
//! bitmap**, MMR-coded or stored raw, which is cut back into symbols by the
//! widths already read (6.5.9, figure 22). That is the construct the whole
//! Huffman variant needed the facsimile decoder for.
//!
//! Refinement/aggregate coding (SDREFAGG) is refused by name rather than
//! approximated.

use super::arith_int::{decode_int, IntCtxSet};
use super::bitmap::Bitmap;
use super::budget::Budget;
use super::generic::{decode_generic_region, decode_mmr_region, GenericParams, GB_CONTEXT_LEN};
use super::huffman::{standard, Table};
use super::mq::{MqContexts, MqDecoder};
use super::reader::Reader;
use super::Jbig2Error;
use crate::filters::ccitt::bits::BitReader;

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

/// What a dictionary is told when its Huffman table selectors and its
/// referred-to table segments do not account for one another (T.88 7.4.2.1.6).
///
/// One message for both directions, because both are the same mistake seen
/// from opposite ends: a selector reading "user-supplied" with no table segment
/// left to bind, and a table segment nothing selected.
const TABLE_COUNT_DISAGREES: &str = "Huffman table count disagrees with the dictionary flags";

/// Decodes a symbol dictionary segment's data (T.88 7.4.2), returning the
/// symbols it exports.
///
/// `input_symbols` are the symbols exported by the referred-to dictionary
/// segments, concatenated in the order the referred-to list gives them
/// (SDINSYMS). They may be re-exported by this dictionary, so they take part in
/// the export runs of 6.5.10 ahead of the symbols coded here.
///
/// `tables` are the Huffman tables the referred-to code table segments carry,
/// in the order the referred-to list names them, which is the order the
/// selectors of 7.4.2.1.6 bind them in.
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
    tables: &[&Table],
    budget: &mut Budget,
) -> Result<Vec<Bitmap>, Jbig2Error> {
    let mut r = Reader::new(data);
    let header = parse_header(&mut r, tables)?;
    let num_input = u32::try_from(input_symbols.len())
        .map_err(|_| Jbig2Error::Malformed("symbol count exceeds the limit"))?;
    if num_input > MAX_SYMBOLS || num_input.saturating_add(header.num_new) > MAX_SYMBOLS {
        return Err(Jbig2Error::Malformed("symbol count exceeds the limit"));
    }

    // Both codings decode the same two things in the same order — the new
    // symbols, then the export flags over the inputs and the new ones — out of
    // a cursor of their own over the bytes the header left.
    let (new_symbols, flags) = match &header.coding {
        Coding::Arithmetic(params) => {
            let mut dec = MqDecoder::new(r.rest());
            let mut gb = MqContexts::new(GB_CONTEXT_LEN);
            let mut ints = IntCtxSet::new();
            let new_symbols = decode_height_classes(
                &mut dec,
                &mut gb,
                &mut ints,
                params,
                header.num_new,
                budget,
            )?;
            let total = input_symbols.len() + new_symbols.len();
            let flags = decode_export_flags(total, || Ok(decode_int(&mut dec, &mut ints.iaex)))?;
            (new_symbols, flags)
        }
        Coding::Huffman(tables) => {
            let coded = r.rest();
            let mut bits = BitReader::new(coded);
            let new_symbols =
                decode_huffman_height_classes(coded, &mut bits, tables, header.num_new, budget)?;
            let total = input_symbols.len() + new_symbols.len();
            // 6.5.10 step 2: the export runs are read with Table B.1 whenever
            // SDHUFF is 1, whatever tables the dictionary selected for its own
            // height and width deltas.
            let runs = standard(1)?;
            let flags = decode_export_flags(total, || runs.decode(&mut bits))?;
            (new_symbols, flags)
        }
    };

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
/// (T.88 7.4.2.1).
struct DictHeader {
    /// SDHUFF, and with it whatever the chosen coding needs in order to read
    /// the data that follows.
    coding: Coding,
    /// SDNUMEXSYMS, the number of symbols the dictionary exports.
    num_ex: u32,
    /// SDNUMNEWSYMS, the number of symbols coded in this segment.
    num_new: u32,
}

/// How a dictionary's coded data is written (T.88 7.4.2.1.1, bit 0).
///
/// The flag decides more than which decoder reads the integers: it decides
/// which fields the header itself carries, and how a height class is laid out
/// (6.5.5). Holding the two sets of parameters in one enum is what keeps a
/// dictionary from being read with half of each.
enum Coding {
    /// SDHUFF = 0. The generic region parameters every symbol bitmap is coded
    /// with: SDTEMPLATE and SDAT, with typical prediction off (6.5.8.1).
    Arithmetic(GenericParams),
    /// SDHUFF = 1, with the tables 7.4.2.1.6 bound to the selectors. Boxed
    /// because three tables are a kilobyte and a half of lines and length
    /// slots, which every arithmetic dictionary would otherwise carry around
    /// as the size of this enum.
    Huffman(Box<HuffmanTables>),
}

/// The Huffman tables a dictionary decodes its integers with (T.88 7.4.2.1.6).
///
/// SDHUFFAGGINST is absent because nothing can select it: 7.4.2.1.1 requires
/// its field to be 0 while SDREFAGG is 0, and SDREFAGG = 1 is refused before
/// the tables are bound at all.
struct HuffmanTables {
    /// SDHUFFDH, the delta on the running height class height (6.5.6).
    dh: Table,
    /// SDHUFFDW, the delta on the running symbol width, whose OOB closes the
    /// height class (6.5.7).
    dw: Table,
    /// SDHUFFBMSIZE, the size in bytes of a height class collective bitmap
    /// (6.5.9).
    bmsize: Table,
}

/// Parses the symbol dictionary flags and the fields that follow them
/// (T.88 7.4.2.1.1 to 7.4.2.1.6).
///
/// The one coding mode this build does not implement is refused before a single
/// further byte is read, because the layout of everything after the flags
/// depends on it.
///
/// SDHUFF decides that layout too, which is why the two branches part company
/// here rather than later. **A Huffman dictionary carries no AT flags** —
/// 7.4.2.1.2 makes that field present only when SDHUFF is 0 — so reading them
/// anyway would leave the cursor eight bytes into SDNUMEXSYMS and turn a
/// perfectly good stream into a plausible-looking wrong answer.
///
/// Bits 8 and 9 — "bitmap coding context used" and "retained" — ask for the
/// arithmetic context array to be carried in from, or handed on to, another
/// dictionary segment. With SDHUFF clear both are accepted and ignored: they
/// change nothing for a dictionary that codes its symbols within one segment,
/// which is every dictionary that does not deliberately split itself, and
/// honouring them would mean keeping a context array alive across the segment
/// walk for a case no encoder in practice emits. With SDHUFF set there is no
/// such array to carry, and 7.4.2.1.1 requires both bits to be 0 along with
/// SDTEMPLATE; a stream that sets one is far more likely to be a header being
/// read at the wrong offset than a dictionary meaning something by it, so the
/// Huffman branch refuses rather than ignores. Bits 13 to 15 are reserved; they
/// select no field, so a stream that sets one still describes a dictionary that
/// can be read.
fn parse_header(r: &mut Reader<'_>, tables: &[&Table]) -> Result<DictHeader, Jbig2Error> {
    let flags = r.u16()?;
    if flags & 0x0002 != 0 {
        return Err(Jbig2Error::Unimplemented(
            "refinement/aggregate symbol coding",
        ));
    }
    let coding = if flags & 0x0001 == 0 {
        // 7.4.2.1.6: the number of selectors reading "user-supplied table" must
        // equal the number of table segments referred to, and with SDHUFF clear
        // every one of those selectors must itself be 0. A referred-to table is
        // therefore bound to nothing, which is a header describing a dictionary
        // other than the one it carries.
        if !tables.is_empty() {
            return Err(Jbig2Error::Malformed(TABLE_COUNT_DISAGREES));
        }
        let template = ((flags >> 10) & 0x3) as u8;

        // 7.4.2.1.2: eight AT bytes for template 0, two for the rest. The slots
        // a template does not use keep their nominal offsets, so the parameters
        // always describe a complete neighbourhood.
        let mut params = GenericParams::nominal(template);
        let at_pairs = if template == 0 { 4 } else { 1 };
        for slot in params.at.iter_mut().take(at_pairs) {
            let dx = r.i8()?;
            let dy = r.i8()?;
            *slot = (dx, dy);
        }
        Coding::Arithmetic(params)
    } else {
        // Bits 8 to 11, all of which 7.4.2.1.1 pins to 0 here.
        if flags & 0x0F00 != 0 {
            return Err(Jbig2Error::Malformed(
                "Huffman dictionary sets an arithmetic-only flag",
            ));
        }
        Coding::Huffman(Box::new(bind_tables(flags, tables)?))
    };

    let num_ex = r.u32()?;
    let num_new = r.u32()?;
    if num_ex > MAX_SYMBOLS || num_new > MAX_SYMBOLS {
        return Err(Jbig2Error::Malformed("symbol count exceeds the limit"));
    }
    Ok(DictHeader {
        coding,
        num_ex,
        num_new,
    })
}

/// Resolves the Huffman table selectors of T.88 7.4.2.1.1 against the standard
/// tables and the referred-to code table segments (7.4.2.1.6).
///
/// The customs are taken in the order the clause lists the selectors —
/// SDHUFFDH, SDHUFFDW, SDHUFFBMSIZE, SDHUFFAGGINST — one referred-to table
/// segment per selector reading "user-supplied", and the count of those
/// selectors must be exactly the count of table segments referred to.
///
/// The OOB requirement is checked for every table rather than only for the
/// custom ones, which costs nothing because the standard tables satisfy it by
/// construction. It is what catches two custom tables bound the wrong way
/// round: SDHUFFDW's OOB is the only thing that closes a height class, so a
/// table without one would run a class until the segment ran out.
fn bind_tables(flags: u16, tables: &[&Table]) -> Result<HuffmanTables, Jbig2Error> {
    let mut used = 0usize;
    // Bits 2 and 3: SDHUFFDH.
    let dh = match (flags >> 2) & 0x3 {
        0 => standard(4)?,
        1 => standard(5)?,
        3 => take_custom(tables, &mut used)?,
        _ => return Err(Jbig2Error::Malformed("reserved SDHUFFDH selection")),
    };
    // Bits 4 and 5: SDHUFFDW.
    let dw = match (flags >> 4) & 0x3 {
        0 => standard(2)?,
        1 => standard(3)?,
        3 => take_custom(tables, &mut used)?,
        _ => return Err(Jbig2Error::Malformed("reserved SDHUFFDW selection")),
    };
    // Bit 6: SDHUFFBMSIZE.
    let bmsize = if flags & 0x0040 == 0 {
        standard(1)?
    } else {
        take_custom(tables, &mut used)?
    };
    // Bit 7: SDHUFFAGGINST, which 7.4.2.1.1 requires to be 0 while SDREFAGG is
    // 0. Since SDREFAGG = 1 is refused, no table is ever bound to it, and a
    // stream that selects one has named a table segment this dictionary would
    // never read.
    if flags & 0x0080 != 0 {
        return Err(Jbig2Error::Malformed(
            "SDHUFFAGGINST selected without aggregate coding",
        ));
    }
    if used != tables.len() {
        return Err(Jbig2Error::Malformed(TABLE_COUNT_DISAGREES));
    }
    if !dw.has_oob() {
        return Err(Jbig2Error::Malformed("SDHUFFDW cannot code OOB"));
    }
    if dh.has_oob() || bmsize.has_oob() {
        return Err(Jbig2Error::Malformed("SDHUFFDH or SDHUFFBMSIZE codes OOB"));
    }
    Ok(HuffmanTables { dh, dw, bmsize })
}

/// The next referred-to table segment's table, in the binding order of
/// T.88 7.4.2.1.6.
///
/// The table is cloned rather than borrowed. A table is a few dozen lines, it
/// is cloned at most three times per dictionary segment, and the alternative is
/// a lifetime threaded through the header, the coding enum and every decoding
/// function below for the sake of a copy that does not show up in a profile.
fn take_custom(tables: &[&Table], used: &mut usize) -> Result<Table, Jbig2Error> {
    let table = tables
        .get(*used)
        .ok_or(Jbig2Error::Malformed(TABLE_COUNT_DISAGREES))?;
    *used += 1;
    Ok((*table).clone())
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
    params: &GenericParams,
    num_new: u32,
    budget: &mut Budget,
) -> Result<Vec<Bitmap>, Jbig2Error> {
    let mut new_symbols: Vec<Bitmap> = Vec::new();
    // The running height, and the running width inside each class below, both
    // accumulate signed deltas and are therefore free to go negative on a
    // malformed stream. Each is held wider than the dimension it becomes so
    // that the check is a comparison rather than a cast that has already lost
    // the sign.
    let mut height: i64 = 0;
    let max_classes = max_height_classes(num_new);
    let mut classes = 0usize;

    while (new_symbols.len() as u32) < num_new {
        classes += 1;
        if classes > max_classes {
            return Err(Jbig2Error::Malformed("too many symbol height classes"));
        }
        let delta = decode_int(dec, &mut ints.iadh).ok_or(Jbig2Error::Malformed(
            "unexpected OOB decoding a height class",
        ))?;
        height += i64::from(delta);
        let class_height = checked_class_height(height)?;

        // OOB closes the height class, and an exhausted decoder reads as OOB,
        // so a truncated segment ends the class rather than looping on
        // synthesized bits.
        let mut width: i64 = 0;
        while let Some(delta) = decode_int(dec, &mut ints.iadw) {
            width += i64::from(delta);
            let symbol_width = checked_symbol_width(width)?;
            if (new_symbols.len() as u32) >= num_new {
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
                params,
                None,
            )?);
        }
    }
    Ok(new_symbols)
}

/// Decodes the new symbols of a Huffman-coded dictionary, height class by
/// height class (T.88 6.5.5, figure 22).
///
/// The walk is the one above with the bitmaps moved: a class is a delta height,
/// then the delta widths of every symbol in it, then — once the OOB from
/// SDHUFFDW has closed the class — a single collective bitmap holding all of
/// those symbols side by side, which step 4 d) cuts up by the widths just read.
/// So the widths are accumulated rather than spent as they arrive, and nothing
/// is pushed to `new_symbols` until the class closes.
///
/// `coded` is the segment's data from the end of its header, and `bits` a
/// cursor into it; the collective bitmap needs both, because its MMR form is a
/// byte-aligned run of exactly BMSIZE bytes handed to a decoder that reads
/// bytes rather than sharing this cursor.
///
/// The loop bounds are the arithmetic walk's, for the same reasons, with one
/// difference behind them: exhausting the data here is
/// [`Jbig2Error::Truncated`] from the table rather than the OOB an exhausted
/// arithmetic decoder synthesises, so a truncated segment cannot close a class
/// by accident.
fn decode_huffman_height_classes(
    coded: &[u8],
    bits: &mut BitReader,
    tables: &HuffmanTables,
    num_new: u32,
    budget: &mut Budget,
) -> Result<Vec<Bitmap>, Jbig2Error> {
    let mut new_symbols: Vec<Bitmap> = Vec::new();
    let mut height: i64 = 0;
    let max_classes = max_height_classes(num_new);
    let mut classes = 0usize;

    while (new_symbols.len() as u32) < num_new {
        classes += 1;
        if classes > max_classes {
            return Err(Jbig2Error::Malformed("too many symbol height classes"));
        }
        // 6.5.6.
        let delta = tables.dh.decode(bits)?.ok_or(Jbig2Error::Malformed(
            "unexpected OOB decoding a height class",
        ))?;
        height += i64::from(delta);
        let class_height = checked_class_height(height)?;

        // 6.5.5 step 4 c): the widths of the class, and TOTWIDTH with them.
        // Both are needed after the loop, the widths to cut the collective
        // bitmap and TOTWIDTH to size it.
        let mut width: i64 = 0;
        let mut total: u64 = 0;
        let mut widths: Vec<u32> = Vec::new();
        while let Some(delta) = tables.dw.decode(bits)? {
            width += i64::from(delta);
            let symbol_width = checked_symbol_width(width)?;
            if (new_symbols.len() + widths.len()) as u64 >= u64::from(num_new) {
                return Err(Jbig2Error::Malformed("more symbols coded than declared"));
            }
            // Charged where the arithmetic walk charges it — as the symbol is
            // brought into existence, before anything is allocated for it — so
            // that a class of ten thousand one-pixel symbols costs the same
            // either way. It is also what bounds this loop when the class has
            // no rows for the region charge to land on.
            budget.charge(SYMBOL_COST)?;
            total = total.saturating_add(u64::from(symbol_width));
            widths.push(symbol_width);
        }
        let total_width =
            u32::try_from(total).map_err(|_| Jbig2Error::Malformed("height class too wide"))?;

        // 6.5.5 step 4 d). The bitmap holds the symbols concatenated left to
        // right with no gaps, so each one is the columns from where the
        // previous ended.
        let collective = decode_collective_bitmap(
            coded,
            bits,
            &tables.bmsize,
            total_width,
            class_height,
            budget,
        )?;
        let mut left = 0u32;
        for symbol_width in widths {
            new_symbols.push(columns_of(&collective, left, symbol_width)?);
            left += symbol_width;
        }
    }
    Ok(new_symbols)
}

/// How many height classes a dictionary declaring `num_new` symbols may spend
/// (T.88 6.5.5).
///
/// One class per symbol is the most a dictionary needs, since a class holds at
/// least one symbol unless it is empty; the slack covers the empty ones. An
/// empty class advances nothing, so without a cap a stream of them is a loop
/// whose length the coded data chooses.
fn max_height_classes(num_new: u32) -> usize {
    (num_new as usize)
        .saturating_mul(LOOP_SLACK)
        .saturating_add(LOOP_SLACK)
}

/// HCHEIGHT after a delta has been added to it (T.88 6.5.5 step 4 b)).
///
/// The running height is accumulated in `i64` because the deltas are signed and
/// a malformed stream is free to drive it negative; this is where that becomes
/// a refusal rather than an enormous unsigned dimension.
fn checked_class_height(height: i64) -> Result<u32, Jbig2Error> {
    if height < 0 {
        return Err(Jbig2Error::Malformed("negative symbol height class"));
    }
    u32::try_from(height).map_err(|_| Jbig2Error::Malformed("symbol too tall"))
}

/// SYMWIDTH after a delta has been added to it (T.88 6.5.5 step 4 c) i)).
fn checked_symbol_width(width: i64) -> Result<u32, Jbig2Error> {
    if width < 0 {
        return Err(Jbig2Error::Malformed("negative symbol width"));
    }
    u32::try_from(width).map_err(|_| Jbig2Error::Malformed("symbol too wide"))
}

/// The `width` columns of `collective` starting at column `left`, as a bitmap
/// of their own (T.88 6.5.5 step 4 d)).
fn columns_of(collective: &Bitmap, left: u32, width: u32) -> Result<Bitmap, Jbig2Error> {
    let mut symbol = Bitmap::new(width, collective.height())?;
    for y in 0..collective.height() {
        for x in 0..width {
            symbol.set(
                x,
                y,
                collective.get(i64::from(left) + i64::from(x), i64::from(y)),
            );
        }
    }
    Ok(symbol)
}

/// Decodes one height class collective bitmap (T.88 6.5.9).
///
/// The field is the symbols of a height class concatenated left to right,
/// preceded by its own size in bytes, and it comes in two forms. A BMSIZE of 0
/// means the rows are stored raw, each padded to a byte boundary; anything else
/// is that many bytes of MMR-coded data, which is the same facsimile coding a
/// generic region may carry (6.2.6) and is decoded by the same function — the
/// one that already refuses impossible dimensions and charges the bitmap
/// against the work budget before allocating it.
///
/// Both byte alignments of the clause matter and both are here: step 2 aligns
/// before the bitmap, step 5 after it. The second one is not "wherever the MMR
/// decoder stopped" but exactly BMSIZE bytes on from the first, which is what
/// lets an encoder omit the EOFB that would otherwise say where the data ended.
fn decode_collective_bitmap(
    coded: &[u8],
    bits: &mut BitReader,
    bmsize_table: &Table,
    width: u32,
    height: u32,
    budget: &mut Budget,
) -> Result<Bitmap, Jbig2Error> {
    // 6.5.9 step 1.
    let size = bmsize_table.decode(bits)?.ok_or(Jbig2Error::Malformed(
        "unexpected OOB decoding a collective bitmap size",
    ))?;
    let size = usize::try_from(size)
        .map_err(|_| Jbig2Error::Malformed("negative collective bitmap size"))?;
    // 6.5.9 step 2.
    bits.align_to_byte();

    if size == 0 {
        // 6.5.9 step 3. The rows are already byte-aligned, so step 5 has
        // nothing left to skip.
        return read_uncompressed(bits, width, height, budget);
    }

    // 6.5.9 step 4, with the parameters of Table 19. `decode_mmr_region`
    // charges this region against the budget from these dimensions before it
    // allocates a row of it, so nothing is charged here.
    let start = bits.bit_pos() / 8;
    let end = start.checked_add(size).ok_or(Jbig2Error::Truncated)?;
    let data = coded.get(start..end).ok_or(Jbig2Error::Truncated)?;
    let bitmap = decode_mmr_region(data, budget, width, height);
    // 6.5.9 step 5.
    skip_bytes(bits, size);
    bitmap
}

/// Reads a collective bitmap stored uncompressed (T.88 6.5.9 step 3).
///
/// The field is HCHEIGHT rows of `ceil(TOTWIDTH / 8)` bytes, each row padded
/// out to its byte boundary with 0 bits. Unlike the MMR form there is nothing
/// to decode, which is exactly why the size has to be checked first: a class
/// declaring a hundred million rows of raw pixels costs nothing to write and
/// the bitmap would be allocated before a byte of it was found to be missing.
fn read_uncompressed(
    bits: &mut BitReader,
    width: u32,
    height: u32,
    budget: &mut Budget,
) -> Result<Bitmap, Jbig2Error> {
    let stride = u64::from(width).div_ceil(8);
    let needed = stride.saturating_mul(u64::from(height));
    if (bits.remaining() as u64) / 8 < needed {
        return Err(Jbig2Error::Truncated);
    }
    budget.charge_region(width, height)?;
    let mut bitmap = Bitmap::new(width, height)?;
    for y in 0..height {
        for x in 0..width {
            let bit = bits.read_bit().ok_or(Jbig2Error::Truncated)?;
            bitmap.set(x, y, bit);
        }
        // The 0 to 7 padding bits that carry the row to a byte boundary.
        bits.align_to_byte();
    }
    Ok(bitmap)
}

/// Advances the bit cursor over `bytes` whole bytes.
///
/// A single [`BitReader::skip`] takes a `u32` count of bits, which 512 MiB of
/// segment data would overflow. The stepping loop runs once for anything
/// smaller and keeps the cursor exact for anything larger, rather than leaving
/// the position to a saturating cast.
fn skip_bytes(bits: &mut BitReader, bytes: usize) {
    let mut left = (bytes as u64).saturating_mul(8);
    while left > 0 {
        let step = left.min(u64::from(u32::MAX));
        bits.skip(step as u32);
        left -= step;
    }
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
///
/// `next_run` is the only thing the two codings disagree about here: 6.5.10
/// step 2 reads EXRUNLENGTH with the IAEX arithmetic procedure when SDHUFF is
/// 0 and with Table B.1 when it is 1. Everything the runs then describe is the
/// same, so the walk is threaded with its source rather than written twice.
fn decode_export_flags(
    total: usize,
    mut next_run: impl FnMut() -> Result<Option<i32>, Jbig2Error>,
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
        let run = next_run()?.ok_or(Jbig2Error::Malformed(
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
    use crate::filters::jbig2::huffman::encoder::{push_value, BitWriter};
    use crate::filters::jbig2::huffman::parse_table_segment;
    use crate::filters::jbig2::mq::encoder::MqEncoder;
    use crate::filters::jbig2::mq::MqContext;
    use crate::filters::jbig2::testing::{
        code_table_segment, dictionary_segment, glyph, huffman_dictionary_segment,
        nominal_at_bytes, reexport_segment, rowless_dictionary_segment, sample_symbols, Collective,
    };

    /// What one 4 x 4 symbol costs: the fixed per-symbol price and its rows.
    const FOUR_BY_FOUR: u64 = SYMBOL_COST + (4 + ROW_COST) * 4;

    /// Decodes with the allowance a real embedded stream gets.
    fn decode(data: &[u8], inputs: &[&Bitmap]) -> Result<Vec<Bitmap>, Jbig2Error> {
        decode_within(data, inputs, &mut Budget::new())
    }

    /// Decodes a dictionary that refers to no code table segment, which is
    /// every fixture whose Huffman tables are the standard ones.
    fn decode_within(
        data: &[u8],
        inputs: &[&Bitmap],
        budget: &mut Budget,
    ) -> Result<Vec<Bitmap>, Jbig2Error> {
        decode_symbol_dict(data, inputs, &[], budget)
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
    fn refagg_reports_itself() {
        let mut segment = 0x0002u16.to_be_bytes().to_vec();
        segment.extend_from_slice(&[0u8; 16]);
        assert_eq!(
            decode(&segment, &[]),
            Err(Jbig2Error::Unimplemented(
                "refinement/aggregate symbol coding"
            )),
        );
    }

    /// The Huffman variant of the same dictionary, across two height classes,
    /// with its collective bitmaps stored raw (T.88 6.5.9 step 3).
    #[test]
    fn decodes_huffman_symbols_across_height_classes() {
        let want = sample_symbols();
        let segment = huffman_dictionary_segment(&want, Collective::Uncompressed, None);
        let got = decode(&segment, &[]).expect("dictionary");
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_same(g, w, i);
        }
    }

    /// The same dictionary with its collective bitmaps MMR-coded (6.5.9
    /// step 4), which is the form the NOTE calls the usual one.
    ///
    /// The two forms must produce the same symbols, and the MMR one must be
    /// read as exactly BMSIZE bytes: the fixture's classes carry no EOFB, so a
    /// decoder that resumed wherever the facsimile decoder happened to stop
    /// would find the next height class at the wrong bit.
    #[test]
    fn decodes_huffman_symbols_from_an_mmr_collective_bitmap() {
        let want = sample_symbols();
        let segment = huffman_dictionary_segment(&want, Collective::Mmr, None);
        let got = decode(&segment, &[]).expect("dictionary");
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_same(g, w, i);
        }
    }

    /// A height class holds its symbols side by side with no gaps, so the
    /// widths read before the bitmap are the only thing that says where one
    /// symbol ends and the next begins (6.5.5 step 4 d)).
    ///
    /// Three symbols of one height and three different widths is the case that
    /// separates a correct split from an off-by-one: every symbol here has ink
    /// in its first and last column, so a boundary out by a pixel loses a
    /// column from one symbol and gains a blank one on its neighbour.
    #[test]
    fn a_height_class_is_split_by_the_widths_it_declared() {
        let want = vec![
            glyph(&["1", "1", "1"]),
            glyph(&["11", "01", "11"]),
            glyph(&["101", "111", "101"]),
        ];
        for collective in [Collective::Uncompressed, Collective::Mmr] {
            let segment = huffman_dictionary_segment(&want, collective, None);
            let got = decode(&segment, &[]).expect("dictionary");
            assert_eq!(got.len(), want.len());
            for (i, (g, w)) in got.iter().zip(&want).enumerate() {
                assert_same(g, w, i);
            }
        }
    }

    /// A user-supplied table reaches the selector it was bound to
    /// (7.4.2.1.6). The table segment is parsed by the code that will parse it
    /// in a real stream, so this pins the binding rather than a fixture's idea
    /// of one.
    ///
    /// The custom table codes 0 to 15 behind a `0` bit and four more, where
    /// Table B.4 spends its `0` on the single value 1 — so a dictionary decoded
    /// with the standard table instead does not merely read a different height,
    /// it loses the bit alignment and fails.
    #[test]
    fn a_custom_table_is_bound_to_its_selector() {
        let table =
            parse_table_segment(&code_table_segment(0), &mut Budget::new()).expect("code table");
        let want = sample_symbols();
        let segment = huffman_dictionary_segment(&want, Collective::Uncompressed, Some(&table));

        let refs = [&table];
        let got = decode_symbol_dict(&segment, &[], &refs, &mut Budget::new()).expect("dictionary");
        assert_eq!(got.len(), want.len());
        for (i, (g, w)) in got.iter().zip(&want).enumerate() {
            assert_same(g, w, i);
        }

        // The same segment with nothing to bind is a header that disagrees with
        // its own referred-to list.
        assert_eq!(
            decode(&segment, &[]),
            Err(Jbig2Error::Malformed(
                "Huffman table count disagrees with the dictionary flags"
            )),
        );
    }

    /// A Huffman dictionary carries no AT flags (7.4.2.1.2), so the eight bytes
    /// an arithmetic one spends on them are SDNUMEXSYMS and SDNUMNEWSYMS here.
    ///
    /// The check is on the counts rather than on the symbols: reading AT bytes
    /// that are not there would take SDNUMEXSYMS from the coded data and leave
    /// the two counts holding whatever the height classes were.
    #[test]
    fn a_huffman_dictionary_carries_no_at_flags() {
        let want = sample_symbols();
        let segment = huffman_dictionary_segment(&want, Collective::Uncompressed, None);
        assert_eq!(&segment[2..6], &(want.len() as u32).to_be_bytes());
        assert_eq!(&segment[6..10], &(want.len() as u32).to_be_bytes());
    }

    /// The flags 7.4.2.1.1 pins to 0 for a Huffman dictionary are refused
    /// rather than ignored, because a header setting one is far more likely to
    /// be read at the wrong offset than to mean anything by it.
    #[test]
    fn a_huffman_dictionary_may_not_set_the_arithmetic_flags() {
        let want = sample_symbols();
        let good = huffman_dictionary_segment(&want, Collective::Uncompressed, None);
        // Bit 8 "context used", bit 9 "context retained", bits 10 and 11
        // SDTEMPLATE.
        for bit in [8u16, 9, 10, 11] {
            let mut segment = good.clone();
            let flags = u16::from_be_bytes([segment[0], segment[1]]) | (1 << bit);
            segment[..2].copy_from_slice(&flags.to_be_bytes());
            assert_eq!(
                decode(&segment, &[]),
                Err(Jbig2Error::Malformed(
                    "Huffman dictionary sets an arithmetic-only flag"
                )),
                "bit {bit}",
            );
        }
    }

    /// The value 2 is not permitted for either of the two-bit table selectors
    /// (7.4.2.1.1), and SDHUFFAGGINST must be 0 while SDREFAGG is.
    #[test]
    fn the_selectors_the_standard_forbids_are_refused() {
        let want = sample_symbols();
        let good = huffman_dictionary_segment(&want, Collective::Uncompressed, None);
        for (bits, want) in [
            (2u16 << 2, "reserved SDHUFFDH selection"),
            (2 << 4, "reserved SDHUFFDW selection"),
            (1 << 7, "SDHUFFAGGINST selected without aggregate coding"),
        ] {
            let mut segment = good.clone();
            let flags = u16::from_be_bytes([segment[0], segment[1]]) | bits;
            segment[..2].copy_from_slice(&flags.to_be_bytes());
            assert_eq!(
                decode(&segment, &[]),
                Err(Jbig2Error::Malformed(want)),
                "flag bits {bits:#06x}",
            );
        }
    }

    /// SDHUFFDW must be able to code OOB and the other selectors must not
    /// (7.4.2.1.6): OOB is the only thing that closes a height class, so a
    /// table bound to the wrong slot would read a class until the segment ran
    /// out.
    #[test]
    fn a_custom_table_bound_to_the_wrong_slot_is_refused() {
        // The fixture's table has no OOB line, which is what SDHUFFDH and
        // SDHUFFBMSIZE require and what SDHUFFDW forbids.
        let table =
            parse_table_segment(&code_table_segment(0), &mut Budget::new()).expect("code table");
        let refs = [&table];
        let mut segment = (0x0001u16 | (3 << 4)).to_be_bytes().to_vec(); // SDHUFFDW custom
        segment.extend_from_slice(&1u32.to_be_bytes());
        segment.extend_from_slice(&1u32.to_be_bytes());
        assert_eq!(
            decode_symbol_dict(&segment, &[], &refs, &mut Budget::new()),
            Err(Jbig2Error::Malformed("SDHUFFDW cannot code OOB")),
        );
    }

    /// No truncation of a Huffman dictionary decodes to a shorter dictionary.
    ///
    /// This is the asymmetry with the arithmetic path, and the reason the
    /// tables report exhaustion rather than OOB: there, an exhausted decoder
    /// synthesises bits forever (T.88 E.3.4), a height class closes on the OOB
    /// that falls out of them, and a segment cut in half can read as a
    /// well-formed short one. Nothing in a prefix-coded stream means "the data
    /// ended", so every prefix of this one has to fail.
    #[test]
    fn no_truncation_of_a_huffman_dictionary_decodes() {
        let segment = huffman_dictionary_segment(&sample_symbols(), Collective::Uncompressed, None);
        assert_eq!(decode(&segment, &[]).map(|s| s.len()), Ok(3));
        for cut in 0..segment.len() {
            assert!(decode(&segment[..cut], &[]).is_err(), "cut at {cut}");
        }
    }

    /// A collective bitmap declaring more bytes than the segment holds is
    /// refused rather than decoded from whatever is there (6.5.9 step 4).
    ///
    /// Built by hand because the point is the field, not the pixels: one class
    /// of one 2 x 2 symbol, whose BMSIZE says two hundred bytes of MMR data
    /// follow and which then ends.
    #[test]
    fn a_collective_bitmap_larger_than_the_segment_is_refused() {
        let dh = standard(4).expect("Table B.4");
        let dw = standard(2).expect("Table B.2");
        let bmsize = standard(1).expect("Table B.1");
        let mut w = BitWriter::default();
        push_value(&mut w, &dh, Some(2)); // HCHEIGHT 2
        push_value(&mut w, &dw, Some(2)); // SYMWIDTH 2
        push_value(&mut w, &dw, None); // OOB closes the class
        push_value(&mut w, &bmsize, Some(200));
        w.align();

        let mut segment = 0x0001u16.to_be_bytes().to_vec();
        segment.extend_from_slice(&1u32.to_be_bytes()); // SDNUMEXSYMS
        segment.extend_from_slice(&1u32.to_be_bytes()); // SDNUMNEWSYMS
        segment.extend_from_slice(&w.finish());
        assert!(segment.len() < 200, "the demand must exceed the segment");
        assert_eq!(decode(&segment, &[]), Err(Jbig2Error::Truncated));
    }

    /// A height class declaring more pixels than the stream's whole allowance
    /// is refused from the dimensions, before a bitmap is allocated for it.
    ///
    /// The demand is a couple of dozen bytes — one delta height, one delta
    /// width and a BMSIZE of 1 — and none of it is proportional to the pixels
    /// asked for, which is exactly why the charge cannot wait for the data.
    #[test]
    fn an_enormous_collective_bitmap_is_refused_by_the_budget() {
        let dh = standard(4).expect("Table B.4");
        let dw = standard(2).expect("Table B.2");
        let b1 = standard(1).expect("Table B.1");
        let mut w = BitWriter::default();
        push_value(&mut w, &dh, Some(60_000));
        push_value(&mut w, &dw, Some(60_000));
        push_value(&mut w, &dw, None);
        push_value(&mut w, &b1, Some(1)); // one byte of MMR for 3.6e9 pixels
        w.align();
        w.push_bytes(&[0x00]);

        let mut segment = 0x0001u16.to_be_bytes().to_vec();
        segment.extend_from_slice(&1u32.to_be_bytes()); // SDNUMEXSYMS
        segment.extend_from_slice(&1u32.to_be_bytes()); // SDNUMNEWSYMS
        segment.extend_from_slice(&w.finish());
        assert!(segment.len() < 64, "the demand is {} bytes", segment.len());
        assert_eq!(decode(&segment, &[]), Err(Jbig2Error::WorkLimit));
    }

    /// A Huffman height class that codes no symbol is well formed and advances
    /// nothing, so a stream of them is refused rather than looped on — the same
    /// cap the arithmetic walk has, reached the same way.
    ///
    /// Each class here still reads a collective bitmap, because 6.5.5 step 4 d)
    /// asks for one whether or not the class took a symbol; with TOTWIDTH 0 and
    /// BMSIZE 0 that is a bitmap of no columns and no bytes.
    #[test]
    fn a_stream_of_empty_huffman_height_classes_is_refused() {
        let dh = standard(4).expect("Table B.4");
        let dw = standard(2).expect("Table B.2");
        let b1 = standard(1).expect("Table B.1");
        let mut w = BitWriter::default();
        for _ in 0..64 {
            // Table B.4 codes no delta below 1, so the class height climbs; it
            // is the symbol count that stays where it was.
            push_value(&mut w, &dh, Some(1));
            push_value(&mut w, &dw, None);
            push_value(&mut w, &b1, Some(0));
            w.align();
        }

        let mut segment = 0x0001u16.to_be_bytes().to_vec();
        segment.extend_from_slice(&1u32.to_be_bytes()); // SDNUMEXSYMS
        segment.extend_from_slice(&1u32.to_be_bytes()); // SDNUMNEWSYMS
        segment.extend_from_slice(&w.finish());
        assert_eq!(
            decode(&segment, &[]),
            Err(Jbig2Error::Malformed("too many symbol height classes")),
        );
    }

    /// Every symbol of a Huffman dictionary is charged the same as an
    /// arithmetic one, so neither coding is the cheap way to conjure bitmaps.
    #[test]
    fn huffman_symbols_are_charged_like_arithmetic_ones() {
        // Both symbols are two rows tall, so they share one height class and
        // one collective bitmap five pixels wide.
        let symbols: Vec<Bitmap> = vec![glyph(&["11", "11"]), glyph(&["111", "111"])];
        let segment = huffman_dictionary_segment(&symbols, Collective::Uncompressed, None);
        let total = SYMBOL_COST * 2 + (5 + ROW_COST) * 2;

        let mut budget = Budget::with_limit(total);
        assert!(decode_within(&segment, &[], &mut budget).is_ok());

        let mut budget = Budget::with_limit(total - 1);
        assert_eq!(
            decode_within(&segment, &[], &mut budget),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// The Huffman path must survive arbitrary bytes exactly as the arithmetic
    /// one does. The flags word is forced to SDHUFF so the sweep reaches the
    /// height class walk rather than being turned away at the first bit.
    #[test]
    fn arbitrary_huffman_bytes_error_rather_than_panicking() {
        let mut state: u32 = 0x6C1D_930B;
        for _ in 0..2_000 {
            let len = (state % 193) as usize;
            let mut data: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            if data.len() >= 2 {
                data[0] = 0;
                data[1] = 1;
            }
            let _ = decode_symbol_dict(&data, &[], &[], &mut Budget::with_limit(1 << 16));
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
            decode_within(&segment, &[], &mut Budget::with_limit(1 << 20)),
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
        assert_eq!(decode_within(&segment, &[], &mut budget), Ok(Vec::new()));

        let mut budget = Budget::with_limit(SYMBOL_COST * 64 - 1);
        assert_eq!(
            decode_within(&segment, &[], &mut budget),
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
        let got = decode_within(&segment, &refs, &mut budget).expect("dictionary");
        assert_same(&got[0], &inputs[0], 0);

        let mut budget = Budget::with_limit(FOUR_BY_FOUR - 1);
        assert_eq!(
            decode_within(&segment, &refs, &mut budget),
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
        assert!(decode_within(&segment, &[], &mut budget).is_ok());

        let mut budget = Budget::with_limit(each * 8 - 1);
        assert_eq!(
            decode_within(&segment, &[], &mut budget),
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
            let _ = decode_within(&data, &[], &mut Budget::with_limit(1 << 16));
        }
    }

    #[test]
    fn every_truncation_of_a_valid_segment_errors_cleanly() {
        let segment = dictionary_segment(&sample_symbols(), 0);
        for cut in 0..segment.len() {
            let _ = decode_within(&segment[..cut], &[], &mut Budget::with_limit(1 << 16));
        }
    }
}
