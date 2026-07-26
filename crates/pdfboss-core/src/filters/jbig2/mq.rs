//! The MQ binary arithmetic decoder (ITU-T T.88 Annex E).
//!
//! MQ is an adaptive binary arithmetic coder: every decoded bit is drawn
//! against a *context*, a `(state index, MPS)` pair that walks the 47-row
//! probability estimation table of T.88 Table E.1 as the statistics of the
//! coded symbol are learned. Callers own the context arrays — the generic
//! region decoder indexes one by a template-shaped neighbourhood, the
//! integer procedures of Annex A by a prefix of the bits decoded so far —
//! and hand a single [`MqContext`] to [`MqDecoder::decode`] per bit.
//!
//! The decoder is fed attacker-controlled bytes, so it is written to be
//! total: reads past the end of the input yield `0xFF` (E.3.4), every
//! arithmetic step is wrapping, and renormalization is bounded. No input can
//! panic, hang, or read out of bounds; a truncated stream simply decodes an
//! unbounded run of trailing bits, which the segment layer discards.

/// The encoding side of the same annex, used to round-trip the decoder
/// against an independent transcription of it.
#[cfg(test)]
pub(crate) mod encoder;

/// The 47-state probability estimation table (T.88 Table E.1), as
/// `(Qe, NMPS, NLPS, SWITCH)`.
///
/// `Qe` is the sub-interval assigned to the less probable symbol, scaled so
/// that the interval register `A` holds 0x8000 after renormalization. `NMPS`
/// and `NLPS` are the next state indices after coding an MPS and an LPS
/// respectively, and `SWITCH` marks the states where coding an LPS also
/// flips the sense of the more probable symbol. State 46 is the terminal
/// non-adapting state and state 45 the most skewed one.
pub(crate) const QE: [(u16, u8, u8, u8); 47] = [
    (0x5601, 1, 1, 1),
    (0x3401, 2, 6, 0),
    (0x1801, 3, 9, 0),
    (0x0AC1, 4, 12, 0),
    (0x0521, 5, 29, 0),
    (0x0221, 38, 33, 0),
    (0x5601, 7, 6, 1),
    (0x5401, 8, 14, 0),
    (0x4801, 9, 14, 0),
    (0x3801, 10, 14, 0),
    (0x3001, 11, 17, 0),
    (0x2401, 12, 18, 0),
    (0x1C01, 13, 20, 0),
    (0x1601, 29, 21, 0),
    (0x5601, 15, 14, 1),
    (0x5401, 16, 14, 0),
    (0x5101, 17, 15, 0),
    (0x4801, 18, 16, 0),
    (0x3801, 19, 17, 0),
    (0x3401, 20, 18, 0),
    (0x3001, 21, 19, 0),
    (0x2801, 22, 19, 0),
    (0x2401, 23, 20, 0),
    (0x2201, 24, 21, 0),
    (0x1C01, 25, 22, 0),
    (0x1801, 26, 23, 0),
    (0x1601, 27, 24, 0),
    (0x1401, 28, 25, 0),
    (0x1201, 29, 26, 0),
    (0x1101, 30, 27, 0),
    (0x0AC1, 31, 28, 0),
    (0x09C1, 32, 29, 0),
    (0x08A1, 33, 30, 0),
    (0x0521, 34, 31, 0),
    (0x0441, 35, 32, 0),
    (0x02A1, 36, 33, 0),
    (0x0221, 37, 34, 0),
    (0x0141, 38, 35, 0),
    (0x0111, 39, 36, 0),
    (0x0085, 40, 37, 0),
    (0x0049, 41, 38, 0),
    (0x0025, 42, 39, 0),
    (0x0015, 43, 40, 0),
    (0x0009, 44, 41, 0),
    (0x0005, 45, 42, 0),
    (0x0001, 45, 43, 0),
    (0x5601, 46, 46, 0),
];

/// One adaptive context: an index into [`QE`] and the current sense of the
/// more probable symbol (T.88 E.3.1).
///
/// Contexts start at index 0 with MPS 0, which is the initialization every
/// JBIG2 segment applies to a freshly allocated or reset context array
/// (T.88 E.3.5).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct MqContext {
    /// Row of [`QE`] this context currently estimates from, always `0..=46`.
    index: u8,
    /// Sense of the more probable symbol, 0 or 1.
    mps: u8,
}

