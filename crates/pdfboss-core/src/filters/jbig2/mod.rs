//! JBIG2 (ISO/IEC 14492 / ITU-T T.88) decoding.
//!
//! Implemented from the published standard. The entry point is
//! [`decode_pdf_stream`], reached from the `JBIG2Decode` arm of
//! [`crate::filters::decode_stream`], which returns packed 1-bit-per-pixel rows
//! so the image layer can treat the result exactly like any other
//! `/BitsPerComponent 1` `/DeviceGray` sample data.
//!
//! Layering, bottom-up: [`mq`] is the binary arithmetic decoder (Annex E);
//! [`arith_int`] builds the integer procedures on top of it (Annex A);
//! [`bitmap`] is the bilevel pixel buffer every region decodes into;
//! [`reader`] is the bounds-checked byte cursor the header parsers run on;
//! [`segment`] splits a PDF-embedded stream into the segments of clause 7;
//! [`generic`] decodes a region of pixels out of the arithmetic decoder, which
//! is the procedure every other region type is ultimately built from;
//! [`refinement`] decodes a region against a reference bitmap it is expected
//! to resemble (6.3); [`huffman`] is the prefix-code table machinery of
//! Annex B, which the Huffman variant of the format uses wherever the
//! arithmetic variant reaches for [`arith_int`]; [`symbol_dict`] and
//! [`text_region`] are the pair a scanned page of text is actually made of,
//! and each decodes both variants along with the refinement each may embed —
//! per placed instance in a text region (6.4.11), per coded symbol in a
//! dictionary (SDREFAGG, 6.5.8.2); [`halftone`] is the corresponding pair for
//! tone — a pattern dictionary and the halftone region that draws its
//! patterns over a grid (6.6, 6.7, with the gray-scale image coding of
//! Annex C between them); and [`page`] walks a segment sequence, compositing
//! each region onto the page.
//!
//! What that leaves undecoded is refused by name rather than approximated,
//! and it is one construct: the intermediate region segments — regions
//! retained in an auxiliary buffer for a later segment to refine rather than
//! composited onto the page. Everything else a scanner emits — generic
//! regions in all four templates, symbol dictionaries with and without
//! refinement/aggregate coding, text regions and their instance refinements,
//! immediate refinement regions over the page, pattern dictionaries and
//! halftone regions, custom code tables, and the MMR coding any of them may
//! use in place of the arithmetic decoder — decodes here.
//!
//! One region coding is not decoded here at all. A generic region may say that
//! its pixels are coded with the two-dimensional facsimile scheme of ITU-T T.6
//! rather than arithmetically (6.2.6), and that scheme is the whole of the
//! `CCITTFaxDecode` filter as well, so it lives in the sibling `ccitt` module
//! and [`generic`] calls into it. The two share the same [`bitmap::Bitmap`] and
//! the same convention that a set pixel is ink, so nothing is converted at the
//! join.
//!
//! Cutting across that stack is [`budget`], the allowance of decoding work one
//! embedded stream is allowed to spend. Every dimension the region decoders
//! loop over is a number the stream chose, and a region need not carry the
//! coded bytes it claims to, so the budget is what ties the cost of decoding to
//! something the input cannot inflate.

pub(crate) mod arith_int;
pub(crate) mod bitmap;
pub(crate) mod budget;
pub(crate) mod generic;
pub(crate) mod halftone;
pub(crate) mod huffman;
pub(crate) mod mq;
pub(crate) mod page;
pub(crate) mod reader;
pub(crate) mod refinement;
pub(crate) mod segment;
pub(crate) mod symbol_dict;
pub(crate) mod text_region;

#[cfg(test)]
pub(crate) mod testing;

use crate::error::Error;
use crate::filters::ccitt::CcittError;
use crate::object::{Dict, Object};
use crate::parser::Resolve;

/// A decoding failure inside the JBIG2 codec.
///
/// These surface to callers as [`crate::error::Error::Decode`]. The variants
/// exist so tests can assert on the *kind* of failure without matching on
/// message text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) enum Jbig2Error {
    /// A field ran past the end of the available bytes.
    Truncated,
    /// A field held a value the standard forbids.
    Malformed(&'static str),
    /// A construct this build does not yet decode.
    Unimplemented(&'static str),
    /// A declared bitmap exceeds [`bitmap::MAX_PIXELS`].
    TooLarge { width: u32, height: u32 },
    /// The stream asked for more decoding work than [`budget::MAX_WORK`]
    /// allows. The stream may be well formed; it is simply more expensive to
    /// decode than this decoder will pay for.
    WorkLimit,
}

impl core::fmt::Display for Jbig2Error {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            Self::Truncated => write!(f, "JBIG2 stream truncated"),
            Self::Malformed(what) => write!(f, "malformed JBIG2 stream: {what}"),
            Self::Unimplemented(what) => write!(f, "unsupported JBIG2 feature: {what}"),
            Self::TooLarge { width, height } => {
                write!(f, "JBIG2 bitmap too large: {width} x {height}")
            }
            Self::WorkLimit => write!(f, "JBIG2 stream exceeds the decoding work limit"),
        }
    }
}

