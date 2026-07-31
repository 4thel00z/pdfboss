//! End-to-end fixture harness for the committed zoo (tests/fixtures).
//!
//! Comparison rules (design contract):
//! - reversible 5-3 cases: EXACT match against `<name>.src.png`;
//! - irreversible 9-7 cases: within +/-2 per sample AND PSNR >= 38 dB
//!   against `<name>.indep.png` (an independent decode of the same file).
//!
//! The per-case tests ran `#[ignore]`d until the decoder pipeline was
//! wired; the orchestration stage removed the ignores and nothing else.
//! The support code below (JSON manifest parsing, PNG oracle reading,
//! comparison metrics) is fully functional and self-tested by the
//! harness self-tests, so it stays untouched.

use pdfboss_jpx::{decode, DecodeLimits, JpxError};
use std::path::{Path, PathBuf};

fn fixture_dir() -> PathBuf {
    PathBuf::from(concat!(env!("CARGO_MANIFEST_DIR"), "/tests/fixtures"))
}

// ---------------------------------------------------------------------
// Minimal JSON reader for manifest.json (test-support; panics on bad
// input, which would mean a corrupted fixture checkout).
// ---------------------------------------------------------------------

// The Null/Bool payloads keep the parser complete even though the current
// manifest never uses them (its booleans are the strings "True"/"False").
#[allow(dead_code)]
#[derive(Debug)]
enum Json {
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    Arr(Vec<Json>),
    Obj(Vec<(String, Json)>),
}

impl Json {
    fn get(&self, key: &str) -> &Json {
        match self {
            Json::Obj(fields) => fields
                .iter()
                .find(|(name, _)| name == key)
                .map(|(_, value)| value)
                .unwrap_or_else(|| panic!("manifest object lacks key {key:?}")),
            other => panic!("expected object with key {key:?}, got {other:?}"),
        }
    }

    fn as_str(&self) -> &str {
        match self {
            Json::Str(text) => text,
            other => panic!("expected string, got {other:?}"),
        }
    }

    fn as_u32(&self) -> u32 {
        match self {
            Json::Num(value) => *value as u32,
            other => panic!("expected number, got {other:?}"),
        }
    }

    fn as_arr(&self) -> &[Json] {
        match self {
            Json::Arr(items) => items,
            other => panic!("expected array, got {other:?}"),
        }
    }
}

fn parse_json(text: &str) -> Json {
    let bytes = text.as_bytes();
    let mut pos = 0usize;
    let value = parse_value(bytes, &mut pos);
    skip_ws(bytes, &mut pos);
    assert_eq!(pos, bytes.len(), "trailing bytes after JSON document");
    value
}

fn skip_ws(bytes: &[u8], pos: &mut usize) {
    while *pos < bytes.len() && matches!(bytes[*pos], b' ' | b'\t' | b'\n' | b'\r') {
        *pos += 1;
    }
}

