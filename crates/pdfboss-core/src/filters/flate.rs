//! FlateDecode: zlib/deflate decompression, tolerant of trailing junk and
//! truncated data, followed by an optional predictor post-pass.

use crate::error::{Error, Result};
use crate::filters::{is_pdf_whitespace, predictor, MAX_DECODED_LEN};
use crate::object::Dict;
use flate2::{Decompress, FlushDecompress, Status};

/// Decodes FlateDecode data; `parms` is the resolved `/DecodeParms`
/// dictionary, if any (predictor parameters).
///
/// Malformed real-world streams are tolerated: junk after the compressed
/// stream is ignored, and truncated or mid-stream-corrupted data yields
/// whatever prefix decoded cleanly.
pub fn decode(data: &[u8], parms: Option<&Dict>) -> Result<Vec<u8>> {
    let inflated = inflate_tolerant(data)?;
    predictor::post_pass(inflated, parms)
}

/// Checks for a plausible zlib header: compression method 8, window size
/// within spec, and a valid header checksum (ISO/RFC 1950).
fn has_zlib_header(data: &[u8]) -> bool {
    data.len() >= 2
        && data[0] & 0x0f == 8
        && data[0] >> 4 <= 7
        && (u16::from(data[0]) << 8 | u16::from(data[1])) % 31 == 0
}

fn inflate_tolerant(data: &[u8]) -> Result<Vec<u8>> {
    let mut input = data;
    // Some writers leave stray EOL bytes before the compressed data; only
    // skip them when doing so uncovers a valid zlib header.
    if !has_zlib_header(input) {
        let skip = input.iter().take_while(|&&b| is_pdf_whitespace(b)).count();
        if skip > 0 && has_zlib_header(&input[skip..]) {
            input = &input[skip..];
        }
    }
    if input.is_empty() {
        return Ok(Vec::new());
    }
    let zlib_first = has_zlib_header(input);
    let (out, clean) = inflate(input, zlib_first)?;
    if clean || !out.is_empty() {
        return Ok(out);
    }
    // Nothing decoded: retry assuming the other framing (headerless raw
    // deflate streams do occur in the wild).
    let (out, clean) = inflate(input, !zlib_first)?;
    if clean || !out.is_empty() {
        return Ok(out);
    }
    Err(Error::Decode("flate: no decodable data".into()))
}

/// Inflates as much of `data` as possible. The returned flag is true when
/// the compressed stream ended cleanly (trailing junk is fine); false means
/// truncation or corruption, with the decoded prefix returned. Output
/// larger than `MAX_DECODED_LEN` (a decompression bomb) is an error.
fn inflate(data: &[u8], zlib_header: bool) -> Result<(Vec<u8>, bool)> {
    let mut inflater = Decompress::new(zlib_header);
    let mut out: Vec<u8> = Vec::with_capacity(data.len().saturating_mul(2).clamp(1024, 1 << 22));
    loop {
        let consumed = (inflater.total_in() as usize).min(data.len());
        let in_before = inflater.total_in();
        let out_before = out.len();
        match inflater.decompress_vec(&data[consumed..], &mut out, FlushDecompress::None) {
            Ok(Status::StreamEnd) => return Ok((out, true)),
            Ok(_) => {
                if out.len() == out.capacity() {
                    // Output buffer exhausted: grow (bounded) and continue.
                    if out.len() >= MAX_DECODED_LEN {
                        return Err(Error::Decode(
                            "flate: decoded stream exceeds size limit".into(),
                        ));
                    }
                    let grow = out.capacity().max(1024).min(MAX_DECODED_LEN - out.len());
                    out.reserve(grow);
                    continue;
                }
                if inflater.total_in() == in_before && out.len() == out_before {
                    // Wants more input than we have: truncated stream.
                    return Ok((out, false));
                }
            }
            Err(_) => return Ok((out, false)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::object::{Name, Object};
    use flate2::write::{DeflateEncoder, ZlibEncoder};
    use flate2::Compression;
    use std::io::Write;

    fn zlib(data: &[u8]) -> Vec<u8> {
        let mut enc = ZlibEncoder::new(Vec::new(), Compression::default());
        enc.write_all(data).unwrap();
        enc.finish().unwrap()
    }

    fn sample_text(repeats: usize) -> Vec<u8> {
        b"the quick brown fox jumps over the lazy dog. "
            .iter()
            .copied()
            .cycle()
            .take(repeats)
            .collect()
    }

    #[test]
    fn round_trips_zlib_data() {
        let text = sample_text(4096);
        assert_eq!(decode(&zlib(&text), None).unwrap(), text);
    }

    #[test]
    fn tolerates_trailing_junk() {
        let text = sample_text(512);
        let mut stored = zlib(&text);
        stored.extend_from_slice(b"\r\nendstream junk that is not deflate");
        assert_eq!(decode(&stored, None).unwrap(), text);
    }

    #[test]
    fn truncated_data_returns_decoded_prefix() {
        let text = sample_text(50_000);
        let compressed = zlib(&text);
        let cut = &compressed[..compressed.len() / 2];
        let out = decode(cut, None).unwrap();
        assert!(!out.is_empty());
        assert!(out.len() < text.len());
        assert_eq!(&text[..out.len()], &out[..]);
    }

    #[test]
    fn accepts_headerless_raw_deflate() {
        let text = sample_text(600);
        let mut enc = DeflateEncoder::new(Vec::new(), Compression::default());
        enc.write_all(&text).unwrap();
        let raw = enc.finish().unwrap();
        assert_eq!(decode(&raw, None).unwrap(), text);
    }

    #[test]
    fn skips_leading_whitespace_before_zlib_header() {
        let text = sample_text(100);
        let mut stored = b"\r\n".to_vec();
        stored.extend_from_slice(&zlib(&text));
        assert_eq!(decode(&stored, None).unwrap(), text);
    }

    #[test]
    fn empty_input_decodes_to_empty() {
        assert!(decode(&[], None).unwrap().is_empty());
    }

    #[test]
    fn garbage_input_is_an_error() {
        let garbage = [0xffu8; 16];
        assert!(matches!(decode(&garbage, None), Err(Error::Decode(_))));
    }

    #[test]
    fn png_predictor_post_pass_applies() {
        // Two rows of 3 bytes, Sub filter (type 1), colors=1 bpc=8.
        let raw = [1u8, 2, 3, 255, 1, 3];
        let filtered = [1u8, 1, 1, 1, 1, 255, 2, 2];
        let mut parms = Dict::new();
        parms.insert(Name("Predictor".into()), Object::Int(11));
        parms.insert(Name("Columns".into()), Object::Int(3));
        assert_eq!(decode(&zlib(&filtered), Some(&parms)).unwrap(), raw);
    }

    #[test]
    fn tiff_predictor_post_pass_applies() {
        let raw = [5u8, 7, 9, 11];
        let diffed = [5u8, 2, 2, 2];
        let mut parms = Dict::new();
        parms.insert(Name("Predictor".into()), Object::Int(2));
        parms.insert(Name("Columns".into()), Object::Int(4));
        assert_eq!(decode(&zlib(&diffed), Some(&parms)).unwrap(), raw);
    }
}
