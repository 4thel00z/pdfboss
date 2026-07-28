//! Arithmetic integer decoding (ITU-T T.88 Annex A).
//!
//! JBIG2 codes every integer parameter — height and width deltas, symbol
//! counts, strip coordinates, refinement offsets — as a sign bit, a unary
//! length class and a run of magnitude bits, each bit drawn from the MQ
//! decoder against a context selected by the bits decoded so far (A.2).
//! Thirteen such procedures exist and each owns a separate context array, so
//! that `IADH` learns the statistics of height deltas without `IADW`'s width
//! deltas interfering; [`IntCtxSet`] is that bundle, carried by every region
//! and dictionary decoder. Symbol IDs use a fourteenth, differently shaped
//! procedure (A.3), here [`IaidCtx`] and [`decode_iaid`].
//!
//! Three properties matter to every caller:
//!
//! * **OOB is not zero.** A.2's out-of-band value is the sign bit set with a
//!   magnitude of zero — the one bit pattern the value coding leaves spare,
//!   since a negative zero has no other meaning. Callers use it as a list
//!   terminator, so [`decode_int`] returns [`Option`] and `None` means OOB.
//!   `Some(0)` and `None` differ by a single decoded bit; conflating them
//!   turns every terminator into a legitimate value.
//! * **OOB always arrives.** The lists of A.2 are terminated by the data, not
//!   by a count, so `repeat { V = decode; if OOB break }` is the shape of
//!   every caller (6.5.5, 6.4.5). A stream that runs out cannot be left
//!   producing values: past the end of the coded data the arithmetic decoder
//!   settles into a cycle, and a cycle that never yields OOB is a loop that
//!   never ends. An exhausted decoder therefore reads as OOB, permanently.
//! * **The context index is clamped.** During the magnitude loop the index
//!   would otherwise grow one bit per decoded bit, up to 32 of them
//!   (A.2, step 4). Folding it back into the upper half of the array is what
//!   keeps every access inside the 512 contexts the procedure owns.
//!
//! Like the MQ layer below it, nothing here can panic or hang: the bits of a
//! truncated stream are supplied by the marker convention of T.88 E.3.4, and
//! a magnitude too large for an [`i32`] is reported as OOB rather than
//! wrapped into a bogus coordinate.

use super::mq::{MqContext, MqContexts, MqDecoder};

/// Contexts owned by one integer procedure (T.88 A.2).
///
/// Nine bits of decoded history select the context: the sign bit, up to five
/// length-class bits and then a sliding prefix of the magnitude bits.
const INT_CTX_LEN: usize = 512;

/// Largest symbol-code length [`IaidCtx`] will allocate contexts for.
///
/// A.3 places no ceiling below the 32-bit width of `SBNUMSYMS`, but the
/// context array is `2^(len + 1)` entries, so an unclamped length is an
/// allocation the input controls. Sixteen bits addresses a symbol dictionary
/// of 65 536 entries; the segment layer rejects anything larger before it
/// reaches here, and clamping keeps this module total either way.
const MAX_SYM_CODE_LEN: u32 = 16;

/// The context array of one integer procedure (T.88 A.2).
///
/// Constructed fresh per segment, or per procedure within a segment, wherever
/// the standard says the arithmetic statistics are not retained.
#[derive(Clone, Debug)]
pub(crate) struct IntCtx(MqContexts);

impl IntCtx {
    /// Allocates the procedure's 512 contexts, all at state 0 / MPS 0.
    pub(crate) fn new() -> Self {
        Self(MqContexts::new(INT_CTX_LEN))
    }

    /// The context at `index`, clamped into the array.
    ///
    /// Decoders that interleave their own bit into an integer procedure's
    /// array need to reach a single context; going through
    /// [`MqContexts::get_mut`] keeps a corrupt index from indexing raw.
    pub(crate) fn context_mut(&mut self, index: usize) -> &mut MqContext {
        self.0.get_mut(index)
    }

    /// Returns every context to state 0 / MPS 0.
    #[allow(dead_code)] // Reached once retained arithmetic contexts are honoured.
    pub(crate) fn reset(&mut self) {
        self.0.reset();
    }

    /// Folds the array's `(index, MPS)` pairs into one value, so tests can
    /// compare whole arrays without their storage being visible here.
    #[cfg(test)]
    fn state_digest(&self) -> u64 {
        self.0.state_digest()
    }
}

/// Advances `PREV` by one decoded magnitude bit (T.88 A.2, step 4).
///
/// Below 256 the index is a plain prefix of the bits decoded so far. At and
/// above it the shifted index is masked to nine bits and bit 8 forced back
/// on, so the walk continues inside the upper half of the array instead of
/// running off its end. Both branches keep the result in `1..512`, which is
/// what makes every access in [`decode_int`]'s magnitude loop in range.
fn next_prev(prev: usize, bit: u8) -> usize {
    let shifted = (prev << 1) | usize::from(bit);
    if prev < 256 {
        shifted
    } else {
        (shifted & 511) | 256
    }
}

/// Decodes one bit of the sign-and-length prefix, folding it into `PREV`
/// (T.88 A.2, steps 1 to 3).
///
/// The prefix is at most six bits, so `PREV` cannot exceed 127 here and needs
/// no clamping; that starts in the magnitude loop.
fn prefix_bit(dec: &mut MqDecoder, cx: &mut IntCtx, prev: &mut usize) -> u8 {
    let bit = dec.decode(cx.context_mut(*prev));
    *prev = (*prev << 1) | usize::from(bit);
    bit
}