fn parse_value(bytes: &[u8], pos: &mut usize) -> Json {
    skip_ws(bytes, pos);
    match bytes.get(*pos) {
        Some(b'{') => {
            *pos += 1;
            let mut fields = Vec::new();
            skip_ws(bytes, pos);
            if bytes.get(*pos) == Some(&b'}') {
                *pos += 1;
                return Json::Obj(fields);
            }
            loop {
                skip_ws(bytes, pos);
                let key = match parse_value(bytes, pos) {
                    Json::Str(key) => key,
                    other => panic!("object key must be a string, got {other:?}"),
                };
                skip_ws(bytes, pos);
                assert_eq!(bytes[*pos], b':', "expected ':' in object");
                *pos += 1;
                fields.push((key, parse_value(bytes, pos)));
                skip_ws(bytes, pos);
                match bytes[*pos] {
                    b',' => *pos += 1,
                    b'}' => {
                        *pos += 1;
                        return Json::Obj(fields);
                    }
                    other => panic!("unexpected byte {other} in object"),
                }
            }
        }
        Some(b'[') => {
            *pos += 1;
            let mut items = Vec::new();
            skip_ws(bytes, pos);
            if bytes.get(*pos) == Some(&b']') {
                *pos += 1;
                return Json::Arr(items);
            }
            loop {
                items.push(parse_value(bytes, pos));
                skip_ws(bytes, pos);
                match bytes[*pos] {
                    b',' => *pos += 1,
                    b']' => {
                        *pos += 1;
                        return Json::Arr(items);
                    }
                    other => panic!("unexpected byte {other} in array"),
                }
            }
        }
        Some(b'"') => {
            *pos += 1;
            let mut text = String::new();
            loop {
                match bytes[*pos] {
                    b'"' => {
                        *pos += 1;
                        return Json::Str(text);
                    }
                    b'\\' => {
                        *pos += 1;
                        let escape = bytes[*pos];
                        *pos += 1;
                        match escape {
                            b'"' => text.push('"'),
                            b'\\' => text.push('\\'),
                            b'/' => text.push('/'),
                            b'n' => text.push('\n'),
                            b't' => text.push('\t'),
                            b'r' => text.push('\r'),
                            b'b' => text.push('\u{8}'),
                            b'f' => text.push('\u{C}'),
                            b'u' => {
                                let hex = std::str::from_utf8(&bytes[*pos..*pos + 4]).unwrap();
                                *pos += 4;
                                let code = u32::from_str_radix(hex, 16).unwrap();
                                text.push(char::from_u32(code).expect("surrogate in manifest"));
                            }
                            other => panic!("unsupported escape {other}"),
                        }
                    }
                    _ => {
                        let start = *pos;
                        while !matches!(bytes[*pos], b'"' | b'\\') {
                            *pos += 1;
                        }
                        text.push_str(std::str::from_utf8(&bytes[start..*pos]).unwrap());
                    }
                }
            }
        }
        Some(b't') => {
            assert_eq!(&bytes[*pos..*pos + 4], b"true");
            *pos += 4;
            Json::Bool(true)
        }
        Some(b'f') => {
            assert_eq!(&bytes[*pos..*pos + 5], b"false");
            *pos += 5;
            Json::Bool(false)
        }
        Some(b'n') => {
            assert_eq!(&bytes[*pos..*pos + 4], b"null");
            *pos += 4;
            Json::Null
        }
        Some(_) => {
            let start = *pos;
            while *pos < bytes.len()
                && matches!(bytes[*pos], b'0'..=b'9' | b'-' | b'+' | b'.' | b'e' | b'E')
            {
                *pos += 1;
            }
            let text = std::str::from_utf8(&bytes[start..*pos]).unwrap();
            Json::Num(
                text.parse()
                    .unwrap_or_else(|e| panic!("bad number {text:?}: {e}")),
            )
        }
        None => panic!("unexpected end of JSON"),
    }
}

// ---------------------------------------------------------------------
// Manifest model.
// ---------------------------------------------------------------------

struct Case {
    name: String,
    file: String,
    source: String,
    mode: String,
    width: u32,
    height: u32,
    irreversible: bool,
}

fn load_manifest() -> Vec<Case> {
    let text = std::fs::read_to_string(fixture_dir().join("manifest.json")).unwrap();
    parse_json(&text)
        .as_arr()
        .iter()
        .map(|entry| {
            let size = entry.get("size").as_arr();
            let irreversible = match entry.get("params").get("irreversible").as_str() {
                "True" => true,
                "False" => false,
                other => panic!("unexpected irreversible flag {other:?}"),
            };
            Case {
                name: entry.get("name").as_str().to_owned(),
                file: entry.get("file").as_str().to_owned(),
                source: entry.get("source").as_str().to_owned(),
                mode: entry.get("mode").as_str().to_owned(),
                width: size[0].as_u32(),
                height: size[1].as_u32(),
                irreversible,
            }
        })
        .collect()
}

fn case_by_name(name: &str) -> Case {
    load_manifest()
        .into_iter()
        .find(|case| case.name == name)
        .unwrap_or_else(|| panic!("case {name:?} missing from manifest.json"))
}

