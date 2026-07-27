//! Stream filters (ISO 32000 §7.4): FlateDecode, LZWDecode, ASCIIHexDecode,
//! ASCII85Decode, RunLengthDecode, JBIG2Decode, plus PNG/TIFF predictors.
//! `DCTDecode` is passthrough (decoded at the image layer); `JPXDecode`,
//! `Crypt` and the rest are unsupported.
//!
//! `JBIG2Decode` (§7.4.7) is decoded here rather than at the image layer,
//! because what comes out of the codec is ordinary 1-bit `/DeviceGray` sample
//! data: nothing downstream needs to know the image was JBIG2-coded at all.
//! It is the one filter that reads the image dictionary it belongs to, since
//! an embedded JBIG2 stream takes its page geometry from `/Width` and
//! `/Height`.
//!
//! Passthrough is reserved for codecs a consumer of the decoded bytes can
//! actually read. `JPXDecode` (§7.4.9) is not one of them: nothing in this
//! workspace decodes a JPEG 2000 codestream, so passing it through would
//! hand back bytes indistinguishable from decoded stream data and let an
//! image be painted from its own codestream. It is rejected instead.

use crate::error::{Error, Result};
use crate::object::{Dict, Name, Object, Stream};
use crate::parser::Resolve;

/// Upper bound on the decoded size of a stream, enforced inside every
/// expanding decoder and after each chain stage. Without it a crafted
/// "decompression bomb" (e.g. chained FlateDecode stages, each ~1000:1)
/// turns a few KiB of input into tens of GiB of allocations.
pub(crate) const MAX_DECODED_LEN: usize = 256 << 20; // 256 MiB

/// Upper bound on the number of entries honored in a `/Filter` array;
/// genuine chains are at most a handful of filters long.
const MAX_FILTER_CHAIN: usize = 32;

pub mod ascii85;
pub mod ascii_hex;
// Private: the facsimile codec is an implementation detail of `decode_stream`
// and of the JBIG2 generic region.
mod ccitt;
pub mod flate;
// Private: the JBIG2 codec is an implementation detail of `decode_stream`.
mod jbig2;
pub mod lzw;
pub mod predictor;
pub mod run_length;

