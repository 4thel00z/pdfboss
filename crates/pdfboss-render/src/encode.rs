//! A PNG encoder specialized for rasterized pages: per-row filter choice,
//! one image-tuned dynamic Huffman table, and zero-run coding instead of
//! LZ77 match search (RFC 1950/1951, ISO 15948).
//!
//! A general deflate encoder spends most of its time in hash-chain match
//! search. Filtered scanlines don't reward that search: their redundancy
//! is runs of zeros and the shape of the literal distribution, both of
//! which a Huffman table built for this image plus run-length coding
//! capture at a fraction of the cost. Sizes land at general-zlib levels;
//! encoding is several times faster.

/// Bytes per pixel: this encoder writes the RGBA pixmaps the rasterizer
/// produces.
const BPP: usize = 4;

/// Encodes `rgba` (row-major, `width * height * 4` bytes) as a complete
/// PNG file.
pub(crate) fn encode_rgba(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
    let stride = width as usize * BPP;
    let mut filtered = vec![0u8; (stride + 1) * height as usize];
    let mut prev_row: &[u8] = &[];
    for (row, out) in rgba
        .chunks_exact(stride)
        .zip(filtered.chunks_exact_mut(stride + 1))
    {
        out[0] = choose_filter(row, prev_row, &mut out[1..]);
        prev_row = row;
    }

    let deflated = deflate_filtered(&filtered);

    let mut zlib = Vec::with_capacity(deflated.len() + 6);
    // CMF/FLG: 32K window, deflate, no preset dictionary, FCHECK making
    // the pair a multiple of 31 (RFC 1950 §2.2).
    zlib.push(0x78);
    zlib.push(0x9c);
    zlib.extend_from_slice(&deflated);
    zlib.extend_from_slice(&adler32(&filtered).to_be_bytes());

    let mut out = Vec::with_capacity(zlib.len() + 128);
    out.extend_from_slice(b"\x89PNG\r\n\x1a\n");
    let mut ihdr = [0u8; 13];
    ihdr[..4].copy_from_slice(&width.to_be_bytes());
    ihdr[4..8].copy_from_slice(&height.to_be_bytes());
    // Bit depth 8, color type 6 (RGBA), deflate, adaptive filtering,
    // no interlace.
    ihdr[8..13].copy_from_slice(&[8, 6, 0, 0, 0]);
    push_chunk(&mut out, b"IHDR", &ihdr);
    push_chunk(&mut out, b"IDAT", &zlib);
    push_chunk(&mut out, b"IEND", &[]);
    out
}

/// Applies the filter minimizing the sum of absolute residuals (the PNG
/// specification's suggested heuristic) and leaves the filtered bytes in
/// `scratch`; returns the filter id. The first row sees an all-zero
/// previous row, exactly as the specification defines.
fn choose_filter(row: &[u8], prev: &[u8], scratch: &mut [u8]) -> u8 {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is baseline on aarch64.
        let sums = unsafe { filter_hw::residual_sums(row, prev) };
        let filter = (0..5).min_by_key(|&f| sums[f]).unwrap_or(0) as u8;
        apply_filter(filter, row, prev, scratch);
        return filter;
    }
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: SSE2 is baseline on x86_64.
        let sums = unsafe { filter_hw::residual_sums(row, prev) };
        let filter = (0..5).min_by_key(|&f| sums[f]).unwrap_or(0) as u8;
        apply_filter(filter, row, prev, scratch);
        return filter;
    }
    #[allow(unreachable_code)]
    {
        let sums = residual_sums_soft(row, prev);
        let filter = (0..5).min_by_key(|&f| sums[f]).unwrap_or(0) as u8;
        apply_filter(filter, row, prev, scratch);
        filter
    }
}

/// The five filters' residual sums, portable form — and the reference the
/// vector forms are held to in the tests.
fn residual_sums_soft(row: &[u8], prev: &[u8]) -> [u64; 5] {
    let mut sums = [0u64; 5];
    if prev.is_empty() {
        // First row: the previous row is all zeros, so Up degenerates to
        // None and Paeth to Sub.
        for (i, &b) in row.iter().enumerate() {
            let a = if i >= BPP { row[i - BPP] } else { 0 };
            sums[0] += residual_cost(b);
            let sub = b.wrapping_sub(a);
            sums[1] += residual_cost(sub);
            sums[2] += residual_cost(b);
            sums[3] += residual_cost(b.wrapping_sub(a / 2));
            sums[4] += residual_cost(sub);
        }
    } else {
        for i in 0..BPP.min(row.len()) {
            let b = row[i];
            let u = prev[i];
            sums[0] += residual_cost(b);
            sums[1] += residual_cost(b);
            sums[2] += residual_cost(b.wrapping_sub(u));
            sums[3] += residual_cost(b.wrapping_sub(u / 2));
            sums[4] += residual_cost(b.wrapping_sub(u));
        }
        for i in BPP..row.len() {
            let b = row[i];
            let a = row[i - BPP];
            let u = prev[i];
            let c = prev[i - BPP];
            sums[0] += residual_cost(b);
            sums[1] += residual_cost(b.wrapping_sub(a));
            sums[2] += residual_cost(b.wrapping_sub(u));
            sums[3] += residual_cost(b.wrapping_sub(((a as u16 + u as u16) / 2) as u8));
            sums[4] += residual_cost(b.wrapping_sub(paeth(a, u, c)));
        }
    }
    sums
}

/// Writes `row` filtered by `filter` into `scratch`.
fn apply_filter(filter: u8, row: &[u8], prev: &[u8], scratch: &mut [u8]) {
    let head = BPP.min(row.len());
    match filter {
        0 => scratch[..row.len()].copy_from_slice(row),
        1 => {
            scratch[..head].copy_from_slice(&row[..head]);
            for i in head..row.len() {
                scratch[i] = row[i].wrapping_sub(row[i - BPP]);
            }
        }
        2 => {
            if prev.is_empty() {
                scratch[..row.len()].copy_from_slice(row);
            } else {
                for i in 0..row.len() {
                    scratch[i] = row[i].wrapping_sub(prev[i]);
                }
            }
        }
        3 => {
            if prev.is_empty() {
                scratch[..head].copy_from_slice(&row[..head]);
                for i in head..row.len() {
                    scratch[i] = row[i].wrapping_sub(row[i - BPP] / 2);
                }
            } else {
                for i in 0..head {
                    scratch[i] = row[i].wrapping_sub(prev[i] / 2);
                }
                for i in head..row.len() {
                    let a = row[i - BPP] as u16;
                    scratch[i] = row[i].wrapping_sub(((a + prev[i] as u16) / 2) as u8);
                }
            }
        }
        _ => {
            if prev.is_empty() {
                scratch[..head].copy_from_slice(&row[..head]);
                for i in head..row.len() {
                    scratch[i] = row[i].wrapping_sub(row[i - BPP]);
                }
            } else {
                for i in 0..head {
                    scratch[i] = row[i].wrapping_sub(prev[i]);
                }
                for i in head..row.len() {
                    scratch[i] = row[i].wrapping_sub(paeth(row[i - BPP], prev[i], prev[i - BPP]));
                }
            }
        }
    }
}

