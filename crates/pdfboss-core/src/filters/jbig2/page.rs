//! Page assembly: turning a PDF-embedded segment stream into pixels
//! (T.88 7.4.8, 8.2, Annex D.3).
//!
//! One walk over the segment list is the whole procedure. A page information
//! segment sizes and pre-fills the page; each immediate region decodes into its
//! own bitmap and composites onto it at the coordinates its region information
//! field gives.
//!
//! The walk carries one piece of state between segments: the symbols each
//! symbol dictionary exported, keyed by its segment number. A text region names
//! the dictionaries it draws on in its header's referred-to list, and the
//! concatenation of their exports in that order is the list its symbol IDs
//! index — so the store has to outlive the segment that filled it, and has to
//! span the globals stream and the page stream as one sequence.
//!
//! Two rules shape everything else here.
//!
//! The page's geometry comes from the caller — the image XObject's `/Width` and
//! `/Height` — not from the page information segment. The segment's own
//! dimensions are read and discarded, which is what makes the "unknown page
//! height" encoding of a striped page a non-issue: nothing here ever needed the
//! field.
//!
//! A segment type this build cannot decode is a hard error naming the missing
//! feature, never a skip. Skipping a symbol dictionary and its text regions
//! yields a blank page that reports success, and a blank page that reports
//! success is indistinguishable from a page that is genuinely blank.
//!
//! Cost is the third. Nothing in the segment format ties how much decoding a
//! stream provokes to how many bytes it occupies: a region segment is a few
//! dozen bytes of header and may declare any dimensions its 32-bit fields can
//! hold, and Annex D.3 sets no limit on how many such segments follow one
//! another. So the whole walk — globals and page stream together — draws on a
//! single work budget, and a stream that asks for more than it is refused
//! partway through rather than decoded.

use std::collections::HashMap;

use super::bitmap::Bitmap;
use super::budget::Budget;
use super::generic::{
    decode_generic_region, decode_mmr_region, parse_generic_flags, GB_CONTEXT_LEN,
};
use super::mq::{MqContexts, MqDecoder};
use super::reader::Reader;
use super::segment::{parse_embedded, parse_region_info, RegionInfo, Segment, SegmentKind};
use super::symbol_dict::{decode_symbol_dict, MAX_SYMBOLS};
use super::text_region::decode_text_region;
use super::Jbig2Error;

/// The length in bytes of the page information segment (T.88 7.4.8).
const PAGE_INFO_LEN: usize = 19;

/// What a page information segment contributes to the decoded pixels
/// (T.88 7.4.8).
///
/// The segment carries far more than this: page width and height, both
/// resolutions, and the striping field. None of them is kept. The caller
/// supplies the page geometry from the PDF image dictionary, and no other field
/// in the segment changes a pixel.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PageInfo {
    /// The value every pixel of the page starts at, from flags bit 2.
    pub(crate) default_pixel: u8,
}

/// Parses a page information segment (T.88 7.4.8).
///
/// The default combination operator in flags bits 3 and 4 is deliberately not
/// returned. Bits 3 to 4 name the operator a region *may* be composited with,
/// and bit 6 says whether a region is allowed to override it; either way what
/// determines the pixels is the operator each region segment declares in its
/// own region information field, which is what this decoder applies. Storing a
/// value nothing reads would only invite a later reader to believe it mattered.
pub(crate) fn parse_page_info(data: &[u8]) -> Result<PageInfo, Jbig2Error> {
    // The two striping bytes that close the segment are never read, so the
    // whole block is length-checked up front rather than field by field: a
    // segment too short to hold one is not a page information segment.
    if data.len() < PAGE_INFO_LEN {
        return Err(Jbig2Error::Truncated);
    }
    let mut r = Reader::new(data);
    // Page width, height, X resolution, Y resolution: read to advance the
    // cursor, then discarded. The height field is the one that may be the
    // 0xFFFF_FFFF "unknown, striped" encoding, and it costs nothing to ignore.
    r.u32()?;
    r.u32()?;
    r.u32()?;
    r.u32()?;
    let flags = r.u8()?;
    Ok(PageInfo {
        default_pixel: (flags >> 2) & 1,
    })
}

/// Decodes a PDF-embedded JBIG2 stream (T.88 Annex D.3) into a page bitmap.
///
/// `width` and `height` come from the image XObject's `/Width` and `/Height`,
/// which is why the page information segment's own dimensions — and its
/// "unknown height" encoding for striped pages — are read and discarded.
///
/// `globals` is the `/JBIG2Globals` stream, if any; its segments are decoded
/// before the page's own, per Annex D.3. Pass an empty slice when the image has
/// no globals.
///
/// A stream may legally omit the page information segment, so the page is
/// allocated on first use with a default pixel value of 0 and replaced if a
/// page information segment turns up asking for 1.
///
/// The stream gets one [`Budget`] for all of it. A stream asking for more
/// decoding work than that fails with [`Jbig2Error::WorkLimit`] instead of
/// running for as long as its dimension fields say to.
pub(crate) fn decode_embedded(
    globals: &[u8],
    data: &[u8],
    width: u32,
    height: u32,
) -> Result<Bitmap, Jbig2Error> {
    decode_embedded_within(globals, data, width, height, &mut Budget::new())
}