/// Decodes one integer with the arithmetic integer procedure (T.88 A.2).
///
/// Returns `None` for the OOB value, which callers use as a list terminator —
/// the end of a height class, of an export flag run, of a text region strip.
/// A magnitude too large to be an [`i32`] is reported the same way: such a
/// value cannot be a valid coordinate or count, and OOB is the one answer
/// every caller already handles.
///
/// An exhausted stream ([`MqDecoder::is_exhausted`]) reads as OOB too, and
/// from then on every call does. This is what bounds the caller's loop. Past
/// the end of the coded data the arithmetic decoder does not stop: it cycles,
/// often through a single state, so the values it returns simply repeat — for
/// most inputs a value rather than the terminator, which leaves a spec-shaped
/// `repeat until OOB` running forever. Nothing is lost by ending it there,
/// because those values were synthesized by the marker fill of T.88 E.3.4 and
/// carry no bit of the input. A caller that needs to tell a truncated segment
/// from a properly terminated list can ask the decoder directly.
pub(crate) fn decode_int(dec: &mut MqDecoder, cx: &mut IntCtx) -> Option<i32> {
    if dec.is_exhausted() {
        return None;
    }

    let mut prev: usize = 1;
    let sign = prefix_bit(dec, cx, &mut prev);

    // The length class: a unary prefix selecting how many magnitude bits
    // follow and the value they are biased by, so that the six classes tile
    // the non-negative integers without a gap (A.2, step 3).
    let (bits, offset): (u32, u32) = if prefix_bit(dec, cx, &mut prev) == 0 {
        (2, 0)
    } else if prefix_bit(dec, cx, &mut prev) == 0 {
        (4, 4)
    } else if prefix_bit(dec, cx, &mut prev) == 0 {
        (6, 20)
    } else if prefix_bit(dec, cx, &mut prev) == 0 {
        (8, 84)
    } else if prefix_bit(dec, cx, &mut prev) == 0 {
        (12, 340)
    } else {
        (32, 4436)
    };

    let mut value: u32 = 0;
    for _ in 0..bits {
        let bit = dec.decode(cx.context_mut(prev));
        prev = next_prev(prev, bit);
        value = (value << 1) | u32::from(bit);
    }

    // Widened before the bias is applied: the 32-bit class plus an offset of
    // 4436 does not fit a `u32`.
    let magnitude = u64::from(value) + u64::from(offset);
    if sign == 0 {
        return i32::try_from(magnitude).ok();
    }
    if magnitude == 0 {
        return None; // OOB: the sign bit set over a zero magnitude.
    }
    let signed = i64::try_from(magnitude).ok()?;
    i32::try_from(-signed).ok()
}

/// The context array of the symbol-ID procedure (T.88 A.3).
///
/// Unlike [`IntCtx`] this one is sized by the segment: the procedure decodes
/// exactly `SBSYMCODELEN` bits and indexes by the prefix of them decoded so
/// far, needing `2^(SBSYMCODELEN + 1)` contexts.
#[derive(Clone, Debug)]
pub(crate) struct IaidCtx {
    /// One context per prefix of the symbol code, `2^(code_len + 1)` of them.
    contexts: MqContexts,
    /// `SBSYMCODELEN`, clamped to [`MAX_SYM_CODE_LEN`].
    sym_code_len: u32,
}

impl IaidCtx {
    /// Allocates contexts for a `sym_code_len`-bit symbol code.
    ///
    /// `sym_code_len` is clamped to [`MAX_SYM_CODE_LEN`]; a longer code would
    /// let the input dictate the allocation. Decoding then yields IDs shorter
    /// than requested, which are still in range for the caller's dictionary.
    pub(crate) fn new(sym_code_len: u32) -> Self {
        let sym_code_len = sym_code_len.min(MAX_SYM_CODE_LEN);
        Self {
            contexts: MqContexts::new(1usize << (sym_code_len + 1)),
            sym_code_len,
        }
    }

    /// The clamped `SBSYMCODELEN` this array was built for.
    pub(crate) fn code_len(&self) -> u32 {
        self.sym_code_len
    }

    /// The context at `index`, clamped into the array.
    pub(crate) fn context_mut(&mut self, index: usize) -> &mut MqContext {
        self.contexts.get_mut(index)
    }

    /// Returns every context to state 0 / MPS 0.
    #[allow(dead_code)] // Reached once retained arithmetic contexts are honoured.
    pub(crate) fn reset(&mut self) {
        self.contexts.reset();
    }

    /// Folds the array's `(index, MPS)` pairs into one value for tests.
    #[cfg(test)]
    fn state_digest(&self) -> u64 {
        self.contexts.state_digest()
    }
}

/// Decodes one symbol ID (T.88 A.3). The result is always
/// `< 1 << cx.code_len()`.
///
/// A.3 states the result as `PREV - 2^SBSYMCODELEN`; accumulating the ID
/// alongside `PREV` gives the same value — `PREV` is that leading one bit
/// followed by the ID — without a subtraction that could underflow.
///
/// There is no OOB here: A.3 codes a fixed number of bits and every caller
/// draws a symbol ID a counted number of times, so this procedure cannot end
/// a loop and does not test for an exhausted stream. A caller that wants to
/// stop at the end of the coded data asks [`MqDecoder::is_exhausted`].
pub(crate) fn decode_iaid(dec: &mut MqDecoder, cx: &mut IaidCtx) -> u32 {
    let mut prev: usize = 1;
    let mut id: u32 = 0;
    for _ in 0..cx.code_len() {
        let bit = dec.decode(cx.context_mut(prev));
        prev = (prev << 1) | usize::from(bit);
        id = (id << 1) | u32::from(bit);
    }
    id
}

