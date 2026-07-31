//! MQ arithmetic decoder (ITU-T T.800 Annex C): the adaptive binary
//! arithmetic coder driven by Tier-1. This crate carries its own copy —
//! zero dependencies.
//!
//! The C.3 procedures implemented here: INITDEC (C.3.5), DECODE (C.3.2)
//! with the MPS/LPS conditional-exchange procedures, RENORMD (C.3.3) and
//! BYTEIN (C.3.4, which undoes the 0xFF bit-stuffing on the compressed
//! stream), driven by the probability table from Table C.2.
//!
//! The decoder is fed attacker-controlled bytes, so it is total: reads
//! past the end of the segment yield 0xFF (which lands on the C.3.4
//! marker branch and feeds 1-bits), every register step is bounded, and
//! no input can panic, hang, or read out of bounds.

/// The 47-state probability estimation table (T.800 Table C.2), as
/// `(Qe, NMPS, NLPS, SWITCH)` rows indexed by `I(CX)`.
///
/// `Qe` is the current LPS sub-interval estimate, scaled so that the
/// renormalized interval register `A` holds 0x8000 (C.1.2: 0x8000 stands
/// for decimal 0,75). `NMPS`/`NLPS` are the next indices after an MPS or
/// LPS renormalization, and `SWITCH` marks the rows where an LPS also
/// flips the MPS sense (C.2.5). Qe is written in decimal; the trailing
/// comment carries the spec's hexadecimal column.
const TABLE_C2: [(u16, u8, u8, u8); 47] = [
    (22017, 1, 1, 1),   // Qe 0x5601
    (13313, 2, 6, 0),   // Qe 0x3401
    (6145, 3, 9, 0),    // Qe 0x1801
    (2753, 4, 12, 0),   // Qe 0x0AC1
    (1313, 5, 29, 0),   // Qe 0x0521
    (545, 38, 33, 0),   // Qe 0x0221
    (22017, 7, 6, 1),   // Qe 0x5601
    (21505, 8, 14, 0),  // Qe 0x5401
    (18433, 9, 14, 0),  // Qe 0x4801
    (14337, 10, 14, 0), // Qe 0x3801
    (12289, 11, 17, 0), // Qe 0x3001
    (9217, 12, 18, 0),  // Qe 0x2401
    (7169, 13, 20, 0),  // Qe 0x1C01
    (5633, 29, 21, 0),  // Qe 0x1601
    (22017, 15, 14, 1), // Qe 0x5601
    (21505, 16, 14, 0), // Qe 0x5401
    (20737, 17, 15, 0), // Qe 0x5101
    (18433, 18, 16, 0), // Qe 0x4801
    (14337, 19, 17, 0), // Qe 0x3801
    (13313, 20, 18, 0), // Qe 0x3401
    (12289, 21, 19, 0), // Qe 0x3001
    (10241, 22, 19, 0), // Qe 0x2801
    (9217, 23, 20, 0),  // Qe 0x2401
    (8705, 24, 21, 0),  // Qe 0x2201
    (7169, 25, 22, 0),  // Qe 0x1C01
    (6145, 26, 23, 0),  // Qe 0x1801
    (5633, 27, 24, 0),  // Qe 0x1601
    (5121, 28, 25, 0),  // Qe 0x1401
    (4609, 29, 26, 0),  // Qe 0x1201
    (4353, 30, 27, 0),  // Qe 0x1101
    (2753, 31, 28, 0),  // Qe 0x0AC1
    (2497, 32, 29, 0),  // Qe 0x09C1
    (2209, 33, 30, 0),  // Qe 0x08A1
    (1313, 34, 31, 0),  // Qe 0x0521
    (1089, 35, 32, 0),  // Qe 0x0441
    (673, 36, 33, 0),   // Qe 0x02A1
    (545, 37, 34, 0),   // Qe 0x0221
    (321, 38, 35, 0),   // Qe 0x0141
    (273, 39, 36, 0),   // Qe 0x0111
    (133, 40, 37, 0),   // Qe 0x0085
    (73, 41, 38, 0),    // Qe 0x0049
    (37, 42, 39, 0),    // Qe 0x0025
    (21, 43, 40, 0),    // Qe 0x0015
    (9, 44, 41, 0),     // Qe 0x0009
    (5, 45, 42, 0),     // Qe 0x0005
    (1, 45, 43, 0),     // Qe 0x0001
    (22017, 46, 46, 0), // Qe 0x5601
];