/// Channel count implied by the manifest's source-image mode.
fn expected_channels(mode: &str) -> u8 {
    match mode {
        "L" | "I;16" => 1,
        "RGB" => 3,
        "RGBA" => 4,
        other => panic!("unexpected manifest mode {other:?}"),
    }
}

// ---------------------------------------------------------------------
// Minimal PNG reader for the oracle images (test-support): 8/16-bit,
// colour types 0 (grey), 2 (RGB), 6 (RGBA), no interlace. 16-bit samples
// are normalized to 8 bits by dropping the low byte — exactly the crate's
// "right-shift 16-bit sources to 8" output contract, so oracle and
// decoder output stay directly comparable.
// ---------------------------------------------------------------------

struct Oracle {
    width: u32,
    height: u32,
    channels: u8,
    /// 8-bit samples, interleaved, row-major.
    samples: Vec<u8>,
}

fn read_oracle(path: &Path) -> Oracle {
    let data = std::fs::read(path).unwrap_or_else(|e| panic!("read {path:?}: {e}"));
    decode_png(&data)
}

fn be32(bytes: &[u8]) -> u32 {
    u32::from(bytes[0]) << 24
        | u32::from(bytes[1]) << 16
        | u32::from(bytes[2]) << 8
        | u32::from(bytes[3])
}

fn decode_png(data: &[u8]) -> Oracle {
    assert_eq!(
        &data[..8],
        &[137, 80, 78, 71, 13, 10, 26, 10],
        "PNG signature"
    );
    let mut pos = 8usize;
    let mut width = 0u32;
    let mut height = 0u32;
    let mut bit_depth = 0u8;
    let mut channels = 0u8;
    let mut idat = Vec::new();
    loop {
        let len = be32(&data[pos..]) as usize;
        let kind = &data[pos + 4..pos + 8];
        let payload = &data[pos + 8..pos + 8 + len];
        pos += 12 + len; // length + type + payload + crc
        match kind {
            b"IHDR" => {
                width = be32(&payload[0..]);
                height = be32(&payload[4..]);
                bit_depth = payload[8];
                channels = match payload[9] {
                    0 => 1,
                    2 => 3,
                    6 => 4,
                    other => panic!("unsupported PNG colour type {other}"),
                };
                assert!(bit_depth == 8 || bit_depth == 16, "unsupported bit depth");
                assert_eq!(payload[10], 0, "compression method");
                assert_eq!(payload[11], 0, "filter method");
                assert_eq!(payload[12], 0, "interlaced oracle not supported");
            }
            b"IDAT" => idat.extend_from_slice(payload),
            b"IEND" => break,
            _ => {}
        }
    }
    let raw = zlib_inflate(&idat);
    let bytes_per_sample = usize::from(bit_depth / 8);
    let bpp = usize::from(channels) * bytes_per_sample;
    let recon = defilter(&raw, width as usize, height as usize, bpp);
    let samples = if bit_depth == 8 {
        recon
    } else {
        // Big-endian 16-bit: the high byte IS the value >> 8.
        recon.iter().step_by(2).copied().collect()
    };
    assert_eq!(
        samples.len(),
        width as usize * height as usize * usize::from(channels)
    );
    Oracle {
        width,
        height,
        channels,
        samples,
    }
}

fn defilter(raw: &[u8], width: usize, height: usize, bpp: usize) -> Vec<u8> {
    let stride = width * bpp;
    assert_eq!(raw.len(), height * (stride + 1), "decompressed size");
    let mut out = vec![0u8; height * stride];
    for row in 0..height {
        let filter = raw[row * (stride + 1)];
        let line = &raw[row * (stride + 1) + 1..(row + 1) * (stride + 1)];
        for i in 0..stride {
            let a = if i >= bpp {
                out[row * stride + i - bpp]
            } else {
                0
            };
            let b = if row > 0 {
                out[(row - 1) * stride + i]
            } else {
                0
            };
            let c = if row > 0 && i >= bpp {
                out[(row - 1) * stride + i - bpp]
            } else {
                0
            };
            let recon = match filter {
                0 => line[i],
                1 => line[i].wrapping_add(a),
                2 => line[i].wrapping_add(b),
                3 => line[i].wrapping_add(((u16::from(a) + u16::from(b)) / 2) as u8),
                4 => line[i].wrapping_add(paeth(a, b, c)),
                other => panic!("bad PNG filter {other}"),
            };
            out[row * stride + i] = recon;
        }
    }
    out
}

fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let p = i32::from(a) + i32::from(b) - i32::from(c);
    let pa = (p - i32::from(a)).abs();
    let pb = (p - i32::from(b)).abs();
    let pc = (p - i32::from(c)).abs();
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

// ---------------------------------------------------------------------
// zlib / deflate (RFC 1950/1951) — enough to read the oracle PNGs.
// ---------------------------------------------------------------------

fn zlib_inflate(data: &[u8]) -> Vec<u8> {
    assert!(data.len() > 6, "zlib stream too short");
    assert_eq!(data[0] & 15, 8, "zlib compression method");
    assert_eq!(
        (u32::from(data[0]) * 256 + u32::from(data[1])) % 31,
        0,
        "zlib header check"
    );
    assert_eq!(data[1] & 32, 0, "preset dictionary unsupported");
    let out = inflate(&data[2..]);
    let trailer = be32(&data[data.len() - 4..]);
    assert_eq!(adler32(&out), trailer, "adler32 mismatch");
    out
}

fn adler32(data: &[u8]) -> u32 {
    let mut s1: u32 = 1;
    let mut s2: u32 = 0;
    for &byte in data {
        s1 = (s1 + u32::from(byte)) % 65521;
        s2 = (s2 + s1) % 65521;
    }
    (s2 << 16) | s1
}

struct Bits<'a> {
    data: &'a [u8],
    pos: usize,
    buffer: u32,
    count: u32,
}

impl<'a> Bits<'a> {
    fn new(data: &'a [u8]) -> Self {
        Bits {
            data,
            pos: 0,
            buffer: 0,
            count: 0,
        }
    }

    /// Takes `count` bits LSB-first (deflate bit order).
    fn take(&mut self, count: u32) -> u32 {
        while self.count < count {
            let byte = self.data[self.pos];
            self.pos += 1;
            self.buffer |= u32::from(byte) << self.count;
            self.count += 8;
        }
        let value = self.buffer & ((1u32 << count) - 1);
        self.buffer >>= count;
        self.count -= count;
        value
    }

    /// Drops the partial bits of the current byte (stored-block alignment).
    fn align_byte(&mut self) {
        let drop = self.count % 8;
        self.buffer >>= drop;
        self.count -= drop;
    }
}

/// Canonical Huffman decoder: counts-per-length plus symbols sorted by
/// (length, symbol).
struct Huffman {
    counts: [u16; 16],
    symbols: Vec<u16>,
}

impl Huffman {
    fn new(lengths: &[u8]) -> Huffman {
        let mut counts = [0u16; 16];
        for &len in lengths {
            counts[usize::from(len)] += 1;
        }
        counts[0] = 0;
        let mut offsets = [0u16; 16];
        for len in 1..16 {
            offsets[len] = offsets[len - 1] + counts[len - 1];
        }
        let mut symbols = vec![0u16; lengths.iter().filter(|&&len| len != 0).count()];
        for (symbol, &len) in lengths.iter().enumerate() {
            if len != 0 {
                symbols[usize::from(offsets[usize::from(len)])] = symbol as u16;
                offsets[usize::from(len)] += 1;
            }
        }
        Huffman { counts, symbols }
    }