impl From<Jbig2Error> for Error {
    fn from(err: Jbig2Error) -> Self {
        Error::Decode(err.to_string())
    }
}

impl From<CcittError> for Jbig2Error {
    /// Restates a facsimile decoding failure as a JBIG2 one, for the MMR arm of
    /// a generic region (6.2.6).
    ///
    /// Every variant has a counterpart, and the interesting one is
    /// `BadParameter`. In the facsimile codec that names a mistake by whoever
    /// called it — the row width and count arrive as arguments there, from a
    /// PDF parameter dictionary. Here they arrive from the region information
    /// field of the segment, so the same complaint is a statement about the
    /// stream, and it becomes [`Jbig2Error::Malformed`].
    fn from(err: CcittError) -> Jbig2Error {
        match err {
            CcittError::UnknownCode => Jbig2Error::Malformed("no such MMR code"),
            CcittError::RunTooLong => Jbig2Error::Malformed("MMR run past the end of the row"),
            CcittError::Malformed(what) | CcittError::BadParameter(what) => {
                Jbig2Error::Malformed(what)
            }
            CcittError::Unimplemented(what) => Jbig2Error::Unimplemented(what),
            CcittError::TooLarge { width, height } => Jbig2Error::TooLarge { width, height },
        }
    }
}

/// Decodes a `JBIG2Decode` stream to 1-bit `/DeviceGray` sample data
/// (ISO 32000-1 7.4.7).
///
/// `dict` is the image XObject's own dictionary, which is where the page
/// geometry comes from: an embedded JBIG2 stream carries no dimensions the
/// decoder may trust, so `/Width` and `/Height` decide how large a page the
/// segments are composited onto. `parms` is this filter's entry in
/// `/DecodeParms`, whose `/JBIG2Globals` names a second stream of segments
/// shared between pages (T.88 Annex D.3); those are walked first, on the same
/// work budget.
///
/// The returned bytes are inverted relative to the coded pixels. JBIG2 codes a
/// 1 as ink and `/DeviceGray` reads a 0 sample as black, so the two conventions
/// are reconciled here — see [`bitmap::Bitmap::to_pdf_samples`], which is the
/// only place in the codec that flips a bit for this reason.
pub(crate) fn decode_pdf_stream(
    data: &[u8],
    parms: Option<&Dict>,
    dict: &Dict,
    resolver: &dyn Resolve,
) -> crate::error::Result<Vec<u8>> {
    let width = dimension(dict, "Width", resolver)?;
    let height = dimension(dict, "Height", resolver)?;
    let globals = match parms.and_then(|p| p.get("JBIG2Globals")) {
        // The decode parameters have already been resolved, so a reference to
        // the globals stream arrives here as the stream itself.
        Some(Object::Stream(stream)) => {
            if has_jbig2_filter(&stream.dict, resolver) {
                // Globals that are themselves JBIG2-coded would send
                // `decode_stream` back through this function, and a document
                // is free to make that cycle unbounded.
                return Err(Error::Decode(
                    "/JBIG2Globals must not itself be JBIG2-coded".into(),
                ));
            }
            super::decode_stream(stream, resolver)?
        }
        // No globals is the ordinary case: a self-contained page stream.
        _ => Vec::new(),
    };
    let page = page::decode_embedded(&globals, data, width, height)?;
    Ok(page.to_pdf_samples())
}

/// Reads `/Width` or `/Height` off the image dictionary as a positive pixel
/// count.
///
/// Both are required entries of an image XObject (ISO 32000-1 Table 89) and
/// both are integers there; a real is accepted and truncated, since a producer
/// that writes `8.0` has still said eight. Zero, negative and out-of-range
/// values are refused rather than clamped: there is no page to decode onto, and
/// a silently resized one would place every region wrongly.
fn dimension(dict: &Dict, key: &'static str, resolver: &dyn Resolve) -> crate::error::Result<u32> {
    let value = match super::resolve_value(dict.get(key), resolver) {
        Some(Object::Int(v)) => v,
        Some(Object::Real(v)) if v.is_finite() => v as i64,
        _ => return Err(Error::MissingKey(key)),
    };
    match u32::try_from(value) {
        Ok(v) if v > 0 => Ok(v),
        _ => Err(Error::Decode(format!(
            "JBIG2 image /{key} of {value} is not a pixel count"
        ))),
    }
}

/// Whether a stream's own `/Filter` chain names `JBIG2Decode`.
///
/// Only the name matters, not its position: a globals stream is refused for
/// carrying the filter at all, so there is nothing to gain from working out
/// whether the chain would have reached it.
fn has_jbig2_filter(dict: &Dict, resolver: &dyn Resolve) -> bool {
    let named = |obj: Option<Object>| matches!(obj, Some(Object::Name(n)) if n.0 == "JBIG2Decode");
    match super::resolve_value(dict.get("Filter"), resolver) {
        Some(Object::Array(items)) => items
            .iter()
            .any(|item| named(super::resolve_value(Some(item), resolver))),
        other => named(other),
    }
}