/// The five filters' residual sums on vector lanes: 16 bytes per step,
/// each filter's residual reduced through the "min(d, -d)" identity —
/// `residual_cost` of a wrapped difference is exactly the smaller of the
/// byte and its two's complement. Row heads (no left neighbour) and tails
/// shorter than a lane fall back to the portable form.
#[cfg(target_arch = "aarch64")]
mod filter_hw {
    use core::arch::aarch64::{
        uint8x16_t, vaddq_u64, vaddvq_u64, vdupq_n_u64, vhaddq_u8, vld1q_u8, vminq_u8, vmovq_n_u8,
        vpaddlq_u16, vpaddlq_u32, vpaddlq_u8, vshrq_n_u8, vsubq_u8,
    };

    #[inline]
    unsafe fn cost(d: uint8x16_t) -> uint8x16_t {
        vminq_u8(d, vsubq_u8(vmovq_n_u8(0), d))
    }

    #[inline]
    unsafe fn widen_accumulate(acc: &mut core::arch::aarch64::uint64x2_t, v: uint8x16_t) {
        *acc = vaddq_u64(*acc, vpaddlq_u32(vpaddlq_u16(vpaddlq_u8(v))));
    }

    /// PNG's Average is the floor average, which NEON has directly.
    #[inline]
    unsafe fn floor_avg(a: uint8x16_t, b: uint8x16_t) -> uint8x16_t {
        vhaddq_u8(a, b)
    }

    /// PaethPredictor over lanes, exact: each half widened to 16 bits so
    /// `p = a + b - c` and the three absolute distances never clip
    /// (ISO 15948 §9.4 defines them over full-range integers).
    #[inline]
    unsafe fn paeth(a: uint8x16_t, b: uint8x16_t, c: uint8x16_t) -> uint8x16_t {
        use core::arch::aarch64::{
            int16x8_t, vabdq_s16, vaddq_s16, vandq_s16, vbslq_s16, vcleq_s16, vcombine_u8,
            vget_high_u8, vget_low_u8, vmovl_u8, vmovn_u16, vreinterpretq_s16_u16,
            vreinterpretq_u16_s16, vsubq_s16,
        };
        unsafe fn half(a: int16x8_t, b: int16x8_t, c: int16x8_t) -> int16x8_t {
            let p = vsubq_s16(vaddq_s16(a, b), c);
            let pa = vabdq_s16(p, a);
            let pb = vabdq_s16(p, b);
            let pc = vabdq_s16(p, c);
            let use_a = vandq_s16(
                vreinterpretq_s16_u16(vcleq_s16(pa, pb)),
                vreinterpretq_s16_u16(vcleq_s16(pa, pc)),
            );
            let use_b = vcleq_s16(pb, pc);
            vbslq_s16(vreinterpretq_u16_s16(use_a), a, vbslq_s16(use_b, b, c))
        }
        let widen = |v| vreinterpretq_s16_u16(vmovl_u8(v));
        let lo = half(
            widen(vget_low_u8(a)),
            widen(vget_low_u8(b)),
            widen(vget_low_u8(c)),
        );
        let hi = half(
            widen(vget_high_u8(a)),
            widen(vget_high_u8(b)),
            widen(vget_high_u8(c)),
        );
        vcombine_u8(
            vmovn_u16(vreinterpretq_u16_s16(lo)),
            vmovn_u16(vreinterpretq_u16_s16(hi)),
        )
    }

    /// # Safety
    /// NEON is baseline on aarch64; callers need no feature check.
    pub unsafe fn residual_sums(row: &[u8], prev: &[u8]) -> [u64; 5] {
        let mut sums = super::residual_sums_head(row, prev);
        let start = super::BPP.min(row.len());
        let mut acc = [vdupq_n_u64(0); 5];
        let mut i = start;
        if !prev.is_empty() {
            while i + 16 <= row.len() {
                let b = vld1q_u8(row.as_ptr().add(i));
                let a = vld1q_u8(row.as_ptr().add(i - super::BPP));
                let u = vld1q_u8(prev.as_ptr().add(i));
                let c = vld1q_u8(prev.as_ptr().add(i - super::BPP));
                widen_accumulate(&mut acc[0], cost(b));
                widen_accumulate(&mut acc[1], cost(vsubq_u8(b, a)));
                widen_accumulate(&mut acc[2], cost(vsubq_u8(b, u)));
                widen_accumulate(&mut acc[3], cost(vsubq_u8(b, floor_avg(a, u))));
                widen_accumulate(&mut acc[4], cost(vsubq_u8(b, paeth(a, u, c))));
                i += 16;
            }
        } else {
            while i + 16 <= row.len() {
                let b = vld1q_u8(row.as_ptr().add(i));
                let a = vld1q_u8(row.as_ptr().add(i - super::BPP));
                let sub = cost(vsubq_u8(b, a));
                widen_accumulate(&mut acc[0], cost(b));
                widen_accumulate(&mut acc[1], sub);
                widen_accumulate(&mut acc[2], cost(b));
                widen_accumulate(&mut acc[3], cost(vsubq_u8(b, vshrq_n_u8::<1>(a))));
                widen_accumulate(&mut acc[4], sub);
                i += 16;
            }
        }
        for f in 0..5 {
            sums[f] += vaddvq_u64(acc[f]);
        }
        super::residual_sums_tail(row, prev, i, &mut sums);
        sums
    }
}

/// See the aarch64 twin above; same contract on SSE2 lanes.
#[cfg(target_arch = "x86_64")]
mod filter_hw {
    use core::arch::x86_64::{
        __m128i, _mm_add_epi64, _mm_and_si128, _mm_andnot_si128, _mm_cmpeq_epi8, _mm_loadu_si128,
        _mm_min_epu8, _mm_or_si128, _mm_sad_epu8, _mm_set1_epi8, _mm_setzero_si128, _mm_srli_epi16,
        _mm_sub_epi8, _mm_xor_si128,
    };

    #[inline]
    unsafe fn cost(d: __m128i) -> __m128i {
        _mm_min_epu8(d, _mm_sub_epi8(_mm_setzero_si128(), d))
    }