    fn decode(&self, bits: &mut Bits<'_>) -> u16 {
        let mut code = 0u32;
        let mut first = 0u32;
        let mut index = 0u32;
        for len in 1..16 {
            code |= bits.take(1);
            let count = u32::from(self.counts[len]);
            if code < first + count {
                return self.symbols[(index + code - first) as usize];
            }
            index += count;
            first = (first + count) << 1;
            code <<= 1;
        }
        panic!("invalid huffman code in oracle PNG");
    }
}

fn inflate(data: &[u8]) -> Vec<u8> {
    let mut bits = Bits::new(data);
    let mut out = Vec::new();
    loop {
        let bfinal = bits.take(1);
        match bits.take(2) {
            0 => {
                bits.align_byte();
                let len = bits.take(16) as usize;
                let nlen = bits.take(16) as usize;
                assert_eq!(len + nlen, 65535, "stored block length check");
                for _ in 0..len {
                    out.push(bits.take(8) as u8);
                }
            }
            1 => {
                let mut lengths = vec![0u8; 288];
                for (symbol, len) in lengths.iter_mut().enumerate() {
                    *len = match symbol {
                        0..=143 => 8,
                        144..=255 => 9,
                        256..=279 => 7,
                        _ => 8,
                    };
                }
                let lit = Huffman::new(&lengths);
                let dist = Huffman::new(&[5u8; 32]);
                inflate_block(&mut bits, &mut out, &lit, &dist);
            }
            2 => {
                let (lit, dist) = dynamic_trees(&mut bits);
                inflate_block(&mut bits, &mut out, &lit, &dist);
            }
            other => panic!("reserved deflate block type {other}"),
        }
        if bfinal == 1 {
            return out;
        }
    }
}

fn dynamic_trees(bits: &mut Bits<'_>) -> (Huffman, Huffman) {
    let hlit = bits.take(5) as usize + 257;
    let hdist = bits.take(5) as usize + 1;
    let hclen = bits.take(4) as usize + 4;
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let mut code_lengths = [0u8; 19];
    for &slot in ORDER.iter().take(hclen) {
        code_lengths[slot] = bits.take(3) as u8;
    }
    let cl_tree = Huffman::new(&code_lengths);
    let mut lengths = vec![0u8; hlit + hdist];
    let mut i = 0usize;
    while i < lengths.len() {
        match cl_tree.decode(bits) {
            symbol @ 0..=15 => {
                lengths[i] = symbol as u8;
                i += 1;
            }
            16 => {
                let previous = lengths[i - 1];
                for _ in 0..3 + bits.take(2) {
                    lengths[i] = previous;
                    i += 1;
                }
            }
            17 => i += 3 + bits.take(3) as usize,
            18 => i += 11 + bits.take(7) as usize,
            other => panic!("bad code-length symbol {other}"),
        }
    }
    (
        Huffman::new(&lengths[..hlit]),
        Huffman::new(&lengths[hlit..]),
    )
}

fn inflate_block(bits: &mut Bits<'_>, out: &mut Vec<u8>, lit: &Huffman, dist: &Huffman) {
    const LEN_BASE: [u16; 29] = [
        3, 4, 5, 6, 7, 8, 9, 10, 11, 13, 15, 17, 19, 23, 27, 31, 35, 43, 51, 59, 67, 83, 99, 115,
        131, 163, 195, 227, 258,
    ];
    const LEN_EXTRA: [u8; 29] = [
        0, 0, 0, 0, 0, 0, 0, 0, 1, 1, 1, 1, 2, 2, 2, 2, 3, 3, 3, 3, 4, 4, 4, 4, 5, 5, 5, 5, 0,
    ];
    const DIST_BASE: [u16; 30] = [
        1, 2, 3, 4, 5, 7, 9, 13, 17, 25, 33, 49, 65, 97, 129, 193, 257, 385, 513, 769, 1025, 1537,
        2049, 3073, 4097, 6145, 8193, 12289, 16385, 24577,
    ];
    const DIST_EXTRA: [u8; 30] = [
        0, 0, 0, 0, 1, 1, 2, 2, 3, 3, 4, 4, 5, 5, 6, 6, 7, 7, 8, 8, 9, 9, 10, 10, 11, 11, 12, 12,
        13, 13,
    ];
    loop {
        let symbol = lit.decode(bits);
        match symbol {
            256 => return,
            0..=255 => out.push(symbol as u8),
            257..=285 => {
                let slot = usize::from(symbol) - 257;
                let run =
                    usize::from(LEN_BASE[slot]) + bits.take(u32::from(LEN_EXTRA[slot])) as usize;
                let dslot = usize::from(dist.decode(bits));
                let distance = usize::from(DIST_BASE[dslot])
                    + bits.take(u32::from(DIST_EXTRA[dslot])) as usize;
                let start = out.len().checked_sub(distance).expect("distance too far");
                for offset in 0..run {
                    let byte = out[start + offset];
                    out.push(byte);
                }
            }
            other => panic!("bad literal/length symbol {other}"),
        }
    }
}

