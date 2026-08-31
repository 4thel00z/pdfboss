//! A baseline JPEG encoder for rasterized pages (ITU-T T.81): 8-bit
//! samples, 4:4:4 sampling in one interleaved scan, the Annex K
//! quantization tables scaled by a quality knob, and the Annex K Huffman
//! tables written verbatim. Chroma is not subsampled because a page is
//! mostly sharp edges, where 4:2:0 smears colored text.

use std::f32::consts::FRAC_1_SQRT_2;

use crate::{Error, Result};

/// Bytes per pixel of the pixmaps the rasterizer produces.
const BPP: usize = 4;

/// The largest side length a SOF0 frame header can describe.
const MAX_SIDE: u32 = 65_535;

/// Table K.1: luminance quantization table, row-major.
const LUMA_QUANT: [u8; 64] = [
    16, 11, 10, 16, 24, 40, 51, 61, //
    12, 12, 14, 19, 26, 58, 60, 55, //
    14, 13, 16, 24, 40, 57, 69, 56, //
    14, 17, 22, 29, 51, 87, 80, 62, //
    18, 22, 37, 56, 68, 109, 103, 77, //
    24, 35, 55, 64, 81, 104, 113, 92, //
    49, 64, 78, 87, 103, 121, 120, 101, //
    72, 92, 95, 98, 112, 100, 103, 99,
];

/// Table K.2: chrominance quantization table, row-major.
const CHROMA_QUANT: [u8; 64] = [
    17, 18, 24, 47, 99, 99, 99, 99, //
    18, 21, 26, 66, 99, 99, 99, 99, //
    24, 26, 56, 99, 99, 99, 99, 99, //
    47, 66, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99, //
    99, 99, 99, 99, 99, 99, 99, 99,
];

/// Figure A.6: the row-major index of the k-th coefficient in zig-zag order.
const ZIGZAG: [usize; 64] = [
    0, 1, 8, 16, 9, 2, 3, 10, //
    17, 24, 32, 25, 18, 11, 4, 5, //
    12, 19, 26, 33, 40, 48, 41, 34, //
    27, 20, 13, 6, 7, 14, 21, 28, //
    35, 42, 49, 56, 57, 50, 43, 36, //
    29, 22, 15, 23, 30, 37, 44, 51, //
    58, 59, 52, 45, 38, 31, 39, 46, //
    53, 60, 61, 54, 47, 55, 62, 63,
];

/// Table K.3: luminance DC code lengths (BITS) and symbols (HUFFVAL).
const DC_LUMA_BITS: [u8; 16] = [0, 1, 5, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0];
const DC_LUMA_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

/// Table K.4: chrominance DC code lengths and symbols.
const DC_CHROMA_BITS: [u8; 16] = [0, 3, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 0, 0];
const DC_CHROMA_VALS: [u8; 12] = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];