    /// Sum of the 16 byte costs, accumulated as two u64 lanes.
    #[inline]
    unsafe fn accumulate(acc: &mut __m128i, v: __m128i) {
        *acc = _mm_add_epi64(*acc, _mm_sad_epu8(v, _mm_setzero_si128()));
    }

    /// Floor average without overflow: `(a & b) + ((a ^ b) >> 1)` —
    /// `_mm_avg_epu8` rounds up, which is not PNG's Average. Per-byte sums
    /// never exceed 255, so a wide add cannot carry across byte lanes.
    #[inline]
    unsafe fn floor_avg(a: __m128i, b: __m128i) -> __m128i {
        let low = _mm_and_si128(_mm_srli_epi16(_mm_xor_si128(a, b), 1), _mm_set1_epi8(0x7f));
        _mm_add_epi64(_mm_and_si128(a, b), low)
    }

    /// PaethPredictor over lanes, exact: each half widened to 16 bits so
    /// `p = a + b - c` and the three absolute distances never clip
    /// (ISO 15948 §9.4 defines them over full-range integers).
    #[inline]
    unsafe fn paeth(a: __m128i, b: __m128i, c: __m128i) -> __m128i {
        use core::arch::x86_64::{
            _mm_add_epi16, _mm_cmpgt_epi16, _mm_max_epi16, _mm_packus_epi16, _mm_sub_epi16,
            _mm_unpackhi_epi8, _mm_unpacklo_epi8,
        };
        unsafe fn half(a: __m128i, b: __m128i, c: __m128i) -> __m128i {
            let p = _mm_sub_epi16(_mm_add_epi16(a, b), c);
            let abs16 = |v| _mm_max_epi16(v, _mm_sub_epi16(_mm_setzero_si128(), v));
            let pa = abs16(_mm_sub_epi16(p, a));
            let pb = abs16(_mm_sub_epi16(p, b));
            let pc = abs16(_mm_sub_epi16(p, c));
            // le(x, y) as "not (x > y)" on signed 16-bit lanes.
            let gt_ab = _mm_cmpgt_epi16(pa, pb);
            let gt_ac = _mm_cmpgt_epi16(pa, pc);
            let gt_bc = _mm_cmpgt_epi16(pb, pc);
            let use_a = _mm_andnot_si128(_mm_or_si128(gt_ab, gt_ac), _mm_cmpeq_epi8(a, a));
            let bc = _mm_or_si128(_mm_andnot_si128(gt_bc, b), _mm_and_si128(gt_bc, c));
            _mm_or_si128(_mm_and_si128(use_a, a), _mm_andnot_si128(use_a, bc))
        }
        let zero = _mm_setzero_si128();
        let lo = half(
            _mm_unpacklo_epi8(a, zero),
            _mm_unpacklo_epi8(b, zero),
            _mm_unpacklo_epi8(c, zero),
        );
        let hi = half(
            _mm_unpackhi_epi8(a, zero),
            _mm_unpackhi_epi8(b, zero),
            _mm_unpackhi_epi8(c, zero),
        );
        _mm_packus_epi16(lo, hi)
    }

    /// # Safety
    /// SSE2 is baseline on x86_64; callers need no feature check.
    pub unsafe fn residual_sums(row: &[u8], prev: &[u8]) -> [u64; 5] {
        let mut sums = super::residual_sums_head(row, prev);
        let start = super::BPP.min(row.len());
        let mut acc = [_mm_setzero_si128(); 5];
        let mut i = start;
        if !prev.is_empty() {
            while i + 16 <= row.len() {
                let b = _mm_loadu_si128(row.as_ptr().add(i).cast());
                let a = _mm_loadu_si128(row.as_ptr().add(i - super::BPP).cast());
                let u = _mm_loadu_si128(prev.as_ptr().add(i).cast());
                let c = _mm_loadu_si128(prev.as_ptr().add(i - super::BPP).cast());
                accumulate(&mut acc[0], cost(b));
                accumulate(&mut acc[1], cost(_mm_sub_epi8(b, a)));
                accumulate(&mut acc[2], cost(_mm_sub_epi8(b, u)));
                accumulate(&mut acc[3], cost(_mm_sub_epi8(b, floor_avg(a, u))));
                accumulate(&mut acc[4], cost(_mm_sub_epi8(b, paeth(a, u, c))));
                i += 16;
            }
        } else {
            while i + 16 <= row.len() {
                let b = _mm_loadu_si128(row.as_ptr().add(i).cast());
                let a = _mm_loadu_si128(row.as_ptr().add(i - super::BPP).cast());
                let sub = cost(_mm_sub_epi8(b, a));
                accumulate(&mut acc[0], cost(b));
                accumulate(&mut acc[1], sub);
                accumulate(&mut acc[2], cost(b));
                let half_a = _mm_and_si128(_mm_srli_epi16(a, 1), _mm_set1_epi8(0x7f));
                accumulate(&mut acc[3], cost(_mm_sub_epi8(b, half_a)));
                accumulate(&mut acc[4], sub);
                i += 16;
            }
        }
        for (f, lane) in acc.iter().enumerate() {
            let mut pair = [0u64; 2];
            core::arch::x86_64::_mm_storeu_si128(pair.as_mut_ptr().cast(), *lane);
            sums[f] += pair[0] + pair[1];
        }
        super::residual_sums_tail(row, prev, i, &mut sums);
        sums
    }
}

/// The first `BPP` bytes of a row (no left neighbour) under all five
/// filters — the scalar head both vector forms start from.
fn residual_sums_head(row: &[u8], prev: &[u8]) -> [u64; 5] {
    let mut sums = [0u64; 5];
    for i in 0..BPP.min(row.len()) {
        let b = row[i];
        let u = if prev.is_empty() { 0 } else { prev[i] };
        sums[0] += residual_cost(b);
        sums[1] += residual_cost(b);
        sums[2] += residual_cost(b.wrapping_sub(u));
        sums[3] += residual_cost(b.wrapping_sub(u / 2));
        sums[4] += residual_cost(b.wrapping_sub(u));
    }
    sums
}

/// The scalar tail from byte `i` on, added into `sums`.
fn residual_sums_tail(row: &[u8], prev: &[u8], i: usize, sums: &mut [u64; 5]) {
    for i in i..row.len() {
        let b = row[i];
        let a = row[i - BPP];
        let (u, c) = if prev.is_empty() {
            (0, 0)
        } else {
            (prev[i], prev[i - BPP])
        };
        sums[0] += residual_cost(b);
        sums[1] += residual_cost(b.wrapping_sub(a));
        sums[2] += residual_cost(b.wrapping_sub(u));
        sums[3] += residual_cost(b.wrapping_sub(((a as u16 + u as u16) / 2) as u8));
        sums[4] += residual_cost(b.wrapping_sub(paeth(a, u, c)));
    }
}

