//! Segment headers (T.88 7.2), the segment type table (7.3), and the
//! PDF-embedded segment sequence of Annex D.3.
//!
//! A `JBIG2Decode` stream is not a JBIG2 file: there is no file header, no
//! page count and no random-access index. Annex D.3 defines it as the bare
//! *embedded stream format*, a flat run of `[header][data][header][data]...`
//! to the end of the stream. Splitting that run into segments is all this
//! module does; interpreting a segment's data belongs to the decoder for its
//! type.
//!
//! Every field here is read from a PDF stream, so every field is bounds-checked
//! and every count is capped before it sizes an allocation.

use super::reader::Reader;
use super::Jbig2Error;

/// The largest referred-to segment count this parser will accept (T.88 7.2.4).
///
/// The long form of the count is a 29-bit field, so a hostile stream can claim
/// half a billion referrals in eleven bytes. No real stream refers to more
/// segments than it contains, and a segment number is itself capped by the
/// four-byte field it lives in, but the parser cannot know the stream's segment
/// count before it has read the stream. A flat ceiling is what stands between a
/// short header and a gigabyte of `Vec`.
pub(crate) const MAX_REFERRED_TO: u32 = 65_536;

/// The fixed part of a region segment's data (T.88 7.4.1): width, height, X, Y
/// and the external combination operator flags.
///
/// Only the unknown-length scan needs it here — it has to step over the block
/// to reach the generic region flags — so the length is all this module knows
/// about it.
const REGION_INFO_LEN: usize = 17;

/// The kind of a segment, from the type field of T.88 7.3, Table 34.
///
/// "Immediate" means the region composites straight onto the page;
/// "intermediate" means it is retained in an auxiliary buffer for a later
/// segment to refer to. "Lossless" is a fidelity assertion by the encoder with
/// no effect on decoding, so the lossless variants decode exactly like their
/// plain counterparts and are kept distinct only to round-trip the type field.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SegmentKind {
    /// Type 0: a dictionary of symbol bitmaps for text regions to draw from.
    SymbolDictionary,
    /// Type 4: a text region retained for later reference.
    IntermediateTextRegion,
    /// Type 6: a text region composited onto the page.
    ImmediateTextRegion,
    /// Type 7: an immediate text region the encoder asserts is lossless.
    ImmediateLosslessTextRegion,
    /// Type 16: a dictionary of halftone patterns.
    PatternDictionary,
    /// Type 20: a halftone region retained for later reference.
    IntermediateHalftoneRegion,
    /// Type 22: a halftone region composited onto the page.
    ImmediateHalftoneRegion,
    /// Type 23: an immediate halftone region the encoder asserts is lossless.
    ImmediateLosslessHalftoneRegion,
    /// Type 36: a generic region retained for later reference.
    IntermediateGenericRegion,
    /// Type 38: a generic region composited onto the page.
    ImmediateGenericRegion,
    /// Type 39: an immediate generic region the encoder asserts is lossless.
    ImmediateLosslessGenericRegion,
    /// Type 40: a refinement region retained for later reference.
    IntermediateRefinementRegion,
    /// Type 42: a refinement region composited onto the page.
    ImmediateRefinementRegion,
    /// Type 43: an immediate refinement region the encoder asserts is lossless.
    ImmediateLosslessRefinementRegion,
    /// Type 48: the page's dimensions, resolution and default pixel value.
    PageInfo,
    /// Type 49: the end of a page.
    EndOfPage,
    /// Type 50: the Y coordinate of a stripe's last row.
    EndOfStripe,
    /// Type 51: the end of the file.
    EndOfFile,
    /// Type 52: a profile declaration, carrying no pixels.
    Profiles,
    /// Type 53: a custom Huffman table.
    Tables,
    /// Type 62: a vendor extension, which a decoder may skip when the segment
    /// is not marked necessary.
    Extension,
}