/// One adaptive context: an index into the Table C.2 state machine plus the
/// current MPS sense (C.1). Tier-1 owns one per Annex D context label and
/// resets them per Table D.7 (and on the Table A.19 reset-context style).
#[derive(Clone, Copy, Debug)]
pub(crate) struct MqContext {
    /// Current state index I(CX) into Table C.2 (0..=46).
    pub index: u8,
    /// Current most-probable-symbol sense MPS(CX) (0 or 1).
    pub mps: u8,
}

impl MqContext {
    /// A context starting at Table C.2 state `index` with MPS = 0, the
    /// Annex D initial sense (Table D.7).
    pub(crate) fn new(index: u8) -> Self {
        MqContext { index, mps: 0 }
    }

    /// The `(Qe, NMPS, NLPS, SWITCH)` row this context estimates from.
    ///
    /// The index is clamped into the table before use. It cannot actually
    /// leave the table — every NMPS/NLPS in Table C.2 points back inside
    /// it, and the constructors take 0..=46 — but clamping keeps that a
    /// property of this function rather than of the whole state machine.
    fn row(&self) -> (u16, u8, u8, u8) {
        TABLE_C2[usize::from(self.index).min(TABLE_C2.len() - 1)]
    }
}

/// The decoder state over one terminated codeword segment (C.3.1 register
/// conventions: C holds code bits, A the interval, CT counts bits until the
/// next BYTEIN, BP indexes the compressed data).
pub(crate) struct MqDecoder<'a> {
    /// Compressed bytes of the current codeword segment.
    data: &'a [u8],
    /// C register (C.3.1).
    c: u32,
    /// A (interval) register.
    a: u32,
    /// Count-down until the next BYTEIN.
    ct: u32,
    /// Next byte position (BP).
    bp: usize,
}

impl<'a> MqDecoder<'a> {
    /// Prepares decoding of one codeword segment, running INITDEC
    /// (C.3.5, Figure C.20): the first byte lands in the low byte of
    /// Chigh, BYTEIN pulls the second, and the 7-bit shift aligns C with
    /// the starting interval A = 0x8000. Construction never fails — a
    /// short or empty segment simply decodes as if padded with 0xFF bytes
    /// (C.3.4 BYTEIN feeds 1-bits past the end).
    pub(crate) fn new(data: &'a [u8]) -> Self {
        let mut dec = MqDecoder {
            data,
            c: 0,
            a: 0,
            ct: 0,
            bp: 0,
        };
        dec.c = u32::from(dec.byte(dec.bp)) << 16;
        dec.byte_in();
        dec.c <<= 7;
        dec.ct -= 7;
        dec.a = 0x8000;
        dec
    }

    /// The segment byte at `i`, or 0xFF past the end. The 0xFF padding is
    /// what makes a truncated segment run into the C.3.4 marker branch and
    /// decode from 1-bits, instead of reading out of bounds.
    fn byte(&self, i: usize) -> u8 {
        self.data.get(i).copied().unwrap_or(255)
    }

    /// BYTEIN (C.3.4, Figure C.19): tops Clow back up with the next
    /// compressed byte, undoing the encoder's bit stuffing.
    ///
    /// A 0xFF followed by a byte above 0x8F can only be a marker — the
    /// encoder never emits that pair — so BP stays put and 1-bits feed the
    /// register (`C += 0xFF00`, CT = 8) from then on. A byte after a plain
    /// 0xFF carries a stuffed bit: only 7 data bits, entering at bit 9 so
    /// the stuff bit lands on the low bit of Chigh, with CT = 7.
    fn byte_in(&mut self) {
        if self.byte(self.bp) == 255 {
            if self.byte(self.bp + 1) > 143 {
                self.c = self.c.wrapping_add(0xFF00);
                self.ct = 8;
            } else {
                self.bp += 1;
                self.c = self.c.wrapping_add(u32::from(self.byte(self.bp)) << 9);
                self.ct = 7;
            }
        } else {
            self.bp += 1;
            self.c = self.c.wrapping_add(u32::from(self.byte(self.bp)) << 8);
            self.ct = 8;
        }
    }