/// The cost of one residual byte under the minimum-sum-of-absolute-values
/// heuristic: bytes are signed differences, so 255 is as cheap as 1.
#[inline]
fn residual_cost(b: u8) -> u64 {
    u64::from((b as i8).unsigned_abs())
}

/// PaethPredictor (ISO 15948 §9.4).
#[inline]
fn paeth(a: u8, b: u8, c: u8) -> u8 {
    let (pa, pb, pc) = {
        let p = a as i16 + b as i16 - c as i16;
        (
            (p - a as i16).abs(),
            (p - b as i16).abs(),
            (p - c as i16).abs(),
        )
    };
    if pa <= pb && pa <= pc {
        a
    } else if pb <= pc {
        b
    } else {
        c
    }
}

// --- deflate ---------------------------------------------------------------

/// The deflate length codes for lengths 3..=258: `(code, extra_bits,
/// base_length)` rows of RFC 1951 §3.2.5, consulted from a length.
fn length_code(len: usize) -> (u16, u32, usize) {
    const TABLE: [(u16, u32, usize); 29] = [
        (257, 0, 3),
        (258, 0, 4),
        (259, 0, 5),
        (260, 0, 6),
        (261, 0, 7),
        (262, 0, 8),
        (263, 0, 9),
        (264, 0, 10),
        (265, 1, 11),
        (266, 1, 13),
        (267, 1, 15),
        (268, 1, 17),
        (269, 2, 19),
        (270, 2, 23),
        (271, 2, 27),
        (272, 2, 31),
        (273, 3, 35),
        (274, 3, 43),
        (275, 3, 51),
        (276, 3, 59),
        (277, 4, 67),
        (278, 4, 83),
        (279, 4, 99),
        (280, 4, 115),
        (281, 5, 131),
        (282, 5, 163),
        (283, 5, 195),
        (284, 5, 227),
        (285, 0, 258),
    ];
    let idx = TABLE.partition_point(|&(_, _, base)| base <= len) - 1;
    TABLE[idx]
}

/// The deflate distance codes: `(code, extra_bits, base_distance)` rows of
/// RFC 1951 §3.2.5, consulted from a distance.
fn distance_code(dist: usize) -> (u16, u32, usize) {
    const TABLE: [(u16, u32, usize); 30] = [
        (0, 0, 1),
        (1, 0, 2),
        (2, 0, 3),
        (3, 0, 4),
        (4, 1, 5),
        (5, 1, 7),
        (6, 2, 9),
        (7, 2, 13),
        (8, 3, 17),
        (9, 3, 25),
        (10, 4, 33),
        (11, 4, 49),
        (12, 5, 65),
        (13, 5, 97),
        (14, 6, 129),
        (15, 6, 193),
        (16, 7, 257),
        (17, 7, 385),
        (18, 8, 513),
        (19, 8, 769),
        (20, 9, 1025),
        (21, 9, 1537),
        (22, 10, 2049),
        (23, 10, 3073),
        (24, 11, 4097),
        (25, 11, 6145),
        (26, 12, 8193),
        (27, 12, 12289),
        (28, 13, 16385),
        (29, 13, 24577),
    ];
    let idx = TABLE.partition_point(|&(_, _, base)| base <= dist) - 1;
    TABLE[idx]
}

/// Greedy tokenizer state: a single-probe hash table over 4-byte windows,
/// the cheap end of the zlib family's match search. One probe per input
/// position finds the runs and repeats filtered scanlines actually
/// contain, at a fraction of a chained lazy search's cost.
const HASH_BITS: u32 = 15;
const MIN_MATCH: usize = 4;
const WINDOW: usize = 32 * 1024;

#[inline]
fn hash4(data: &[u8], i: usize) -> usize {
    let word = u32::from_le_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]);
    (word.wrapping_mul(0x9e37_79b1) >> (32 - HASH_BITS)) as usize
}

/// One deflate token: what [`tokenize`] hands its callback.
enum Token<'a> {
    /// A stretch of literal bytes: incompressible regions cost one
    /// callback per stretch rather than one per byte.
    Literals(&'a [u8]),
    Match {
        len: usize,
        dist: usize,
    },
}

/// Word-wise scan to the end of the zero run starting at `i`.
#[inline]
fn zero_run_end(data: &[u8], mut i: usize) -> usize {
    while i + 8 <= data.len() {
        let word = u64::from_le_bytes(data[i..i + 8].try_into().unwrap());
        if word != 0 {
            return i + (word.trailing_zeros() / 8) as usize;
        }
        i += 8;
    }
    while i < data.len() && data[i] == 0 {
        i += 1;
    }
    i
}