/// Drives the arithmetic layers over arbitrary bytes, for the robustness
/// sweep below.
///
/// It interleaves three integer procedures and a symbol-ID decode against one
/// [`mq::MqDecoder`], which is the shape every region decoder has — several
/// procedures drawing from a single arithmetic stream, each adapting its own
/// contexts. Reaching that shape directly, rather than through a segment,
/// is what lets the sweep put arbitrary bytes into it: a real symbol
/// dictionary's coded data is preceded by a header that would reject almost
/// all of them before the arithmetic layer saw a byte.
///
/// Nothing is asserted about the values. The property under test is that the
/// call returns at all: any byte string, of any length, must decode to *some*
/// sequence of integers without panicking, hanging, or reading out of bounds,
/// because these bytes come from a PDF and a PDF is attacker-controlled. The
/// marker convention of T.88 E.3.4 is what makes that true past the end of the
/// buffer.
///
/// A fixed round count is deliberately the weaker check. It cannot see a
/// braid that keeps returning values forever, because it stops counting first;
/// `the_braid_runs_out_of_data` below is the one that runs to the end.
#[cfg(test)]
pub(crate) fn exercise_arithmetic_layers(data: &[u8], rounds: usize) {
    use std::hint::black_box;

    let mut dec = mq::MqDecoder::new(data);
    let mut set = arith_int::IntCtxSet::new();
    let mut iaid = arith_int::IaidCtx::new(8);
    for _ in 0..rounds {
        black_box(arith_int::decode_int(&mut dec, &mut set.iadh));
        black_box(arith_int::decode_int(&mut dec, &mut set.iadw));
        black_box(arith_int::decode_int(&mut dec, &mut set.iads));
        black_box(arith_int::decode_iaid(&mut dec, &mut iaid));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic pseudo-random sweep: no input, however malformed, may
    /// panic, hang, or read out of bounds. The generator is a fixed linear
    /// congruential sequence so a failure reproduces exactly from the case
    /// number alone.
    #[test]
    fn arithmetic_layers_survive_arbitrary_input() {
        let mut state: u32 = 0x1234_5678;
        for case in 0u32..2_000 {
            let len = (state % 257) as usize;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();
            exercise_arithmetic_layers(&data, 64);
            state = state.wrapping_add(case);
        }
    }

    /// The braid a region decoder actually runs — several integer procedures
    /// and a symbol ID drawing from one arithmetic stream — reaches the end of
    /// its data, whatever the data is.
    ///
    /// The sweeps above stop after a fixed number of rounds, which is exactly
    /// how a decoder that never stops on its own goes unnoticed: past the end
    /// of the input the arithmetic layer cycles, and the values the braid
    /// reads out of that cycle repeat without ever including a terminator. A
    /// symbol dictionary looping on those would never finish its height class.
    /// Here the loop condition is the decoder's own, so a braid that cannot
    /// finish trips the limit instead of being cut short by it.
    #[test]
    fn the_braid_runs_out_of_data() {
        let mut state: u32 = 0x2B7E_1516;
        for case in 0u32..600 {
            let len = (state % 33) as usize;
            let data: Vec<u8> = (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    (state >> 24) as u8
                })
                .collect();

            let mut dec = mq::MqDecoder::new(&data);
            let mut set = arith_int::IntCtxSet::new();
            let mut iaid = arith_int::IaidCtx::new(8);
            let mut rounds = 0usize;
            while !dec.is_exhausted() {
                let _ = arith_int::decode_int(&mut dec, &mut set.iadh);
                let _ = arith_int::decode_int(&mut dec, &mut set.iadw);
                let _ = arith_int::decode_int(&mut dec, &mut set.iads);
                let _ = arith_int::decode_iaid(&mut dec, &mut iaid);
                rounds += 1;
                assert!(rounds < 100_000, "case {case} never ran out of data");
            }
            // The height-class loop of 6.5.5 ends here and stays ended.
            for _ in 0..64 {
                assert_eq!(arith_int::decode_int(&mut dec, &mut set.iadw), None);
                let _ = arith_int::decode_iaid(&mut dec, &mut iaid);
            }
            state = state.wrapping_add(case);
        }
    }

    /// Degenerate inputs the sweep is unlikely to generate.
    ///
    /// `FF 90` and `FF 8F` straddle the marker test of T.88 E.3.4: `0x90` is
    /// above the `0x8F` threshold and takes the marker path, which leaves `BP`
    /// where it is and stuffs `0xFF00` into `C`, while `0x8F` is the largest
    /// byte that still advances `BP`. An off-by-one in that comparison shows up
    /// here and almost nowhere else.
    #[test]
    fn arithmetic_layers_survive_degenerate_input() {
        for data in [
            vec![],
            vec![0x00],
            vec![0xFF],
            vec![0xFF, 0xFF, 0xFF, 0xFF],
            vec![0xFF, 0x90],
            vec![0xFF, 0x8F],
            vec![0x00; 4096],
            vec![0xFF; 4096],
        ] {
            exercise_arithmetic_layers(&data, 256);
        }
    }
}