    /// RENORMD (C.3.3, Figure C.18): doubles A and C until A regains bit
    /// 15, pulling a byte in whenever CT runs out.
    ///
    /// A is non-zero on entry — the LPS path set it to Qe >= 1, the MPS
    /// path left at least 0x8000 - 0x5601 — so 16 shifts always suffice;
    /// the loop is bounded there to make termination structural rather
    /// than inferred, so no input can hang the decoder. Bits shifted past
    /// the top of C are spent code bits and fall away.
    fn renormd(&mut self) {
        for _ in 0..16 {
            if self.ct == 0 {
                self.byte_in();
            }
            self.a <<= 1;
            self.c <<= 1;
            self.ct -= 1;
            if self.a & 0x8000 != 0 {
                break;
            }
        }
    }

    /// MPS_EXCHANGE (C.3.2, Figure C.16): decides the symbol when the MPS
    /// sub-interval was selected but A fell below 0x8000, so the two
    /// sub-intervals may have swapped sizes; updates the estimate.
    fn mps_exchange(&self, cx: &mut MqContext) -> u8 {
        let (qe, nmps, nlps, switch) = cx.row();
        if self.a < u32::from(qe) {
            let d = 1 - cx.mps;
            if switch == 1 {
                cx.mps = 1 - cx.mps;
            }
            cx.index = nlps;
            d
        } else {
            let d = cx.mps;
            cx.index = nmps;
            d
        }
    }

    /// LPS_EXCHANGE (C.3.2, Figure C.17): decides the symbol when the LPS
    /// sub-interval was selected, sets A to that sub-interval, and updates
    /// the estimate.
    fn lps_exchange(&mut self, cx: &mut MqContext) -> u8 {
        let (qe, nmps, nlps, switch) = cx.row();
        let d = if self.a < u32::from(qe) {
            cx.index = nmps;
            cx.mps
        } else {
            let d = 1 - cx.mps;
            if switch == 1 {
                cx.mps = 1 - cx.mps;
            }
            cx.index = nlps;
            d
        };
        self.a = u32::from(qe);
        d
    }