/// One pass over the filtered image, reporting each token to `emit`. The
/// tokenization is deterministic, so running it twice — once to count,
/// once to write — costs two cheap scans instead of one buffered token
/// stream. After repeated probe misses the scan accelerates LZ4-style,
/// stepping further between probes so incompressible stretches cost a
/// fraction of a probe per byte.
fn tokenize(data: &[u8], mut emit: impl FnMut(Token<'_>)) {
    let mut table = vec![u32::MAX; 1 << HASH_BITS];
    let mut i = 0;
    let mut lit_start = 0;
    let mut misses = 0u32;
    while i + MIN_MATCH <= data.len() {
        // Zero-run fast path: the dominant token in filtered page renders.
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 0 && data[i + 3] == 0 {
            let end = zero_run_end(data, i + 4);
            // Keep one literal zero ahead of the first match so distance 1
            // has a byte to point back into.
            let first = i + usize::from(i == 0 || data[i - 1] != 0);
            if end - first < MIN_MATCH {
                misses += 1;
                i += 1;
                continue;
            }
            if lit_start < first {
                emit(Token::Literals(&data[lit_start..first]));
            }
            let mut run = end - first;
            while run >= MIN_MATCH {
                let piece = run.min(258);
                emit(Token::Match {
                    len: piece,
                    dist: 1,
                });
                run -= piece;
            }
            lit_start = end - run;
            i = end;
            misses = 0;
            continue;
        }
        let h = hash4(data, i);
        let candidate = table[h] as usize;
        table[h] = i as u32;
        if candidate != u32::MAX as usize
            && i - candidate <= WINDOW
            && data[candidate..candidate + MIN_MATCH] == data[i..i + MIN_MATCH]
        {
            let mut len = MIN_MATCH;
            let max = (data.len() - i).min(258);
            while len < max && data[candidate + len] == data[i + len] {
                len += 1;
            }
            if lit_start < i {
                emit(Token::Literals(&data[lit_start..i]));
            }
            emit(Token::Match {
                len,
                dist: i - candidate,
            });
            // Re-seed the table sparsely inside the match: every position
            // would cost more than the matches it finds.
            let mut j = i + 1;
            let end = i + len;
            while j + MIN_MATCH <= data.len() && j < end {
                table[hash4(data, j)] = j as u32;
                j += 7;
            }
            i = end;
            lit_start = end;
            misses = 0;
            continue;
        }
        misses += 1;
        i += 1 + (misses >> 6) as usize;
    }
    if lit_start < data.len() {
        emit(Token::Literals(&data[lit_start..]));
    }
}

/// Deflates the filtered image as one dynamic-Huffman block: a counting
/// pass builds the image's own Huffman tables, a second identical pass
/// writes the stream.
fn deflate_filtered(data: &[u8]) -> Vec<u8> {
    let mut lit_freq = [1u32; 286];
    let mut dist_freq = [1u32; 30];
    // The tables come from a sample: half the image reads statistically
    // like all of it, at half the counting cost. The 1-floors above keep
    // every code the emission pass may need describable even when the
    // sample never produced it (the two tokenizations genuinely differ —
    // the emission pass sees hash-table state the sample pass did not).
    let sample_len = if data.len() > 256 * 1024 {
        data.len() / 2
    } else {
        data.len()
    };
    tokenize(&data[..sample_len], |token| match token {
        Token::Literals(bytes) => {
            for &b in bytes {
                lit_freq[b as usize] += 1;
            }
        }
        Token::Match { len, dist } => {
            lit_freq[length_code(len).0 as usize] += 1;
            dist_freq[distance_code(dist).0 as usize] += 1;
        }
    });

    let lit_lens = huffman_lengths(&lit_freq, 15);
    let lit_codes = canonical_codes(&lit_lens);
    let dist_lens = huffman_lengths(&dist_freq, 15);
    let dist_codes = canonical_codes(&dist_lens);
    let hdist = dist_lens.iter().rposition(|&l| l != 0).map_or(1, |p| p + 1);

    let mut bits = BitWriter::with_capacity(data.len() / 2 + 64);
    bits.write(1, 1); // BFINAL
    bits.write(2, 2); // dynamic Huffman
    write_code_lengths(&mut bits, &lit_lens, &dist_lens[..hdist]);
    tokenize(data, |token| match token {
        Token::Literals(bytes) => {
            for &b in bytes {
                let (code, len) = lit_codes[b as usize];
                bits.write_rev(code, len);
            }
        }
        Token::Match { len, dist } => {
            let (lcode, extra, base) = length_code(len);
            let (code, nbits) = lit_codes[lcode as usize];
            bits.write_rev(code, nbits);
            bits.write((len - base) as u32, extra);
            let (dcode, dextra, dbase) = distance_code(dist);
            let (code, nbits) = dist_codes[dcode as usize];
            bits.write_rev(code, nbits);
            bits.write((dist - dbase) as u32, dextra);
        }
    });
    let (code, len) = lit_codes[256];
    bits.write_rev(code, len);
    bits.finish()
}

/// Emits the dynamic block's code-length preamble (RFC 1951 §3.2.7):
/// HLIT/HDIST/HCLEN, the code-length-code lengths in their fixed
/// permutation, then both length arrays run-length coded with symbols
/// 16/17/18 under the code-length code.
fn write_code_lengths(bits: &mut BitWriter, lit_lens: &[u8], dist_lens: &[u8]) {
    const ORDER: [usize; 19] = [
        16, 17, 18, 0, 8, 7, 9, 6, 10, 5, 11, 4, 12, 3, 13, 2, 14, 1, 15,
    ];
    let hlit = lit_lens.len().max(257);
    let hdist = dist_lens.len().max(1);

    // RLE the concatenated length arrays into code-length symbols.
    let mut all = Vec::with_capacity(hlit + hdist);
    all.extend_from_slice(&lit_lens[..hlit]);
    all.extend_from_slice(&dist_lens[..hdist]);
    let mut cl_syms: Vec<(u8, u32, u32)> = Vec::new(); // (symbol, extra value, extra bits)
    let mut i = 0;
    while i < all.len() {
        let v = all[i];
        let mut run = 1;
        while i + run < all.len() && all[i + run] == v {
            run += 1;
        }
        if v == 0 {
            let mut left = run;
            while left >= 3 {
                if left >= 11 {
                    let n = left.min(138);
                    cl_syms.push((18, (n - 11) as u32, 7));
                    left -= n;
                } else {
                    let n = left.min(10);
                    cl_syms.push((17, (n - 3) as u32, 3));
                    left -= n;
                }
            }
            for _ in 0..left {
                cl_syms.push((0, 0, 0));
            }
        } else {
            cl_syms.push((v, 0, 0));
            let mut left = run - 1;
            while left >= 3 {
                let n = left.min(6);
                cl_syms.push((16, (n - 3) as u32, 2));
                left -= n;
            }
            for _ in 0..left {
                cl_syms.push((v, 0, 0));
            }
        }
        i += run;
    }

    let mut cl_freq = [0u32; 19];
    for &(s, _, _) in &cl_syms {
        cl_freq[s as usize] += 1;
    }
    let cl_lens = huffman_lengths(&cl_freq, 7);
    let cl_codes = canonical_codes(&cl_lens);
    let hclen = ORDER
        .iter()
        .rposition(|&s| cl_lens[s] != 0)
        .map_or(4, |p| (p + 1).max(4));

    bits.write((hlit - 257) as u32, 5);
    bits.write((hdist - 1) as u32, 5);
    bits.write((hclen - 4) as u32, 4);
    for &s in &ORDER[..hclen] {
        bits.write(u32::from(cl_lens[s]), 3);
    }
    for &(s, extra, extra_bits) in &cl_syms {
        let (code, len) = cl_codes[s as usize];
        bits.write_rev(code, len);
        bits.write(extra, extra_bits);
    }
}

/// Length-limited Huffman code lengths for `freq`, longest code at most
/// `limit` bits. Zero-frequency symbols get length 0. The tree is built
/// with the classic two-queue merge; when it overflows the limit the
/// frequencies are halved (floors at 1) and rebuilt — the standard
/// pragmatic limiter, always terminating because frequencies converge to
/// equal.
fn huffman_lengths<const N: usize>(freq: &[u32; N], limit: u8) -> Vec<u8> {
    let mut scaled: Vec<u32> = freq.to_vec();
    loop {
        let lens = huffman_lengths_unlimited(&scaled);
        if lens.iter().all(|&l| l <= limit) {
            return lens;
        }
        for f in scaled.iter_mut() {
            if *f > 0 {
                *f = (*f).div_ceil(2);
            }
        }
    }
}

fn huffman_lengths_unlimited(freq: &[u32]) -> Vec<u8> {
    #[derive(Clone)]
    struct Node {
        weight: u64,
        kids: Option<(usize, usize)>,
        symbol: usize,
    }
    let mut nodes: Vec<Node> = freq
        .iter()
        .enumerate()
        .filter(|&(_, &f)| f > 0)
        .map(|(symbol, &f)| Node {
            weight: u64::from(f),
            kids: None,
            symbol,
        })
        .collect();
    let mut lens = vec![0u8; freq.len()];
    match nodes.len() {
        0 => return lens,
        1 => {
            // A single used symbol still needs one bit on the wire.
            lens[nodes[0].symbol] = 1;
            return lens;
        }
        _ => {}
    }
    // Two-queue merge over the sorted leaves.
    nodes.sort_by_key(|n| n.weight);
    let mut merged: Vec<Node> = Vec::with_capacity(nodes.len());
    let mut all: Vec<Node> = Vec::with_capacity(nodes.len() * 2);
    let (mut li, mut mi) = (0usize, 0usize);
    let take = |li: &mut usize,
                mi: &mut usize,
                all: &mut Vec<Node>,
                merged: &mut Vec<Node>,
                nodes: &[Node]|
     -> usize {
        let from_leaf = match (nodes.get(*li), merged.get(*mi)) {
            (Some(l), Some(m)) => l.weight <= m.weight,
            (Some(_), None) => true,
            (None, Some(_)) => false,
            (None, None) => unreachable!("both queues empty mid-merge"),
        };
        if from_leaf {
            all.push(nodes[*li].clone());
            *li += 1;
        } else {
            all.push(merged[*mi].clone());
            *mi += 1;
        }
        all.len() - 1
    };
    let total = nodes.len();
    let mut remaining = total;
    while remaining > 1 {
        let a = take(&mut li, &mut mi, &mut all, &mut merged, &nodes);
        let b = take(&mut li, &mut mi, &mut all, &mut merged, &nodes);
        merged.push(Node {
            weight: all[a].weight + all[b].weight,
            kids: Some((a, b)),
            symbol: usize::MAX,
        });
        remaining -= 1;
    }
    // Depth-first depth assignment from the final root.
    let root = merged.last().unwrap().clone();
    let mut stack = vec![(root, 0u8)];
    while let Some((node, depth)) = stack.pop() {
        match node.kids {
            None => lens[node.symbol] = depth.max(1),
            Some((a, b)) => {
                stack.push((all[a].clone(), depth + 1));
                stack.push((all[b].clone(), depth + 1));
            }
        }
    }
    lens
}

/// Canonical deflate codes for the given lengths (RFC 1951 §3.2.2):
/// `(code, bits)` per symbol, code value in natural (MSB-first) order —
/// [`BitWriter::write_rev`] reverses it onto the LSB-first stream.
fn canonical_codes(lens: &[u8]) -> Vec<(u32, u32)> {
    let max = lens.iter().copied().max().unwrap_or(0) as usize;
    let mut bl_count = vec![0u32; max + 1];
    for &l in lens {
        if l > 0 {
            bl_count[l as usize] += 1;
        }
    }
    let mut next = vec![0u32; max + 2];
    let mut code = 0u32;
    for bits in 1..=max {
        code = (code + bl_count[bits - 1]) << 1;
        next[bits] = code;
    }
    lens.iter()
        .map(|&l| {
            if l == 0 {
                return (0, 0);
            }
            let c = next[l as usize];
            next[l as usize] += 1;
            (c, u32::from(l))
        })
        .collect()
}

/// LSB-first deflate bit stream over a 64-bit accumulator. The
/// accumulator drains eight bytes at a time into spare capacity the
/// writer maintains ahead of the cursor, so the per-symbol cost is an
/// or, a shift, and one length check.
struct BitWriter {
    out: Vec<u8>,
    len: usize,
    acc: u64,
    filled: u32,
}

impl BitWriter {
    fn with_capacity(cap: usize) -> BitWriter {
        BitWriter {
            out: vec![0; cap.max(64)],
            len: 0,
            acc: 0,
            filled: 0,
        }
    }

    /// Drains whole bytes of the accumulator with one 8-byte store.
    #[inline]
    fn drain(&mut self) {
        if self.out.len() < self.len + 8 {
            self.out.resize(self.out.len() * 2 + 64, 0);
        }
        self.out[self.len..self.len + 8].copy_from_slice(&self.acc.to_le_bytes());
        let bytes = (self.filled / 8) as usize;
        self.len += bytes;
        self.acc >>= bytes * 8;
        self.filled &= 7;
    }

    /// Writes `n <= 32` bits of `v`, LSB first (extra bits and headers).
    #[inline]
    fn write(&mut self, v: u32, n: u32) {
        self.acc |= u64::from(v) << self.filled;
        self.filled += n;
        if self.filled >= 32 {
            self.drain();
        }
    }

    /// Writes an `n`-bit Huffman code given in natural order (RFC 1951
    /// packs codes most significant bit first onto the LSB-first stream).
    #[inline]
    fn write_rev(&mut self, code: u32, n: u32) {
        let rev = code.reverse_bits() >> (32 - n);
        self.write(rev, n);
    }

    fn finish(mut self) -> Vec<u8> {
        self.filled += 7;
        self.drain();
        self.out.truncate(self.len);
        self.out
    }
}

/// RFC 1950 Adler-32 over the uncompressed stream.
fn adler32(data: &[u8]) -> u32 {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is baseline on aarch64.
        return unsafe { adler_hw::adler32(data) };
    }
    #[cfg(target_arch = "x86_64")]
    if std::arch::is_x86_feature_detected!("ssse3") {
        // SAFETY: the `ssse3` target feature was just detected at runtime.
        return unsafe { adler_hw::adler32(data) };
    }
    #[allow(unreachable_code)]
    adler32_soft(data)
}