/// The encoding side of Annex A, for building coded fixtures (T.88 A.2, A.3).
///
/// A symbol dictionary or text region is a braid of integer procedures and
/// generic-region bits sharing one arithmetic decoder, which is not something
/// that can realistically be authored by hand. Coding a fixture through these
/// and reading it back through [`decode_int`] and [`decode_iaid`] is how the
/// layers above are tested; it also pins the value each bit pattern denotes,
/// which the decode side alone cannot assert about itself.
///
/// Compiled only under `cfg(test)`: nothing shipped encodes.
#[cfg(test)]
pub(crate) mod encoder {
    use super::{next_prev, IaidCtx, IntCtx};
    use crate::filters::jbig2::mq::encoder::MqEncoder;

    /// The six length classes of T.88 A.2, as `(magnitude bits, bias, the
    /// unary prefix that selects the class after the sign bit)`.
    ///
    /// The biases tile the non-negative integers without a gap or an overlap:
    /// each class starts where the previous one runs out, so every magnitude
    /// has exactly one encoding and [`encode_int`] can take the first class
    /// that holds it.
    const LENGTH_CLASSES: [(u32, u64, &[u8]); 6] = [
        (2, 0, &[0]),
        (4, 4, &[1, 0]),
        (6, 20, &[1, 1, 0]),
        (8, 84, &[1, 1, 1, 0]),
        (12, 340, &[1, 1, 1, 1, 0]),
        (32, 4436, &[1, 1, 1, 1, 1]),
    ];

    /// Codes one bit into an integer procedure's array, advancing `PREV` the
    /// way [`decode_int`](super::decode_int) will when it reads the bit back.
    ///
    /// [`next_prev`] is applied to the prefix bits as well as the magnitude
    /// ones. That is the same walk: below 256 it is a plain shift, and the
    /// sign bit plus a unary class prefix is at most six bits, so `PREV` never
    /// reaches the clamp before the magnitude loop starts.
    fn code_bit(enc: &mut MqEncoder, cx: &mut IntCtx, prev: &mut usize, bit: u8) {
        enc.encode(cx.context_mut(*prev), bit);
        *prev = next_prev(*prev, bit);
    }

    /// Encodes one integer with the arithmetic integer procedure (T.88 A.2):
    /// a sign bit, the unary length class that holds the magnitude, and that
    /// class's magnitude bits, most significant first.
    ///
    /// `None` encodes OOB, which is the sign bit set over a magnitude of zero.
    /// `Some(0)` must therefore take the positive branch — a negative zero is
    /// the terminator, not a value.
    ///
    /// The magnitude is widened to [`u64`] before the class bias is removed:
    /// `i32::MIN` has a magnitude of 2^31, which the 32-bit class carries only
    /// once its bias of 4436 is subtracted, and the comparison against the
    /// last class's ceiling of `4436 + 2^32` does not fit a [`u32`] either.
    pub(crate) fn encode_int(enc: &mut MqEncoder, cx: &mut IntCtx, value: Option<i32>) {
        let (sign, magnitude) = match value {
            None => (1u8, 0u64),
            Some(v) => (u8::from(v < 0), i64::from(v).unsigned_abs()),
        };
        let (bits, offset, prefix) = LENGTH_CLASSES
            .into_iter()
            .find(|(bits, offset, _)| magnitude < offset + (1u64 << bits))
            .unwrap_or(LENGTH_CLASSES[5]);

        let mut prev = 1usize;
        code_bit(enc, cx, &mut prev, sign);
        for bit in prefix {
            code_bit(enc, cx, &mut prev, *bit);
        }
        let payload = magnitude.saturating_sub(offset);
        for shift in (0..bits).rev() {
            let bit = u8::try_from((payload >> shift) & 1).unwrap_or(0);
            code_bit(enc, cx, &mut prev, bit);
        }
    }

    /// Encodes one symbol ID (T.88 A.3): `SBSYMCODELEN` bits, most
    /// significant first.
    ///
    /// `PREV` is a plain shift here, with no clamp — A.3 draws exactly
    /// `code_len` bits and the array is sized `2^(code_len + 1)`, so the walk
    /// cannot leave it. IDs at or above `1 << code_len` are truncated to their
    /// low bits, which is the only thing a `code_len`-bit code can carry.
    pub(crate) fn encode_iaid(enc: &mut MqEncoder, cx: &mut IaidCtx, id: u32) {
        let mut prev = 1usize;
        for shift in (0..cx.code_len()).rev() {
            let bit = u8::try_from((id >> shift) & 1).unwrap_or(0);
            enc.encode(cx.context_mut(prev), bit);
            prev = (prev << 1) | usize::from(bit);
        }
    }
}

