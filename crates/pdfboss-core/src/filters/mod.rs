//! Stream filters (ISO 32000 §7.4): FlateDecode, LZWDecode, ASCIIHexDecode,
//! ASCII85Decode, RunLengthDecode, CCITTFaxDecode, JBIG2Decode, plus PNG/TIFF
//! predictors. `DCTDecode` and `JPXDecode` are passthrough (decoded at the
//! image layer); `Crypt` and the rest are unsupported.
//!
//! The two bilevel codecs, `CCITTFaxDecode` (§7.4.6) and `JBIG2Decode`
//! (§7.4.7), are decoded here rather than at the image layer, because what
//! comes out of either is ordinary 1-bit `/DeviceGray` sample data: nothing
//! downstream needs to know the image was coded that way at all. `JBIG2Decode`
//! is the one filter that reads the image dictionary it belongs to, since an
//! embedded JBIG2 stream takes its page geometry from `/Width` and `/Height`;
//! `CCITTFaxDecode` takes its geometry from its own `/DecodeParms` instead.
//!
//! The two also disagree about polarity, which is worth stating in one place
//! because both arms are in this file. JBIG2 defines a 1 pixel as ink and
//! `/DeviceGray` reads a 0 sample as black, so the `JBIG2Decode` arm always
//! inverts. `CCITTFaxDecode` has `/BlackIs1`, whose default of false already
//! means "0 bits are black" — the `/DeviceGray` convention — so that arm
//! inverts by default and does *not* invert when `/BlackIs1` is set.
//!
//! Passthrough is reserved for codecs a consumer of the decoded bytes can
//! actually read. Both image codecs qualify: the render image layer decodes
//! a raw JPEG (`DCTDecode`) itself and a JPEG 2000 codestream (`JPXDecode`,
//! §7.4.9) through `pdfboss-jpx`. Every other codec is rejected rather than
//! handed back, because a caller cannot tell an undecoded codestream from
//! decoded stream data and would paint it as pixel samples.

use crate::error::{Error, Result};
use crate::object::{Dict, Name, Object, Stream};
use crate::parser::Resolve;
use crate::source::AsyncObjectSource;

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

/// Reads a boolean-valued entry from an optional parameter dictionary,
/// coercing integers (nonzero is true, matching the coercion [`int_parm`] makes
/// in the other direction); anything else yields `default`.
pub(crate) fn bool_parm(parms: Option<&Dict>, key: &str, default: bool) -> bool {
    match parms.and_then(|d| d.get(key)) {
        Some(Object::Bool(v)) => *v,
        Some(Object::Int(v)) => *v != 0,
        _ => default,
    }
}