/// The portable form — and the reference the vector forms are held to in
/// the tests.
fn adler32_soft(data: &[u8]) -> u32 {
    const MOD: u32 = 65521;
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in data.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= MOD;
        b %= MOD;
    }
    (b << 16) | a
}

/// Adler-32 on 16-byte lanes: per block `a += Σd` and
/// `b += 16·a_prev + Σ(16−i)·d[i]`, sums reduced per block, the modulus
/// deferred across the same 5552-byte span the scalar form uses.
#[cfg(target_arch = "aarch64")]
mod adler_hw {
    use core::arch::aarch64::{
        vaddlvq_u16, vaddlvq_u8, vget_high_u8, vget_low_u8, vld1q_u8, vmull_u8,
    };

    /// # Safety
    /// NEON is baseline on aarch64; callers need no feature check.
    pub unsafe fn adler32(data: &[u8]) -> u32 {
        const MOD: u32 = 65521;
        let weights_lo = [16u8, 15, 14, 13, 12, 11, 10, 9];
        let weights_hi = [8u8, 7, 6, 5, 4, 3, 2, 1];
        let wlo = core::arch::aarch64::vld1_u8(weights_lo.as_ptr());
        let whi = core::arch::aarch64::vld1_u8(weights_hi.as_ptr());
        let (mut a, mut b) = (1u32, 0u32);
        for chunk in data.chunks(5552) {
            let (blocks, tail) = chunk.as_chunks::<16>();
            for block in blocks {
                let d = vld1q_u8(block.as_ptr());
                b = b.wrapping_add(a.wrapping_mul(16));
                b = b.wrapping_add(
                    vaddlvq_u16(vmull_u8(vget_low_u8(d), wlo))
                        + vaddlvq_u16(vmull_u8(vget_high_u8(d), whi)),
                );
                a = a.wrapping_add(u32::from(vaddlvq_u8(d)));
            }
            for &byte in tail {
                a = a.wrapping_add(u32::from(byte));
                b = b.wrapping_add(a);
            }
            a %= MOD;
            b %= MOD;
        }
        (b << 16) | a
    }
}