impl SegmentKind {
    /// Decodes the six-bit type field of the segment header flags (T.88 7.3).
    ///
    /// The caller masks the flags byte down to bits 0 to 5 first; the page
    /// association size and deferred-non-retain bits live above them. Values
    /// outside Table 34 are rejected rather than skipped: a segment this
    /// decoder cannot name is a segment whose length it cannot trust to mean
    /// what it says.
    pub(crate) fn from_bits(bits: u8) -> Result<SegmentKind, Jbig2Error> {
        match bits {
            0 => Ok(SegmentKind::SymbolDictionary),
            4 => Ok(SegmentKind::IntermediateTextRegion),
            6 => Ok(SegmentKind::ImmediateTextRegion),
            7 => Ok(SegmentKind::ImmediateLosslessTextRegion),
            16 => Ok(SegmentKind::PatternDictionary),
            20 => Ok(SegmentKind::IntermediateHalftoneRegion),
            22 => Ok(SegmentKind::ImmediateHalftoneRegion),
            23 => Ok(SegmentKind::ImmediateLosslessHalftoneRegion),
            36 => Ok(SegmentKind::IntermediateGenericRegion),
            38 => Ok(SegmentKind::ImmediateGenericRegion),
            39 => Ok(SegmentKind::ImmediateLosslessGenericRegion),
            40 => Ok(SegmentKind::IntermediateRefinementRegion),
            42 => Ok(SegmentKind::ImmediateRefinementRegion),
            43 => Ok(SegmentKind::ImmediateLosslessRefinementRegion),
            48 => Ok(SegmentKind::PageInfo),
            49 => Ok(SegmentKind::EndOfPage),
            50 => Ok(SegmentKind::EndOfStripe),
            51 => Ok(SegmentKind::EndOfFile),
            52 => Ok(SegmentKind::Profiles),
            53 => Ok(SegmentKind::Tables),
            62 => Ok(SegmentKind::Extension),
            _ => Err(Jbig2Error::Malformed("unknown segment type")),
        }
    }
}

/// A parsed segment header (T.88 7.2).
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SegmentHeader {
    /// The segment number (7.2.2). Numbers increase within a stream but are
    /// not required to start at zero or to be contiguous.
    pub(crate) number: u32,
    /// The segment type (7.2.3 bits 0 to 5, decoded per 7.3).
    pub(crate) kind: SegmentKind,
    /// The numbers of the segments this one refers to (7.2.5).
    pub(crate) referred_to: Vec<u32>,
    /// The page association (7.2.6). In an embedded stream this is always 1;
    /// the value is carried through but nothing depends on it.
    pub(crate) page: u32,
    /// The data length (7.2.7). `None` is the `0xFFFF_FFFF` unknown-length
    /// form, whose extent is recovered by scanning for the terminator.
    pub(crate) data_len: Option<u32>,
}

/// A segment header together with the bytes it governs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Segment<'a> {
    /// The parsed header.
    pub(crate) header: SegmentHeader,
    /// The segment's data, borrowed from the input.
    pub(crate) data: &'a [u8],
}