/// Chases indirect references through `resolver`, depth-capped at
/// [`crate::source::MAX_RESOLVE_DEPTH`] to break reference cycles; direct
/// objects are cloned. Returns `None` when a reference cannot be resolved.
///
/// The cap is shared with the source-level chase deliberately: the
/// image-codec gate reads `/Filter` through `src.resolve` and this decoder
/// reads it here, and a chain one side follows further than the other is a
/// stream the gate vouches for but the decoder leaves encoded (or the
/// reverse). `deep_filter_ref_chains_decode_like_the_gate_reads_them` pins
/// the agreement.
fn resolve_value(obj: Option<&Object>, resolver: &dyn Resolve) -> Option<Object> {
    let mut cur = obj?.clone();
    for _ in 0..crate::source::MAX_RESOLVE_DEPTH {
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
/// bytes. Three filters are accepted only as the last element of the chain:
/// the passthroughs `DCTDecode` and `JPXDecode`, and `JBIG2Decode`, whose
/// codec reads the stream dictionary the chain belongs to. `CCITTFaxDecode`
/// is not among them — it consumes only the bytes it is handed, so it decodes
/// at any position, and a chain that puts something after it fails in that
/// later stage rather than being reported as an unsupported filter it is not.
/// `Crypt` and unknown filters yield [`Error::UnsupportedFilter`].
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
            // Fax coding decodes to samples here for the same reason JBIG2
            // does: what the codec produces is ordinary 1-bit `/DeviceGray`
            // data, and nothing downstream needs to know the image was faxed.
            // Unlike JBIG2 it reads no part of the stream dictionary, only the
            // bytes handed to it and its own `/DecodeParms`, so it is not
            // restricted to the end of the chain.
            "CCITTFaxDecode" | "CCF" => ccitt::decode_pdf_stream(&data, parms)?,
            // JBIG2 is decoded to samples here: the result is 1-bit
            // `/DeviceGray` data like any other, so the image layer has
            // nothing left to do. Only as the last filter, since the codec
            // reads the bytes of the stream itself, not of a later stage.
            "JBIG2Decode" if pos == last => {
                jbig2::decode_pdf_stream(&data, parms, &stream.dict, resolver)?
            }
            // JPEG and JPEG 2000 stay encoded; the image layer decodes
            // them. No other codec may be handed back undecoded: a caller
            // cannot tell codestream bytes from decoded samples, and would
            // paint them. (`JPXDecode` has no inline-image abbreviation:
            // ISO 32000-1 Table 94 defines none.)
            "DCTDecode" | "DCT" if pos == last => data,
            "JPXDecode" if pos == last => data,
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

/// Whether `name` (abbreviations included) is one of the two image codecs
/// [`decode_stream`] passes through still encoded — `DCTDecode` and
/// `JPXDecode` (ISO 32000-1 7.4.9). Their decoded form exists only at the
/// image layer; every other consumer of stream bytes must refuse them,
/// because a raw JPEG or JPEG 2000 codestream is indistinguishable from
/// decoded data to anything that is not an image decoder.
pub fn is_image_codec(name: &str) -> bool {
    matches!(name, "DCTDecode" | "DCT" | "JPXDecode")
}

/// The last `Name` of a stream's `/Filter` value — the codec the data is
/// still encoded in when [`decode_stream`] passes an image codec through
/// untouched, or `None` when the chain ends in a filter that decodes here.
///
/// Reads `/Filter` exactly as [`decode_stream`] does: the value and every
/// array element are resolved through the source, non-Name elements are
/// skipped (`/Filter [/JPXDecode null]` still trails `JPXDecode`), and an
/// unusable value has no trailing filter at all. The two must agree on
/// which filter is "last", or a passthrough goes unrecognized and its
/// codestream bytes get consumed as decoded data;
/// `trailing_filter_reads_the_chain_like_decode_stream` pins the
/// agreement.
pub async fn trailing_filter_with<S: AsyncObjectSource>(src: &S, dict: &Dict) -> Option<Name> {
    // The overwhelmingly common shapes — a bare name, or an array of bare
    // names — are read straight off the dictionary. Each `resolve` is a
    // boxed future plus an owned-Object clone, and this gate runs in front
    // of every content fetch (up to once per form invocation), so the
    // resolve is reserved for values that actually contain a reference.
    let owned;
    let filter = match dict.get("Filter")? {
        Object::Ref(_) => {
            owned = src.resolve(dict.get("Filter")?).await.ok()?;
            &owned
        }
        direct => direct,
    };
    match filter {
        Object::Name(n) => Some(n.clone()),
        Object::Array(items) => {
            for item in items.iter().rev() {
                match item {
                    Object::Name(n) => return Some(n.clone()),
                    Object::Ref(_) => {
                        if let Ok(Object::Name(n)) = src.resolve(item).await {
                            return Some(n);
                        }
                    }
                    // Not a name and not a road to one: skipped, exactly
                    // as decode_stream skips it.
                    _ => {}
                }
            }
            None
        }
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::filters::ccitt::codes::Mode;
    use crate::filters::ccitt::testing::{
        bitmap_from_rows, encode_g3_1d, encode_g3_1d_byte_aligned, encode_g4, pack, push_mode,
        push_run,
    };
    use crate::filters::jbig2::bitmap::Bitmap;
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

    /// The same map as an [`ObjectSource`], so [`Immediate`] can drive the
    /// asynchronous `/Filter` reading over it.
    struct MapSource(HashMap<(u32, u16), Object>);

    impl crate::source::ObjectSource for MapSource {
        fn get(&self, r: ObjRef) -> Result<Object> {
            self.0
                .get(&(r.num, r.gen))
                .cloned()
                .ok_or_else(|| Error::Decode("missing object".into()))
        }

        fn stream_data(&self, s: &Stream) -> Result<Vec<u8>> {
            decode_stream(s, &NoResolve)
        }
    }

    #[test]
    fn trailing_filter_reads_the_chain_like_decode_stream() {
        // The two readings of /Filter must agree on which filter is last,
        // or a passthrough goes unrecognized and its codestream bytes get
        // consumed as decoded data. Each case runs both sides: when
        // decode_stream leaves a JPXDecode-trailing stream encoded (the
        // data comes back byte-identical), trailing_filter_with must name
        // JPXDecode — and only then.
        use crate::source::{block_on, Immediate};

        let raw: &[u8] = b"raw codestream bytes";
        let cases: Vec<(Object, Option<&str>)> = vec![
            (Object::Name(name("JPXDecode")), Some("JPXDecode")),
            // The regression shape: a null AFTER the codec must not hide it.
            (
                Object::Array(vec![Object::Name(name("JPXDecode")), Object::Null]),
                Some("JPXDecode"),
            ),
            // A reference to the codec name, at both levels.
            (
                Object::Array(vec![Object::Ref(ObjRef { num: 7, gen: 0 })]),
                Some("JPXDecode"),
            ),
            (Object::Ref(ObjRef { num: 7, gen: 0 }), Some("JPXDecode")),
            // No usable name at all: no filters run, no trailing filter.
            (Object::Array(vec![Object::Null]), None),
            (Object::Int(3), None),
        ];
        let map = HashMap::from([((7u32, 0u16), Object::Name(name("JPXDecode")))]);
        for (filter, trailing) in cases {
            let s = make_stream(vec![("Filter", filter.clone())], raw);
            let decoded = decode_stream(&s, &MapResolve(map.clone())).expect("lenient");
            assert_eq!(decoded, raw, "every case leaves the data as stored");
            let src = Immediate(MapSource(map.clone()));
            let got = block_on(trailing_filter_with(&src, &s.dict));
            assert_eq!(
                got.as_ref().map(|n| n.0.as_str()),
                trailing,
                "trailing filter of {filter:?}"
            );
            assert_eq!(
                got.map(|n| is_image_codec(&n.0)).unwrap_or(false),
                trailing.is_some(),
                "passthrough recognition of {filter:?}"
            );
        }
    }

    /// A direct `/Filter` value costs the gate no `resolve` round-trip.
    /// The overwhelmingly common shapes — a bare name, or an array of bare
    /// names — are read straight off the dictionary; the boxed resolve
    /// future (plus its owned-Object clone) is reserved for values that
    /// actually contain a reference. Every content fetch pays this gate,
    /// up to once per form invocation, which is what makes the fast path
    /// worth pinning.
    #[test]
    fn direct_filter_values_cost_the_gate_no_resolve() {
        use crate::source::{block_on, resolve_with, AsyncObjectSource, BoxFuture};
        use std::sync::atomic::{AtomicUsize, Ordering};

        /// Counts `resolve` calls — each one is a boxed future plus an
        /// owned-Object clone, which is the cost the fast path exists to
        /// avoid, whether or not a fetch follows.
        struct CountingSource {
            map: HashMap<(u32, u16), Object>,
            resolves: AtomicUsize,
        }

        impl AsyncObjectSource for CountingSource {
            fn get(&self, r: ObjRef) -> BoxFuture<'_, Result<Object>> {
                let got = self
                    .map
                    .get(&(r.num, r.gen))
                    .cloned()
                    .ok_or_else(|| Error::Decode("missing object".into()));
                Box::pin(std::future::ready(got))
            }

            fn stream_data<'a>(&'a self, s: &'a Stream) -> BoxFuture<'a, Result<Vec<u8>>> {
                Box::pin(std::future::ready(Ok(s.data.clone())))
            }

            fn resolve<'a>(&'a self, o: &'a Object) -> BoxFuture<'a, Result<Object>> {
                self.resolves.fetch_add(1, Ordering::Relaxed);
                Box::pin(resolve_with(self, o))
            }
        }

        let map = HashMap::from([((7u32, 0u16), Object::Name(name("JPXDecode")))]);
        let direct: Vec<(Object, Option<&str>)> = vec![
            (Object::Name(name("DCTDecode")), Some("DCTDecode")),
            (
                Object::Array(vec![
                    Object::Name(name("FlateDecode")),
                    Object::Name(name("DCTDecode")),
                ]),
                Some("DCTDecode"),
            ),
            (Object::Array(vec![Object::Null]), None),
            (Object::Int(3), None),
        ];
        for (filter, expected) in direct {
            let src = CountingSource {
                map: map.clone(),
                resolves: AtomicUsize::new(0),
            };
            let dict = make_dict(vec![("Filter", filter.clone())]);
            let got = block_on(trailing_filter_with(&src, &dict));
            assert_eq!(got.map(|n| n.0), expected.map(str::to_string));
            assert_eq!(
                src.resolves.load(Ordering::Relaxed),
                0,
                "direct value {filter:?} must not resolve"
            );
        }
        // The reference shapes still resolve — the fast path must not
        // have cost them their answer.
        for filter in [
            Object::Ref(ObjRef { num: 7, gen: 0 }),
            Object::Array(vec![Object::Ref(ObjRef { num: 7, gen: 0 })]),
        ] {
            let src = CountingSource {
                map: map.clone(),
                resolves: AtomicUsize::new(0),
            };
            let dict = make_dict(vec![("Filter", filter)]);
            let got = block_on(trailing_filter_with(&src, &dict));
            assert_eq!(got.map(|n| n.0), Some("JPXDecode".to_string()));
            assert!(src.resolves.load(Ordering::Relaxed) > 0);
        }
    }

    /// The gate (`trailing_filter_with`) and the decoder chase `/Filter`
    /// reference chains to the same depth. Before they shared the cap, a
    /// 9-hop chain sat exactly in the gap: deep enough that the decoder
    /// gave up (leniently returning the bytes still encoded), shallow
    /// enough that the gate resolved it and vouched for the stream —
    /// handing still-compressed bytes to a content parser with a clean
    /// result.
    #[test]
    fn deep_filter_ref_chains_decode_like_the_gate_reads_them() {
        use crate::source::{block_on, Immediate};

        let payload: &[u8] = b"BT (deep) Tj ET";
        let compressed = zlib(payload);
        // /Filter -> 1 0 R -> 2 0 R -> ... -> 9 0 R = /FlateDecode.
        let mut map = HashMap::new();
        for n in 1u32..9 {
            map.insert((n, 0u16), Object::Ref(ObjRef { num: n + 1, gen: 0 }));
        }
        map.insert((9, 0), Object::Name(name("FlateDecode")));
        let s = make_stream(
            vec![("Filter", Object::Ref(ObjRef { num: 1, gen: 0 }))],
            &compressed,
        );
        let src = Immediate(MapSource(map.clone()));
        let gate_sees = block_on(trailing_filter_with(&src, &s.dict));
        assert_eq!(
            gate_sees.map(|n| n.0),
            Some("FlateDecode".to_string()),
            "the gate resolves the chain and passes the stream"
        );
        let decoded = decode_stream(&s, &MapResolve(map)).expect("decode");
        assert_eq!(
            decoded, payload,
            "the decoder must chase /Filter as deep as the gate does"
        );
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
        let s = make_stream(
            vec![("Filter", Object::Name(name("NotAFilterDecode")))],
            b"x",
        );
        match decode_stream(&s, &NoResolve) {
            Err(Error::UnsupportedFilter(n)) => assert_eq!(n, "NotAFilterDecode"),
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
    fn jpx_passthrough_when_last() {
        // JPEG 2000 data stays encoded here and is decoded at the image
        // layer, exactly like DCTDecode (ISO 32000-1 7.4.9).
        let jp2 = b"\x00\x00\x00\x0cjP  \r\n\x87\n";
        let s = make_stream(vec![("Filter", Object::Name(name("JPXDecode")))], jp2);
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), jp2);
    }

    #[test]
    fn flate_then_jpx_passthrough() {
        // The passthrough survives an earlier stage: the chain runs
        // FlateDecode and hands the still-encoded codestream on.
        let jp2 = b"\x00\x00\x00\x0cjP  \r\n\x87\n";
        let s = make_stream(
            vec![(
                "Filter",
                Object::Array(vec![
                    Object::Name(name("FlateDecode")),
                    Object::Name(name("JPXDecode")),
                ]),
            )],
            &zlib(jp2),
        );
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), jp2);
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

    /// A `CCITTFaxDecode` stream over `data`. An empty `parms` omits
    /// `/DecodeParms` entirely, which is how the ISO 32000-1 Table 11 defaults
    /// are exercised.
    fn ccitt_stream(data: Vec<u8>, parms: Vec<(&str, Object)>) -> Stream {
        let mut entries = vec![("Filter", Object::Name(name("CCITTFaxDecode")))];
        if !parms.is_empty() {
            entries.push(("DecodeParms", Object::Dict(make_dict(parms))));
        }
        make_stream(entries, &data)
    }

    /// Decodes `bm` as a pure two-dimensional stream through the filter, with
    /// `extra` added to the parameter dictionary.
    fn decode_g4(bm: &Bitmap, extra: Vec<(&str, Object)>) -> Result<Vec<u8>> {
        let mut parms = vec![
            ("K", Object::Int(-1)),
            ("Columns", Object::Int(i64::from(bm.width()))),
            ("Rows", Object::Int(i64::from(bm.height()))),
        ];
        parms.extend(extra);
        decode_stream(&ccitt_stream(encode_g4(bm), parms), &NoResolve)
    }

    /// `/BlackIs1` is the fax filter's polarity switch, and it points the
    /// opposite way to `JBIG2Decode`'s unconditional inversion (ISO 32000-1
    /// Table 11). Both arms live in this file, so both directions are pinned
    /// here.
    ///
    /// The fixture's rows are `11110000` and `00001111` with black written as
    /// `1`. `/BlackIs1` false — the default — means a 0 bit is black in the
    /// decoded output, so the samples are the complement of those rows;
    /// `/BlackIs1` true means they are the rows themselves. Stating both by
    /// hand is what separates a missing inversion from a doubled one, since
    /// each alone produces a perfectly plausible image.
    #[test]
    fn ccitt_black_is_1_selects_the_output_polarity() {
        let bm = bitmap_from_rows(&["11110000", "00001111"]);
        let expected_default = vec![0b0000_1111, 0b1111_0000];
        let expected_black_is_1 = vec![0b1111_0000, 0b0000_1111];

        assert_eq!(
            decode_g4(&bm, vec![]).expect("decode"),
            expected_default,
            "the default is /BlackIs1 false: black is a 0 sample",
        );
        assert_eq!(
            decode_g4(&bm, vec![("BlackIs1", Object::Bool(false))]).expect("decode"),
            expected_default,
            "stating the default changes nothing",
        );
        assert_eq!(
            decode_g4(&bm, vec![("BlackIs1", Object::Bool(true))]).expect("decode"),
            expected_black_is_1,
            "/BlackIs1 keeps black as a 1 sample",
        );
    }

    /// An all-black row is the shortest statement of the polarity, and the one
    /// a reader can check without counting bits.
    #[test]
    fn ccitt_an_all_black_row_defaults_to_zero_samples() {
        let bm = bitmap_from_rows(&["11111111"]);
        assert_eq!(decode_g4(&bm, vec![]).expect("decode"), vec![0x00]);
        assert_eq!(
            decode_g4(&bm, vec![("BlackIs1", Object::Bool(true))]).expect("decode"),
            vec![0xFF],
        );
    }

    /// A row whose width is not a whole number of bytes is padded out to one,
    /// and the padding has to read as *white* under both polarities. Padding
    /// that inverts with the image grows a black stripe down the right edge of
    /// every fax page whose width is not a multiple of eight — which is most of
    /// them.
    #[test]
    fn ccitt_row_padding_reads_as_white_under_both_polarities() {
        let bm = bitmap_from_rows(&["1010"]);
        assert_eq!(
            decode_g4(&bm, vec![]).expect("decode"),
            vec![0b0101_1111],
            "black 1010, then four white padding bits, which are 1 by default",
        );
        assert_eq!(
            decode_g4(&bm, vec![("BlackIs1", Object::Bool(true))]).expect("decode"),
            vec![0b1010_0000],
            "and 0 under /BlackIs1, which is white there",
        );
    }

    /// `/CCF` is the abbreviation ISO 32000-1 §8.9.7 gives the filter inside an
    /// inline image, and it names the same codec.
    #[test]
    fn ccitt_abbreviated_filter_name_is_accepted() {
        let bm = bitmap_from_rows(&["11110000"]);
        let s = make_stream(
            vec![
                ("Filter", Object::Name(name("CCF"))),
                (
                    "DecodeParms",
                    Object::Dict(make_dict(vec![
                        ("K", Object::Int(-1)),
                        ("Columns", Object::Int(8)),
                        ("Rows", Object::Int(1)),
                        ("BlackIs1", Object::Bool(true)),
                    ])),
                ),
            ],
            &encode_g4(&bm),
        );
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), vec![0b1111_0000]);
    }

    /// Scanners routinely compress the coded bytes as well, so the filter has
    /// to work as the second stage of a chain, with its parameters at the
    /// matching index of the `/DecodeParms` array.
    #[test]
    fn flate_then_ccitt_decodes() {
        let bm = bitmap_from_rows(&["11110000", "00001111"]);
        let parms = make_dict(vec![
            ("K", Object::Int(-1)),
            ("Columns", Object::Int(8)),
            ("Rows", Object::Int(2)),
            ("BlackIs1", Object::Bool(true)),
        ]);
        let s = make_stream(
            vec![
                (
                    "Filter",
                    Object::Array(vec![
                        Object::Name(name("FlateDecode")),
                        Object::Name(name("CCITTFaxDecode")),
                    ]),
                ),
                (
                    "DecodeParms",
                    Object::Array(vec![Object::Null, Object::Dict(parms)]),
                ),
            ],
            &zlib(&encode_g4(&bm)),
        );
        assert_eq!(
            decode_stream(&s, &NoResolve).unwrap(),
            vec![0b1111_0000, 0b0000_1111],
        );
    }

    /// With no `/DecodeParms` at all every Table 11 default applies: `/K` 0, so
    /// pure one-dimensional coding; `/Columns` 1728; `/Rows` 0, so the height
    /// comes from the data; `/BlackIs1` false; `/EncodedByteAlign` false;
    /// `/EndOfLine` false.
    #[test]
    fn ccitt_defaults_match_the_specification() {
        let mut bm = Bitmap::new(1728, 3).expect("fixture");
        for y in 0..3 {
            for x in 0..8 {
                bm.set(x, y, 1);
            }
        }
        let out = decode_stream(&ccitt_stream(encode_g3_1d(&bm), vec![]), &NoResolve)
            .expect("decode with every default");
        let stride = 1728 / 8;
        assert_eq!(out.len(), stride * 3, "1728 columns, three rows inferred");
        for y in 0..3 {
            let row = &out[y * stride..(y + 1) * stride];
            assert_eq!(row[0], 0x00, "row {y} starts with eight black samples");
            assert!(
                row[1..].iter().all(|b| *b == 0xFF),
                "row {y} is white after them",
            );
        }
    }

    /// `/K` 0 with `/EncodedByteAlign` — the other end of the parameter space
    /// from the pure two-dimensional case, and the combination a fax machine's
    /// own output takes.
    #[test]
    fn ccitt_one_dimensional_byte_aligned_rows_decode() {
        let bm = bitmap_from_rows(&["11110000", "00111100", "00001111"]);
        let s = ccitt_stream(
            encode_g3_1d_byte_aligned(&bm),
            vec![
                ("K", Object::Int(0)),
                ("Columns", Object::Int(8)),
                ("Rows", Object::Int(3)),
                ("EncodedByteAlign", Object::Bool(true)),
                ("BlackIs1", Object::Bool(true)),
            ],
        );
        assert_eq!(
            decode_stream(&s, &NoResolve).unwrap(),
            vec![0b1111_0000, 0b0011_1100, 0b0000_1111],
        );
    }

    /// `/Rows` 0 — the default — means the height is however many rows the data
    /// holds.
    #[test]
    fn ccitt_an_unstated_row_count_is_inferred_from_the_data() {
        let bm = bitmap_from_rows(&["11110000"; 5]);
        let s = ccitt_stream(
            encode_g4(&bm),
            vec![
                ("K", Object::Int(-1)),
                ("Columns", Object::Int(8)),
                ("BlackIs1", Object::Bool(true)),
            ],
        );
        assert_eq!(decode_stream(&s, &NoResolve).unwrap(), vec![0b1111_0000; 5]);
    }

    /// A row of no pixels is not a narrow image, it is not an image.
    #[test]
    fn ccitt_zero_columns_is_a_decode_error() {
        let s = ccitt_stream(vec![0u8; 8], vec![("Columns", Object::Int(0))]);
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::Decode(_))
        ));
    }

    /// `/Rows` counts rows, so a negative value describes nothing.
    #[test]
    fn ccitt_a_negative_row_count_is_a_decode_error() {
        let s = ccitt_stream(
            vec![0u8; 8],
            vec![("Columns", Object::Int(8)), ("Rows", Object::Int(-1))],
        );
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::Decode(_))
        ));
    }

    /// Both dimensions come from the file, so both are refused from the
    /// declared values before any allocation is attempted — eight bytes of
    /// input must not be able to ask for a hundred-gigapixel bitmap.
    #[test]
    fn ccitt_an_image_past_the_allocation_cap_is_refused() {
        let s = ccitt_stream(
            vec![0u8; 8],
            vec![
                ("K", Object::Int(-1)),
                ("Columns", Object::Int(100_000)),
                ("Rows", Object::Int(100_000)),
            ],
        );
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::Decode(_))
        ));
    }

    /// A single row wider than the whole allocation cap is refused too, rather
    /// than quietly yielding an image of no rows.
    #[test]
    fn ccitt_a_row_wider_than_the_allocation_cap_is_refused() {
        let s = ccitt_stream(
            vec![0u8; 8],
            vec![("K", Object::Int(-1)), ("Columns", Object::Int(1 << 28))],
        );
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::Decode(_))
        ));
    }

    /// An image can be short enough that its area is unremarkable and still be
    /// far too wide to decode, because what a width buys is not area: it is the
    /// decoder's row state, at eight bytes per column against the bitmap's one
    /// byte per pixel. These dimensions multiply to exactly the allocation cap,
    /// so the area test passes them; the per-side cap is what refuses them.
    #[test]
    fn ccitt_a_wide_short_image_inside_the_allocation_cap_is_still_refused() {
        let s = ccitt_stream(
            vec![0u8; 8],
            vec![
                ("K", Object::Int(-1)),
                ("Columns", Object::Int(1 << 26)),
                ("Rows", Object::Int(2)),
            ],
        );
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::Decode(_))
        ));
    }

    /// The mirror shape, which the area test passes for the same reason and
    /// which no stream needs to send: a column of pixels a hundred and
    /// thirty-four million rows tall. Its packed output is eight times its
    /// area, since every row pads seven bits to reach a byte, and an empty
    /// stream is enough to ask for it.
    #[test]
    fn ccitt_a_tall_narrow_image_inside_the_allocation_cap_is_still_refused() {
        let s = ccitt_stream(
            Vec::new(),
            vec![
                ("K", Object::Int(-1)),
                ("Columns", Object::Int(1)),
                ("Rows", Object::Int(1 << 27)),
            ],
        );
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::Decode(_))
        ));
    }

    /// Unlike `JBIG2Decode` and `DCTDecode`, the fax filter reads nothing but
    /// the bytes handed to it, so it is not confined to the end of the chain.
    /// A chain that puts a stage after it therefore fails in *that* stage,
    /// rather than the fax filter being reported as an unsupported filter it is
    /// not.
    #[test]
    fn ccitt_before_another_filter_fails_in_the_later_stage() {
        let bm = bitmap_from_rows(&["11110000"]);
        let s = make_stream(
            vec![
                (
                    "Filter",
                    Object::Array(vec![
                        Object::Name(name("CCITTFaxDecode")),
                        Object::Name(name("FlateDecode")),
                    ]),
                ),
                (
                    "DecodeParms",
                    Object::Array(vec![
                        Object::Dict(make_dict(vec![
                            ("K", Object::Int(-1)),
                            ("Columns", Object::Int(8)),
                            ("Rows", Object::Int(1)),
                        ])),
                        Object::Null,
                    ]),
                ),
            ],
            &encode_g4(&bm),
        );
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::Decode(_)),
        ));
    }

    /// Corruption inside a stream that has not run out is an error, not a
    /// guess: a run length that would leave the row it is in cannot be
    /// honoured, and honouring it would write over the row below.
    #[test]
    fn ccitt_a_run_past_the_row_end_is_a_decode_error() {
        // Horizontal mode, then a white run of 1728 and a black run of 0, in a
        // row ten pixels wide. The trailing zero bits are there so the failure
        // is read as corruption rather than as the data running out mid-code.
        let mut bits = Vec::new();
        push_mode(&mut bits, Mode::Horizontal);
        push_run(&mut bits, true, 1728);
        push_run(&mut bits, false, 0);
        bits.extend(std::iter::repeat_n(0u8, 24));
        let s = ccitt_stream(
            pack(&bits),
            vec![
                ("K", Object::Int(-1)),
                ("Columns", Object::Int(10)),
                ("Rows", Object::Int(1)),
            ],
        );
        assert!(matches!(
            decode_stream(&s, &NoResolve),
            Err(Error::Decode(_))
        ));
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