/// See the aarch64 twin above; same contract on SSSE3 lanes.
#[cfg(target_arch = "x86_64")]
mod adler_hw {
    use core::arch::x86_64::{
        _mm_add_epi32, _mm_cvtsi128_si32, _mm_loadu_si128, _mm_madd_epi16, _mm_maddubs_epi16,
        _mm_sad_epu8, _mm_set1_epi16, _mm_setr_epi8, _mm_setzero_si128, _mm_shuffle_epi32,
    };

    /// # Safety
    /// Requires the `ssse3` target feature.
    #[target_feature(enable = "ssse3")]
    pub unsafe fn adler32(data: &[u8]) -> u32 {
        const MOD: u32 = 65521;
        let weights = _mm_setr_epi8(16, 15, 14, 13, 12, 11, 10, 9, 8, 7, 6, 5, 4, 3, 2, 1);
        let ones = _mm_set1_epi16(1);
        let zero = _mm_setzero_si128();
        let (mut a, mut b) = (1u32, 0u32);
        for chunk in data.chunks(5552) {
            let (blocks, tail) = chunk.as_chunks::<16>();
            for block in blocks {
                let d = _mm_loadu_si128(block.as_ptr().cast());
                b = b.wrapping_add(a.wrapping_mul(16));
                let weighted = _mm_madd_epi16(_mm_maddubs_epi16(d, weights), ones);
                let folded = _mm_add_epi32(weighted, _mm_shuffle_epi32(weighted, 0b00_01_10_11));
                let folded = _mm_add_epi32(folded, _mm_shuffle_epi32(folded, 0b00_00_00_01));
                b = b.wrapping_add(_mm_cvtsi128_si32(folded) as u32);
                let sums = _mm_sad_epu8(d, zero);
                let total = _mm_cvtsi128_si32(sums) as u32
                    + _mm_cvtsi128_si32(_mm_shuffle_epi32(sums, 0b00_00_00_10)) as u32;
                a = a.wrapping_add(total);
            }
            for &byte in tail {
                a = a.wrapping_add(u32::from(byte));
                b = b.wrapping_add(a);
            }
            a %= MOD;
            b %= MOD;
        }
        (b << 16) | a
    }
}

/// Appends one PNG chunk: length, type, data, CRC-32 of type+data.
fn push_chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    out.extend_from_slice(&(data.len() as u32).to_be_bytes());
    out.extend_from_slice(kind);
    out.extend_from_slice(data);
    let mut crc = Crc32::new();
    crc.update(kind);
    crc.update(data);
    out.extend_from_slice(&crc.finish().to_be_bytes());
}

/// CRC-32 (ISO 3309, the PNG polynomial), slicing-by-eight: eight table
/// lookups fold eight bytes per step, with the classic byte-at-a-time
/// loop finishing the tail.
struct Crc32 {
    state: u32,
}

impl Crc32 {
    fn new() -> Crc32 {
        Crc32 { state: !0 }
    }

    fn update(&mut self, data: &[u8]) {
        let mut crc = self.state;
        let (chunks, tail) = data.as_chunks::<8>();
        for chunk in chunks {
            let lo = u32::from_le_bytes(chunk[..4].try_into().unwrap()) ^ crc;
            let hi = u32::from_le_bytes(chunk[4..].try_into().unwrap());
            crc = CRC_TABLES[7][(lo & 0xff) as usize]
                ^ CRC_TABLES[6][((lo >> 8) & 0xff) as usize]
                ^ CRC_TABLES[5][((lo >> 16) & 0xff) as usize]
                ^ CRC_TABLES[4][(lo >> 24) as usize]
                ^ CRC_TABLES[3][(hi & 0xff) as usize]
                ^ CRC_TABLES[2][((hi >> 8) & 0xff) as usize]
                ^ CRC_TABLES[1][((hi >> 16) & 0xff) as usize]
                ^ CRC_TABLES[0][(hi >> 24) as usize];
        }
        for &b in tail {
            crc = CRC_TABLES[0][((crc ^ u32::from(b)) & 0xff) as usize] ^ (crc >> 8);
        }
        self.state = crc;
    }

    fn finish(self) -> u32 {
        !self.state
    }
}

const CRC_TABLES: [[u32; 256]; 8] = {
    let mut tables = [[0u32; 256]; 8];
    let mut n = 0;
    while n < 256 {
        let mut c = n as u32;
        let mut k = 0;
        while k < 8 {
            c = if c & 1 != 0 {
                0xedb88320 ^ (c >> 1)
            } else {
                c >> 1
            };
            k += 1;
        }
        tables[0][n] = c;
        n += 1;
    }
    let mut t = 1;
    while t < 8 {
        let mut n = 0;
        while n < 256 {
            let prev = tables[t - 1][n];
            tables[t][n] = tables[0][(prev & 0xff) as usize] ^ (prev >> 8);
            n += 1;
        }
        t += 1;
    }
    tables
};

#[cfg(test)]
mod tests {
    use super::*;