/// Parses one segment header from `r` (T.88 7.2).
///
/// On success the cursor sits on the first byte of the segment's data. Fields
/// are read in the order the standard gives them: segment number, flags,
/// referred-to count and retain flags, referred-to numbers, page association,
/// data length.
pub(crate) fn parse_header(r: &mut Reader<'_>) -> Result<SegmentHeader, Jbig2Error> {
    // 7.2.2 — segment number.
    let number = r.u32()?;

    // 7.2.3 — flags. Bits 0 to 5 are the type, bit 6 the page association
    // size, bit 7 the deferred-non-retain hint, which does not affect decoding.
    let flags = r.u8()?;
    let kind = SegmentKind::from_bits(flags & 0x3F)?;
    let long_page_association = flags & 0x40 != 0;

    // 7.2.4 — referred-to count. The top three bits of the first byte pick
    // the form: 0..=4 is the count itself, 7 means the count is in the low 29
    // bits of a four-byte field starting at that same byte. 5 and 6 are
    // reserved.
    let first = r.u8()?;
    let count = if (first >> 5) == 7 {
        // Re-read the same byte as the first of four.
        r.seek_back(1)?;
        let long = r.u32()? & 0x1FFF_FFFF;
        if long > MAX_REFERRED_TO {
            return Err(Jbig2Error::Malformed("referred-to segment count too large"));
        }
        // ceil((long + 1) / 8) retain-flag bytes follow, one bit per
        // referred-to segment plus one for the segment itself.
        let retain_bytes = usize::try_from(long / 8 + 1).map_err(|_| Jbig2Error::Truncated)?;
        r.take(retain_bytes)?;
        long
    } else if (first >> 5) <= 4 {
        // The low five bits are the retain flags, which this decoder ignores:
        // it holds every segment for the life of the stream.
        u32::from(first >> 5)
    } else {
        return Err(Jbig2Error::Malformed("reserved referred-to count form"));
    };

    // 7.2.5 — the width of each referred-to number is set by *this* segment's
    // number, not by the values being referred to.
    let ref_width = if number <= 256 {
        1
    } else if number <= 65_536 {
        2
    } else {
        4
    };
    // `count` is capped at MAX_REFERRED_TO above, but a stream that claims the
    // cap must still supply the bytes; reserving no more than remain keeps a
    // short header from reserving for a long one.
    let capacity = usize::try_from(count)
        .unwrap_or(usize::MAX)
        .min(r.remaining());
    let mut referred_to = Vec::with_capacity(capacity);
    for _ in 0..count {
        let number = match ref_width {
            1 => u32::from(r.u8()?),
            2 => u32::from(r.u16()?),
            _ => r.u32()?,
        };
        referred_to.push(number);
    }

    // 7.2.6 — page association, one byte or four per flags bit 6.
    let page = if long_page_association {
        r.u32()?
    } else {
        u32::from(r.u8()?)
    };

    // 7.2.7 — data length. 0xFFFF_FFFF means the length is not stated and the
    // data must be scanned for its terminator.
    let raw_len = r.u32()?;
    let data_len = if raw_len == u32::MAX {
        None
    } else {
        Some(raw_len)
    };

    Ok(SegmentHeader {
        number,
        kind,
        referred_to,
        page,
        data_len,
    })
}

/// Locates the end of an immediate generic region segment whose header
/// declared an unknown data length (T.88 7.2.7).
///
/// The data runs to a two-byte terminator — `FF AC` for arithmetic coding,
/// `00 00` for MMR — followed by a four-byte row count. The encoder is
/// required to keep that sequence out of the coded data, so the first match
/// after the AT pixels is the real end.
///
/// The scan starts past the region information field and the AT pixels
/// precisely because those *are* allowed to contain the terminator bytes: an
/// AT offset of −1 is the byte `0xFF`, and a region 172 pixels wide puts `0xAC`
/// in its width field.
fn unknown_length_extent(kind: SegmentKind, data: &[u8]) -> Result<usize, Jbig2Error> {
    if kind != SegmentKind::ImmediateGenericRegion {
        return Err(Jbig2Error::Malformed(
            "unknown length on a non-generic segment",
        ));
    }

    // 7.4.6.1 — the generic region flags sit immediately after the region
    // information field.
    let flags = *data.get(REGION_INFO_LEN).ok_or(Jbig2Error::Truncated)?;
    let mmr = flags & 1;
    let template = (flags >> 1) & 3;

    // 7.4.6.2 — AT pixels are present only for arithmetic coding: four pairs
    // of signed bytes for template 0, one pair otherwise.
    let at_len = match (mmr, template) {
        (1, _) => 0,
        (_, 0) => 8,
        _ => 2,
    };
    let start = REGION_INFO_LEN + 1 + at_len;
    let terminator: [u8; 2] = if mmr == 1 { [0x00, 0x00] } else { [0xFF, 0xAC] };

    let tail = data.get(start..).ok_or(Jbig2Error::Truncated)?;
    let offset = tail
        .windows(2)
        .position(|pair| pair == terminator)
        .ok_or(Jbig2Error::Truncated)?;

    // The terminator itself plus the four-byte row count that follows it.
    let extent = start
        .checked_add(offset)
        .and_then(|end| end.checked_add(2 + 4))
        .ok_or(Jbig2Error::Truncated)?;
    if extent > data.len() {
        return Err(Jbig2Error::Truncated);
    }
    Ok(extent)
}