// ---------------------------------------------------------------------
// Comparison metrics.
// ---------------------------------------------------------------------

fn max_abs_diff(a: &[u8], b: &[u8]) -> u8 {
    a.iter()
        .zip(b)
        .map(|(&x, &y)| x.abs_diff(y))
        .max()
        .unwrap_or(0)
}

fn psnr(a: &[u8], b: &[u8]) -> f64 {
    assert_eq!(a.len(), b.len());
    let sum_sq: f64 = a
        .iter()
        .zip(b)
        .map(|(&x, &y)| {
            let diff = f64::from(x) - f64::from(y);
            diff * diff
        })
        .sum();
    if sum_sq == 0.0 {
        return f64::INFINITY;
    }
    let mse = sum_sq / a.len() as f64;
    10.0 * (255.0 * 255.0 / mse).log10()
}

// ---------------------------------------------------------------------
// The end-to-end case runner.
// ---------------------------------------------------------------------

fn run_case(name: &str) {
    let case = case_by_name(name);
    let data = std::fs::read(fixture_dir().join(&case.file)).unwrap();
    let image =
        decode(&data, &DecodeLimits::default()).unwrap_or_else(|e| panic!("decode {name}: {e}"));
    assert_eq!(
        (image.width, image.height),
        (case.width, case.height),
        "{name}: dimensions"
    );
    assert_eq!(
        image.components,
        expected_channels(&case.mode),
        "{name}: component count"
    );
    if case.irreversible {
        // 9-7 path: match the independent decode within +/-2 per sample
        // and PSNR >= 38 dB.
        let oracle = read_oracle(&fixture_dir().join(format!("{name}.indep.png")));
        assert_eq!((oracle.width, oracle.height), (case.width, case.height));
        assert_eq!(oracle.channels, image.components);
        let diff = max_abs_diff(&image.samples, &oracle.samples);
        assert!(diff <= 2, "{name}: max per-sample diff {diff} > 2");
        let quality = psnr(&image.samples, &oracle.samples);
        assert!(quality >= 38.0, "{name}: PSNR {quality:.2} < 38");
    } else {
        // 5-3 path: bit-exact against the encoder's source image.
        let oracle = read_oracle(&fixture_dir().join(&case.source));
        assert_eq!((oracle.width, oracle.height), (case.width, case.height));
        assert_eq!(oracle.channels, image.components);
        assert_eq!(
            image.samples, oracle.samples,
            "{name}: reversible decode must be exact"
        );
    }
}

// ---------------------------------------------------------------------
// Non-ignored harness self-tests: keep the zoo, the oracles and the
// comparison machinery honest while the decoder is still stubbed.
// ---------------------------------------------------------------------

#[test]
fn manifest_lists_all_committed_fixtures() {
    let manifest = load_manifest();
    assert_eq!(manifest.len(), 18, "fixture zoo case count");
    for case in &manifest {
        assert!(
            fixture_dir().join(&case.file).is_file(),
            "{} missing",
            case.file
        );
        assert!(
            fixture_dir().join(&case.source).is_file(),
            "{} missing",
            case.source
        );
        let indep = format!("{}.indep.png", case.name);
        assert!(fixture_dir().join(&indep).is_file(), "{indep} missing");
    }
}