/// Returns true for the six PDF whitespace bytes (ISO 32000 §7.2.2):
/// NUL, HT, LF, FF, CR and SP.
pub(crate) fn is_pdf_whitespace(b: u8) -> bool {
    matches!(b, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ')
}

/// Reads an integer-valued entry from an optional parameter dictionary,
/// coercing reals (truncated) and booleans; anything else yields `default`.
pub(crate) fn int_parm(parms: Option<&Dict>, key: &str, default: i64) -> i64 {
    match parms.and_then(|d| d.get(key)) {
        Some(Object::Int(v)) => *v,
        Some(Object::Real(v)) => *v as i64,
        Some(Object::Bool(v)) => i64::from(*v),
        _ => default,
    }
}

/// Chases indirect references through `resolver` (bounded depth to break
/// reference cycles); direct objects are cloned. Returns `None` when a
/// reference cannot be resolved.
fn resolve_value(obj: Option<&Object>, resolver: &dyn Resolve) -> Option<Object> {
    let mut cur = obj?.clone();
    for _ in 0..8 {
        match cur {
            Object::Ref(r) => cur = resolver.resolve_ref(r)?,
            other => return Some(other),
        }
    }
    None
}

/// Clones `dict` with every value resolved through `resolver`, so that the
/// individual decoders never see indirect references. Unresolvable values
/// become `null` (and thus fall back to their defaults).
fn resolve_dict_values(dict: &Dict, resolver: &dyn Resolve) -> Dict {
    let mut out = Dict::new();
    for (key, value) in dict.iter() {
        let resolved = resolve_value(Some(value), resolver).unwrap_or(Object::Null);
        out.insert(key.clone(), resolved);
    }
    out
}

/// Extracts the parameter dictionary for the filter at `index` from the
/// resolved `/DecodeParms` value. A single dictionary applies to the first
/// filter; an array aligns by position; `null` or missing entries mean no
/// parameters.
fn parms_at(parms: Option<&Object>, index: usize, resolver: &dyn Resolve) -> Option<Dict> {
    match parms {
        Some(Object::Dict(d)) if index == 0 => Some(resolve_dict_values(d, resolver)),
        Some(Object::Array(items)) => match resolve_value(items.get(index), resolver) {
            Some(Object::Dict(d)) => Some(resolve_dict_values(&d, resolver)),
            _ => None,
        },
        _ => None,
    }
}

/// Applies the stream's `/Filter` chain (name or array) with the matching
/// `/DecodeParms` (dict, array, or null) in order and returns the decoded
/// bytes. Two filters are accepted only as the last element of the chain: the
/// passthrough `DCTDecode`, and `JBIG2Decode`, whose codec reads the stream
/// dictionary the chain belongs to. `JPXDecode`, `Crypt` and unknown filters
/// yield [`Error::UnsupportedFilter`].
pub fn decode_stream(stream: &Stream, resolver: &dyn Resolve) -> Result<Vec<u8>> {
    let filter = resolve_value(stream.dict.get("Filter"), resolver);
    // Filters keep their original position so that `/DecodeParms` arrays
    // stay aligned even when unusable (e.g. null) entries are skipped.
    let filters: Vec<(usize, Name)> = match &filter {
        None | Some(Object::Null) => return Ok(stream.data.clone()),
        Some(Object::Name(n)) => vec![(0, n.clone())],
        Some(Object::Array(items)) => items
            .iter()
            .enumerate()
            .filter_map(|(i, item)| match resolve_value(Some(item), resolver) {
                Some(Object::Name(n)) => Some((i, n)),
                _ => None,
            })
            .collect(),
        // Lenient: an unusable /Filter value leaves the data as stored.
        Some(_) => return Ok(stream.data.clone()),
    };
    if filters.is_empty() {
        return Ok(stream.data.clone());
    }
    if filters.len() > MAX_FILTER_CHAIN {
        return Err(Error::Decode(format!(
            "filter chain of {} entries exceeds the limit of {MAX_FILTER_CHAIN}",
            filters.len()
        )));
    }
    let parms_obj = resolve_value(stream.dict.get("DecodeParms"), resolver);
    let last = filters.len() - 1;
    let mut data = stream.data.clone();
    for (pos, (index, name)) in filters.iter().enumerate() {
        let parms = parms_at(parms_obj.as_ref(), *index, resolver);
        let parms = parms.as_ref();
        data = match name.0.as_str() {
            "FlateDecode" | "Fl" => flate::decode(&data, parms)?,
            "LZWDecode" | "LZW" => lzw::decode(&data, parms)?,
            "ASCIIHexDecode" | "AHx" => ascii_hex::decode(&data)?,
            "ASCII85Decode" | "A85" => ascii85::decode(&data)?,
            "RunLengthDecode" | "RL" => run_length::decode(&data)?,
            // JBIG2 is decoded to samples here: the result is 1-bit
            // `/DeviceGray` data like any other, so the image layer has
            // nothing left to do. Only as the last filter, since the codec
            // reads the bytes of the stream itself, not of a later stage.
            "JBIG2Decode" if pos == last => {
                jbig2::decode_pdf_stream(&data, parms, &stream.dict, resolver)?
            }
            // JPEG stays encoded; the image layer decodes it. No other
            // codec may be handed back undecoded: a caller cannot tell
            // codestream bytes from decoded samples, and would paint them.
            "DCTDecode" | "DCT" if pos == last => data,
            other => return Err(Error::UnsupportedFilter(other.to_string())),
        };
        // Defense in depth: the expanding decoders cap their own output,
        // but no stage may hand oversized data to the next one either.
        if data.len() > MAX_DECODED_LEN {
            return Err(Error::Decode("decoded stream exceeds size limit".into()));
        }
    }
    Ok(data)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::ObjRef;
    use crate::parser::NoResolve;
    use flate2::write::ZlibEncoder;
    use flate2::Compression;
    use std::collections::HashMap;
    use std::io::Write;

    fn name(s: &str) -> Name {
        Name(s.to_string())
    }

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    fn hex_encode(data: &[u8]) -> Vec<u8> {
        let mut out: Vec<u8> = data
            .iter()
            .flat_map(|b| format!("{b:02X}").into_bytes())
            .collect();
        out.push(b'>');
        out
    }

    fn make_stream(entries: Vec<(&str, Object)>, data: &[u8]) -> Stream {
        let mut dict = Dict::new();
        for (k, v) in entries {
            dict.insert(name(k), v);
        }
        Stream {
            dict,
            data: data.to_vec(),
        }
    }

    fn make_dict(entries: Vec<(&str, Object)>) -> Dict {
        let mut dict = Dict::new();
        for (k, v) in entries {
            dict.insert(name(k), v);
        }
        dict
    }

    struct MapResolve(HashMap<(u32, u16), Object>);

    impl Resolve for MapResolve {
        fn resolve_ref(&self, r: ObjRef) -> Option<Object> {
            self.0.get(&(r.num, r.gen)).cloned()
        }
    }

    #[test]
    fn no_filter_returns_raw_data() {
        let s = make_stream(vec![], b"raw bytes");
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), b"raw bytes");
    }

    #[test]
    fn null_filter_returns_raw_data() {
        let s = make_stream(vec![("Filter", Object::Null)], b"raw bytes");
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), b"raw bytes");
    }

    #[test]
    fn empty_filter_array_returns_raw_data() {
        let s = make_stream(vec![("Filter", Object::Array(vec![]))], b"raw");
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), b"raw");
    }

    #[test]
    fn single_name_filter_hex() {
        let s = make_stream(
            vec![("Filter", Object::Name(name("ASCIIHexDecode")))],
            b"48656C6C6F>",
        );
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), b"Hello");
    }

    #[test]
    fn abbreviated_filter_names_accepted() {
        let s = make_stream(vec![("Filter", Object::Name(name("AHx")))], b"4869>");
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), b"Hi");
    }

    #[test]
    fn chained_hex_then_flate() {
        let text = b"chained filters exercise the whole pipeline";
        let stored = hex_encode(&zlib(text));
        let s = make_stream(
            vec![(
                "Filter",
                Object::Array(vec![
                    Object::Name(name("ASCIIHexDecode")),
                    Object::Name(name("FlateDecode")),
                ]),
            )],
            &stored,
        );
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), text);
    }

    #[test]
    fn decode_parms_single_dict_png_predictor() {
        // Two rows of 4 bytes, PNG "Up" filter (type 2) applied per row.
        let raw = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let filtered = [2u8, 10, 20, 30, 40, 2, 40, 40, 40, 40];
        let parms = make_dict(vec![
            ("Predictor", Object::Int(12)),
            ("Columns", Object::Int(4)),
        ]);
        let s = make_stream(
            vec![
                ("Filter", Object::Name(name("FlateDecode"))),
                ("DecodeParms", Object::Dict(parms)),
            ],
            &zlib(&filtered),
        );
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), raw);
    }

    #[test]
    fn decode_parms_array_aligns_with_filter_array() {
        let raw = [10u8, 20, 30, 40, 50, 60, 70, 80];
        let filtered = [2u8, 10, 20, 30, 40, 2, 40, 40, 40, 40];
        let stored = hex_encode(&zlib(&filtered));
        let parms = make_dict(vec![
            ("Predictor", Object::Int(12)),
            ("Columns", Object::Int(4)),
        ]);
        let s = make_stream(
            vec![
                (
                    "Filter",
                    Object::Array(vec![
                        Object::Name(name("ASCIIHexDecode")),
                        Object::Name(name("FlateDecode")),
                    ]),
                ),
                (
                    "DecodeParms",
                    Object::Array(vec![Object::Null, Object::Dict(parms)]),
                ),
            ],
            &stored,
        );
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), raw);
    }

    #[test]
    fn indirect_filter_and_parms_resolved() {
        // /Filter 5 0 R -> /FlateDecode, /DecodeParms 6 0 R -> dict whose
        // /Columns is itself the indirect reference 7 0 R -> 4.
        let raw = [5u8, 7, 9, 11];
        let diffed = [5u8, 2, 2, 2]; // TIFF horizontal differencing, colors=1
        let parms = make_dict(vec![
            ("Predictor", Object::Int(2)),
            ("Colors", Object::Int(1)),
            ("Columns", Object::Ref(ObjRef { num: 7, gen: 0 })),
        ]);
        let mut map = HashMap::new();
        map.insert((5, 0), Object::Name(name("FlateDecode")));
        map.insert((6, 0), Object::Dict(parms));
        map.insert((7, 0), Object::Int(4));
        let resolver = MapResolve(map);
        let s = make_stream(
            vec![
                ("Filter", Object::Ref(ObjRef { num: 5, gen: 0 })),
                ("DecodeParms", Object::Ref(ObjRef { num: 6, gen: 0 })),
            ],
            &zlib(&diffed),
        );
        assert_eq!(decode_stream(&s, &resolver).unwrap(), raw);
    }

    #[test]
    fn crypt_filter_is_unsupported() {
        let s = make_stream(vec![("Filter", Object::Name(name("Crypt")))], b"x");
        match decode_stream(&s, &NoResolve) {
            Err(Error::UnsupportedFilter(n)) => assert_eq!(n, "Crypt"),
            other => panic!("expected UnsupportedFilter, got {other:?}"),
        }
    }

    #[test]
    fn unknown_filter_is_unsupported() {
        let s = make_stream(vec![("Filter", Object::Name(name("CCITTFaxDecode")))], b"x");
        match decode_stream(&s, &NoResolve) {
            Err(Error::UnsupportedFilter(n)) => assert_eq!(n, "CCITTFaxDecode"),
            other => panic!("expected UnsupportedFilter, got {other:?}"),
        }
    }

    #[test]
    fn dct_passthrough_when_last() {
        let jpeg = b"\xff\xd8pretend jpeg payload";
        let s = make_stream(vec![("Filter", Object::Name(name("DCTDecode")))], jpeg);
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), jpeg);
    }

    #[test]
    fn flate_then_dct_passthrough() {
        let jpeg = b"\xff\xd8fake jpeg";
        let s = make_stream(
            vec![(
                "Filter",
                Object::Array(vec![
                    Object::Name(name("FlateDecode")),
                    Object::Name(name("DCTDecode")),
                ]),
            )],
            &zlib(jpeg),
        );
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), jpeg);
    }

    #[test]
    fn dct_not_last_is_unsupported() {
        let s = make_stream(
            vec![(
                "Filter",
                Object::Array(vec![
                    Object::Name(name("DCTDecode")),
                    Object::Name(name("FlateDecode")),
                ]),
            )],
            b"x",
        );
        match decode_stream(&s, &NoResolve) {
            Err(Error::UnsupportedFilter(n)) => assert_eq!(n, "DCTDecode"),
            other => panic!("expected UnsupportedFilter, got {other:?}"),
        }
    }

    #[test]
    fn jpx_is_unsupported_even_as_the_last_filter() {
        // JPEG 2000 codestreams are not decoded anywhere in the workspace,
        // so handing the bytes back as if they were decoded stream data
        // would let a caller paint a codestream as pixel samples.
        let s = make_stream(
            vec![("Filter", Object::Name(name("JPXDecode")))],
            b"\x00\x00\x00\x0cjP  \r\n\x87\n",
        );
        match decode_stream(&s, &NoResolve) {
            Err(Error::UnsupportedFilter(n)) => assert_eq!(n, "JPXDecode"),
            other => panic!("expected UnsupportedFilter, got {other:?}"),
        }
    }

    #[test]
    fn flate_then_jpx_is_unsupported() {
        // The rejection survives an earlier stage: the chain runs FlateDecode
        // first and still refuses to hand the codestream on.
        let s = make_stream(
            vec![(
                "Filter",
                Object::Array(vec![
                    Object::Name(name("FlateDecode")),
                    Object::Name(name("JPXDecode")),
                ]),
            )],
            &zlib(b"\x00\x00\x00\x0cjP  \r\n\x87\n"),
        );
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::UnsupportedFilter(n)) if n == "JPXDecode"
        ));
    }

    #[test]
    fn jpx_not_last_is_unsupported() {
        let s = make_stream(
            vec![(
                "Filter",
                Object::Array(vec![
                    Object::Name(name("JPXDecode")),
                    Object::Name(name("FlateDecode")),
                ]),
            )],
            b"x",
        );
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::UnsupportedFilter(n)) if n == "JPXDecode"
        ));
    }

    #[test]
    fn null_entries_in_filter_array_are_skipped() {
        let s = make_stream(
            vec![(
                "Filter",
                Object::Array(vec![Object::Null, Object::Name(name("ASCIIHexDecode"))]),
            )],
            b"4F4B>",
        );
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), b"OK");
    }

    #[test]
    fn overlong_filter_chain_is_rejected() {
        let chain: Vec<Object> = (0..MAX_FILTER_CHAIN + 1)
            .map(|_| Object::Name(name("ASCIIHexDecode")))
            .collect();
        let s = make_stream(vec![("Filter", Object::Array(chain))], b"4869>");
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::Decode(_))
        ));
    }

    #[test]
    fn decompression_bomb_chain_is_rejected() {
        // Two chained FlateDecode stages: a few hundred KiB of stored bytes
        // would otherwise inflate to hundreds of MiB. The inner payload is
        // compressed in chunks so the test never holds the expanded form.
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::fast());
        let chunk = vec![0u8; 1 << 20];
        let mut remaining = MAX_DECODED_LEN + (1 << 20);
        while remaining > 0 {
            let n = remaining.min(chunk.len());
            enc.write_all(&chunk[..n]).unwrap();
            remaining -= n;
        }
        let inner = enc.finish().unwrap();
        let s = make_stream(
            vec![(
                "Filter",
                Object::Array(vec![
                    Object::Name(name("FlateDecode")),
                    Object::Name(name("FlateDecode")),
                ]),
            )],
            &zlib(&inner),
        );
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::Decode(_))
        ));
    }

    /// A `JBIG2Decode` stream decodes to packed 1-bit `/DeviceGray` samples
    /// with ink as 0 (ISO 32000-1 §7.4.7).
    ///
    /// The fixture's two rows are `10000000` and `01010101`, so the samples
    /// are the complement of those bits, byte for byte. Asserting them
    /// literally is what makes a missing — or a doubled — inversion a failure
    /// rather than a plausible-looking page.
    #[test]
    fn jbig2_stream_decodes_to_inverted_packed_samples() {
        let (data, width, height) = jbig2::testing::generic_region_stream();
        let s = make_stream(
            vec![
                ("Filter", Object::Name(name("JBIG2Decode"))),
                ("Width", Object::Int(i64::from(width))),
                ("Height", Object::Int(i64::from(height))),
            ],
            &data,
        );
        assert_eq!(
            decode_stream(&s, &NoResolve).unwrap(),
            vec![0b0111_1111, 0b1010_1010],
        );
    }

    /// The segments may equally arrive through `/JBIG2Globals`, which is how a
    /// document shares one symbol dictionary across its pages (T.88 Annex D.3).
    #[test]
    fn jbig2_globals_are_decoded_before_the_page_stream() {
        let (data, width, height) = jbig2::testing::generic_region_stream();
        // Everything but the trailing end-of-page segment moves into globals,
        // leaving the page stream with only that segment: the pixels can then
        // only have come from the globals.
        let split = data.len() - 11;
        let globals = Stream {
            dict: Dict::new(),
            data: data[..split].to_vec(),
        };
        let parms = make_dict(vec![("JBIG2Globals", Object::Stream(globals))]);
        let s = make_stream(
            vec![
                ("Filter", Object::Name(name("JBIG2Decode"))),
                ("DecodeParms", Object::Dict(parms)),
                ("Width", Object::Int(i64::from(width))),
                ("Height", Object::Int(i64::from(height))),
            ],
            &data[split..],
        );
        assert_eq!(
            decode_stream(&s, &NoResolve).unwrap(),
            vec![0b0111_1111, 0b1010_1010],
        );
    }

    /// `/JBIG2Globals` is nearly always an indirect reference, since the point
    /// of it is to be shared between the images of many pages.
    #[test]
    fn jbig2_globals_are_resolved_through_a_reference() {
        let (data, width, height) = jbig2::testing::generic_region_stream();
        let split = data.len() - 11;
        let globals = Stream {
            dict: Dict::new(),
            data: data[..split].to_vec(),
        };
        let parms = make_dict(vec![(
            "JBIG2Globals",
            Object::Ref(ObjRef { num: 4, gen: 0 }),
        )]);
        let mut map = HashMap::new();
        map.insert((4, 0), Object::Stream(globals));
        let s = make_stream(
            vec![
                ("Filter", Object::Name(name("JBIG2Decode"))),
                ("DecodeParms", Object::Dict(parms)),
                ("Width", Object::Int(i64::from(width))),
                ("Height", Object::Int(i64::from(height))),
            ],
            &data[split..],
        );
        assert_eq!(
            decode_stream(&s, &MapResolve(map)).unwrap(),
            vec![0b0111_1111, 0b1010_1010],
        );
    }

    /// Globals that are themselves JBIG2-coded would send `decode_stream` back
    /// into the codec, and a document may make that cycle unbounded.
    #[test]
    fn jbig2_globals_may_not_be_jbig2_coded() {
        let inner = Stream {
            dict: make_dict(vec![
                ("Filter", Object::Name(name("JBIG2Decode"))),
                ("Width", Object::Int(8)),
                ("Height", Object::Int(8)),
            ]),
            data: Vec::new(),
        };
        let parms = make_dict(vec![("JBIG2Globals", Object::Stream(inner))]);
        let s = make_stream(
            vec![
                ("Filter", Object::Name(name("JBIG2Decode"))),
                ("DecodeParms", Object::Dict(parms)),
                ("Width", Object::Int(8)),
                ("Height", Object::Int(8)),
            ],
            b"",
        );
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::Decode(_))
        ));
    }

    /// Without `/Width` and `/Height` there is no page to decode onto.
    #[test]
    fn jbig2_without_dimensions_is_a_decode_error() {
        let s = make_stream(vec![("Filter", Object::Name(name("JBIG2Decode")))], b"");
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::MissingKey("Width")),
        ));
    }

    /// A dimension of zero is not a small page, it is not a page.
    #[test]
    fn jbig2_with_a_zero_dimension_is_a_decode_error() {
        let s = make_stream(
            vec![
                ("Filter", Object::Name(name("JBIG2Decode"))),
                ("Width", Object::Int(8)),
                ("Height", Object::Int(0)),
            ],
            b"",
        );
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::Decode(_))
        ));
    }

    /// The dimensions may be indirect, like any other dictionary value.
    #[test]
    fn jbig2_dimensions_are_resolved_through_references() {
        let (data, width, height) = jbig2::testing::generic_region_stream();
        let mut map = HashMap::new();
        map.insert((9, 0), Object::Int(i64::from(width)));
        let s = make_stream(
            vec![
                ("Filter", Object::Name(name("JBIG2Decode"))),
                ("Width", Object::Ref(ObjRef { num: 9, gen: 0 })),
                ("Height", Object::Int(i64::from(height))),
            ],
            &data,
        );
        assert_eq!(
            decode_stream(&s, &MapResolve(map)).unwrap(),
            vec![0b0111_1111, 0b1010_1010],
        );
    }

    /// A malformed JBIG2 stream is a decode error, not samples.
    #[test]
    fn jbig2_garbage_is_a_decode_error() {
        let s = make_stream(
            vec![
                ("Filter", Object::Name(name("JBIG2Decode"))),
                ("Width", Object::Int(8)),
                ("Height", Object::Int(8)),
            ],
            &[0u8; 8],
        );
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::Decode(_))
        ));
    }

    /// `JBIG2Decode` reads the stream's own bytes, so it cannot sit before
    /// another filter in the chain.
    #[test]
    fn jbig2_not_last_is_unsupported() {
        let s = make_stream(
            vec![
                (
                    "Filter",
                    Object::Array(vec![
                        Object::Name(name("JBIG2Decode")),
                        Object::Name(name("FlateDecode")),
                    ]),
                ),
                ("Width", Object::Int(8)),
                ("Height", Object::Int(8)),
            ],
            b"x",
        );
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::UnsupportedFilter(n)) if n == "JBIG2Decode"
        ));
    }

    /// An earlier stage may still compress the embedded stream, which is how
    /// the segments usually arrive.
    #[test]
    fn flate_then_jbig2_decodes() {
        let (data, width, height) = jbig2::testing::generic_region_stream();
        let s = make_stream(
            vec![
                (
                    "Filter",
                    Object::Array(vec![
                        Object::Name(name("FlateDecode")),
                        Object::Name(name("JBIG2Decode")),
                    ]),
                ),
                ("Width", Object::Int(i64::from(width))),
                ("Height", Object::Int(i64::from(height))),
            ],
            &zlib(&data),
        );
        assert_eq!(
            decode_stream(&s, &NoResolve).unwrap(),
            vec![0b0111_1111, 0b1010_1010],
        );
    }

    #[test]
    fn int_parm_coercions() {
        let d = make_dict(vec![
            ("A", Object::Int(3)),
            ("B", Object::Real(2.9)),
            ("C", Object::Bool(true)),
            ("D", Object::Name(name("nope"))),
        ]);
        assert_eq!(int_parm(Some(&d), "A", 0), 3);
        assert_eq!(int_parm(Some(&d), "B", 0), 2);
        assert_eq!(int_parm(Some(&d), "C", 0), 1);
        assert_eq!(int_parm(Some(&d), "D", 7), 7);
        assert_eq!(int_parm(Some(&d), "missing", 7), 7);
        assert_eq!(int_parm(None, "A", 7), 7);
    }
}