/// [`decode_embedded`], with the work budget supplied rather than created.
///
/// Splitting it out keeps the budget an explicit parameter of the segment walk,
/// which is what the region types still to come will need in order to share one
/// allowance with the generic regions — and it lets the exhaustion behaviour be
/// tested against a small budget instead of by actually spending a full one.
fn decode_embedded_within(
    globals: &[u8],
    data: &[u8],
    width: u32,
    height: u32,
    budget: &mut Budget,
) -> Result<Bitmap, Jbig2Error> {
    let global_segments = parse_embedded(globals)?;
    let page_segments = parse_embedded(data)?;

    // Exported symbols, keyed by the segment number that exported them. A text
    // region names the dictionaries it draws on by number (7.4.4.2), and a
    // dictionary names the ones supplying its own inputs the same way
    // (7.4.3.1.7), so the store outlives each segment and spans the globals and
    // the page stream alike.
    let mut symbols: HashMap<u32, Vec<Bitmap>> = HashMap::new();
    let mut page: Option<Bitmap> = None;
    for segment in global_segments.iter().chain(page_segments.iter()) {
        match segment.header.kind {
            SegmentKind::PageInfo => {
                let info = parse_page_info(segment.data)?;
                if page.is_none() {
                    page = Some(Bitmap::filled(width, height, info.default_pixel)?);
                }
            }
            SegmentKind::ImmediateGenericRegion | SegmentKind::ImmediateLosslessGenericRegion => {
                let (info, region) = decode_generic_region_segment(segment, budget)?;
                let mut target = match page.take() {
                    Some(existing) => existing,
                    None => Bitmap::new(width, height)?,
                };
                target.combine(&region, offset(info.x), offset(info.y), info.op);
                page = Some(target);
            }
            SegmentKind::SymbolDictionary => {
                // The borrow of `symbols` ends with this block, because the
                // insert that follows needs the map mutably.
                let exported = {
                    let inputs = gather_symbols(&symbols, &segment.header.referred_to)?;
                    decode_symbol_dict(segment.data, &inputs, budget)?
                };
                symbols.insert(segment.header.number, exported);
            }
            SegmentKind::ImmediateTextRegion | SegmentKind::ImmediateLosslessTextRegion => {
                let available = gather_symbols(&symbols, &segment.header.referred_to)?;
                let (info, region) = decode_text_region(segment.data, &available, budget)?;
                let mut target = match page.take() {
                    Some(existing) => existing,
                    None => Bitmap::new(width, height)?,
                };
                target.combine(&region, offset(info.x), offset(info.y), info.op);
                page = Some(target);
            }
            // Segments that carry no pixels for this decoder. End of stripe
            // (7.4.9) states the Y coordinate of a stripe's last row, which is
            // informational once the caller has supplied the page height;
            // profiles and extensions carry no image data at all.
            SegmentKind::EndOfPage
            | SegmentKind::EndOfStripe
            | SegmentKind::EndOfFile
            | SegmentKind::Profiles
            | SegmentKind::Extension => {}
            // An intermediate region is not composited onto the page: it is
            // retained in an auxiliary buffer for a later refinement segment to
            // read, which is a mechanism this build does not have.
            SegmentKind::IntermediateTextRegion => {
                return Err(Jbig2Error::Unimplemented("intermediate region"))
            }
            SegmentKind::PatternDictionary => {
                return Err(Jbig2Error::Unimplemented("pattern dictionary"))
            }
            SegmentKind::IntermediateHalftoneRegion
            | SegmentKind::ImmediateHalftoneRegion
            | SegmentKind::ImmediateLosslessHalftoneRegion => {
                return Err(Jbig2Error::Unimplemented("halftone region"))
            }
            SegmentKind::IntermediateGenericRegion => {
                return Err(Jbig2Error::Unimplemented("intermediate region"))
            }
            SegmentKind::IntermediateRefinementRegion
            | SegmentKind::ImmediateRefinementRegion
            | SegmentKind::ImmediateLosslessRefinementRegion => {
                return Err(Jbig2Error::Unimplemented("refinement region"))
            }
            SegmentKind::Tables => return Err(Jbig2Error::Unimplemented("custom Huffman table")),
        }
    }

    match page {
        Some(page) => Ok(page),
        None => Bitmap::new(width, height),
    }
}

/// A region's page coordinate as a composition offset.
///
/// A location above `i32::MAX` cannot be represented as an offset; clamping
/// puts the region off the right or bottom edge, where it clips away, rather
/// than wrapping it negative and painting it over the top-left corner.
fn offset(coordinate: u32) -> i32 {
    i32::try_from(coordinate).unwrap_or(i32::MAX)
}

/// The symbols a segment's referred-to list supplies, concatenated in the order
/// the list names them (T.88 7.4.3.1.7, 7.4.4.2).
///
/// Order is the whole point: a symbol ID indexes this concatenation, so naming
/// two dictionaries the other way round names different glyphs. Segment numbers
/// are not sorted here for the same reason.
///
/// A number the store does not hold is refused rather than skipped. Within this
/// build the only segment types a text region or a dictionary may legitimately
/// refer to are symbol dictionaries — custom table segments are rejected by the
/// walk above before any region can name one — so a reference that resolves to
/// nothing is a reference to the wrong thing, and quietly dropping it would
/// shift every symbol ID after it.
///
/// The running total is capped at [`MAX_SYMBOLS`] as the list is walked, before
/// the next dictionary's symbols are appended. Nothing stops a header naming
/// one dictionary many times — the referred-to cap of the segment parser allows
/// tens of thousands of entries, each four bytes — so without this a few
/// hundred kilobytes of referred-to numbers would multiply one decoded
/// dictionary into billions of references. Both consumers refuse an input list
/// longer than this anyway; checking here is what keeps the refusal from
/// arriving after the allocation.
fn gather_symbols<'a>(
    store: &'a HashMap<u32, Vec<Bitmap>>,
    referred_to: &[u32],
) -> Result<Vec<&'a Bitmap>, Jbig2Error> {
    let mut out: Vec<&Bitmap> = Vec::new();
    for number in referred_to {
        let exported = store.get(number).ok_or(Jbig2Error::Malformed(
            "referred-to segment is not a symbol dictionary",
        ))?;
        if out.len().saturating_add(exported.len()) > MAX_SYMBOLS as usize {
            return Err(Jbig2Error::Malformed("symbol count exceeds the limit"));
        }
        out.extend(exported.iter());
    }
    Ok(out)
}

/// Decodes one immediate generic region segment (T.88 7.4.6) into its own
/// bitmap, returning it alongside the region information field that says where
/// it goes.
///
/// Each arithmetically coded generic region segment gets a fresh arithmetic
/// decoder and a fresh context array: unlike the symbol dictionary, which codes
/// every symbol of a height class through one shared array, a generic region
/// segment's coded data begins and ends within the segment.
///
/// The flags byte may instead say the region is MMR-coded (6.2.6), in which
/// case the data after it is a facsimile bit stream rather than an arithmetic
/// one and no AT bytes precede it. Everything outside the region's own pixels —
/// where it goes, how tall it is when the header did not say, and what it costs
/// — is settled the same way for both codings.
fn decode_generic_region_segment(
    segment: &Segment<'_>,
    budget: &mut Budget,
) -> Result<(RegionInfo, Bitmap), Jbig2Error> {
    let mut r = Reader::new(segment.data);
    let info = parse_region_info(&mut r)?;
    let (mmr, params) = parse_generic_flags(&mut r)?;

    // 7.2.7: when the header declared an unknown data length, the four bytes
    // after the terminator hold the real number of rows and supersede the
    // height in the region information field — which such a segment is free to
    // leave at 0xFFFF_FFFF, since the encoder did not know it either. Those
    // four bytes are raw stream data with nothing to check them against, which
    // is exactly why the row count is spent through the budget below rather
    // than trusted: the standard gives no upper bound on it, so the decoder
    // has to supply one.
    let height = match segment.header.data_len {
        Some(_) => info.height,
        None => trailing_row_count(segment.data)?,
    };

    if mmr {
        // The terminator and row count of an unknown-length segment are still
        // in this slice, and are simply never reached: the facsimile decoder
        // stops after the rows it was asked for.
        return Ok((
            info,
            decode_mmr_region(r.rest(), budget, info.width, height)?,
        ));
    }

    let mut dec = MqDecoder::new(r.rest());
    let mut cx = MqContexts::new(GB_CONTEXT_LEN);
    let bitmap =
        decode_generic_region(&mut dec, &mut cx, budget, info.width, height, &params, None)?;
    Ok((info, bitmap))
}