impl MqContext {
    /// The context's current row in [`QE`].
    pub(crate) fn index(&self) -> u8 {
        self.index
    }

    /// The context's current more-probable-symbol sense, 0 or 1.
    pub(crate) fn mps(&self) -> u8 {
        self.mps
    }

    /// The `(Qe, NMPS, NLPS, SWITCH)` row this context estimates from.
    ///
    /// The index is clamped into the table before use. It cannot leave the
    /// table in the first place — every NMPS and NLPS in Table E.1 points
    /// back inside it — but clamping keeps that a property of this function
    /// rather than of the whole state machine.
    fn row(&self) -> (u16, u8, u8, u8) {
        QE[(self.index as usize).min(QE.len() - 1)]
    }
}

/// An array of adaptive contexts, addressed by the caller's template index.
///
/// Region decoders derive the index from pixel neighbourhoods, so a corrupt
/// template or an out-of-range symbol code can produce an index past the end
/// of the array. [`MqContexts::get_mut`] clamps instead of indexing raw: a
/// bogus index decodes garbage bits, which the caller's own bounds checks
/// reject, rather than aborting the process.
#[derive(Clone, Debug)]
pub(crate) struct MqContexts(Vec<MqContext>);

impl MqContexts {
    /// Allocates `len` contexts, all initialized to state 0 / MPS 0
    /// (T.88 E.3.5). A zero length is rounded up to one entry so that
    /// [`MqContexts::get_mut`] always has something to return.
    pub(crate) fn new(len: usize) -> Self {
        Self(vec![MqContext::default(); len.max(1)])
    }

    /// The context at `i`, clamped into range.
    pub(crate) fn get_mut(&mut self, i: usize) -> &mut MqContext {
        let last = self.0.len() - 1; // Non-empty by construction.
        &mut self.0[i.min(last)]
    }

    /// Returns every context to state 0 / MPS 0, as required when a segment
    /// declares that its arithmetic statistics are not retained.
    pub(crate) fn reset(&mut self) {
        self.0.fill(MqContext::default());
    }

    /// Folds every context's `(index, MPS)` pair into a single value, so that
    /// tests elsewhere in the codec can compare whole arrays without the
    /// storage becoming visible outside this module.
    #[cfg(test)]
    pub(crate) fn state_digest(&self) -> u64 {
        self.0.iter().fold(0xcbf2_9ce4_8422_2325, |acc, cx| {
            let state = u64::from(cx.index) * 2 + u64::from(cx.mps);
            (acc ^ state).wrapping_mul(0x0000_0100_0000_01b3)
        })
    }
}

/// The MQ arithmetic decoder over one byte stream (T.88 Annex E).
///
/// Constructed with [`MqDecoder::new`], which performs INITDEC (E.3.5); each
/// [`MqDecoder::decode`] call runs the DECODE procedure (E.3.2) against the
/// caller's context and returns a single bit.
pub(crate) struct MqDecoder<'a> {
    /// The coded data. Reads past its end yield `0xFF` (see [`MqDecoder::byte`]).
    data: &'a [u8],
    /// Index of the current byte, `BP` in the standard.
    bp: usize,
    /// Code register, `C`. The high 16 bits are the `CHIGH` compared against Qe.
    c: u32,
    /// Interval register, `A`; 16 significant bits.
    a: u32,
    /// Count of shifts remaining before the next BYTEIN, `CT`.
    ct: i32,
}

impl<'a> MqDecoder<'a> {
    /// Starts decoding `data`, performing INITDEC (T.88 E.3.5).
    pub(crate) fn new(data: &'a [u8]) -> Self {
        let mut dec = Self {
            data,
            bp: 0,
            c: 0,
            a: 0,
            ct: 0,
        };
        dec.c = u32::from(dec.byte(dec.bp)) << 16;
        dec.byte_in();
        dec.c <<= 7;
        dec.ct -= 7;
        dec.a = 0x8000;
        dec
    }

    /// The input byte at `i`, or `0xFF` past the end. Feeding `0xFF` past the
    /// end is what makes a truncated stream run into the BYTEIN marker path
    /// and terminate, rather than reading out of bounds.
    fn byte(&self, i: usize) -> u8 {
        self.data.get(i).copied().unwrap_or(0xFF)
    }