/// Splits a PDF-embedded JBIG2 stream into its segments (T.88 Annex D.3).
///
/// The stream is a flat `[header][data]` run with no index and no trailer, so
/// the only way to find segment *n* is to have parsed segments 0 through
/// *n − 1*. Each iteration consumes at least the eleven bytes of a minimal
/// header, which is what makes the loop terminate regardless of what the data
/// says.
///
/// The same function parses a `/JBIG2Globals` stream, which has the identical
/// shape and holds segments shared between pages.
pub(crate) fn parse_embedded(data: &[u8]) -> Result<Vec<Segment<'_>>, Jbig2Error> {
    let mut r = Reader::new(data);
    let mut segments = Vec::new();
    while !r.is_empty() {
        let header = parse_header(&mut r)?;
        let len = match header.data_len {
            Some(len) => usize::try_from(len).map_err(|_| Jbig2Error::Truncated)?,
            None => unknown_length_extent(header.kind, r.rest())?,
        };
        let data = r.take(len)?;
        segments.push(Segment { header, data });
    }
    Ok(segments)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a short-form header: segment number, flags, one referred-to
    /// byte, page association, data length.
    fn short_header(number: u32, kind: u8, refs: &[u8], page: u8, len: u32) -> Vec<u8> {
        let mut out = number.to_be_bytes().to_vec();
        out.push(kind); // page association size 0 -> 1 byte
        out.push((refs.len() as u8) << 5);
        out.extend_from_slice(refs);
        out.push(page);
        out.extend_from_slice(&len.to_be_bytes());
        out
    }

    #[test]
    fn parses_a_minimal_header() {
        let bytes = short_header(1, 48, &[], 1, 19);
        let mut r = Reader::new(&bytes);
        let h = parse_header(&mut r).expect("header");
        assert_eq!(h.number, 1);
        assert_eq!(h.kind, SegmentKind::PageInfo);
        assert!(h.referred_to.is_empty());
        assert_eq!(h.page, 1);
        assert_eq!(h.data_len, Some(19));
        assert!(r.is_empty());
    }

    #[test]
    fn parses_referred_to_segments() {
        let bytes = short_header(3, 6, &[1, 2], 1, 0);
        let mut r = Reader::new(&bytes);
        let h = parse_header(&mut r).expect("header");
        assert_eq!(h.referred_to, vec![1, 2]);
        assert_eq!(h.kind, SegmentKind::ImmediateTextRegion);
    }

    /// Referred-to numbers widen with this segment's own number (7.2.5).
    #[test]
    fn referred_to_field_width_follows_the_segment_number() {
        // Segment 300 > 256, so each referred-to number is two bytes.
        let mut bytes = 300u32.to_be_bytes().to_vec();
        bytes.push(38);
        bytes.push(1 << 5); // one referred-to segment
        bytes.extend_from_slice(&[0x01, 0x2C]); // 300, as u16
        bytes.push(1);
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let mut r = Reader::new(&bytes);
        let h = parse_header(&mut r).expect("header");
        assert_eq!(h.referred_to, vec![300]);

        // Segment 70000 > 65536, so each referred-to number is four bytes.
        let mut bytes = 70_000u32.to_be_bytes().to_vec();
        bytes.push(38);
        bytes.push(1 << 5);
        bytes.extend_from_slice(&69_999u32.to_be_bytes());
        bytes.push(1);
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let mut r = Reader::new(&bytes);
        let h = parse_header(&mut r).expect("header");
        assert_eq!(h.referred_to, vec![69_999]);
    }

    /// The width is set by the *referring* segment's number even when the
    /// values it refers to would fit in fewer bytes: segment 70000 referring
    /// to segment 1 still spends four bytes on the reference. Reading it as
    /// one byte would consume `0x00` and desynchronise every later field.
    #[test]
    fn referred_to_width_ignores_the_referred_to_values() {
        let mut bytes = 70_000u32.to_be_bytes().to_vec();
        bytes.push(38);
        bytes.push(1 << 5);
        bytes.extend_from_slice(&1u32.to_be_bytes());
        bytes.push(1);
        bytes.extend_from_slice(&5u32.to_be_bytes());
        let mut r = Reader::new(&bytes);
        let h = parse_header(&mut r).expect("header");
        assert_eq!(h.referred_to, vec![1]);
        assert_eq!(h.page, 1);
        assert_eq!(h.data_len, Some(5));
        assert!(r.is_empty());
    }

    /// The long form: top three bits are 7, count lives in the low 29 bits of
    /// a four-byte field, followed by ceil((count + 1) / 8) retain bytes.
    #[test]
    fn parses_the_long_form_referred_to_count() {
        let count = 6u32; // > 4, so the short form cannot express it
        let mut bytes = 1u32.to_be_bytes().to_vec();
        bytes.push(38);
        bytes.extend_from_slice(&(0xE000_0000u32 | count).to_be_bytes());
        bytes.extend_from_slice(&[0u8; 1]); // (6 + 8) / 8 = 1 retain byte
        bytes.extend_from_slice(&[1, 2, 3, 4, 5, 6]); // six 1-byte numbers
        bytes.push(1);
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let mut r = Reader::new(&bytes);
        let h = parse_header(&mut r).expect("header");
        assert_eq!(h.referred_to, vec![1, 2, 3, 4, 5, 6]);
    }

    #[test]
    fn rejects_the_reserved_referred_to_counts() {
        for bad in [5u8, 6] {
            let mut bytes = short_header(1, 38, &[], 1, 0);
            bytes[5] = bad << 5; // the referred-to count byte
            let mut r = Reader::new(&bytes);
            assert!(
                parse_header(&mut r).is_err(),
                "count {bad} must be rejected"
            );
        }
    }

    /// A 29-bit count field can claim half a billion referrals. Refuse before
    /// reserving a vector for them.
    #[test]
    fn rejects_an_absurd_referred_to_count() {
        let mut bytes = 1u32.to_be_bytes().to_vec();
        bytes.push(38);
        bytes.extend_from_slice(&0xFFFF_FFFFu32.to_be_bytes());
        let mut r = Reader::new(&bytes);
        assert_eq!(
            parse_header(&mut r),
            Err(Jbig2Error::Malformed("referred-to segment count too large")),
        );
    }

    #[test]
    fn parses_a_four_byte_page_association() {
        let mut bytes = 1u32.to_be_bytes().to_vec();
        bytes.push(38 | 0x40); // flags bit 6 set
        bytes.push(0);
        bytes.extend_from_slice(&7u32.to_be_bytes());
        bytes.extend_from_slice(&0u32.to_be_bytes());
        let mut r = Reader::new(&bytes);
        let h = parse_header(&mut r).expect("header");
        assert_eq!(h.page, 7);
    }

    #[test]
    fn unknown_data_length_becomes_none() {
        let bytes = short_header(1, 38, &[], 1, 0xFFFF_FFFF);
        let mut r = Reader::new(&bytes);
        assert_eq!(parse_header(&mut r).expect("header").data_len, None);
    }

    #[test]
    fn rejects_unknown_segment_types() {
        for bad in [1u8, 2, 3, 5, 8, 17, 37, 63] {
            let bytes = short_header(1, bad, &[], 1, 0);
            let mut r = Reader::new(&bytes);
            assert!(parse_header(&mut r).is_err(), "type {bad} must be rejected");
        }
    }

    #[test]
    fn splits_an_embedded_stream_into_segments() {
        let mut stream = short_header(0, 48, &[], 1, 3);
        stream.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        stream.extend_from_slice(&short_header(1, 51, &[], 1, 0));
        let segs = parse_embedded(&stream).expect("segments");
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].header.kind, SegmentKind::PageInfo);
        assert_eq!(segs[0].data, &[0xAA, 0xBB, 0xCC]);
        assert_eq!(segs[1].header.kind, SegmentKind::EndOfFile);
        assert!(segs[1].data.is_empty());
    }

    #[test]
    fn a_declared_length_past_the_end_is_truncated() {
        let mut stream = short_header(0, 48, &[], 1, 99);
        stream.extend_from_slice(&[0xAA]);
        assert_eq!(parse_embedded(&stream), Err(Jbig2Error::Truncated));
    }

    #[test]
    fn an_empty_stream_yields_no_segments() {
        assert_eq!(parse_embedded(&[]).expect("segments").len(), 0);
    }

    /// Truncation anywhere in the header sequence must error, never panic.
    #[test]
    fn every_truncation_point_errors_cleanly() {
        let mut stream = short_header(0, 48, &[], 1, 3);
        stream.extend_from_slice(&[0xAA, 0xBB, 0xCC]);
        for cut in 0..stream.len() {
            let _ = parse_embedded(&stream[..cut]);
        }
    }

    #[test]
    fn unknown_length_scans_to_the_terminator() {
        let mut seg = short_header(1, 38, &[], 1, 0xFFFF_FFFF);
        // Region info: 4x1 at (0, 0), operator OR.
        seg.extend_from_slice(&4u32.to_be_bytes());
        seg.extend_from_slice(&1u32.to_be_bytes());
        seg.extend_from_slice(&0u32.to_be_bytes());
        seg.extend_from_slice(&0u32.to_be_bytes());
        seg.push(0);
        seg.push(0); // flags: MMR 0, template 0, no TPGDON
        seg.extend_from_slice(&[3, 255, 253, 255, 2, 254, 254, 254]); // nominal AT
        seg.extend_from_slice(&[0x12, 0x34]); // coded data
        seg.extend_from_slice(&[0xFF, 0xAC]); // terminator
        seg.extend_from_slice(&9u32.to_be_bytes()); // row count
        seg.extend_from_slice(&short_header(2, 51, &[], 1, 0));

        let segs = parse_embedded(&seg).expect("segments");
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[1].header.kind, SegmentKind::EndOfFile);
        assert_eq!(segs[0].data.len(), 17 + 1 + 8 + 2 + 2 + 4);
    }

    #[test]
    fn unknown_length_without_a_terminator_is_truncated() {
        let mut seg = short_header(1, 38, &[], 1, 0xFFFF_FFFF);
        seg.extend_from_slice(&[0u8; 17 + 1 + 8]);
        seg.extend_from_slice(&[0x12, 0x34]);
        assert_eq!(parse_embedded(&seg), Err(Jbig2Error::Truncated));
    }

    #[test]
    fn unknown_length_is_rejected_on_other_segment_types() {
        let seg = short_header(1, 0, &[], 1, 0xFFFF_FFFF);
        assert!(parse_embedded(&seg).is_err());
    }

    /// With MMR set the terminator is `00 00`, and there are no AT bytes to
    /// step over. A scan that looked for `FF AC` regardless, or that skipped
    /// eight bytes that are not there, would land in the wrong place.
    #[test]
    fn unknown_length_uses_the_mmr_terminator() {
        let mut seg = short_header(1, 38, &[], 1, 0xFFFF_FFFF);
        seg.extend_from_slice(&[0u8; 17]); // region info
        seg.push(0x01); // flags: MMR 1
        seg.extend_from_slice(&[0x12, 0x34, 0xFF, 0xAC]); // coded data, not a terminator here
        seg.extend_from_slice(&[0x00, 0x00]); // terminator
        seg.extend_from_slice(&3u32.to_be_bytes()); // row count
        seg.extend_from_slice(&short_header(2, 51, &[], 1, 0));

        let segs = parse_embedded(&seg).expect("segments");
        assert_eq!(segs.len(), 2);
        assert_eq!(segs[0].data.len(), 17 + 1 + 4 + 2 + 4);
        assert_eq!(segs[1].header.kind, SegmentKind::EndOfFile);
    }

    /// The terminator's own bytes may appear inside the region information
    /// field and the AT pixels; the scan starts after them, so they cannot be
    /// mistaken for the end of the data.
    #[test]
    fn unknown_length_ignores_a_terminator_before_the_coded_data() {
        let mut seg = short_header(1, 38, &[], 1, 0xFFFF_FFFF);
        seg.extend_from_slice(&[0xFFu8; 17]); // region info, all 0xFF
        seg.push(0x00); // flags: MMR 0, template 0
        seg.extend_from_slice(&[0xFF, 0xAC, 0xFF, 0xAC, 0xFF, 0xAC, 0xFF, 0xAC]); // AT
        seg.extend_from_slice(&[0x12]); // coded data
        seg.extend_from_slice(&[0xFF, 0xAC]); // the real terminator
        seg.extend_from_slice(&7u32.to_be_bytes());

        let segs = parse_embedded(&seg).expect("segments");
        assert_eq!(segs.len(), 1);
        assert_eq!(segs[0].data.len(), 17 + 1 + 8 + 1 + 2 + 4);
    }

    /// A terminator found with fewer than four bytes left cannot carry its row
    /// count, so the segment is truncated rather than silently short.
    #[test]
    fn unknown_length_needs_room_for_the_row_count() {
        let mut seg = short_header(1, 38, &[], 1, 0xFFFF_FFFF);
        seg.extend_from_slice(&[0u8; 17 + 1 + 8]);
        seg.extend_from_slice(&[0xFF, 0xAC]);
        seg.extend_from_slice(&[0x00, 0x00, 0x00]); // one byte short of a row count
        assert_eq!(parse_embedded(&seg), Err(Jbig2Error::Truncated));
    }

    /// Every prefix of a stream carrying an unknown-length segment must error
    /// or parse, never panic.
    #[test]
    fn every_truncation_point_of_an_unknown_length_segment_errors_cleanly() {
        let mut seg = short_header(1, 38, &[], 1, 0xFFFF_FFFF);
        seg.extend_from_slice(&[0u8; 17 + 1 + 8]);
        seg.extend_from_slice(&[0x12, 0x34, 0xFF, 0xAC]);
        seg.extend_from_slice(&9u32.to_be_bytes());
        for cut in 0..seg.len() {
            let _ = parse_embedded(&seg[..cut]);
        }
    }

    /// Exactly the rows of the 7.3 table are accepted; every other value of
    /// the six-bit type field is rejected.
    #[test]
    fn segment_kind_covers_the_whole_type_field() {
        for bits in 0..=63u8 {
            let expected = matches!(
                bits,
                0 | 4 | 6 | 7 | 16 | 20 | 22 | 23 | 36 | 38..=40 | 42 | 43 | 48..=53 | 62
            );
            assert_eq!(
                SegmentKind::from_bits(bits).is_ok(),
                expected,
                "type {bits}"
            );
        }
    }
}