    /// Decodes with the `png` crate — the independent implementation the
    /// rest of the codebase already trusts — and returns the RGBA pixels.
    fn round_trip(width: u32, height: u32, rgba: &[u8]) -> Vec<u8> {
        let encoded = encode_rgba(width, height, rgba);
        let decoder = png::Decoder::new(std::io::Cursor::new(encoded));
        let mut reader = decoder.read_info().expect("decodable header");
        assert_eq!(reader.info().width, width);
        assert_eq!(reader.info().height, height);
        let mut buf = vec![0u8; reader.output_buffer_size().expect("sized")];
        let info = reader.next_frame(&mut buf).expect("decodable image");
        buf.truncate(info.buffer_size());
        buf
    }

    #[test]
    fn flat_image_round_trips() {
        let rgba = vec![0x7fu8; 16 * 8 * 4];
        assert_eq!(round_trip(16, 8, &rgba), rgba);
    }

    #[test]
    fn transparent_image_round_trips() {
        let rgba = vec![0u8; 33 * 7 * 4];
        assert_eq!(round_trip(33, 7, &rgba), rgba);
    }

    #[test]
    fn single_pixel_round_trips() {
        let rgba = [1u8, 2, 3, 4];
        assert_eq!(round_trip(1, 1, &rgba), rgba);
    }

    #[test]
    fn gradient_round_trips() {
        let (w, h) = (61u32, 23u32);
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                rgba.extend_from_slice(&[(x * 4) as u8, (y * 11) as u8, (x + y) as u8, 255]);
            }
        }
        assert_eq!(round_trip(w, h, &rgba), rgba);
    }

    /// Pseudo-random pixels defeat every filter and run: the worst case is
    /// pure literals under a deep Huffman table.
    #[test]
    fn noise_round_trips() {
        let (w, h) = (57u32, 41u32);
        let mut state = 0x12345678u32;
        let mut rgba = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h * 4 {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            rgba.push((state >> 24) as u8);
        }
        assert_eq!(round_trip(w, h, &rgba), rgba);
    }

    /// Sparse ink over a white page — the shape of a rendered text page,
    /// where the zero-run coding does its work.
    #[test]
    fn page_like_image_round_trips() {
        let (w, h) = (200u32, 120u32);
        let mut rgba = vec![255u8; (w * h * 4) as usize];
        for y in (10..100).step_by(9) {
            for x in 12..180 {
                let i = ((y * w + x) * 4) as usize;
                rgba[i..i + 4].copy_from_slice(&[20, 20, 20, 255]);
            }
        }
        assert_eq!(round_trip(w, h, &rgba), rgba);
    }

    /// The vector residual sums must agree with the portable form byte
    /// for byte — a silent drift would still round-trip but quietly pick
    /// worse filters.
    #[test]
    fn vector_residual_sums_match_the_portable_form() {
        let mut state = 0xdeadbeefu32;
        let mut next = move || {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            (state >> 24) as u8
        };
        for len in [4usize, 16, 20, 64, 100, 257] {
            let row: Vec<u8> = (0..len).map(|_| next()).collect();
            let prev: Vec<u8> = (0..len).map(|_| next()).collect();
            for p in [&[][..], &prev[..]] {
                let soft = residual_sums_soft(&row, p);
                #[cfg(any(target_arch = "aarch64", target_arch = "x86_64"))]
                {
                    // SAFETY: NEON/SSE2 are baseline on these targets.
                    let hard = unsafe { filter_hw::residual_sums(&row, p) };
                    assert_eq!(hard, soft, "len {len} prev? {}", !p.is_empty());
                }
            }
        }
    }

    /// The vector Adler-32 must equal the portable form on every length
    /// and alignment.
    #[test]
    fn vector_adler_matches_the_portable_form() {
        let mut state = 0xabcdef01u32;
        let data: Vec<u8> = (0..20000)
            .map(|_| {
                state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                (state >> 24) as u8
            })
            .collect();
        for len in [0usize, 1, 15, 16, 17, 5551, 5552, 5553, 20000] {
            assert_eq!(
                adler32(&data[..len]),
                adler32_soft(&data[..len]),
                "len {len}"
            );
        }
    }

    #[test]
    fn zero_height_image_encodes_to_a_valid_container() {
        let out = encode_rgba(5, 0, &[]);
        assert_eq!(&out[..8], b"\x89PNG\r\n\x1a\n");
    }
}

#[cfg(test)]
mod stage_times {
    use super::*;

    /// Not a correctness test: prints per-stage times on a page-shaped
    /// pixmap so optimization goes where the time is. Run explicitly:
    /// `cargo test --release -p pdfboss-render stage_times -- --nocapture --ignored`
    #[test]
    #[ignore]
    fn print_stage_times() {
        let (w, h) = (612usize, 792usize);
        let mut rgba = vec![255u8; w * h * 4];
        // Page-like content: text-ish runs and a photo-ish band.
        let mut state = 1u32;
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                if y % 9 < 2 && x > 30 && x < 580 {
                    rgba[i..i + 3].copy_from_slice(&[30, 30, 30]);
                } else if (300..500).contains(&y) {
                    state = state.wrapping_mul(1664525).wrapping_add(1013904223);
                    rgba[i] = (state >> 24) as u8;
                    rgba[i + 1] = (state >> 16) as u8;
                    rgba[i + 2] = (state >> 8) as u8;
                }
            }
        }
        let stride = w * 4;
        let reps = 50;

        let t0 = std::time::Instant::now();
        let mut filtered = Vec::new();
        for _ in 0..reps {
            filtered = vec![0u8; (stride + 1) * h];
            let mut prev: &[u8] = &[];
            for (row, out) in rgba
                .chunks_exact(stride)
                .zip(filtered.chunks_exact_mut(stride + 1))
            {
                out[0] = choose_filter(row, prev, &mut out[1..]);
                prev = row;
            }
        }
        println!("filter+copy: {:?}/page", t0.elapsed() / reps);

        let t0 = std::time::Instant::now();
        let mut out = Vec::new();
        for _ in 0..reps {
            out = deflate_filtered(&filtered);
        }
        println!(
            "deflate:     {:?}/page ({} bytes)",
            t0.elapsed() / reps,
            out.len()
        );

        let t0 = std::time::Instant::now();
        let mut a = 0u32;
        for _ in 0..reps {
            a = adler32(&filtered);
        }
        println!("adler32:     {:?}/page (sum {a:08x})", t0.elapsed() / reps);

        let t0 = std::time::Instant::now();
        for _ in 0..reps {
            let mut crc = Crc32::new();
            crc.update(&out);
            std::hint::black_box(crc.finish());
        }
        println!("crc32:       {:?}/page", t0.elapsed() / reps);

        let t0 = std::time::Instant::now();
        for _ in 0..reps {
            std::hint::black_box(encode_rgba(w as u32, h as u32, &rgba));
        }
        println!("whole:       {:?}/page", t0.elapsed() / reps);
    }
}