/// Table K.5: luminance AC code lengths and symbols.
const AC_LUMA_BITS: [u8; 16] = [0, 2, 1, 3, 3, 2, 4, 3, 5, 5, 4, 4, 0, 0, 1, 0x7d];
const AC_LUMA_VALS: [u8; 162] = [
    0x01, 0x02, 0x03, 0x00, 0x04, 0x11, 0x05, 0x12, 0x21, 0x31, 0x41, 0x06, 0x13, 0x51, 0x61, 0x07,
    0x22, 0x71, 0x14, 0x32, 0x81, 0x91, 0xa1, 0x08, 0x23, 0x42, 0xb1, 0xc1, 0x15, 0x52, 0xd1, 0xf0,
    0x24, 0x33, 0x62, 0x72, 0x82, 0x09, 0x0a, 0x16, 0x17, 0x18, 0x19, 0x1a, 0x25, 0x26, 0x27, 0x28,
    0x29, 0x2a, 0x34, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48, 0x49,
    0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68, 0x69,
    0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x83, 0x84, 0x85, 0x86, 0x87, 0x88, 0x89,
    0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5, 0xa6, 0xa7,
    0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3, 0xc4, 0xc5,
    0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda, 0xe1, 0xe2,
    0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf1, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

/// Table K.6: chrominance AC code lengths and symbols.
const AC_CHROMA_BITS: [u8; 16] = [0, 2, 1, 2, 4, 4, 3, 4, 7, 5, 4, 4, 0, 1, 2, 0x77];
const AC_CHROMA_VALS: [u8; 162] = [
    0x00, 0x01, 0x02, 0x03, 0x11, 0x04, 0x05, 0x21, 0x31, 0x06, 0x12, 0x41, 0x51, 0x07, 0x61, 0x71,
    0x13, 0x22, 0x32, 0x81, 0x08, 0x14, 0x42, 0x91, 0xa1, 0xb1, 0xc1, 0x09, 0x23, 0x33, 0x52, 0xf0,
    0x15, 0x62, 0x72, 0xd1, 0x0a, 0x16, 0x24, 0x34, 0xe1, 0x25, 0xf1, 0x17, 0x18, 0x19, 0x1a, 0x26,
    0x27, 0x28, 0x29, 0x2a, 0x35, 0x36, 0x37, 0x38, 0x39, 0x3a, 0x43, 0x44, 0x45, 0x46, 0x47, 0x48,
    0x49, 0x4a, 0x53, 0x54, 0x55, 0x56, 0x57, 0x58, 0x59, 0x5a, 0x63, 0x64, 0x65, 0x66, 0x67, 0x68,
    0x69, 0x6a, 0x73, 0x74, 0x75, 0x76, 0x77, 0x78, 0x79, 0x7a, 0x82, 0x83, 0x84, 0x85, 0x86, 0x87,
    0x88, 0x89, 0x8a, 0x92, 0x93, 0x94, 0x95, 0x96, 0x97, 0x98, 0x99, 0x9a, 0xa2, 0xa3, 0xa4, 0xa5,
    0xa6, 0xa7, 0xa8, 0xa9, 0xaa, 0xb2, 0xb3, 0xb4, 0xb5, 0xb6, 0xb7, 0xb8, 0xb9, 0xba, 0xc2, 0xc3,
    0xc4, 0xc5, 0xc6, 0xc7, 0xc8, 0xc9, 0xca, 0xd2, 0xd3, 0xd4, 0xd5, 0xd6, 0xd7, 0xd8, 0xd9, 0xda,
    0xe2, 0xe3, 0xe4, 0xe5, 0xe6, 0xe7, 0xe8, 0xe9, 0xea, 0xf2, 0xf3, 0xf4, 0xf5, 0xf6, 0xf7, 0xf8,
    0xf9, 0xfa,
];

/// The end-of-block and zero-run-length AC symbols.
const EOB: usize = 0x00;
const ZRL: usize = 0xF0;

/// Encodes `rgba` (row-major, `width * height * 4` bytes) as a baseline
/// JPEG at `quality` (1 to 100, clamped). Alpha is dropped.
pub(crate) fn encode_jpeg(width: u32, height: u32, rgba: &[u8], quality: u8) -> Result<Vec<u8>> {
    if width == 0 || height == 0 || width > MAX_SIDE || height > MAX_SIDE {
        return Err(Error::Other(format!(
            "jpeg encode: {width} x {height} px is outside 1 to {MAX_SIDE} per side"
        )));
    }
    let (luma_quant, chroma_quant) = scaled_tables(quality.clamp(1, 100));
    let dc_luma = Huffman::build(&DC_LUMA_BITS, &DC_LUMA_VALS);
    let ac_luma = Huffman::build(&AC_LUMA_BITS, &AC_LUMA_VALS);
    let dc_chroma = Huffman::build(&DC_CHROMA_BITS, &DC_CHROMA_VALS);
    let ac_chroma = Huffman::build(&AC_CHROMA_BITS, &AC_CHROMA_VALS);

    let mut out = Vec::with_capacity((width as usize * height as usize) / 4 + 1024);
    write_headers(&mut out, width, height, &luma_quant, &chroma_quant);

    let luma_quantizer = Quantizer::new(&luma_quant);
    let chroma_quantizer = Quantizer::new(&chroma_quant);
    let mut writer = BitWriter::new(out);
    let mut previous_dc = [0i32; 3];
    let mut blocks = [[0f32; 64]; 3];
    for block_y in 0..height.div_ceil(8) {
        for block_x in 0..width.div_ceil(8) {
            gather_block(rgba, width, height, block_x, block_y, &mut blocks);
            for (component, samples) in blocks.iter().enumerate() {
                let (quantizer, dc, ac) = match component {
                    0 => (&luma_quantizer, &dc_luma, &ac_luma),
                    _ => (&chroma_quantizer, &dc_chroma, &ac_chroma),
                };
                let coefficients = fdct_quantize(samples, quantizer);
                encode_block(
                    &mut writer,
                    &coefficients,
                    &mut previous_dc[component],
                    dc,
                    ac,
                );
            }
        }
    }
    let mut out = writer.finish();
    out.extend_from_slice(&[0xFF, 0xD9]);
    Ok(out)
}

/// The Annex K tables scaled for `quality` the conventional way: 50 leaves
/// them as printed, 100 makes every divisor 1, lower values coarsen.
fn scaled_tables(quality: u8) -> ([u8; 64], [u8; 64]) {
    let quality = u32::from(quality);
    let scale = if quality < 50 {
        5000 / quality
    } else {
        200 - 2 * quality
    };
    let scaled = |base: &[u8; 64]| -> [u8; 64] {
        let mut out = [0u8; 64];
        for (dst, &entry) in out.iter_mut().zip(base) {
            *dst = ((u32::from(entry) * scale + 50) / 100).clamp(1, 255) as u8;
        }
        out
    };
    (scaled(&LUMA_QUANT), scaled(&CHROMA_QUANT))
}

/// SOI, APP0 (JFIF 1.01), two DQT, SOF0, four DHT and SOS, in that order.
fn write_headers(out: &mut Vec<u8>, width: u32, height: u32, luma: &[u8; 64], chroma: &[u8; 64]) {
    out.extend_from_slice(&[0xFF, 0xD8]);
    segment(out, 0xE0, |body| {
        body.extend_from_slice(b"JFIF\0");
        body.extend_from_slice(&[1, 1, 0, 0, 1, 0, 1, 0, 0]);
    });
    for (id, table) in [(0u8, luma), (1u8, chroma)] {
        segment(out, 0xDB, |body| {
            body.push(id);
            body.extend(ZIGZAG.iter().map(|&k| table[k]));
        });
    }
    segment(out, 0xC0, |body| {
        body.push(8);
        body.extend_from_slice(&(height as u16).to_be_bytes());
        body.extend_from_slice(&(width as u16).to_be_bytes());
        body.push(3);
        body.extend_from_slice(&[1, 0x11, 0, 2, 0x11, 1, 3, 0x11, 1]);
    });
    let tables: [(u8, &[u8; 16], &[u8]); 4] = [
        (0x00, &DC_LUMA_BITS, &DC_LUMA_VALS),
        (0x10, &AC_LUMA_BITS, &AC_LUMA_VALS),
        (0x01, &DC_CHROMA_BITS, &DC_CHROMA_VALS),
        (0x11, &AC_CHROMA_BITS, &AC_CHROMA_VALS),
    ];
    for (class_and_id, bits, vals) in tables {
        segment(out, 0xC4, |body| {
            body.push(class_and_id);
            body.extend_from_slice(bits);
            body.extend_from_slice(vals);
        });
    }
    segment(out, 0xDA, |body| {
        body.push(3);
        body.extend_from_slice(&[1, 0x00, 2, 0x11, 3, 0x11]);
        body.extend_from_slice(&[0, 63, 0]);
    });
}

/// Appends one marker segment: `FF marker`, a big-endian length that
/// counts itself, then the body `fill` writes.
fn segment(out: &mut Vec<u8>, marker: u8, fill: impl FnOnce(&mut Vec<u8>)) {
    out.extend_from_slice(&[0xFF, marker, 0, 0]);
    let start = out.len();
    fill(out);
    let len = (out.len() - start + 2) as u16;
    out[start - 2..start].copy_from_slice(&len.to_be_bytes());
}

/// Fills the Y, Cb and Cr blocks for the 8x8 tile at (`block_x`,
/// `block_y`), level-shifted by 128; pixels past the right or bottom edge
/// repeat the last column or row.
fn gather_block(
    rgba: &[u8],
    width: u32,
    height: u32,
    block_x: u32,
    block_y: u32,
    blocks: &mut [[f32; 64]; 3],
) {
    for y in 0..8u32 {
        let src_y = (block_y * 8 + y).min(height - 1) as usize;
        for x in 0..8u32 {
            let src_x = (block_x * 8 + x).min(width - 1) as usize;
            let at = (src_y * width as usize + src_x) * BPP;
            let (r, g, b) = (
                f32::from(rgba[at]),
                f32::from(rgba[at + 1]),
                f32::from(rgba[at + 2]),
            );
            let i = (y * 8 + x) as usize;
            blocks[0][i] = 0.299 * r + 0.587 * g + 0.114 * b - 128.0;
            blocks[1][i] = -0.168_736 * r - 0.331_264 * g + 0.5 * b;
            blocks[2][i] = 0.5 * r - 0.418_688 * g - 0.081_312 * b;
        }
    }
}

/// What each 1-D output of [`aan`] is scaled by relative to the DCT-II of
/// A.3.3: `1` at k = 0, `sqrt(2) cos(k pi / 16)` above, times `2 sqrt 2`
/// per pass.
const AAN_SCALE: [f32; 8] = [
    1.0, 1.3870399, 1.306563, 1.1758755, 1.0, 0.78569496, 0.5411961, 0.27589938,
];

/// One multiplier per coefficient, row-major, folding the AAN scale of
/// both passes and the quantizer, so quantizing is a multiply and a round.
struct Quantizer {
    scale: [f32; 64],
}

impl Quantizer {
    fn new(quant: &[u8; 64]) -> Quantizer {
        let mut scale = [0f32; 64];
        for (n, entry) in scale.iter_mut().enumerate() {
            let (v, u) = (n / 8, n % 8);
            *entry = 1.0 / (AAN_SCALE[v] * AAN_SCALE[u] * 8.0 * f32::from(quant[n]));
        }
        Quantizer { scale }
    }
}

/// The forward DCT of one level-shifted block, rows then columns through
/// [`aan`], quantized through `quantizer` and returned in zig-zag order.
fn fdct_quantize(samples: &[f32; 64], quantizer: &Quantizer) -> [i32; 64] {
    let mut block = *samples;
    for row in block.as_chunks_mut::<8>().0 {
        aan(row);
    }
    for x in 0..8 {
        let mut column = [0f32; 8];
        for (y, value) in column.iter_mut().enumerate() {
            *value = block[y * 8 + x];
        }
        aan(&mut column);
        for (y, value) in column.iter().enumerate() {
            block[y * 8 + x] = *value;
        }
    }
    let mut zigzag = [0i32; 64];
    for (k, &natural) in ZIGZAG.iter().enumerate() {
        zigzag[k] = (block[natural] * quantizer.scale[natural]).round() as i32;
    }
    zigzag
}

/// One 8-point pass of the Arai, Agui and Nakajima factorization of the
/// DCT-II: 5 multiplies and 29 additions, leaving each output scaled by
/// its [`AAN_SCALE`] entry.
fn aan(d: &mut [f32; 8]) {
    let (sum07, diff07) = (d[0] + d[7], d[0] - d[7]);
    let (sum16, diff16) = (d[1] + d[6], d[1] - d[6]);
    let (sum25, diff25) = (d[2] + d[5], d[2] - d[5]);
    let (sum34, diff34) = (d[3] + d[4], d[3] - d[4]);

    let (even0, even3) = (sum07 + sum34, sum07 - sum34);
    let (even1, even2) = (sum16 + sum25, sum16 - sum25);
    d[0] = even0 + even1;
    d[4] = even0 - even1;
    let rotated = (even2 + even3) * FRAC_1_SQRT_2;
    d[2] = even3 + rotated;
    d[6] = even3 - rotated;

    let odd0 = diff34 + diff25;
    let odd1 = diff25 + diff16;
    let odd2 = diff16 + diff07;
    let shared = (odd0 - odd2) * 0.38268343;
    let low = 0.5411961 * odd0 + shared;
    let high = 1.306563 * odd2 + shared;
    let middle = odd1 * FRAC_1_SQRT_2;
    let (plus, minus) = (diff07 + middle, diff07 - middle);
    d[5] = minus + low;
    d[3] = minus - low;
    d[1] = plus + high;
    d[7] = plus - high;
}

/// Writes one block's DC difference and run-length coded AC coefficients.
fn encode_block(
    writer: &mut BitWriter,
    coefficients: &[i32; 64],
    previous_dc: &mut i32,
    dc: &Huffman,
    ac: &Huffman,
) {
    let diff = coefficients[0] - *previous_dc;
    *previous_dc = coefficients[0];
    let size = category(diff);
    writer.put(dc.code(size as usize), dc.len(size as usize));
    writer.put(magnitude_bits(diff, size), size);

    let mut run = 0usize;
    for &value in &coefficients[1..] {
        if value == 0 {
            run += 1;
            continue;
        }
        while run > 15 {
            writer.put(ac.code(ZRL), ac.len(ZRL));
            run -= 16;
        }
        let size = category(value);
        let symbol = (run << 4) | size as usize;
        writer.put(ac.code(symbol), ac.len(symbol));
        writer.put(magnitude_bits(value, size), size);
        run = 0;
    }
    if run > 0 {
        writer.put(ac.code(EOB), ac.len(EOB));
    }
}

/// The SSSS category of Table F.1: the bit length of `value`'s magnitude.
fn category(value: i32) -> u32 {
    32 - value.unsigned_abs().leading_zeros()
}

/// The `size` low-order bits that follow a category code: the value itself
/// when positive, its one's complement when negative (F.1.2.1).
fn magnitude_bits(value: i32, size: u32) -> u32 {
    if value >= 0 {
        value as u32
    } else {
        (value - 1) as u32 & ((1 << size) - 1)
    }
}

/// A Huffman table as the encoder needs it: code and length per symbol,
/// assigned canonically from BITS and HUFFVAL (Annex C).
struct Huffman {
    code: [u16; 256],
    len: [u8; 256],
}

impl Huffman {
    fn build(bits: &[u8; 16], vals: &[u8]) -> Huffman {
        let mut table = Huffman {
            code: [0; 256],
            len: [0; 256],
        };
        let mut code = 0u16;
        let mut next = vals.iter();
        for (i, &count) in bits.iter().enumerate() {
            for _ in 0..count {
                let symbol = *next.next().expect("HUFFVAL shorter than BITS") as usize;
                table.code[symbol] = code;
                table.len[symbol] = i as u8 + 1;
                code += 1;
            }
            code <<= 1;
        }
        table
    }

    fn code(&self, symbol: usize) -> u32 {
        u32::from(self.code[symbol])
    }

    fn len(&self, symbol: usize) -> u32 {
        let len = u32::from(self.len[symbol]);
        if len == 0 {
            unreachable!("symbol {symbol:#04x} has no code in this table");
        }
        len
    }
}

/// Packs bits most-significant first into the entropy-coded segment,
/// stuffing a zero byte after every `FF` (B.1.1.5).
struct BitWriter {
    out: Vec<u8>,
    acc: u32,
    pending: u32,
}

impl BitWriter {
    fn new(out: Vec<u8>) -> BitWriter {
        BitWriter {
            out,
            acc: 0,
            pending: 0,
        }
    }

    fn put(&mut self, code: u32, len: u32) {
        if len == 0 {
            return;
        }
        self.acc = (self.acc << len) | (code & ((1u32 << len) - 1));
        self.pending += len;
        while self.pending >= 8 {
            let byte = (self.acc >> (self.pending - 8)) as u8;
            self.out.push(byte);
            if byte == 0xFF {
                self.out.push(0);
            }
            self.pending -= 8;
        }
        self.acc &= (1u32 << self.pending) - 1;
    }

    /// Pads the last byte with one bits and hands the buffer back.
    fn finish(mut self) -> Vec<u8> {
        if self.pending > 0 {
            let pad = 8 - self.pending;
            self.put((1 << pad) - 1, pad);
        }
        self.out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn symbols(table: &Huffman) -> Vec<usize> {
        (0..256).filter(|&s| table.len[s] > 0).collect()
    }

    /// The separable DCT-II written out directly from A.3.3 and quantized
    /// with a division per coefficient: the reference the fast transform
    /// is held to.
    fn reference_fdct_quantize(samples: &[f32; 64], quant: &[u8; 64]) -> [i32; 64] {
        let mut cos = [[0f32; 8]; 8];
        for (k, row) in cos.iter_mut().enumerate() {
            let c = if k == 0 {
                std::f32::consts::FRAC_1_SQRT_2
            } else {
                1.0
            };
            for (n, entry) in row.iter_mut().enumerate() {
                *entry = c / 2.0 * (((2 * n + 1) * k) as f32 * std::f32::consts::PI / 16.0).cos();
            }
        }
        let mut rows = [[0f32; 8]; 8];
        for v in 0..8 {
            for x in 0..8 {
                rows[v][x] = (0..8).map(|y| cos[v][y] * samples[y * 8 + x]).sum();
            }
        }
        let mut zigzag = [0i32; 64];
        for (k, &natural) in ZIGZAG.iter().enumerate() {
            let (v, u) = (natural / 8, natural % 8);
            let coefficient: f32 = (0..8).map(|x| rows[v][x] * cos[u][x]).sum();
            zigzag[k] = (coefficient / f32::from(quant[natural])).round() as i32;
        }
        zigzag
    }

    /// Level-shifted noise blocks from a fixed-seed linear congruential
    /// generator, so the comparison is deterministic and dependency-free.
    fn noise_blocks(count: usize) -> Vec<[f32; 64]> {
        let mut state = 0x2545_F491_4F6C_DD1Du64;
        (0..count)
            .map(|_| {
                let mut block = [0f32; 64];
                for sample in &mut block {
                    state = state
                        .wrapping_mul(6_364_136_223_846_793_005)
                        .wrapping_add(1_442_695_040_888_963_407);
                    *sample = ((state >> 40) & 0xFF) as f32 - 128.0;
                }
                block
            })
            .collect()
    }

    #[test]
    fn fast_dct_matches_the_reference_within_one_quantized_step() {
        let flat = [1u8; 64];
        for quant in [flat, LUMA_QUANT, CHROMA_QUANT] {
            let quantizer = Quantizer::new(&quant);
            for block in noise_blocks(200) {
                let fast = fdct_quantize(&block, &quantizer);
                let reference = reference_fdct_quantize(&block, &quant);
                for k in 0..64 {
                    assert!(
                        (fast[k] - reference[k]).abs() <= 1,
                        "coefficient {k}: fast {} vs reference {}",
                        fast[k],
                        reference[k]
                    );
                }
            }
        }
    }

    #[test]
    fn ac_tables_cover_every_run_size_pair_plus_eob_and_zrl() {
        let mut expected: Vec<usize> = (0..16)
            .flat_map(|run| (1..=10).map(move |size| (run << 4) | size))
            .collect();
        expected.extend([EOB, ZRL]);
        expected.sort_unstable();
        for (bits, vals) in [
            (&AC_LUMA_BITS, &AC_LUMA_VALS),
            (&AC_CHROMA_BITS, &AC_CHROMA_VALS),
        ] {
            assert_eq!(
                bits.iter().map(|&b| usize::from(b)).sum::<usize>(),
                vals.len()
            );
            assert_eq!(symbols(&Huffman::build(bits, vals)), expected);
        }
    }

    #[test]
    fn dc_tables_cover_categories_zero_to_eleven() {
        for (bits, vals) in [
            (&DC_LUMA_BITS, &DC_LUMA_VALS),
            (&DC_CHROMA_BITS, &DC_CHROMA_VALS),
        ] {
            assert_eq!(
                symbols(&Huffman::build(bits, vals)),
                (0..12).collect::<Vec<_>>()
            );
        }
    }

    #[test]
    fn luminance_ac_codes_match_the_published_eob_and_zrl() {
        let table = Huffman::build(&AC_LUMA_BITS, &AC_LUMA_VALS);
        assert_eq!((table.code(EOB), table.len(EOB)), (0b1010, 4));
        assert_eq!((table.code(ZRL), table.len(ZRL)), (0b111_1111_1001, 11));
        assert_eq!((table.code(0x01), table.len(0x01)), (0b00, 2));
    }

    #[test]
    fn quality_fifty_leaves_the_tables_unscaled_and_hundred_flattens_them() {
        let (luma, chroma) = scaled_tables(50);
        assert_eq!(luma, LUMA_QUANT);
        assert_eq!(chroma, CHROMA_QUANT);
        let (luma, chroma) = scaled_tables(100);
        assert!(luma.iter().chain(&chroma).all(|&q| q == 1));
        let (coarse, _) = scaled_tables(10);
        assert_eq!(coarse[0], 80);
    }

    #[test]
    fn categories_and_magnitude_bits_follow_table_f1() {
        assert_eq!(category(0), 0);
        assert_eq!((category(1), magnitude_bits(1, 1)), (1, 0b1));
        assert_eq!((category(-1), magnitude_bits(-1, 1)), (1, 0b0));
        assert_eq!((category(3), magnitude_bits(3, 2)), (2, 0b11));
        assert_eq!((category(-3), magnitude_bits(-3, 2)), (2, 0b00));
        assert_eq!((category(-2), magnitude_bits(-2, 2)), (2, 0b01));
        assert_eq!(category(1023), 10);
        assert_eq!(category(-1024), 11);
    }

    #[test]
    fn bit_writer_stuffs_ff_and_pads_with_ones() {
        let mut writer = BitWriter::new(Vec::new());
        writer.put(0xFF, 8);
        writer.put(0b101, 3);
        assert_eq!(writer.finish(), vec![0xFF, 0x00, 0b1011_1111]);
    }

    #[test]
    fn a_flat_block_quantizes_to_a_lone_dc_coefficient() {
        let samples = [64.0f32; 64];
        let coefficients = fdct_quantize(&samples, &Quantizer::new(&LUMA_QUANT));
        assert_eq!(coefficients[0], (8.0f32 * 64.0 / 16.0).round() as i32);
        assert!(coefficients[1..].iter().all(|&c| c == 0));
    }
}