#[test]
fn oracle_pngs_decode_with_manifest_dimensions() {
    for case in load_manifest() {
        for oracle_file in [case.source.clone(), format!("{}.indep.png", case.name)] {
            let oracle = read_oracle(&fixture_dir().join(&oracle_file));
            assert_eq!(
                (oracle.width, oracle.height),
                (case.width, case.height),
                "{oracle_file}: dimensions"
            );
            assert_eq!(
                oracle.channels,
                expected_channels(&case.mode),
                "{oracle_file}: channels"
            );
        }
    }
}

#[test]
fn png_reader_matches_independently_verified_samples() {
    // Spot anchors extracted from the oracle files with an unrelated PNG
    // implementation before this harness was written.
    let gray = read_oracle(&fixture_dir().join("gray-53-jp2.src.png"));
    assert_eq!((gray.width, gray.height, gray.channels), (97, 61, 1));
    assert_eq!(gray.samples[0], 0);
    assert_eq!(*gray.samples.last().unwrap(), 196);
    assert_eq!(
        gray.samples.iter().map(|&v| u64::from(v)).sum::<u64>(),
        746_010
    );

    let rgb = read_oracle(&fixture_dir().join("rgb-53-jp2.src.png"));
    assert_eq!((rgb.width, rgb.height, rgb.channels), (130, 83, 3));
    assert_eq!(&rgb.samples[..3], &[0, 0, 0]);
    assert_eq!(&rgb.samples[rgb.samples.len() - 3..], &[133, 246, 211]);
    assert_eq!(
        rgb.samples.iter().map(|&v| u64::from(v)).sum::<u64>(),
        3_714_250
    );

    let rgba = read_oracle(&fixture_dir().join("rgba-53-jp2.src.png"));
    assert_eq!((rgba.width, rgba.height, rgba.channels), (64, 64, 4));
    assert_eq!(
        &rgba.samples[rgba.samples.len() - 4..],
        &[59, 189, 126, 192]
    );
    assert_eq!(
        rgba.samples.iter().map(|&v| u64::from(v)).sum::<u64>(),
        2_009_088
    );

    // 16-bit greyscale, normalized by >> 8: last sample 26052 >> 8 = 101.
    let gray16 = read_oracle(&fixture_dir().join("gray16-53-jp2.src.png"));
    assert_eq!((gray16.width, gray16.height, gray16.channels), (80, 50, 1));
    assert_eq!(*gray16.samples.last().unwrap(), 101);
    assert_eq!(
        gray16.samples.iter().map(|&v| u64::from(v)).sum::<u64>(),
        201_516
    );
}

#[test]
fn reversible_oracles_are_pixel_identical() {
    // For 5-3 reversible cases the independent decode must equal the
    // source exactly — which also cross-checks the PNG reader over every
    // filter/colour-type combination the zoo uses.
    for case in load_manifest().iter().filter(|case| !case.irreversible) {
        let src = read_oracle(&fixture_dir().join(&case.source));
        let indep = read_oracle(&fixture_dir().join(format!("{}.indep.png", case.name)));
        assert_eq!(
            src.samples, indep.samples,
            "{}: reversible oracles differ",
            case.name
        );
    }
}

#[test]
fn irreversible_oracles_stay_within_the_documented_tolerance() {
    // The 9-7 cases differ from their source by at most 1 (measured) and
    // sit far above the 38 dB acceptance floor, so the +/-2 / 38 dB rule
    // has real headroom. gray-97-jp2 measures ~62.1 dB.
    for case in load_manifest().iter().filter(|case| case.irreversible) {
        let src = read_oracle(&fixture_dir().join(&case.source));
        let indep = read_oracle(&fixture_dir().join(format!("{}.indep.png", case.name)));
        let diff = max_abs_diff(&src.samples, &indep.samples);
        assert!(diff <= 2, "{}: oracle diff {diff}", case.name);
        let quality = psnr(&src.samples, &indep.samples);
        assert!(quality >= 38.0, "{}: oracle PSNR {quality:.2}", case.name);
    }
    let gray = read_oracle(&fixture_dir().join("gray-97-jp2.src.png"));
    let indep = read_oracle(&fixture_dir().join("gray-97-jp2.indep.png"));
    let quality = psnr(&gray.samples, &indep.samples);
    assert!(
        (61.5..62.5).contains(&quality),
        "gray-97-jp2 oracle PSNR drifted: {quality:.2}"
    );
}