    /// BYTEIN (T.88 E.3.4): tops the code register back up with the next
    /// input byte, or with the 1-bits the marker convention supplies once the
    /// stream has run out.
    ///
    /// A `0xFF` followed by a byte above `0x8F` is a marker: the encoder can
    /// never emit that pair, so the decoder stops consuming input and feeds
    /// 1-bits forever. Past the end of the buffer [`MqDecoder::byte`] yields
    /// `0xFF` for both bytes, so a truncated stream lands here and stays here.
    fn byte_in(&mut self) {
        if self.byte(self.bp) == 0xFF {
            if self.byte(self.bp + 1) > 0x8F {
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

    /// RENORMD (T.88 E.3.3): shifts `A` and `C` left until `A` regains its
    /// most significant bit, pulling in a byte whenever `CT` runs out.
    ///
    /// `A` is non-zero on entry — the MPS path reaches here with
    /// `A >= 0x8000 - 0x5601`, the LPS path with `A = Qe >= 1` — so bit 15 is
    /// reached within 16 shifts. The loop is bounded at 16 to make that
    /// termination structural rather than inferred from an invariant, so no
    /// input can hang the decoder.
    fn renorm(&mut self) {
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

    /// MPS_EXCHANGE (T.88 E.3.2): decides the bit when the MPS sub-interval
    /// was selected but `A` lost its most significant bit, so the two
    /// sub-intervals may have swapped sizes, and advances the context's state.
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

    /// LPS_EXCHANGE (T.88 E.3.2): decides the bit when the LPS sub-interval
    /// was selected, sets `A` to that sub-interval, and advances the
    /// context's state.
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

    /// DECODE (T.88 E.3.2): decodes one bit against `cx`, returning 0 or 1.
    ///
    /// The subtractions are wrapping only as belt and braces: `A >= 0x8000`
    /// and `Qe <= 0x5601` on entry, so `A - Qe` is positive, and the
    /// `C - (Qe << 16)` branch is guarded by `CHIGH >= Qe`.
    pub(crate) fn decode(&mut self, cx: &mut MqContext) -> u8 {
        let qe = u32::from(cx.row().0);
        self.a = self.a.wrapping_sub(qe);
        if (self.c >> 16) < qe {
            let d = self.lps_exchange(cx);
            self.renorm();
            d
        } else {
            self.c = self.c.wrapping_sub(qe << 16);
            if self.a & 0x8000 == 0 {
                let d = self.mps_exchange(cx);
                self.renorm();
                d
            } else {
                cx.mps
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A decoder over an empty buffer must terminate rather than panic: every
    /// read past the end feeds 0xFF, which drives the marker path in BYTEIN.
    #[test]
    fn empty_input_does_not_panic() {
        let mut dec = MqDecoder::new(&[]);
        let mut cx = MqContexts::new(1 << 16);
        for _ in 0..10_000 {
            let _ = dec.decode(cx.get_mut(0));
        }
    }

    /// Truncation mid-stream is equally survivable.
    #[test]
    fn truncated_input_does_not_panic() {
        let mut dec = MqDecoder::new(&[0x84, 0xC7, 0x3B]);
        let mut cx = MqContexts::new(512);
        for _ in 0..10_000 {
            let _ = dec.decode(cx.get_mut(7));
        }
    }

    /// A context index beyond the array is clamped, never indexed raw.
    #[test]
    fn out_of_range_context_is_clamped() {
        let mut cx = MqContexts::new(4);
        let _ = cx.get_mut(99_999);
    }

    /// Fresh contexts start at state 0 with MPS 0 (T.88 E.3.5).
    #[test]
    fn contexts_start_at_state_zero() {
        let cx = MqContext::default();
        assert_eq!((cx.index(), cx.mps()), (0, 0));
    }

    /// The published Qe table has exactly 47 rows and its terminal row is the
    /// non-renormalizing 0x5601 self-loop.
    #[test]
    fn qe_table_shape() {
        assert_eq!(QE.len(), 47);
        assert_eq!(QE[0].0, 0x5601);
        assert_eq!(QE[46], (0x5601, 46, 46, 0));
        assert_eq!(QE[45], (0x0001, 45, 43, 0));
        // Every NMPS/NLPS must point back inside the table.
        for (idx, &(_, nmps, nlps, switch)) in QE.iter().enumerate() {
            assert!((nmps as usize) < 47, "row {idx} NMPS out of range");
            assert!((nlps as usize) < 47, "row {idx} NLPS out of range");
            assert!(switch <= 1, "row {idx} SWITCH must be 0 or 1");
        }
    }

    /// Spot-checks the rows of Table E.1 whose NMPS/NLPS break the otherwise
    /// monotone `index + 1` pattern, and the three rows carrying SWITCH — the
    /// entries a transcription slip is most likely to land on.
    #[test]
    fn qe_table_irregular_rows() {
        assert_eq!(QE[4], (0x0521, 5, 29, 0));
        assert_eq!(QE[5], (0x0221, 38, 33, 0));
        assert_eq!(QE[6], (0x5601, 7, 6, 1));
        assert_eq!(QE[13], (0x1601, 29, 21, 0));
        assert_eq!(QE[14], (0x5601, 15, 14, 1));
        assert_eq!(QE[21], (0x2801, 22, 19, 0));
        assert_eq!(QE[28], (0x1201, 29, 26, 0));
        assert_eq!(QE[37], (0x0141, 38, 35, 0));
        // SWITCH is set on exactly the three states where an LPS is as likely
        // as an MPS (Qe = 0x5601 with a non-self-looping NLPS).
        let switches: Vec<usize> = QE
            .iter()
            .enumerate()
            .filter(|(_, row)| row.3 == 1)
            .map(|(idx, _)| idx)
            .collect();
        assert_eq!(switches, vec![0, 6, 14]);
    }

    /// Qe is the LPS sub-interval, so it can never exceed half of the 0x8000
    /// the interval register is renormalized to, and never reach zero.
    #[test]
    fn qe_values_are_a_valid_subinterval() {
        for (idx, &(qe, ..)) in QE.iter().enumerate() {
            assert!(qe > 0, "row {idx} Qe must be non-zero");
            assert!(qe <= 0x5601, "row {idx} Qe exceeds the half interval");
        }
    }

    /// An all-zero stream, traced by hand through E.3.5 and E.3.2.
    ///
    /// INITDEC leaves `C = 0`, `CT = 1`, `A = 0x8000`, so `CHIGH` is below Qe
    /// on every bit and DECODE takes the LPS branch throughout. The first bit
    /// walks state 0 to state 1 via NMPS; the second takes NLPS to state 6,
    /// the 0x5601 SWITCH self-loop, where the MPS sense flips on every
    /// subsequent bit and the output alternates forever.
    #[test]
    fn all_zero_stream_alternates_from_the_switch_state() {
        let mut dec = MqDecoder::new(&[0x00; 64]);
        let mut cx = MqContexts::new(1);
        let bits: Vec<u8> = (0..16).map(|_| dec.decode(cx.get_mut(0))).collect();
        assert_eq!(bits, vec![0, 1, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0, 1, 0]);
        assert_eq!(cx.get_mut(0).index(), 6);
    }

    /// A stream whose first byte is 0xFF, traced by hand through E.3.4.
    ///
    /// INITDEC reads the 0xFF, then BYTEIN sees a following byte at or below
    /// 0x8F and takes the stuffed-bit branch (`CT = 7`, shift by 9) rather
    /// than the marker branch, leaving `CHIGH = 0x7F80`. That is above Qe, so
    /// the first bit comes from the MPS branch with a SWITCH at state 0.
    #[test]
    fn stuffed_byte_branch_matches_the_hand_trace() {
        let mut dec = MqDecoder::new(&[0xFF, 0x00]);
        let mut cx = MqContexts::new(1);
        let bits: Vec<u8> = (0..4).map(|_| dec.decode(cx.get_mut(0))).collect();
        assert_eq!(bits, vec![1, 1, 1, 1]);
        assert_eq!(cx.get_mut(0).mps(), 1, "SWITCH at state 0 flips the MPS");
    }

    /// Pins the width of the stuffed-bit shift in BYTEIN's `B == 0xFF`,
    /// `B1 <= 0x8F` branch (T.88 E.3.4).
    ///
    /// After a 0xFF the encoder stuffs a bit, so the following byte carries
    /// only seven data bits and enters the code register shifted by 9, not
    /// by 8. The difference lands in the low bits of `C` and needs sixteen
    /// renormalizations to reach `CHIGH`, so nothing shorter than this
    /// sequence can distinguish the two: bit 16 is the first to move. This
    /// two-byte input also runs off the end of the buffer immediately after,
    /// exercising the `0xFF` fill and then the marker lock.
    #[test]
    fn stuffed_bit_shift_is_nine_not_eight() {
        let mut dec = MqDecoder::new(&[0xFF, 0x8E]);
        let mut cx = MqContexts::new(1);
        let bits: Vec<u8> = (0..24).map(|_| dec.decode(cx.get_mut(0))).collect();
        assert_eq!(
            bits,
            vec![1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 1, 0, 0, 0, 1, 1, 1, 1, 1]
        );
    }

    /// A stream whose code register starts high, traced by hand: `CHIGH`
    /// exceeds Qe from the first bit, so DECODE takes the MPS branch and
    /// E.3.2's `C -= Qe << 16` runs.
    #[test]
    fn mps_branch_matches_the_hand_trace() {
        let mut dec = MqDecoder::new(&[0x80, 0x00, 0x00, 0x00]);
        let mut cx = MqContexts::new(1);
        let bits: Vec<u8> = (0..5).map(|_| dec.decode(cx.get_mut(0))).collect();
        assert_eq!(bits, vec![0, 0, 0, 0, 0]);
        assert_eq!(cx.get_mut(0).index(), 2);
        assert_eq!(cx.get_mut(0).mps(), 0);
    }

    /// The same bytes decode to the same bits: no hidden state escapes the
    /// decoder and the context array.
    #[test]
    fn decoding_is_deterministic() {
        let data: Vec<u8> = (0..96u32).map(|i| (i * 37 + 11) as u8).collect();
        let run = || {
            let mut dec = MqDecoder::new(&data);
            let mut cx = MqContexts::new(16);
            (0..400)
                .map(|i| dec.decode(cx.get_mut(i % 19)))
                .collect::<Vec<u8>>()
        };
        let first = run();
        assert_eq!(first, run());
        assert!(first.iter().all(|&b| b <= 1), "DECODE must yield 0 or 1");
        // A non-degenerate stream must exercise both branches of DECODE.
        assert!(first.contains(&0) && first.contains(&1));
    }

    /// `reset` returns an adapted array to the E.3.5 initial state.
    #[test]
    fn reset_restores_initial_state() {
        let mut dec = MqDecoder::new(&[0x9A, 0x33, 0x71, 0x0C]);
        let mut cx = MqContexts::new(8);
        for i in 0..200 {
            let _ = dec.decode(cx.get_mut(i % 8));
        }
        cx.reset();
        for i in 0..8 {
            assert_eq!(*cx.get_mut(i), MqContext::default());
        }
    }

    /// Every context index in a full-width array is reachable and independent:
    /// adapting one leaves its neighbours untouched.
    #[test]
    fn contexts_are_independent() {
        let mut dec = MqDecoder::new(&[0x5A; 32]);
        let mut cx = MqContexts::new(4);
        for _ in 0..64 {
            let _ = dec.decode(cx.get_mut(2));
        }
        assert_ne!(*cx.get_mut(2), MqContext::default());
        for i in [0usize, 1, 3] {
            assert_eq!(*cx.get_mut(i), MqContext::default());
        }
    }

    /// A stream made only of marker bytes never advances past the marker, so
    /// BP stays put and the decoder keeps producing bits from 1-fill.
    #[test]
    fn marker_boundary_is_survivable() {
        for data in [
            vec![0xFF, 0x90],
            vec![0xFF, 0x8F],
            vec![0xFF, 0xFF],
            vec![0xFF],
            vec![0x00],
        ] {
            let mut dec = MqDecoder::new(&data);
            let mut cx = MqContexts::new(2);
            for _ in 0..2_000 {
                let _ = dec.decode(cx.get_mut(1));
            }
        }
    }

    /// Every bit coded by the Annex E encoder comes back out of the Annex E
    /// decoder unchanged, over a run long enough to walk the whole Qe table.
    ///
    /// The two sides are separate transcriptions of the same annex — the
    /// encoder renormalizes after transferring a byte where the decoder
    /// transfers before shifting, and stuffs bits where the decoder skips
    /// them — so a slip in Table E.1, in either interval update or in either
    /// byte procedure shows up as a mismatched bit rather than as two
    /// self-consistent errors.
    #[test]
    fn encoder_round_trips_through_the_decoder() {
        let mut state: u32 = 0x2545_F491;
        let mut next = move || {
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            state
        };
        // A skewed source (mostly zeros) drives the states towards the tail of
        // the table; the context index picks a different context per bit so
        // the whole array adapts.
        let bits: Vec<u8> = (0..4_000)
            .map(|_| u8::from(next() % 16 == 0))
            .collect::<Vec<u8>>();
        let indices: Vec<usize> = (0..bits.len()).map(|i| i % 7).collect();

        let mut enc = encoder::MqEncoder::new();
        let mut enc_cx = MqContexts::new(7);
        for (bit, index) in bits.iter().zip(&indices) {
            enc.encode(enc_cx.get_mut(*index), *bit);
        }
        let coded = enc.finish();

        let mut dec = MqDecoder::new(&coded);
        let mut dec_cx = MqContexts::new(7);
        let decoded: Vec<u8> = indices
            .iter()
            .map(|index| dec.decode(dec_cx.get_mut(*index)))
            .collect();
        assert_eq!(decoded, bits);
        // Both sides must have walked their context arrays identically.
        assert_eq!(dec_cx.state_digest(), enc_cx.state_digest());
    }

    /// Degenerate bit sequences round-trip too: a long run of one symbol
    /// drives the state index to the tail of the table and exercises the
    /// encoder's carry propagation and bit stuffing.
    #[test]
    fn degenerate_runs_round_trip() {
        for pattern in [
            vec![0u8; 600],
            vec![1u8; 600],
            (0..600).map(|i| u8::try_from(i % 2).unwrap_or(0)).collect(),
            (0..600).map(|i| u8::from(i % 97 == 0)).collect(),
            vec![],
            vec![1],
        ] {
            let mut enc = encoder::MqEncoder::new();
            let mut enc_cx = MqContexts::new(1);
            for bit in &pattern {
                enc.encode(enc_cx.get_mut(0), *bit);
            }
            let coded = enc.finish();

            let mut dec = MqDecoder::new(&coded);
            let mut dec_cx = MqContexts::new(1);
            let decoded: Vec<u8> = (0..pattern.len())
                .map(|_| dec.decode(dec_cx.get_mut(0)))
                .collect();
            assert_eq!(decoded, pattern, "run of {} bits", pattern.len());
        }
    }

    /// A single context over a fixed bit pattern: what goes in comes out.
    ///
    /// The smallest round trip there is, and the one whose failure mode is
    /// most legible — a mismatch at bit 0 means INITENC and INITDEC disagree,
    /// a mismatch only near the end means FLUSH is wrong, and scattered
    /// mismatches mean the BYTEOUT carry path is wrong.
    #[test]
    fn round_trips_a_single_context() {
        let bits: Vec<u8> = (0..500).map(|i| u8::from(i % 7 == 0)).collect();

        let mut enc = encoder::MqEncoder::new();
        let mut enc_cx = MqContext::default();
        for bit in &bits {
            enc.encode(&mut enc_cx, *bit);
        }
        let coded = enc.finish();
        assert!(!coded.is_empty());

        let mut dec = MqDecoder::new(&coded);
        let mut dec_cx = MqContexts::new(1);
        for (i, want) in bits.iter().enumerate() {
            assert_eq!(dec.decode(dec_cx.get_mut(0)), *want, "bit {i}");
        }
    }

    /// A thousand contexts adapting independently is the case a region
    /// decoder actually presents: consecutive bits are drawn against
    /// unrelated contexts, so every bit depends on a different row of
    /// Table E.1 and the interval state carries across all of them.
    #[test]
    fn round_trips_across_many_contexts() {
        let mut state: u32 = 0xACE1_0FF1;
        let mut next = move || {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            state
        };
        let plan: Vec<(usize, u8)> = (0..4_000)
            .map(|_| {
                let n = next();
                let index = usize::try_from(n >> 8).unwrap_or(0) % 1_024;
                (index, u8::from(n & 1 == 0))
            })
            .collect();

        let mut enc = encoder::MqEncoder::new();
        let mut enc_cx = vec![MqContext::default(); 1_024];
        for (index, bit) in &plan {
            enc.encode(&mut enc_cx[*index], *bit);
        }
        let coded = enc.finish();

        let mut dec = MqDecoder::new(&coded);
        let mut dec_cx = MqContexts::new(1_024);
        for (i, (index, want)) in plan.iter().enumerate() {
            assert_eq!(
                dec.decode(dec_cx.get_mut(*index)),
                *want,
                "bit {i} in context {index}"
            );
        }
    }

    /// A long run of one symbol walks the estimator down to the skewed tail of
    /// Table E.1 and codes twenty thousand bits into a handful of bytes — the
    /// property that makes an adaptive coder worth its complexity, and one no
    /// self-consistent pair of transcriptions could fake.
    #[test]
    fn a_constant_run_compresses_and_round_trips() {
        let mut enc = encoder::MqEncoder::new();
        let mut enc_cx = MqContext::default();
        for _ in 0..20_000 {
            enc.encode(&mut enc_cx, 0);
        }
        let coded = enc.finish();
        assert!(
            coded.len() < 64,
            "20 000 identical bits took {} bytes",
            coded.len()
        );

        let mut dec = MqDecoder::new(&coded);
        let mut dec_cx = MqContexts::new(1);
        for i in 0..20_000 {
            assert_eq!(dec.decode(dec_cx.get_mut(0)), 0, "bit {i}");
        }
    }

    /// The carry path in BYTEOUT is the encoder's sharpest edge: an interval
    /// that keeps landing just above the byte boundary propagates a carry into
    /// a byte already emitted. Alternating bits through one context, which
    /// pins the estimator near the `0x5601` states where each sub-interval is
    /// about half the register, is the pattern most likely to reach it.
    #[test]
    fn round_trips_alternating_bits() {
        let bits: Vec<u8> = (0..3_000).map(|i| u8::from(i % 2 == 1)).collect();

        let mut enc = encoder::MqEncoder::new();
        let mut enc_cx = MqContext::default();
        for bit in &bits {
            enc.encode(&mut enc_cx, *bit);
        }
        let coded = enc.finish();

        let mut dec = MqDecoder::new(&coded);
        let mut dec_cx = MqContexts::new(1);
        for (i, want) in bits.iter().enumerate() {
            assert_eq!(dec.decode(dec_cx.get_mut(0)), *want, "bit {i}");
        }
    }

    /// FLUSH ends the code stream with `FF AC` (T.88 E.3.8).
    ///
    /// That pair is both the decoder's marker — BYTEIN sees `0xFF` followed by
    /// a byte above `0x8F` and switches to feeding 1-bits — and the sequence
    /// the unknown-data-length segment scan of 7.2.7 searches for, so it has
    /// to be present byte for byte.
    #[test]
    fn finish_appends_the_standard_terminator() {
        let mut enc = encoder::MqEncoder::new();
        let mut enc_cx = MqContext::default();
        enc.encode(&mut enc_cx, 1);
        let coded = enc.finish();
        assert_eq!(&coded[coded.len() - 2..], &[0xFF, 0xAC]);
    }

    /// Encoding nothing at all still produces a stream the decoder can read:
    /// FLUSH is what makes the terminator unconditional, and a zero-symbol
    /// region is legal.
    #[test]
    fn an_empty_encode_still_flushes() {
        let coded = encoder::MqEncoder::new().finish();
        assert_eq!(&coded[coded.len() - 2..], &[0xFF, 0xAC]);
        let mut dec = MqDecoder::new(&coded);
        let mut dec_cx = MqContexts::new(1);
        for _ in 0..100 {
            let _ = dec.decode(dec_cx.get_mut(0));
        }
    }

    /// A zero-length context array is rounded up to one entry, so `get_mut`
    /// always has something to hand back.
    #[test]
    fn zero_length_context_array_is_usable() {
        let mut cx = MqContexts::new(0);
        assert_eq!(*cx.get_mut(0), MqContext::default());
        assert_eq!(*cx.get_mut(usize::MAX), MqContext::default());
    }
}