/// The thirteen integer procedures of T.88 Annex A, named as the standard
/// names them.
///
/// Every symbol dictionary and region decoder carries one of these for the
/// duration of a segment. The names are the standard's: `IADH` and `IADW` are
/// the height and width deltas of a symbol dictionary (6.5.6, 6.5.7), `IAEX`
/// and `IAAI` its export runs and aggregate counts (6.5.10, 6.5.8.2.1), and the
/// remaining nine drive text regions — strip offsets, first symbol and
/// subsequent symbol coordinates, the refinement flag and its four offsets
/// (6.4.5 onward).
#[derive(Clone, Debug)]
pub(crate) struct IntCtxSet {
    /// `IADH`: height class delta of a symbol dictionary.
    pub(crate) iadh: IntCtx,
    /// `IADW`: symbol width delta within a height class.
    pub(crate) iadw: IntCtx,
    /// `IAEX`: length of an export or non-export run.
    pub(crate) iaex: IntCtx,
    /// `IAAI`: number of symbol instances in an aggregate.
    #[allow(dead_code)] // Read once aggregate symbol coding is decoded (6.5.8.2).
    pub(crate) iaai: IntCtx,
    /// `IADT`: strip coordinate delta of a text region.
    pub(crate) iadt: IntCtx,
    /// `IAFS`: S coordinate of the first symbol instance in a strip.
    pub(crate) iafs: IntCtx,
    /// `IADS`: S coordinate delta of each subsequent symbol instance.
    pub(crate) iads: IntCtx,
    /// `IAIT`: T coordinate of a symbol instance within its strip.
    pub(crate) iait: IntCtx,
    // The five refinement procedures below are the ones no decoder in this
    // build reaches: a text region rejects REFINE and a symbol dictionary
    // rejects SDREFAGG, so nothing decodes the flag or the four offsets of
    // 6.4.11. They are allocated all the same, because the thirteen arrays are
    // constructed as one set per segment and a set missing five of them would
    // have to be assembled differently once refinement lands.
    /// `IARI`: whether a symbol instance carries a refinement.
    #[allow(dead_code)] // Read once refinement coding is decoded (6.4.11).
    pub(crate) iari: IntCtx,
    /// `IARDW`: width delta of a refined symbol instance.
    #[allow(dead_code)] // Read once refinement coding is decoded (6.4.11).
    pub(crate) iardw: IntCtx,
    /// `IARDH`: height delta of a refined symbol instance.
    #[allow(dead_code)] // Read once refinement coding is decoded (6.4.11).
    pub(crate) iardh: IntCtx,
    /// `IARDX`: horizontal offset of a refined symbol instance.
    #[allow(dead_code)] // Read once refinement coding is decoded (6.4.11).
    pub(crate) iardx: IntCtx,
    /// `IARDY`: vertical offset of a refined symbol instance.
    #[allow(dead_code)] // Read once refinement coding is decoded (6.4.11).
    pub(crate) iardy: IntCtx,
}

impl IntCtxSet {
    /// Allocates all thirteen arrays, every context at state 0 / MPS 0.
    pub(crate) fn new() -> Self {
        Self {
            iadh: IntCtx::new(),
            iadw: IntCtx::new(),
            iaex: IntCtx::new(),
            iaai: IntCtx::new(),
            iadt: IntCtx::new(),
            iafs: IntCtx::new(),
            iads: IntCtx::new(),
            iait: IntCtx::new(),
            iari: IntCtx::new(),
            iardw: IntCtx::new(),
            iardh: IntCtx::new(),
            iardx: IntCtx::new(),
            iardy: IntCtx::new(),
        }
    }

    /// Returns all thirteen arrays to state 0 / MPS 0.
    ///
    /// Nothing in this build calls it: every segment that needs fresh
    /// statistics gets a fresh set, and the one place the standard asks for an
    /// in-place reset is the retained-context flags of a symbol dictionary
    /// (7.4.2.1.1 bits 8 and 9), which this build reads and ignores.
    #[allow(dead_code)] // Reached once retained arithmetic contexts are honoured.
    pub(crate) fn reset(&mut self) {
        for cx in self.all_mut() {
            cx.reset();
        }
    }

    /// The thirteen arrays in declaration order.
    #[allow(dead_code)] // Reached from `reset` and from this module's tests.
    fn all_mut(&mut self) -> [&mut IntCtx; 13] {
        [
            &mut self.iadh,
            &mut self.iadw,
            &mut self.iaex,
            &mut self.iaai,
            &mut self.iadt,
            &mut self.iafs,
            &mut self.iads,
            &mut self.iait,
            &mut self.iari,
            &mut self.iardw,
            &mut self.iardh,
            &mut self.iardx,
            &mut self.iardy,
        ]
    }
}

#[cfg(test)]
impl IntCtxSet {
    /// The digest of each of the thirteen arrays, in declaration order.
    fn digests(&self) -> [u64; 13] {
        [
            self.iadh.state_digest(),
            self.iadw.state_digest(),
            self.iaex.state_digest(),
            self.iaai.state_digest(),
            self.iadt.state_digest(),
            self.iafs.state_digest(),
            self.iads.state_digest(),
            self.iait.state_digest(),
            self.iari.state_digest(),
            self.iardw.state_digest(),
            self.iardh.state_digest(),
            self.iardx.state_digest(),
            self.iardy.state_digest(),
        ]
    }
}

#[cfg(test)]
mod tests {
    use super::encoder::{encode_iaid, encode_int};
    use super::*;
    use crate::filters::jbig2::mq::{encoder::MqEncoder, MqDecoder};

    /// Every value A.2 can carry survives a round trip through the encoding
    /// side of the same clause: the boundaries of all six length classes,
    /// both signs, the extremes of [`i32`], and OOB interleaved with the
    /// value zero it must not be confused with.
    #[test]
    fn integers_round_trip_through_annex_a() {
        let values: Vec<Option<i32>> = vec![
            Some(0),
            None,
            Some(0),
            Some(1),
            Some(3),
            Some(4),
            Some(19),
            Some(20),
            Some(83),
            Some(84),
            Some(339),
            Some(340),
            Some(4_435),
            Some(4_436),
            Some(65_536),
            Some(100_000),
            Some(i32::MAX),
            None,
            Some(-1),
            Some(-3),
            Some(-4),
            Some(-19),
            Some(-20),
            Some(-83),
            Some(-84),
            Some(-339),
            Some(-340),
            Some(-4_435),
            Some(-4_436),
            Some(-70_000),
            Some(-100_000),
            Some(i32::MIN),
            None,
        ];

        let mut enc = MqEncoder::new();
        let mut enc_cx = IntCtx::new();
        for value in &values {
            encode_int(&mut enc, &mut enc_cx, *value);
        }
        let coded = enc.finish();

        let mut dec = MqDecoder::new(&coded);
        let mut dec_cx = IntCtx::new();
        let decoded: Vec<Option<i32>> = values
            .iter()
            .map(|_| decode_int(&mut dec, &mut dec_cx))
            .collect();
        assert_eq!(decoded, values);
        assert_eq!(dec_cx.state_digest(), enc_cx.state_digest());
    }

