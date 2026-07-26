//! Page assembly: turning a PDF-embedded segment stream into pixels
//! (T.88 7.4.8, 8.2, Annex D.3).
//!
//! One walk over the segment list is the whole procedure. A page information
//! segment sizes and pre-fills the page; each immediate generic region decodes
//! into its own bitmap and composites onto it at the coordinates its region
//! information field gives.
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

use super::bitmap::Bitmap;
use super::budget::Budget;
use super::generic::{decode_generic_region, parse_generic_flags, GB_CONTEXT_LEN};
use super::mq::{MqContexts, MqDecoder};
use super::reader::Reader;
use super::segment::{parse_embedded, parse_region_info, RegionInfo, Segment, SegmentKind};
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
#[allow(dead_code)] // The `JBIG2Decode` filter arm is wired up in a later change.
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
                // A location above `i32::MAX` cannot be represented as an
                // offset; clamping puts the region off the right or bottom
                // edge, where it clips away, rather than wrapping it negative
                // and painting it over the top-left corner.
                let x = i32::try_from(info.x).unwrap_or(i32::MAX);
                let y = i32::try_from(info.y).unwrap_or(i32::MAX);
                target.combine(&region, x, y, info.op);
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
            SegmentKind::SymbolDictionary => {
                return Err(Jbig2Error::Unimplemented("symbol dictionary"))
            }
            SegmentKind::IntermediateTextRegion
            | SegmentKind::ImmediateTextRegion
            | SegmentKind::ImmediateLosslessTextRegion => {
                return Err(Jbig2Error::Unimplemented("text region"))
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

/// Decodes one immediate generic region segment (T.88 7.4.6) into its own
/// bitmap, returning it alongside the region information field that says where
/// it goes.
///
/// Each generic region segment gets a fresh arithmetic decoder and a fresh
/// context array: unlike the symbol dictionary, which codes every symbol of a
/// height class through one shared array, a generic region segment's coded data
/// begins and ends within the segment.
fn decode_generic_region_segment(
    segment: &Segment<'_>,
    budget: &mut Budget,
) -> Result<(RegionInfo, Bitmap), Jbig2Error> {
    let mut r = Reader::new(segment.data);
    let info = parse_region_info(&mut r)?;
    let (mmr, params) = parse_generic_flags(&mut r)?;
    if mmr {
        return Err(Jbig2Error::Unimplemented("MMR coding"));
    }

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
    use crate::filters::jbig2::budget::ROW_COST;
    use crate::filters::jbig2::generic::{context_at, GenericParams, GB_CONTEXT_LEN};
    use crate::filters::jbig2::mq::{encoder::MqEncoder, MqContext};
    use crate::filters::jbig2::testing::header;

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
            (0u8, "symbol dictionary"),
            (6, "text region"),
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

    /// MMR coding lives in a later build and must say so rather than decoding
    /// noise.
    #[test]
    fn an_mmr_generic_region_is_an_unimplemented_error() {
        let mut region = Vec::new();
        region.extend_from_slice(&8u32.to_be_bytes());
        region.extend_from_slice(&8u32.to_be_bytes());
        region.extend_from_slice(&0u32.to_be_bytes());
        region.extend_from_slice(&0u32.to_be_bytes());
        region.push(0);
        region.push(1); // MMR
        let mut stream = header(0, 38, &[], 1, region.len() as u32);
        stream.extend_from_slice(&region);
        assert_eq!(
            decode_embedded(&[], &stream, 8, 8),
            Err(Jbig2Error::Unimplemented("MMR coding")),
        );
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

    /// A stream whose first content segment is a symbol dictionary — the shape
    /// of every page in a text-scanned document — reports the missing feature
    /// by name rather than rendering a blank page.
    #[test]
    fn a_symbol_coded_page_names_what_is_missing() {
        let mut stream = header(0, 48, &[], 1, 19);
        stream.extend_from_slice(&[0u8; 19]);
        stream.extend_from_slice(&header(1, 0, &[], 1, 0)); // symbol dictionary
        assert_eq!(
            decode_embedded(&[], &stream, 1994, 2832),
            Err(Jbig2Error::Unimplemented("symbol dictionary")),
        );
    }
}