/// The four-byte row count that closes an unknown-length generic region
/// segment (T.88 7.2.7).
///
/// The segment splitter has already located the terminator and included both it
/// and this count in the segment's data, so the count is always the last four
/// bytes. Reading it from the end rather than re-scanning keeps one definition
/// of where the segment stops.
fn trailing_row_count(data: &[u8]) -> Result<u32, Jbig2Error> {
    let start = data.len().checked_sub(4).ok_or(Jbig2Error::Truncated)?;
    match data.get(start..) {
        Some([a, b, c, d]) => Ok(u32::from_be_bytes([*a, *b, *c, *d])),
        _ => Err(Jbig2Error::Truncated),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::ccitt::testing::{bitmap_from_rows, encode_g4};
    use crate::filters::jbig2::budget::ROW_COST;
    use crate::filters::jbig2::generic::{context_at, GenericParams, GB_CONTEXT_LEN};
    use crate::filters::jbig2::mq::{encoder::MqEncoder, MqContext};
    use crate::filters::jbig2::symbol_dict::SYMBOL_COST;
    use crate::filters::jbig2::testing::{
        dictionary_segment, expect_at, glyph, header, reexport_segment, rowless_dictionary_segment,
        split_after_segment, text_segment_for_page, Op,
    };

    /// Assembles a complete embedded stream: page info, one immediate generic
    /// region carrying `bm` at (`x`, `y`), then end of page and end of file.
    fn stream_with_region(
        page_w: u32,
        page_h: u32,
        bm: &Bitmap,
        x: u32,
        y: u32,
        op: u8,
    ) -> Vec<u8> {
        let mut out = Vec::new();

        let mut info = Vec::new();
        info.extend_from_slice(&page_w.to_be_bytes());
        info.extend_from_slice(&page_h.to_be_bytes());
        info.extend_from_slice(&0u32.to_be_bytes()); // x resolution
        info.extend_from_slice(&0u32.to_be_bytes()); // y resolution
        info.push(0); // flags: default pixel 0, default operator OR
        info.extend_from_slice(&0u16.to_be_bytes()); // striping
        out.extend_from_slice(&header(0, 48, &[], 1, info.len() as u32));
        out.extend_from_slice(&info);

        let params = GenericParams::nominal(0);
        let mut region = Vec::new();
        region.extend_from_slice(&bm.width().to_be_bytes());
        region.extend_from_slice(&bm.height().to_be_bytes());
        region.extend_from_slice(&x.to_be_bytes());
        region.extend_from_slice(&y.to_be_bytes());
        region.push(op);
        region.push(0); // MMR 0, template 0, TPGDON 0
        for (dx, dy) in params.at {
            region.push(dx as u8);
            region.push(dy as u8);
        }
        region.extend_from_slice(&encode_bitmap(bm, &params));
        out.extend_from_slice(&header(1, 38, &[], 1, region.len() as u32));
        out.extend_from_slice(&region);

        out.extend_from_slice(&header(2, 49, &[], 1, 0)); // end of page
        out.extend_from_slice(&header(3, 51, &[], 1, 0)); // end of file
        out
    }

    /// One immediate generic region segment declaring `width` by `height` and
    /// carrying no coded data at all.
    ///
    /// A stream does not have to supply the bits it asks to have decoded: past
    /// the end of the data the arithmetic decoder keeps answering (T.88
    /// E.3.4). That is what makes 31 bytes enough to demand any amount of
    /// decoding, and it is the shape every cost test below uses.
    fn empty_region_segment(number: u32, width: u32, height: u32) -> Vec<u8> {
        let mut region = Vec::new();
        region.extend_from_slice(&width.to_be_bytes());
        region.extend_from_slice(&height.to_be_bytes());
        region.extend_from_slice(&0u32.to_be_bytes()); // x
        region.extend_from_slice(&0u32.to_be_bytes()); // y
        region.push(0); // OR
        region.push(0); // MMR 0, template 0, TPGDON 0
        for (dx, dy) in GenericParams::nominal(0).at {
            region.push(dx as u8);
            region.push(dy as u8);
        }
        let mut out = header(number, 38, &[], 1, region.len() as u32);
        out.extend_from_slice(&region);
        out
    }

    /// The same demand with the MMR flag set: no AT bytes, and a facsimile bit
    /// stream that is not there either.
    ///
    /// The facsimile decoder stops as soon as the bits run out, so what an
    /// MMR region buys with an enormous declared height is not decoding but the
    /// bitmap and the row walk that precede it — which is exactly why the
    /// charge has to be made from the header rather than from the data.
    fn empty_mmr_region_segment(number: u32, width: u32, height: u32) -> Vec<u8> {
        let mut region = Vec::new();
        region.extend_from_slice(&width.to_be_bytes());
        region.extend_from_slice(&height.to_be_bytes());
        region.extend_from_slice(&0u32.to_be_bytes()); // x
        region.extend_from_slice(&0u32.to_be_bytes()); // y
        region.push(0); // OR
        region.push(1); // MMR 1, no AT bytes
        let mut out = header(number, 38, &[], 1, region.len() as u32);
        out.extend_from_slice(&region);
        out
    }

    /// Encodes a bitmap with TPGDON off, which is what every fixture here
    /// declares in its generic region flags byte.
    fn encode_bitmap(bm: &Bitmap, params: &GenericParams) -> Vec<u8> {
        let mut enc = MqEncoder::new();
        let mut cx = vec![MqContext::default(); GB_CONTEXT_LEN];
        for y in 0..bm.height() {
            for x in 0..bm.width() {
                let ctx = context_at(bm, x, y, params) as usize;
                enc.encode(&mut cx[ctx], bm.get(i64::from(x), i64::from(y)));
            }
        }
        enc.finish()
    }

    fn checker(width: u32, height: u32) -> Bitmap {
        let mut bm = Bitmap::new(width, height).expect("bitmap");
        for y in 0..height {
            for x in 0..width {
                bm.set(x, y, u8::from((x + y) % 2 == 0));
            }
        }
        bm
    }

    #[test]
    fn decodes_a_single_generic_region_onto_the_page() {
        let bm = checker(16, 8);
        let stream = stream_with_region(32, 16, &bm, 4, 2, 0);
        let page = decode_embedded(&[], &stream, 32, 16).expect("page");
        assert_eq!((page.width(), page.height()), (32, 16));
        for y in 0..8u32 {
            for x in 0..16u32 {
                assert_eq!(
                    page.get(i64::from(x + 4), i64::from(y + 2)),
                    bm.get(i64::from(x), i64::from(y)),
                    "({x}, {y})",
                );
            }
        }
        // Outside the region the page keeps its default pixel value.
        assert_eq!(page.get(0, 0), 0);
        assert_eq!(page.get(31, 15), 0);
    }

    /// The page size comes from the caller — the PDF image dictionary — not
    /// from the page information segment, and disagreement is not an error.
    #[test]
    fn the_caller_page_size_wins_over_the_segment() {
        let bm = checker(8, 4);
        let stream = stream_with_region(1000, 1000, &bm, 0, 0, 0);
        let page = decode_embedded(&[], &stream, 12, 6).expect("page");
        assert_eq!((page.width(), page.height()), (12, 6));
    }

    #[test]
    fn a_region_hanging_off_the_page_is_clipped() {
        let bm = checker(16, 16);
        let stream = stream_with_region(20, 20, &bm, 12, 12, 0);
        let page = decode_embedded(&[], &stream, 20, 20).expect("page");
        assert_eq!(page.get(19, 19), bm.get(7, 7));
    }

    #[test]
    fn the_page_default_pixel_value_is_honoured() {
        let bm = Bitmap::new(4, 4).expect("4x4"); // all zeros
        let mut stream = stream_with_region(16, 16, &bm, 0, 0, 4); // REPLACE
                                                                   // Flip page information flags bit 2: default pixel value 1.
        let flags_at = 11 + 16; // header is 11 bytes, flags follow four u32s
        stream[flags_at] |= 0b100;
        let page = decode_embedded(&[], &stream, 16, 16).expect("page");
        assert_eq!(page.get(15, 15), 1, "outside the region");
        assert_eq!(page.get(0, 0), 0, "REPLACE inside the region");
    }

    /// Globals are prepended to the page's own segments (Annex D.3).
    #[test]
    fn globals_are_parsed_before_the_page_stream() {
        let bm = checker(8, 8);
        let full = stream_with_region(16, 16, &bm, 0, 0, 0);
        // Split after the page information segment: header (11) + data (19).
        let (globals, rest) = full.split_at(11 + 19);
        let page = decode_embedded(globals, rest, 16, 16).expect("page");
        assert_eq!(page.get(0, 0), bm.get(0, 0));
        assert_eq!(page.get(7, 7), bm.get(7, 7));
    }

    /// A stream with no page information segment still decodes: the page
    /// defaults to zeros, which is what every embedded stream in practice
    /// wants anyway.
    #[test]
    fn a_missing_page_information_segment_is_tolerated() {
        let bm = checker(8, 8);
        let full = stream_with_region(16, 16, &bm, 0, 0, 0);
        let rest = &full[11 + 19..];
        let page = decode_embedded(&[], rest, 16, 16).expect("page");
        assert_eq!(page.get(3, 3), bm.get(3, 3));
    }

    /// Every operator reaches composition intact.
    #[test]
    fn region_operators_reach_composition() {
        // A page pre-filled with 1s, XOR-ed with an all-1s region, is all 0s.
        let bm = Bitmap::filled(8, 8, 1).expect("8x8");
        let mut stream = stream_with_region(8, 8, &bm, 0, 0, 2); // XOR
        stream[11 + 16] |= 0b100; // page default pixel 1
        let page = decode_embedded(&[], &stream, 8, 8).expect("page");
        for y in 0..8u32 {
            for x in 0..8u32 {
                assert_eq!(page.get(i64::from(x), i64::from(y)), 0, "({x}, {y})");
            }
        }
    }

    /// Segment types later plans own must fail loudly, naming themselves, so
    /// the render report can say what is missing rather than showing a blank.
    #[test]
    fn unimplemented_segment_types_are_named_errors() {
        for (kind, want) in [
            (4u8, "intermediate region"),
            (16, "pattern dictionary"),
            (22, "halftone region"),
            (36, "intermediate region"),
            (42, "refinement region"),
            (53, "custom Huffman table"),
        ] {
            let stream = header(0, kind, &[], 1, 0);
            assert_eq!(
                decode_embedded(&[], &stream, 8, 8),
                Err(Jbig2Error::Unimplemented(want)),
                "segment type {kind}",
            );
        }
    }

    /// An immediate generic region whose MMR flag is set carries a
    /// two-dimensional facsimile stream instead of arithmetically coded pixels,
    /// and no AT bytes at all (T.88 6.2.6, 7.4.6.2). Its pixels must reach the
    /// page at the offset its region information field gives, the same as any
    /// other region's.
    #[test]
    fn a_page_with_an_mmr_region_decodes() {
        let bm = bitmap_from_rows(&[
            "01111110", "01000010", "01011010", "01000010", "01111110", "00011000",
        ]);
        let (x, y) = (2u32, 3u32);

        let mut region = Vec::new();
        region.extend_from_slice(&bm.width().to_be_bytes());
        region.extend_from_slice(&bm.height().to_be_bytes());
        region.extend_from_slice(&x.to_be_bytes());
        region.extend_from_slice(&y.to_be_bytes());
        region.push(0); // OR
        region.push(1); // MMR 1: no AT bytes follow
        region.extend_from_slice(&encode_g4(&bm));
        let mut stream = header(0, 38, &[], 1, region.len() as u32);
        stream.extend_from_slice(&region);

        let page = decode_embedded(&[], &stream, 16, 16).expect("page");
        for row in 0..16u32 {
            for col in 0..16u32 {
                let want = bm.get(i64::from(col) - i64::from(x), i64::from(row) - i64::from(y));
                assert_eq!(
                    page.get(i64::from(col), i64::from(row)),
                    want,
                    "({col}, {row})",
                );
            }
        }
    }

    /// The polarity relationship, end to end: an MMR region and an
    /// arithmetically coded one carrying the same image must composite to the
    /// same page. A set pixel is ink in both codings, so neither path may
    /// invert — the one inversion this decoder performs is at the filter
    /// boundary, on the assembled page.
    #[test]
    fn an_mmr_region_and_an_arithmetic_region_paint_the_same_page() {
        let bm = checker(8, 8);

        let mut region = Vec::new();
        region.extend_from_slice(&bm.width().to_be_bytes());
        region.extend_from_slice(&bm.height().to_be_bytes());
        region.extend_from_slice(&0u32.to_be_bytes());
        region.extend_from_slice(&0u32.to_be_bytes());
        region.push(0); // OR
        region.push(1); // MMR 1
        region.extend_from_slice(&encode_g4(&bm));
        let mut stream = header(0, 38, &[], 1, region.len() as u32);
        stream.extend_from_slice(&region);

        let mmr = decode_embedded(&[], &stream, 16, 16).expect("mmr page");
        let arithmetic = decode_embedded(&[], &stream_with_region(16, 16, &bm, 0, 0, 0), 16, 16)
            .expect("arithmetic page");
        assert_eq!(mmr, arithmetic);
    }

    /// Informational segments are consumed without complaint.
    #[test]
    fn end_of_stripe_profiles_and_extension_are_ignored() {
        let mut stream = Vec::new();
        let mut eos = header(0, 50, &[], 1, 4);
        eos.extend_from_slice(&99u32.to_be_bytes());
        stream.extend_from_slice(&eos);
        stream.extend_from_slice(&header(1, 52, &[], 1, 0));
        let mut ext = header(2, 62, &[], 1, 4);
        ext.extend_from_slice(&[0, 0, 0, 0]);
        stream.extend_from_slice(&ext);
        stream.extend_from_slice(&header(3, 51, &[], 1, 0));
        let page = decode_embedded(&[], &stream, 4, 4).expect("page");
        assert_eq!((page.width(), page.height()), (4, 4));
    }

    #[test]
    fn a_truncated_stream_errors_rather_than_panicking() {
        let bm = checker(16, 8);
        let full = stream_with_region(32, 16, &bm, 0, 0, 0);
        for cut in 0..full.len() {
            let _ = decode_embedded(&[], &full[..cut], 32, 16);
        }
    }

    #[test]
    fn arbitrary_bytes_error_rather_than_panicking() {
        let mut state: u32 = 0x0BAD_F00D;
        for _ in 0..1_000 {
            let len = (state % 129) as usize;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            let _ = decode_embedded(&[], &data, 16, 16);
        }
    }

    /// A segment whose header declared an unknown data length takes its row
    /// count from the four bytes after the terminator, not from the region
    /// information field (7.2.7).
    ///
    /// The fixture leaves the region height at `0xFFFF_FFFF`, which is what an
    /// encoder that did not know the height when it wrote the header emits. A
    /// decoder that trusted that field would ask for four billion rows and be
    /// refused, so this cannot pass by accident.
    #[test]
    fn an_unknown_length_region_takes_its_height_from_the_row_count() {
        let bm = checker(8, 4);
        let params = GenericParams::nominal(0);

        let mut region = Vec::new();
        region.extend_from_slice(&bm.width().to_be_bytes());
        region.extend_from_slice(&u32::MAX.to_be_bytes()); // height not yet known
        region.extend_from_slice(&0u32.to_be_bytes());
        region.extend_from_slice(&0u32.to_be_bytes());
        region.push(0); // OR
        region.push(0); // MMR 0, template 0, TPGDON 0
        for (dx, dy) in params.at {
            region.push(dx as u8);
            region.push(dy as u8);
        }
        // `finish` already closes the coded data with the FF AC terminator the
        // 7.2.7 scan looks for; the row count follows it.
        region.extend_from_slice(&encode_bitmap(&bm, &params));
        region.extend_from_slice(&bm.height().to_be_bytes());

        let mut stream = header(0, 38, &[], 1, u32::MAX);
        stream.extend_from_slice(&region);

        let page = decode_embedded(&[], &stream, 8, 4).expect("page");
        for y in 0..4u32 {
            for x in 0..8u32 {
                assert_eq!(
                    page.get(i64::from(x), i64::from(y)),
                    bm.get(i64::from(x), i64::from(y)),
                    "({x}, {y})",
                );
            }
        }
    }

    /// An MMR region may be written that way too, and its terminator is a
    /// different pair of bytes: `00 00` rather than `FF AC` (7.2.7). The height
    /// still comes from the four bytes after it, and the facsimile decoder
    /// still has to stop before reading either as image data.
    #[test]
    fn an_unknown_length_mmr_region_takes_its_height_from_the_row_count() {
        let bm = bitmap_from_rows(&["01111110", "01000010", "01011010", "01111110"]);
        let coded = encode_g4(&bm);
        assert!(
            !coded.windows(2).any(|pair| pair == [0x00, 0x00]),
            "the fixture must not contain the terminator it is delimited by",
        );

        let mut region = Vec::new();
        region.extend_from_slice(&bm.width().to_be_bytes());
        region.extend_from_slice(&u32::MAX.to_be_bytes()); // height not yet known
        region.extend_from_slice(&0u32.to_be_bytes());
        region.extend_from_slice(&0u32.to_be_bytes());
        region.push(0); // OR
        region.push(1); // MMR 1, no AT bytes
        region.extend_from_slice(&coded);
        region.extend_from_slice(&[0x00, 0x00]); // the terminator, then the count
        region.extend_from_slice(&bm.height().to_be_bytes());

        let mut stream = header(0, 38, &[], 1, u32::MAX);
        stream.extend_from_slice(&region);

        let page = decode_embedded(&[], &stream, 8, 4).expect("page");
        for y in 0..4u32 {
            for x in 0..8u32 {
                assert_eq!(
                    page.get(i64::from(x), i64::from(y)),
                    bm.get(i64::from(x), i64::from(y)),
                    "({x}, {y})",
                );
            }
        }
    }

    #[test]
    fn a_short_page_information_segment_is_truncated() {
        for len in 0..19 {
            assert_eq!(parse_page_info(&vec![0u8; len]), Err(Jbig2Error::Truncated));
        }
        assert_eq!(
            parse_page_info(&[0u8; 19])
                .expect("page info")
                .default_pixel,
            0,
        );
    }

    /// A region no pixels wide allocates nothing, so a cap on the bitmap never
    /// sees it — and the row loop then runs for as many rows as four
    /// attacker-chosen bytes say. Thirty-one bytes must not buy four billion
    /// passes over a page.
    #[test]
    fn a_narrow_region_of_enormous_height_is_refused() {
        for width in [0u32, 1, 2] {
            let stream = empty_region_segment(0, width, u32::MAX);
            assert!(stream.len() < 64, "the demand is {} bytes", stream.len());
            assert_eq!(
                decode_embedded(&[], &stream, 8, 8),
                Err(Jbig2Error::WorkLimit),
                "width {width}",
            );
        }
    }

    /// A region coded the other way must be refused on the same terms. The MMR
    /// arm reaches a different decoder through a different bit reader, so it is
    /// a second place the charge could have been left out — and a region no
    /// pixels wide still allocates nothing there either.
    #[test]
    fn a_narrow_mmr_region_of_enormous_height_is_refused() {
        for width in [0u32, 1, 2] {
            let stream = empty_mmr_region_segment(0, width, u32::MAX);
            assert!(stream.len() < 64, "the demand is {} bytes", stream.len());
            assert_eq!(
                decode_embedded(&[], &stream, 8, 8),
                Err(Jbig2Error::WorkLimit),
                "width {width}",
            );
        }
    }

    /// And MMR regions spend the *stream's* budget, not one of their own: a
    /// second allowance for the second coding would let a stream have both.
    #[test]
    fn mmr_regions_draw_on_the_same_budget_as_arithmetic_ones() {
        // Every region here costs (16 + ROW_COST) * 16, whichever way it is
        // coded, because the charge is made from the declared dimensions.
        let each = (16 + ROW_COST) * 16;
        let mut stream = empty_mmr_region_segment(0, 16, 16);
        stream.extend_from_slice(&empty_region_segment(1, 16, 16));

        let mut budget = Budget::with_limit(each * 2);
        assert!(decode_embedded_within(&[], &stream, 16, 16, &mut budget).is_ok());

        let mut budget = Budget::with_limit(each * 2 - 1);
        assert_eq!(
            decode_embedded_within(&[], &stream, 16, 16, &mut budget),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// The same demand made through the unknown-length encoding of 7.2.7,
    /// where the row count is four raw bytes at the end of the segment with
    /// nothing in the format to bound them — not even the region information
    /// field's own height, which such a segment is entitled to leave unset.
    #[test]
    fn an_unknown_length_region_cannot_buy_unbounded_rows() {
        let params = GenericParams::nominal(0);
        let mut region = Vec::new();
        region.extend_from_slice(&0u32.to_be_bytes()); // width: allocates nothing
        region.extend_from_slice(&u32::MAX.to_be_bytes()); // height not yet known
        region.extend_from_slice(&0u32.to_be_bytes());
        region.extend_from_slice(&0u32.to_be_bytes());
        region.push(0); // OR
        region.push(0); // MMR 0, template 0, TPGDON 0
        for (dx, dy) in params.at {
            region.push(dx as u8);
            region.push(dy as u8);
        }
        region.extend_from_slice(&[0xFF, 0xAC]); // the 7.2.7 terminator
        region.extend_from_slice(&u32::MAX.to_be_bytes()); // and the row count

        let mut stream = header(0, 38, &[], 1, u32::MAX);
        stream.extend_from_slice(&region);
        assert!(stream.len() < 64, "the demand is {} bytes", stream.len());
        assert_eq!(
            decode_embedded(&[], &stream, 8, 8),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// A region at the largest size the allocation cap permits is affordable,
    /// but only so many of them are: the budget covers the whole stream, so
    /// repeating the segment cannot repeat the cost indefinitely.
    ///
    /// Charged, not decoded — the point of the budget is that the refusal
    /// happens from the header alone.
    #[test]
    fn the_stream_budget_bounds_a_repeated_region() {
        let mut budget = Budget::new();
        // 8192 x 16384 is MAX_PIXELS exactly, the largest region there is.
        assert_eq!(budget.charge_region(8192, 16384), Ok(()));
        assert_eq!(
            budget.charge_region(8192, 16384),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// The budget spans the whole walk rather than resetting per segment, so a
    /// stream of individually affordable regions is refused once their total
    /// runs out.
    ///
    /// The budget is supplied rather than taken from [`decode_embedded`]
    /// because exhausting the real one requires decoding a real page's worth of
    /// pixels first, which is precisely the cost this exists to avoid paying.
    #[test]
    fn regions_across_a_stream_draw_on_one_budget() {
        // Every region here costs (16 + ROW_COST) * 16.
        let each = (16 + ROW_COST) * 16;
        let mut stream = Vec::new();
        for number in 0..4u32 {
            stream.extend_from_slice(&empty_region_segment(number, 16, 16));
        }

        let mut budget = Budget::with_limit(each * 4);
        assert!(decode_embedded_within(&[], &stream, 16, 16, &mut budget).is_ok());

        let mut budget = Budget::with_limit(each * 4 - 1);
        assert_eq!(
            decode_embedded_within(&[], &stream, 16, 16, &mut budget),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// Globals draw on the same budget as the page's own segments, or a stream
    /// could have two.
    #[test]
    fn globals_draw_on_the_same_budget_as_the_page() {
        let each = (16 + ROW_COST) * 16;
        let globals = empty_region_segment(0, 16, 16);
        let stream = empty_region_segment(1, 16, 16);

        let mut budget = Budget::with_limit(each * 2);
        assert!(decode_embedded_within(&globals, &stream, 16, 16, &mut budget).is_ok());

        let mut budget = Budget::with_limit(each * 2 - 1);
        assert_eq!(
            decode_embedded_within(&globals, &stream, 16, 16, &mut budget),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// The page information segment of a fixture, sized `page`, with every
    /// flag clear.
    fn page_info_segment(number: u32, page: (u32, u32)) -> Vec<u8> {
        let mut info = Vec::new();
        info.extend_from_slice(&page.0.to_be_bytes());
        info.extend_from_slice(&page.1.to_be_bytes());
        info.extend_from_slice(&0u32.to_be_bytes()); // x resolution
        info.extend_from_slice(&0u32.to_be_bytes()); // y resolution
        info.push(0); // default pixel 0, default operator OR
        info.extend_from_slice(&0u16.to_be_bytes()); // striping
        let mut out = header(number, 48, &[], 1, info.len() as u32);
        out.extend_from_slice(&info);
        out
    }

    /// The shape of a scanned page: page information, one symbol dictionary,
    /// then a text region whose header names the dictionary's segment number,
    /// which is the only way it finds its symbols (T.88 7.4.4.2).
    fn symbol_coded_stream(page: (u32, u32), symbols: &[Bitmap], ops: &[Op]) -> Vec<u8> {
        let mut out = page_info_segment(0, page);

        let dict = dictionary_segment(symbols, 0);
        out.extend_from_slice(&header(1, 0, &[], 1, dict.len() as u32));
        out.extend_from_slice(&dict);

        let region = text_segment_for_page(page, symbols.len() as u32, ops);
        out.extend_from_slice(&header(2, 6, &[1], 1, region.len() as u32));
        out.extend_from_slice(&region);

        out.extend_from_slice(&header(3, 49, &[], 1, 0)); // end of page
        out
    }

    #[test]
    fn decodes_a_symbol_coded_page_end_to_end() {
        let symbols = vec![
            glyph(&["101", "010", "101", "010"]),
            glyph(&["11111", "10001", "10001", "11111"]),
        ];
        let stream = symbol_coded_stream(
            (32, 24),
            &symbols,
            &[
                Op::Strip(2),
                Op::First(1, 0),
                Op::Next(2, 1),
                Op::EndStrip,
                Op::Strip(8),
                Op::First(-1, 0),
                Op::EndStrip,
            ],
        );
        let page = decode_embedded(&[], &stream, 32, 24).expect("page");
        expect_at(&page, &symbols[0], 1, 2);
        expect_at(&page, &symbols[1], 5, 2);
        expect_at(&page, &symbols[0], 0, 10);
        assert_eq!(
            page.get(31, 23),
            0,
            "nothing painted outside the placements"
        );
    }

    /// A dictionary carried in `/JBIG2Globals` must reach a text region in the
    /// page's own stream (Annex D.3): the symbol store spans both walks.
    #[test]
    fn a_text_region_finds_symbols_in_the_globals_stream() {
        let symbols = vec![glyph(&["11", "11"])];
        let full = symbol_coded_stream(
            (16, 16),
            &symbols,
            &[Op::Strip(0), Op::First(3, 0), Op::EndStrip],
        );
        // Everything up to and including the dictionary becomes the globals.
        let (globals, rest) = full.split_at(split_after_segment(&full, 1));
        let page = decode_embedded(globals, rest, 16, 16).expect("page");
        expect_at(&page, &symbols[0], 3, 0);
    }

    /// A text region naming a segment number the stream does not contain has
    /// no symbols, and must say so rather than painting nothing.
    #[test]
    fn a_text_region_with_a_dangling_reference_is_rejected() {
        let region = text_segment_for_page((8, 8), 1, &[Op::Strip(0), Op::First(0, 0)]);
        // Segment 1 refers to segment 0, which this stream does not hold: a
        // well-formed header pointing at nothing.
        let mut stream = header(1, 6, &[0], 1, region.len() as u32);
        stream.extend_from_slice(&region);
        assert_eq!(
            decode_embedded(&[], &stream, 8, 8),
            Err(Jbig2Error::Malformed(
                "referred-to segment is not a symbol dictionary",
            )),
        );
    }

    /// A referred-to segment that exists but carries no symbols is the same
    /// error: the text region would otherwise silently place nothing.
    #[test]
    fn a_text_region_referring_to_a_non_dictionary_is_rejected() {
        let mut stream = page_info_segment(0, (8, 8));
        let region = text_segment_for_page((8, 8), 1, &[Op::Strip(0), Op::First(0, 0)]);
        stream.extend_from_slice(&header(1, 6, &[0], 1, region.len() as u32));
        stream.extend_from_slice(&region);
        assert_eq!(
            decode_embedded(&[], &stream, 8, 8),
            Err(Jbig2Error::Malformed(
                "referred-to segment is not a symbol dictionary",
            )),
        );
    }

    /// A text region that names no segment at all has no symbol list to build
    /// from, which the text region decoder rejects by name.
    #[test]
    fn a_text_region_referring_to_nothing_is_rejected() {
        let region = text_segment_for_page((8, 8), 1, &[Op::Strip(0), Op::First(0, 0)]);
        let mut stream = header(0, 6, &[], 1, region.len() as u32);
        stream.extend_from_slice(&region);
        assert_eq!(
            decode_embedded(&[], &stream, 8, 8),
            Err(Jbig2Error::Malformed("text region with no symbols")),
        );
    }

    /// A referred-to list may name the same dictionary as often as it likes,
    /// and the segment format allows tens of thousands of entries in four bytes
    /// each — so the gathered list is capped before it is built, not after.
    ///
    /// Eight symbols repeated 8 193 times is 65 544, one dictionary's worth
    /// past the limit. The same header shape at the format's own maximum would
    /// ask for billions of references from a few hundred kilobytes of input.
    #[test]
    fn a_repeated_referred_to_segment_cannot_multiply_the_symbol_list() {
        let exported: Vec<Bitmap> = (0..8).map(|_| glyph(&["1"])).collect();
        let store = HashMap::from([(1u32, exported)]);

        let within = vec![1u32; 8_192];
        assert_eq!(gather_symbols(&store, &within).map(|v| v.len()), Ok(65_536));

        let beyond = vec![1u32; 8_193];
        assert_eq!(
            gather_symbols(&store, &beyond).map(|v| v.len()),
            Err(Jbig2Error::Malformed("symbol count exceeds the limit")),
        );
    }

    /// Capping the gathered list bounds how many symbols a single dictionary
    /// sees, not how many bitmaps a stream ends up holding: every symbol a
    /// dictionary re-exports is a copy, and the copies are what the store keeps.
    ///
    /// So the copies are charged one by one. Here one 4 x 4 symbol is decoded
    /// and then named four times over by a segment that codes nothing at all,
    /// and the stream pays five symbols' worth — which is what stops the same
    /// shape at the referred-to list's own maximum from turning one bitmap into
    /// 65 536 of them.
    #[test]
    fn naming_one_dictionary_repeatedly_cannot_multiply_its_symbols() {
        let symbol = glyph(&["1010", "0101", "1010", "0101"]);
        let source = dictionary_segment(std::slice::from_ref(&symbol), 0);
        let copier = reexport_segment(4);

        let mut stream = header(1, 0, &[], 1, source.len() as u32);
        stream.extend_from_slice(&source);
        stream.extend_from_slice(&header(2, 0, &[1, 1, 1, 1], 1, copier.len() as u32));
        stream.extend_from_slice(&copier);

        // The original, then four copies of it, at the same price each.
        let total = (SYMBOL_COST + (4 + ROW_COST) * 4) * 5;

        let mut budget = Budget::with_limit(total);
        assert!(decode_embedded_within(&[], &stream, 8, 8, &mut budget).is_ok());

        let mut budget = Budget::with_limit(total - 1);
        assert_eq!(
            decode_embedded_within(&[], &stream, 8, 8, &mut budget),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// The same bound one level up, for the cheapest symbols there are: a
    /// dictionary of rowless symbols costs almost nothing to write and nothing
    /// at all to decode, and Annex D.3 puts no limit on how many segments follow
    /// one another, so without a per-symbol charge a stream's cost would grow
    /// with its length rather than stopping at the allowance.
    #[test]
    fn a_stream_of_rowless_dictionaries_runs_out_of_budget() {
        let dict = rowless_dictionary_segment(64);
        let mut stream = Vec::new();
        for number in 0..8u32 {
            stream.extend_from_slice(&header(number, 0, &[], 1, dict.len() as u32));
            stream.extend_from_slice(&dict);
        }

        let each = SYMBOL_COST * 64;

        let mut budget = Budget::with_limit(each * 8);
        assert!(decode_embedded_within(&[], &stream, 8, 8, &mut budget).is_ok());

        let mut budget = Budget::with_limit(each * 8 - 1);
        assert_eq!(
            decode_embedded_within(&[], &stream, 8, 8, &mut budget),
            Err(Jbig2Error::WorkLimit),
        );
    }

    /// A dictionary's own referred-to list supplies its input symbols
    /// (7.4.3.1.7), which it may re-export — so a text region naming only the
    /// second dictionary still reaches the first one's symbol.
    #[test]
    fn a_dictionary_re_exports_symbols_from_the_dictionary_it_refers_to() {
        let first = [glyph(&["11", "11"])];
        let second = glyph(&["10", "01"]);

        let mut stream = page_info_segment(0, (16, 16));

        let dict_one = dictionary_segment(&first, 0);
        stream.extend_from_slice(&header(1, 0, &[], 1, dict_one.len() as u32));
        stream.extend_from_slice(&dict_one);

        // The second dictionary takes the first's export as its input and
        // exports both, which `dictionary_segment` cannot express — its runs
        // always skip the inputs — so this one is built here.
        let dict_two = re_exporting_dictionary(&second);
        stream.extend_from_slice(&header(2, 0, &[1], 1, dict_two.len() as u32));
        stream.extend_from_slice(&dict_two);

        let region = text_segment_for_page(
            (16, 16),
            2,
            &[Op::Strip(0), Op::First(0, 0), Op::Next(2, 1), Op::EndStrip],
        );
        stream.extend_from_slice(&header(3, 6, &[2], 1, region.len() as u32));
        stream.extend_from_slice(&region);

        let page = decode_embedded(&[], &stream, 16, 16).expect("page");
        expect_at(&page, &first[0], 0, 0);
        expect_at(&page, &second, 3, 0);
    }

    /// Symbols arrive in referred-to order, not in segment-number order: a
    /// text region naming its dictionaries the other way round indexes the
    /// other one's symbol first.
    #[test]
    fn symbols_are_concatenated_in_referred_to_order() {
        let wide = vec![glyph(&["1111", "1111"])];
        let narrow = vec![glyph(&["11", "11"])];

        // Symbol id 0 must resolve to the *narrow* glyph, which lives in the
        // dictionary named second by segment number and first by reference.
        let mut stream = page_info_segment(0, (16, 16));
        let dict_wide = dictionary_segment(&wide, 0);
        stream.extend_from_slice(&header(1, 0, &[], 1, dict_wide.len() as u32));
        stream.extend_from_slice(&dict_wide);
        let dict_narrow = dictionary_segment(&narrow, 0);
        stream.extend_from_slice(&header(2, 0, &[], 1, dict_narrow.len() as u32));
        stream.extend_from_slice(&dict_narrow);

        let region = text_segment_for_page((16, 16), 2, &[Op::Strip(0), Op::First(0, 0)]);
        stream.extend_from_slice(&header(3, 6, &[2, 1], 1, region.len() as u32));
        stream.extend_from_slice(&region);

        let page = decode_embedded(&[], &stream, 16, 16).expect("page");
        expect_at(&page, &narrow[0], 0, 0);
        assert_eq!(page.get(2, 0), 0, "the wide glyph was not the one placed");
    }

    /// A lossless text region (type 7) composites exactly like an immediate
    /// one; only the encoder's promise about fidelity differs.
    #[test]
    fn a_lossless_text_region_paints_too() {
        let symbols = vec![glyph(&["11", "11"])];
        let mut stream = page_info_segment(0, (8, 8));
        let dict = dictionary_segment(&symbols, 0);
        stream.extend_from_slice(&header(1, 0, &[], 1, dict.len() as u32));
        stream.extend_from_slice(&dict);
        let region = text_segment_for_page((8, 8), 1, &[Op::Strip(1), Op::First(2, 0)]);
        stream.extend_from_slice(&header(2, 7, &[1], 1, region.len() as u32));
        stream.extend_from_slice(&region);

        let page = decode_embedded(&[], &stream, 8, 8).expect("page");
        expect_at(&page, &symbols[0], 2, 1);
    }

    /// A symbol dictionary whose export runs re-export its one input symbol
    /// alongside its one new symbol (6.5.10): a zero-length skip run flips the
    /// flag, then a run of two covers both.
    fn re_exporting_dictionary(new: &Bitmap) -> Vec<u8> {
        use crate::filters::jbig2::arith_int::encoder::encode_int;
        use crate::filters::jbig2::arith_int::IntCtxSet;
        use crate::filters::jbig2::testing::nominal_at_bytes;

        let params = GenericParams::nominal(0);
        let mut enc = MqEncoder::new();
        let mut ints = IntCtxSet::new();
        let mut gb = vec![MqContext::default(); GB_CONTEXT_LEN];

        encode_int(&mut enc, &mut ints.iadh, Some(new.height() as i32));
        encode_int(&mut enc, &mut ints.iadw, Some(new.width() as i32));
        for y in 0..new.height() {
            for x in 0..new.width() {
                let ctx = usize::from(context_at(new, x, y, &params));
                enc.encode(&mut gb[ctx], new.get(i64::from(x), i64::from(y)));
            }
        }
        encode_int(&mut enc, &mut ints.iadw, None);
        encode_int(&mut enc, &mut ints.iaex, Some(0)); // zero-length skip
        encode_int(&mut enc, &mut ints.iaex, Some(2)); // export both

        let mut out = 0u16.to_be_bytes().to_vec(); // arithmetic, template 0
        out.extend_from_slice(&nominal_at_bytes());
        out.extend_from_slice(&2u32.to_be_bytes()); // SDNUMEXSYMS
        out.extend_from_slice(&1u32.to_be_bytes()); // SDNUMNEWSYMS
        out.extend_from_slice(&enc.finish());
        out
    }
}