#[test]
fn decode_rejects_non_jpeg2000_input() {
    for bad in [
        b"%PDF-1.7 not an image".as_slice(),
        b"".as_slice(),
        &[255, 217],
    ] {
        assert!(matches!(
            decode(bad, &DecodeLimits::default()),
            Err(JpxError::NotJpeg2000)
        ));
    }
}

#[test]
fn decode_fails_soft_on_every_fixture() {
    // Stage-proof contract: the fixtures are valid JPEG 2000, so decode()
    // must never classify them as NotJpeg2000/Malformed. While stages are
    // stubbed the expected outcome is Unsupported("decoder scaffold");
    // once the pipeline lands, Ok. Any panic fails the test run itself.
    for case in load_manifest() {
        let data = std::fs::read(fixture_dir().join(&case.file)).unwrap();
        match decode(&data, &DecodeLimits::default()) {
            Ok(_) | Err(JpxError::Unsupported(_)) => {}
            Err(other) => panic!("{}: unexpected error {other}", case.name),
        }
    }
}

#[test]
fn single_byte_mutations_never_panic() {
    // Fuzz-shaped robustness (design testing rule 5): 200 deterministic
    // single-byte mutations per fixture must decode to Ok or Err, never
    // panic. Deterministic LCG (Knuth MMIX constants) — no external RNG.
    let mut state: u64 = 42;
    let mut step = || {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        state
    };
    for case in load_manifest() {
        let data = std::fs::read(fixture_dir().join(&case.file)).unwrap();
        for _ in 0..200 {
            let mut mutated = data.clone();
            let offset = (step() >> 33) as usize % mutated.len();
            let flip = ((step() >> 24) as u8) | 1; // nonzero => byte changes
            mutated[offset] ^= flip;
            let _ = decode(&mutated, &DecodeLimits::default());
        }
    }
}

// ---------------------------------------------------------------------
// Per-case end-to-end tests over the wired decoder pipeline.
// ---------------------------------------------------------------------

macro_rules! zoo_case {
    ($test_name:ident, $case:literal) => {
        #[test]
        fn $test_name() {
            run_case($case);
        }
    };
}

zoo_case!(gray_53_jp2, "gray-53-jp2");
zoo_case!(gray_97_jp2, "gray-97-jp2");
zoo_case!(rgb_53_jp2, "rgb-53-jp2");
zoo_case!(rgb_97_jp2, "rgb-97-jp2");
zoo_case!(gray_53_raw, "gray-53-raw");
zoo_case!(rgb_97_raw, "rgb-97-raw");
zoo_case!(rgba_53_jp2, "rgba-53-jp2");
zoo_case!(rgb_tiled, "rgb-tiled");
zoo_case!(rgb_layers, "rgb-layers");
zoo_case!(rgb_res3, "rgb-res3");
zoo_case!(rgb_cb16, "rgb-cb16");
zoo_case!(rgb_precinct, "rgb-precinct");
zoo_case!(rgb_prog_lrcp, "rgb-prog-lrcp");
zoo_case!(rgb_prog_rlcp, "rgb-prog-rlcp");
zoo_case!(rgb_prog_rpcl, "rgb-prog-rpcl");
zoo_case!(rgb_prog_pcrl, "rgb-prog-pcrl");
zoo_case!(rgb_prog_cprl, "rgb-prog-cprl");
zoo_case!(gray16_53_jp2, "gray16-53-jp2");