    /// Zero and OOB differ by a single decoded bit — the sign set over a
    /// magnitude of nothing — and every caller reads OOB as a list
    /// terminator. Confusing the two turns each terminator into a legitimate
    /// value, so the distinction gets a test of its own rather than only
    /// riding along inside a larger sweep.
    #[test]
    fn zero_and_oob_are_distinguishable() {
        let mut enc = MqEncoder::new();
        let mut enc_cx = IntCtx::new();
        encode_int(&mut enc, &mut enc_cx, Some(0));
        encode_int(&mut enc, &mut enc_cx, None);
        encode_int(&mut enc, &mut enc_cx, Some(0));
        let coded = enc.finish();

        let mut dec = MqDecoder::new(&coded);
        let mut dec_cx = IntCtx::new();
        assert_eq!(decode_int(&mut dec, &mut dec_cx), Some(0));
        assert_eq!(decode_int(&mut dec, &mut dec_cx), None);
        assert_eq!(decode_int(&mut dec, &mut dec_cx), Some(0));
    }

    /// Several procedures braided into one arithmetic stream come back in the
    /// same braid.
    ///
    /// This is the shape a symbol dictionary or text region actually has: one
    /// MQ decoder, thirteen context arrays, and a decode order fixed by the
    /// clause rather than by the data. Each array must adapt only on the bits
    /// drawn against it, so a value coded through `IADH` and a value coded
    /// through `IADW` between two `IADH` values must not disturb one another.
    /// Repeating a value through the same procedure also pins that the
    /// adaptation is applied — the second `Some(7)` costs fewer bits than the
    /// first and still decodes the same.
    #[test]
    fn round_trips_interleaved_procedures() {
        let plan: [(usize, Option<i32>); 9] = [
            (0, Some(7)),
            (1, Some(-3)),
            (0, Some(7)),
            (2, None),
            (1, Some(1_200)),
            (0, Some(0)),
            (2, Some(-1)),
            (1, None),
            (0, Some(42)),
        ];

        let mut enc = MqEncoder::new();
        let mut enc_cx = [IntCtx::new(), IntCtx::new(), IntCtx::new()];
        for (which, value) in &plan {
            encode_int(&mut enc, &mut enc_cx[*which], *value);
        }
        let coded = enc.finish();

        let mut dec = MqDecoder::new(&coded);
        let mut dec_cx = [IntCtx::new(), IntCtx::new(), IntCtx::new()];
        for (i, (which, want)) in plan.iter().enumerate() {
            assert_eq!(decode_int(&mut dec, &mut dec_cx[*which]), *want, "step {i}");
        }
        for (which, (enc_one, dec_one)) in enc_cx.iter().zip(&dec_cx).enumerate() {
            assert_eq!(
                dec_one.state_digest(),
                enc_one.state_digest(),
                "procedure {which} adapted differently on the two sides"
            );
        }
    }