    /// Decodes one binary decision in context `cx` (C.3.2 DECODE,
    /// Figure C.15), updating the context's state index and MPS sense per
    /// the Table C.2 transition columns. Never fails: exhausted segments
    /// keep producing bits from the 0xFF padding rule, and Tier-1's pass
    /// budget bounds the call count.
    ///
    /// The subtractions are wrapping only as belt and braces: A >= 0x8000
    /// and Qe <= 0x5601 on entry, so `A - Qe` cannot underflow, and the
    /// `Chigh -= Qe` branch is guarded by `Chigh >= Qe`.
    pub(crate) fn decode(&mut self, cx: &mut MqContext) -> u32 {
        let qe = u32::from(cx.row().0);
        self.a = self.a.wrapping_sub(qe);
        let d = if (self.c >> 16) < qe {
            let d = self.lps_exchange(cx);
            self.renormd();
            d
        } else {
            self.c = self.c.wrapping_sub(qe << 16);
            if self.a & 0x8000 == 0 {
                let d = self.mps_exchange(cx);
                self.renormd();
                d
            } else {
                cx.mps
            }
        };
        u32::from(d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Decodes `n` decisions from `data`, drawing contexts round-robin from
    /// `contexts`, and returns them as a '0'/'1' string. Asserts along the
    /// way that DECODE only ever yields 0 or 1 (C.3.2).
    fn decode_run(data: &[u8], contexts: &mut [MqContext], n: usize) -> String {
        let mut dec = MqDecoder::new(data);
        (0..n)
            .map(|i| {
                let d = dec.decode(&mut contexts[i % contexts.len()]);
                assert!(d <= 1, "decision {i} was {d}, not a binary decision");
                if d == 0 {
                    '0'
                } else {
                    '1'
                }
            })
            .collect()
    }

    /// Table C.2 has exactly 47 rows, every transition stays inside the
    /// table, SWITCH is a flag, and Qe is a valid LPS sub-interval: non-zero
    /// and at most 22017 (0x5601), the largest value the spec assigns.
    #[test]
    fn table_c2_shape() {
        assert_eq!(TABLE_C2.len(), 47);
        for (idx, &(qe, nmps, nlps, switch)) in TABLE_C2.iter().enumerate() {
            assert!(usize::from(nmps) < 47, "row {idx} NMPS out of range");
            assert!(usize::from(nlps) < 47, "row {idx} NLPS out of range");
            assert!(switch <= 1, "row {idx} SWITCH must be 0 or 1");
            assert!(qe >= 1, "row {idx} Qe must be non-zero");
            assert!(qe <= 22017, "row {idx} Qe exceeds the half interval");
        }
    }

    /// Spot-checks the rows a transcription slip is most likely to hit:
    /// the ones whose NMPS/NLPS break the `index + 1` pattern, the three
    /// SWITCH rows, the most skewed row 45 and the non-adapting row 46
    /// (C.2.5: "The final index state 46 can be used to establish a fixed
    /// 0,5 probability estimate").
    ///
    /// Hand conversions from the spec's hexadecimal column:
    /// 0x5601 = 5*4096 + 6*256 + 1 = 22017; 0x0521 = 5*256 + 2*16 + 1 =
    /// 1313; 0x0221 = 2*256 + 2*16 + 1 = 545; 0x1601 = 4096 + 6*256 + 1 =
    /// 5633; 0x0001 = 1.
    #[test]
    fn table_c2_irregular_rows() {
        assert_eq!(TABLE_C2[0], (22017, 1, 1, 1));
        assert_eq!(TABLE_C2[4], (1313, 5, 29, 0));
        assert_eq!(TABLE_C2[5], (545, 38, 33, 0));
        assert_eq!(TABLE_C2[6], (22017, 7, 6, 1));
        assert_eq!(TABLE_C2[13], (5633, 29, 21, 0));
        assert_eq!(TABLE_C2[14], (22017, 15, 14, 1));
        assert_eq!(TABLE_C2[45], (1, 45, 43, 0));
        assert_eq!(TABLE_C2[46], (22017, 46, 46, 0));
        let switches: Vec<usize> = TABLE_C2
            .iter()
            .enumerate()
            .filter(|(_, row)| row.3 == 1)
            .map(|(idx, _)| idx)
            .collect();
        assert_eq!(switches, vec![0, 6, 14]);
    }

    /// Table C.2 is a probability estimator (C.2.5), so its transitions must
    /// move the estimate the right way: an MPS renormalization must never
    /// raise Qe, and an LPS renormalization must never lower it — except on
    /// the SWITCH rows, where the two symbols trade places and the new Qe
    /// estimates the other symbol. This is the one check spot-checking can't
    /// replace: a row copied to the wrong place survives a row-by-row
    /// comparison of two transcriptions, but not a direction check.
    #[test]
    fn the_estimator_moves_the_right_direction() {
        for (idx, &(qe, nmps, nlps, switch)) in TABLE_C2.iter().enumerate() {
            let after_mps = TABLE_C2[usize::from(nmps)].0;
            assert!(after_mps <= qe, "row {idx}: an MPS raised Qe");
            if switch == 0 {
                let after_lps = TABLE_C2[usize::from(nlps)].0;
                assert!(after_lps >= qe, "row {idx}: an LPS lowered Qe");
            }
        }
    }

    /// An all-zero stream, decoded by hand from Figures C.15–C.20.
    ///
    /// INITDEC (C.3.5): C = 0x00 << 16 = 0; BYTEIN sees B = 0 (not 0xFF), so
    /// C += 0 and CT = 8; C <<= 7 keeps C = 0, CT = 1, A = 0x8000 = 32768.
    /// Chigh stays 0 throughout, so every decision takes the C.15 LPS branch
    /// and the decision is made in LPS_EXCHANGE (Figure C.17):
    ///
    /// 1. I=0, Qe=22017: A = 32768-22017 = 10751 < Qe, so the sub-intervals
    ///    had swapped — conditional exchange, D = MPS = 0, I = NMPS(0) = 1,
    ///    A = 22017; RENORMD doubles once (44034), CT 1 -> 0.
    /// 2. I=1, Qe=13313: A = 44034-13313 = 30721 >= Qe — a true LPS,
    ///    D = 1-MPS = 1, SWITCH(1)=0, I = NLPS(1) = 6, A = 13313; RENORMD
    ///    pulls byte 0x00 in (CT=8) and doubles twice: A = 53252, CT = 6.
    /// 3. I=6, Qe=22017: A = 53252-22017 = 31235 >= Qe — true LPS, D = 1,
    ///    SWITCH(6)=1 flips MPS to 1, I = NLPS(6) = 6, A = 22017 -> 44034.
    /// 4. I=6, MPS=1: A = 44034-22017 = 22017 >= Qe — true LPS, D = 1-1 = 0,
    ///    MPS flips back to 0, I stays 6, A = 22017 -> 44034.
    ///
    /// Decision 4 now repeats with the sense alternating: the stream settles
    /// into 1,0,1,0,… from the SWITCH self-loop at state 6.
    #[test]
    fn all_zero_stream_matches_the_hand_trace() {
        let mut cx = [MqContext::new(0)];
        let bits = decode_run(&[0u8; 64], &mut cx, 16);
        assert_eq!(bits, "0110101010101010");
        assert_eq!(cx[0].index, 6);
        assert_eq!(cx[0].mps, 0);
    }

    /// A stream whose code register starts high, decoded by hand: the MPS
    /// branch of Figure C.15 (`Chigh -= Qe`) runs, with and without
    /// renormalization.
    ///
    /// INITDEC: C = 0x80 << 16; BYTEIN adds 0x00 (CT=8); C <<= 7 makes
    /// Chigh = 0x4000 = 16384, CT = 1, A = 32768.
    ///
    /// 1. I=0, Qe=22017: A = 10751; Chigh 16384 < Qe — LPS_EXCHANGE with
    ///    A < Qe: conditional exchange, D = 0, I = 1, A = 22017; RENORMD
    ///    doubles once: A = 44034, Chigh = 0x8000 = 32768, CT = 0.
    /// 2. I=1, Qe=13313: A = 30721; Chigh 32768 >= Qe — MPS branch, Chigh
    ///    becomes 19455; A lost bit 15, so MPS_EXCHANGE with A >= Qe:
    ///    D = MPS = 0, I = NMPS(1) = 2; RENORMD doubles once after BYTEIN:
    ///    A = 61442, Chigh = 38910, CT = 7.
    /// 3. I=2, Qe=6145: A = 55297 keeps bit 15; Chigh 38910 >= Qe, so the
    ///    no-renormalization fast path: D = MPS = 0, Chigh = 32765.
    /// 4. A = 49152, Chigh = 26620: D = 0 again. 5. A = 43007: D = 0.
    #[test]
    fn mps_branch_matches_the_hand_trace() {
        let mut cx = [MqContext::new(0)];
        let bits = decode_run(&[0x80, 0x00, 0x00, 0x00], &mut cx, 5);
        assert_eq!(bits, "00000");
        assert_eq!(cx[0].index, 2);
        assert_eq!(cx[0].mps, 0);
    }

    /// A stream opening with 0xFF, decoded by hand: BYTEIN (Figure C.19)
    /// must take the stuffed branch — the byte after 0xFF is at most 0x8F,
    /// carries only 7 data bits, and enters C shifted by 9 with CT = 7.
    ///
    /// INITDEC: C = 0xFF << 16; BYTEIN sees B = 0xFF and B1 = 0x00 <= 0x8F,
    /// so BP moves to 1 and C += 0x00 << 9 (CT = 7); C <<= 7, CT = 0, so
    /// Chigh = 0x7F80 = 32640 and A = 32768.
    ///
    /// 1. I=0, Qe=22017: A = 10751; Chigh 32640 >= Qe — MPS branch, Chigh
    ///    becomes 10623; A lost bit 15 and A < Qe, so MPS_EXCHANGE takes
    ///    the conditional-exchange side: D = 1-MPS = 1, SWITCH(0) flips MPS
    ///    to 1, I = NLPS(0) = 1. Later decisions keep decoding 1.
    #[test]
    fn stuffed_byte_branch_matches_the_hand_trace() {
        let mut cx = [MqContext::new(0)];
        let bits = decode_run(&[0xFF, 0x00], &mut cx, 4);
        assert_eq!(bits, "1111");
        assert_eq!(cx[0].index, 2);
        assert_eq!(cx[0].mps, 1);
    }

    /// Shared T.88/T.800 coder-family vector: T.800 Annex C and T.88
    /// Annex E define the identical MQ coder, so these decisions were
    /// generated from this repo's own JBIG2 MQ decoder (pdfboss-core,
    /// ITU-T T.88) as an independent oracle. The stuffed byte after 0xFF
    /// enters C shifted by 9, not 8; the difference sits in Clow and takes
    /// sixteen renormalizations to surface, so bit 16 is the first that can
    /// tell the shifts apart.
    #[test]
    fn stuffed_bit_shift_is_nine_not_eight() {
        let mut cx = [MqContext::new(0)];
        let bits = decode_run(&[0xFF, 0x8E], &mut cx, 24);
        assert_eq!(bits, "111111111111111100011111");
        assert_eq!((cx[0].index, cx[0].mps), (26, 1));
    }

    /// Shared T.88/T.800 coder-family vector (same oracle as above): 96
    /// pseudo-random bytes decoded against eight independent contexts. This
    /// walks both C.15 branches, both exchange procedures and both BYTEIN
    /// data branches, and pins every context's final (I, MPS) pair, so a
    /// slip anywhere in Table C.2 or the register procedures shows up as a
    /// mismatched decision.
    #[test]
    fn matches_the_coder_family_oracle_over_random_data() {
        let data: Vec<u8> = (0..96u32).map(|i| ((i * 37 + 11) % 256) as u8).collect();
        let mut cx = [MqContext::new(0); 8];
        let bits = decode_run(&data, &mut cx, 256);
        let want = concat!(
            "0111011100010100000111110011111000010110011111000001111100011111",
            "0011111000011111000111000011110000111100000111110101111101111110",
            "1001110010011110001101010011110100010101110111110001110100111100",
            "1101110000011100100111000001011110011110001111111011110100011111",
        );
        assert_eq!(bits, want);
        let states: Vec<(u8, u8)> = cx.iter().map(|c| (c.index, c.mps)).collect();
        assert_eq!(
            states,
            vec![
                (16, 0),
                (22, 0),
                (17, 0),
                (5, 1),
                (22, 1),
                (5, 1),
                (14, 0),
                (14, 0)
            ]
        );
    }

    /// Shared T.88/T.800 coder-family vector (same oracle): an empty
    /// segment decodes as if padded with 0xFF bytes, so INITDEC lands on
    /// the C.19 marker branch (B1 = 0xFF > 0x8F feeds C += 0xFF00) and the
    /// decoder produces 1-bits from the fill.
    #[test]
    fn empty_segment_decodes_from_ff_padding() {
        let mut cx = [MqContext::new(0)];
        let bits = decode_run(&[], &mut cx, 8);
        assert_eq!(bits, "11111111");
        assert_eq!((cx[0].index, cx[0].mps), (3, 1));
    }

    /// Truncated and marker-shaped segments must keep yielding decisions
    /// without panicking or reading out of bounds: past the marker, BP
    /// stops advancing and BYTEIN feeds 1-bits forever (C.3.4).
    #[test]
    fn exhausted_segments_keep_producing_decisions() {
        for data in [
            vec![],
            vec![0xFF],
            vec![0xFF, 0x8F],
            vec![0xFF, 0x90],
            vec![0xFF, 0xFF],
            vec![0x84, 0xC7, 0x3B],
            vec![0x00],
        ] {
            let mut dec = MqDecoder::new(&data);
            let mut cx = [MqContext::new(0); 4];
            for i in 0..10_000 {
                let d = dec.decode(&mut cx[i % 4]);
                assert!(d <= 1, "{data:?} yielded {d} at step {i}");
            }
        }
    }

    /// State 46 is the non-adapting row (C.2.5): NMPS and NLPS both point
    /// back at 46, so the uniform context Tier-1 starts there never leaves
    /// it, whatever the data does.
    #[test]
    fn state_46_never_adapts() {
        let data: Vec<u8> = (0..64u32).map(|i| ((i * 151 + 3) % 256) as u8).collect();
        let mut dec = MqDecoder::new(&data);
        let mut cx = MqContext::new(46);
        for _ in 0..500 {
            let d = dec.decode(&mut cx);
            assert!(d <= 1);
            assert_eq!(cx.index, 46, "the uniform context left state 46");
        }
    }

    /// Same bytes, same decisions: the decoder's state lives entirely in
    /// the registers and the caller's contexts.
    #[test]
    fn decoding_is_deterministic() {
        let data: Vec<u8> = (0..48u32).map(|i| ((i * 91 + 17) % 256) as u8).collect();
        let run = || {
            let mut cx = [MqContext::new(0); 3];
            decode_run(&data, &mut cx, 200)
        };
        let first = run();
        assert_eq!(first, run());
        assert!(first.contains('0') && first.contains('1'));
    }
}