    /// A long pseudo-random run round-trips too, so the context array is
    /// walked far enough for the magnitude-loop clamp to matter: a value in
    /// the 32-bit class folds `PREV` back into the upper half of the array
    /// twenty-three times.
    #[test]
    fn long_runs_of_integers_round_trip() {
        let mut state: u32 = 0x1357_9BDF;
        let values: Vec<Option<i32>> = (0..500)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                match state % 8 {
                    0 => None,
                    1 => Some(0),
                    2..=4 => Some(i32::try_from(state % 5_000).unwrap_or(0)),
                    _ => Some(-i32::try_from(state % 100_000).unwrap_or(0)),
                }
            })
            .collect();

        let mut enc = MqEncoder::new();
        let mut enc_cx = IntCtx::new();
        for value in &values {
            encode_int(&mut enc, &mut enc_cx, *value);
        }
        let coded = enc.finish();

        let mut dec = MqDecoder::new(&coded);
        let mut dec_cx = IntCtx::new();
        let decoded: Vec<Option<i32>> = values
            .iter()
            .map(|_| decode_int(&mut dec, &mut dec_cx))
            .collect();
        assert_eq!(decoded, values);
    }

    /// Symbol IDs round-trip at every code length, including the degenerate
    /// zero-bit code (T.88 A.3).
    #[test]
    fn symbol_ids_round_trip_through_annex_a() {
        for len in 0u32..=12 {
            let modulus = 1u32 << len;
            let ids: Vec<u32> = (0..128u32)
                .map(|i| i.wrapping_mul(37).wrapping_add(5) % modulus)
                .collect();

            let mut enc = MqEncoder::new();
            let mut enc_cx = IaidCtx::new(len);
            for id in &ids {
                encode_iaid(&mut enc, &mut enc_cx, *id);
            }
            let coded = enc.finish();

            let mut dec = MqDecoder::new(&coded);
            let mut dec_cx = IaidCtx::new(len);
            let decoded: Vec<u32> = ids
                .iter()
                .map(|_| decode_iaid(&mut dec, &mut dec_cx))
                .collect();
            assert_eq!(decoded, ids, "code length {len}");
        }
    }

    /// Decoding through one procedure must leave the other twelve untouched:
    /// each integer procedure owns its own context array (T.88 A.2).
    #[test]
    fn context_set_has_thirteen_independent_arrays() {
        let mut set = IntCtxSet::new();
        let mut dec = MqDecoder::new(&[0x55; 32]);
        let _ = decode_int(&mut dec, &mut set.iadh);
        let fresh = IntCtx::new();
        assert_eq!(set.iadw.state_digest(), fresh.state_digest());
    }

    /// Adapting any one of the thirteen arrays changes that array's digest and
    /// no other's.
    #[test]
    fn each_procedure_adapts_alone() {
        let untouched = IntCtxSet::new().digests();
        for field in 0..untouched.len() {
            let mut set = IntCtxSet::new();
            let mut dec = MqDecoder::new(&[0x9B; 32]);
            for (position, cx) in set.all_mut().into_iter().enumerate() {
                if position == field {
                    let _ = decode_int(&mut dec, cx);
                }
            }
            let after = set.digests();
            assert_ne!(
                after[field], untouched[field],
                "field {field} never adapted"
            );
            for (other, digest) in after.iter().enumerate() {
                if other != field {
                    assert_eq!(
                        *digest, untouched[other],
                        "adapting field {field} disturbed field {other}"
                    );
                }
            }
        }
    }

    /// `reset` returns adapted arrays to the state a fresh set starts in.
    #[test]
    fn reset_restores_the_initial_state() {
        let fresh = IntCtxSet::new().digests();
        let mut set = IntCtxSet::new();
        let mut dec = MqDecoder::new(&[0x3C; 64]);
        for cx in set.all_mut() {
            let _ = decode_int(&mut dec, cx);
        }
        assert_ne!(set.digests(), fresh);
        set.reset();
        assert_eq!(set.digests(), fresh);

        let mut iaid = IaidCtx::new(6);
        let before = iaid.state_digest();
        let _ = decode_iaid(&mut dec, &mut iaid);
        assert_ne!(iaid.state_digest(), before);
        iaid.reset();
        assert_eq!(iaid.state_digest(), before);
    }

    /// A truncated stream decodes an unbounded run of trailing bits rather
    /// than panicking; `decode_int` must inherit that from the MQ layer.
    ///
    /// Returning from each call is the weaker half of totality. The half that
    /// matters to a caller is that the list ends, which
    /// `integer_lists_terminate_on_every_input` is what checks.
    #[test]
    fn decode_int_terminates_on_empty_input() {
        let mut dec = MqDecoder::new(&[]);
        let mut cx = IntCtx::new();
        for _ in 0..1_000 {
            let _ = decode_int(&mut dec, &mut cx);
        }
    }

    /// No byte string, however malformed, may panic or hang the procedure.
    #[test]
    fn decode_int_never_panics_on_adversarial_bytes() {
        for seed in 0u32..512 {
            let data: Vec<u8> = (0..64u32)
                .map(|i| u8::try_from(seed.wrapping_mul(31).wrapping_add(i) % 256).unwrap_or(0))
                .collect();
            let mut dec = MqDecoder::new(&data);
            let mut cx = IntCtx::new();
            for _ in 0..200 {
                let _ = decode_int(&mut dec, &mut cx);
            }
        }
    }

    /// Inputs whose decoded values, not whose length, decide when a caller
    /// stops: every single byte, the degenerate fills, and a pseudo-random
    /// spread of lengths.
    ///
    /// A single byte is enough to reach the decoder's post-marker cycle, so
    /// the short cases are the interesting ones rather than a formality.
    fn terminator_corpus() -> Vec<Vec<u8>> {
        let mut corpus: Vec<Vec<u8>> = vec![
            vec![],
            vec![0xFF, 0xAC],
            vec![0xFF, 0x90],
            vec![0xFF, 0x8F],
            vec![0x00; 8],
            vec![0xAA; 8],
            vec![0x55; 8],
            vec![0xFF; 8],
            vec![0x00; 64],
            vec![0xAA, 0x55, 0xAA, 0x55],
        ];
        corpus.extend((0u32..256).map(|b| vec![u8::try_from(b).unwrap_or(0)]));
        let mut state: u32 = 0x0BAD_F00D;
        corpus.extend((0u32..512).map(|_| {
            state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
            let len = (state >> 20) as usize % 40;
            (0..len)
                .map(|_| {
                    state = state.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
                    u8::try_from(state >> 24).unwrap_or(0)
                })
                .collect()
        }));
        corpus
    }

    /// The list loop of T.88 6.5.5 step 4(c) — `repeat { DW = decode IADW; if
    /// OOB break }` — ends on every input.
    ///
    /// This is the property the fixed-round sweeps cannot see. Each call
    /// returns, so nothing panics, but what ends a caller's loop is the
    /// *value*, and once the coded data is spent the decoder cycles: the same
    /// registers and contexts, so the same value, forever. For most inputs
    /// that value is not the terminator, and a loop waiting for one would run
    /// until the process was killed. The limit here is generous enough that
    /// only a loop that genuinely never ends can reach it.
    #[test]
    fn integer_lists_terminate_on_every_input() {
        for data in terminator_corpus() {
            let mut dec = MqDecoder::new(&data);
            let mut cx = IntCtx::new();
            let mut read = 0usize;
            while decode_int(&mut dec, &mut cx).is_some() {
                read += 1;
                assert!(
                    read < 100_000,
                    "no terminator in {data:?} after {read} values"
                );
            }
        }
    }

    /// Once the coded data is spent, OOB is all there is.
    ///
    /// Terminating one list is not enough: a symbol dictionary nests them, one
    /// width list per height class (6.5.5), and a text region nests a symbol
    /// list inside a strip list (6.4.5). Every one of those loops ends only if
    /// the terminator keeps coming, so an exhausted stream must never go back
    /// to producing values.
    #[test]
    fn an_exhausted_stream_yields_nothing_but_oob() {
        for data in terminator_corpus() {
            let mut dec = MqDecoder::new(&data);
            let mut set = IntCtxSet::new();
            let mut spun = 0usize;
            while !dec.is_exhausted() {
                let _ = decode_int(&mut dec, &mut set.iadh);
                spun += 1;
                assert!(spun < 100_000, "{data:?} never ran out of data");
            }
            for step in 0..256 {
                for cx in set.all_mut() {
                    assert_eq!(
                        decode_int(&mut dec, cx),
                        None,
                        "{data:?} produced a value at step {step} past the end"
                    );
                }
            }
        }
    }

    /// The fabricated terminator must not arrive early: a list coded per A.2
    /// comes back whole, values and terminator alike.
    ///
    /// This is the other side of `integer_lists_terminate_on_every_input`. A
    /// decoder that answered OOB too eagerly would pass that test and truncate
    /// every real symbol dictionary, so the two are only meaningful together.
    #[test]
    fn a_coded_list_survives_to_its_own_terminator() {
        let mut state: u32 = 0x5EED_1234;
        let mut values: Vec<Option<i32>> = (0..600)
            .map(|_| {
                state ^= state << 13;
                state ^= state >> 17;
                state ^= state << 5;
                Some(i32::try_from(state % 200_000).unwrap_or(0) - 100_000)
            })
            .collect();
        values.push(None);

        let mut enc = MqEncoder::new();
        let mut enc_cx = IntCtx::new();
        for value in &values {
            encode_int(&mut enc, &mut enc_cx, *value);
        }
        let coded = enc.finish();

        let mut dec = MqDecoder::new(&coded);
        let mut dec_cx = IntCtx::new();
        let mut decoded: Vec<Option<i32>> = Vec::new();
        loop {
            let value = decode_int(&mut dec, &mut dec_cx);
            decoded.push(value);
            if value.is_none() {
                break;
            }
        }
        assert_eq!(decoded, values);
    }

    /// The leading bits an integer procedure draws from `data`, walking
    /// `PREV` as T.88 A.2 does.
    ///
    /// [`next_prev`] is applied to every bit, not only the magnitude ones: it
    /// is a plain shift below 256 and the prefix never reaches that, so this
    /// selects exactly the contexts [`decode_int`] selects. It reports the bit
    /// sequence that procedure is about to interpret, and says nothing about
    /// how those bits map to a value.
    fn leading_bits(data: &[u8], count: usize, cx: &mut IntCtx) -> Vec<u8> {
        let mut dec = MqDecoder::new(data);
        let mut prev = 1usize;
        (0..count)
            .map(|_| {
                let bit = dec.decode(cx.context_mut(prev));
                prev = next_prev(prev, bit);
                bit
            })
            .collect()
    }

    /// Searches for a coded byte string whose leading integer-procedure bits
    /// are exactly `pattern`, so that a bit sequence chosen by hand can be fed
    /// to [`decode_int`].
    ///
    /// The candidates are a fixed multiplicative sweep, so the search is
    /// deterministic: it either always finds the same input or always fails.
    fn input_with_leading_bits(pattern: &[u8]) -> Vec<u8> {
        let mut cx = IntCtx::new();
        for counter in 0u32..(1 << 20) {
            let data = counter.wrapping_mul(2_654_435_761).to_be_bytes();
            cx.reset();
            if leading_bits(&data, pattern.len(), &mut cx) == pattern {
                return data.to_vec();
            }
        }
        panic!("no coded input produces the bit pattern {pattern:?}");
    }

    /// The sign bit, the unary length class and the magnitude bits map to a
    /// value exactly as T.88 A.2 specifies — including OOB, which is the sign
    /// bit set over a magnitude of zero and is one decoded bit away from the
    /// value zero.
    ///
    /// Each case names the bit sequence and the value it denotes; the input
    /// producing that sequence is searched for through the MQ layer, so the
    /// expectations come from the standard rather than from this module.
    #[test]
    fn bit_patterns_map_to_the_values_annex_a_assigns() {
        let cases: [(&[u8], Option<i32>); 15] = [
            // Length class 0: two magnitude bits, no bias.
            (&[0, 0, 0, 0], Some(0)),
            (&[0, 0, 0, 1], Some(1)),
            (&[0, 0, 1, 0], Some(2)),
            (&[0, 0, 1, 1], Some(3)),
            (&[1, 0, 0, 1], Some(-1)),
            (&[1, 0, 1, 1], Some(-3)),
            // The sign bit over a zero magnitude is OOB, not negative zero.
            (&[1, 0, 0, 0], None),
            // Length class 1: four magnitude bits biased by 4.
            (&[0, 1, 0, 0, 0, 0, 0], Some(4)),
            (&[0, 1, 0, 1, 1, 1, 1], Some(19)),
            (&[1, 1, 0, 0, 0, 0, 1], Some(-5)),
            // Length class 2: six magnitude bits biased by 20.
            (&[0, 1, 1, 0, 0, 0, 0, 0, 0, 0], Some(20)),
            (&[1, 1, 1, 0, 0, 0, 0, 0, 1, 0], Some(-22)),
            // Length class 3: eight magnitude bits biased by 84.
            (&[0, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0], Some(84)),
            (&[1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 1], Some(-85)),
            // Length class 4: twelve magnitude bits biased by 340.
            (
                &[0, 1, 1, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0],
                Some(340),
            ),
        ];
        for (pattern, want) in cases {
            let data = input_with_leading_bits(pattern);
            let mut dec = MqDecoder::new(&data);
            let mut cx = IntCtx::new();
            assert_eq!(decode_int(&mut dec, &mut cx), want, "bits {pattern:?}");
        }
    }

    /// A sweep over pseudo-random input reaches every shape of result: OOB,
    /// zero, negatives and the long length classes. Coverage only — the exact
    /// bit-to-value mapping is pinned by
    /// `bit_patterns_map_to_the_values_annex_a_assigns`.
    #[test]
    fn oob_and_zero_are_both_reachable_and_distinct() {
        let mut saw_oob = false;
        let mut saw_zero = false;
        let mut saw_negative = false;
        let mut saw_large = false;
        for seed in 0u32..2_048 {
            let data: Vec<u8> = (0..48u32)
                .map(|i| {
                    let mixed = seed
                        .wrapping_mul(2_654_435_761)
                        .wrapping_add(i.wrapping_mul(97));
                    u8::try_from(mixed >> 24).unwrap_or(0)
                })
                .collect();
            let mut dec = MqDecoder::new(&data);
            let mut cx = IntCtx::new();
            for _ in 0..64 {
                match decode_int(&mut dec, &mut cx) {
                    None => saw_oob = true,
                    Some(0) => saw_zero = true,
                    Some(v) if v < 0 => saw_negative = true,
                    Some(v) if v > 4_435 => saw_large = true,
                    Some(_) => {}
                }
            }
        }
        assert!(saw_oob, "OOB never decoded");
        assert!(saw_zero, "the value zero never decoded");
        assert!(saw_negative, "no negative value decoded");
        assert!(saw_large, "no long-form value decoded");
    }

    /// The `PREV` update of the value loop (T.88 A.2, step 4) keeps the
    /// context index inside the 512-entry array. Without the clamp the index
    /// grows without bound and the procedure reads outside the array it owns.
    #[test]
    fn value_context_index_stays_in_the_array() {
        for prev in 1usize..512 {
            for bit in [0u8, 1] {
                let next = next_prev(prev, bit);
                assert!(next < 512, "prev {prev} bit {bit} escaped to {next}");
                assert!(next >= 1, "prev {prev} bit {bit} fell to zero");
            }
        }
    }

    /// Below 256 the index is a plain prefix of the decoded bits; at and above
    /// it, bit 8 is pinned so the index folds back into the upper half
    /// (T.88 A.2, step 4).
    #[test]
    fn value_context_index_folds_at_two_hundred_and_fifty_six() {
        assert_eq!(next_prev(1, 0), 2);
        assert_eq!(next_prev(1, 1), 3);
        assert_eq!(next_prev(255, 0), 510);
        assert_eq!(next_prev(255, 1), 511);
        assert_eq!(next_prev(256, 0), 256);
        assert_eq!(next_prev(256, 1), 257);
        assert_eq!(next_prev(511, 0), 510);
        assert_eq!(next_prev(511, 1), 511);
    }

    /// Every symbol ID is in range for its code length (T.88 A.3).
    #[test]
    fn iaid_returns_value_in_range() {
        let data: Vec<u8> = (0..128u32)
            .map(|i| u8::try_from(i.wrapping_mul(7) % 256).unwrap_or(0))
            .collect();
        for len in 1u32..=9 {
            let mut dec = MqDecoder::new(&data);
            let mut cx = IaidCtx::new(len);
            for _ in 0..50 {
                let id = decode_iaid(&mut dec, &mut cx);
                assert!(id < (1 << len), "id {id} out of range for len {len}");
            }
        }
    }

    /// A zero-length symbol code decodes no bits at all, so the only symbol in
    /// the dictionary is symbol 0 (T.88 A.3).
    #[test]
    fn iaid_with_zero_code_len_is_always_zero() {
        let mut dec = MqDecoder::new(&[0xAA; 8]);
        let mut cx = IaidCtx::new(0);
        assert_eq!(decode_iaid(&mut dec, &mut cx), 0);
    }

    /// An absurd code length is clamped rather than allocating an array the
    /// spec's 32-bit ceiling would allow.
    #[test]
    fn iaid_code_len_is_clamped() {
        assert_eq!(IaidCtx::new(0).code_len(), 0);
        assert_eq!(IaidCtx::new(9).code_len(), 9);
        assert_eq!(IaidCtx::new(16).code_len(), 16);
        assert_eq!(IaidCtx::new(17).code_len(), 16);
        assert_eq!(IaidCtx::new(u32::MAX).code_len(), 16);
    }

    /// The accessors hand out live storage from the array the decode
    /// procedures index, not a copy.
    #[test]
    fn context_mut_reaches_the_decoding_storage() {
        let mut cx = IntCtx::new();
        let before = cx.state_digest();
        let mut dec = MqDecoder::new(&[0x00; 8]);
        let _ = dec.decode(cx.context_mut(1));
        assert_ne!(cx.state_digest(), before, "IntCtx::context_mut is a copy");

        let mut iaid = IaidCtx::new(4);
        let before = iaid.state_digest();
        let _ = dec.decode(iaid.context_mut(1));
        assert_ne!(
            iaid.state_digest(),
            before,
            "IaidCtx::context_mut is a copy"
        );
    }

    /// A corrupt index from the caller is clamped into the array, never
    /// indexed raw.
    #[test]
    fn context_mut_clamps_out_of_range_indices() {
        let mut cx = IntCtx::new();
        let _ = cx.context_mut(usize::MAX);
        let mut iaid = IaidCtx::new(2);
        let _ = iaid.context_mut(usize::MAX);
    }

    /// The same bytes decode to the same integers: no state escapes the
    /// decoder and its context set.
    #[test]
    fn decoding_is_deterministic() {
        let data: Vec<u8> = (0..96u32)
            .map(|i| u8::try_from(i.wrapping_mul(37).wrapping_add(11) % 256).unwrap_or(0))
            .collect();
        let run = || {
            let mut dec = MqDecoder::new(&data);
            let mut set = IntCtxSet::new();
            let mut iaid = IaidCtx::new(5);
            let mut out = Vec::new();
            for _ in 0..40 {
                out.push(decode_int(&mut dec, &mut set.iadh));
                out.push(decode_int(&mut dec, &mut set.iads));
                out.push(Some(
                    i32::try_from(decode_iaid(&mut dec, &mut iaid)).unwrap_or(-1),
                ));
            }
            out
        };
        let first = run();
        assert_eq!(first, run());
    }
}
