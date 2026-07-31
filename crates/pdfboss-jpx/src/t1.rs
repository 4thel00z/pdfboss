//! Tier-1: EBCOT code-block decoding (ITU-T T.800 Annex D) — three coding
//! passes per bit-plane over 4-row stripes (D.1 scan pattern):
//! significance propagation (D.3.1) with sign decoding (D.3.2), magnitude
//! refinement (D.3.3), and cleanup (D.3.4); plus selective arithmetic
//! bypass (D.6), context reset / per-pass termination (D.4, Tables
//! D.8/D.9), vertically causal contexts (D.7) and the segmentation symbol
//! (D.5), all switched by the Table A.19 style bits.

use crate::error::{JpxError, Result};
use crate::geometry::{BandKind, Rect};
use crate::mq::{MqContext, MqDecoder};
use crate::packet::{CodeBlockInput, CodeBlockSegment};

/// Decoded coefficients of one code-block — the t1 → dequant seam.
// Constructed by the t1 stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct CodeBlockCoefficients {
    /// The code-block's rect in ABSOLUTE band coordinates (identical to
    /// the input's; dequantization places samples with it).
    pub rect: Rect,
    /// Per-sample magnitude in raster order over `rect`: each decoded
    /// bit MSB_i sits at its Equation (E-1) weight, bit `Mb - i`, so
    /// undecoded low planes are zero. Length `rect.width() *
    /// rect.height()`.
    pub magnitudes: Vec<u32>,
    /// Per-sample sign, true = negative (D.3.2), raster order.
    pub negative: Vec<bool>,
    /// Per-sample count Nb(u, v) of decoded magnitude bit-planes (D.2) —
    /// the reconstruction parameter r of E.1.1.2/E.1.2.2 needs it.
    pub decoded_planes: Vec<u8>,
    /// Entropy corruption was detected (a segment ran dry mid-pass, a D.5
    /// segmentation-symbol mismatch, pass counts disagreeing with the
    /// termination layout, or a codeword range outside the tile body):
    /// the coefficients keep everything decoded up to that point, and the
    /// caller reports one warning per corrupt block (leniency doctrine).
    pub corrupt: bool,
}

/// One band's worth of decoded code-blocks, assembled by the orchestration
/// in `decode()` and consumed by dequantization.
// Constructed by decode(); the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct BandCoefficients {
    /// Band kind (Table B.1 / Table E.1).
    pub kind: BandKind,
    /// Decomposition level nb (Equation (E-5)).
    pub level: u8,
    /// Absolute band rect (B-15).
    pub rect: Rect,
    /// Decoded blocks, same order as the packet stage emitted them.
    pub blocks: Vec<CodeBlockCoefficients>,
}

/// Rows per code-block scan stripe (D.1: "the first four coefficients of
/// the first column are scanned, followed by ...").
const STRIPE_ROWS: usize = 4;

/// Sample bound per Table A.18: `xcb + ycb <= 12`, so a code-block never
/// holds more than 2^12 coefficients. Enforced, never trusted.
const MAX_BLOCK_SAMPLES: u64 = 4096;

/// The Annex D context labels, packed as one array: 0..=8 zero coding
/// (Table D.1), 9..=13 sign coding (Table D.3), 14..=16 magnitude
/// refinement (Table D.4), 17 run-length, 18 UNIFORM (D.3.4).
const CTX_RUN_LENGTH: usize = 17;
const CTX_UNIFORM: usize = 18;
const CONTEXT_COUNT: usize = 19;

/// First 0-based coding pass decoded raw under selective bypass: Table D.9
/// switches to raw coding at the fifth bit-plane's significance
/// propagation pass — after the first cleanup plus three full triples,
/// i.e. after 10 passes.
const FIRST_BYPASS_PASS: u64 = 10;

/// Code-block style bits (Table A.19). Bit 2 (termination on every pass)
/// and bit 4 (predictable termination) need no decoder action here: Tier-2
/// already split the codeword segments at every signalled termination
/// (B.10.7), and predictable termination constrains the encoder — a
/// decoder may verify it but must still accept the data (D.4.2).
const STYLE_BYPASS: u8 = 1;
const STYLE_RESET: u8 = 2;
const STYLE_VCAUSAL: u8 = 8;
const STYLE_SEGSYM: u8 = 32;

/// The three coding passes of D.3, in their per-bit-plane order.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum PassKind {
    /// D.3.1 (includes D.3.2 sign decoding).
    SignificancePropagation,
    /// D.3.3.
    MagnitudeRefinement,
    /// D.3.4 (includes run-length and D.3.2 sign decoding).
    Cleanup,
}

/// Maps a 0-based coding pass index to (relative bit-plane, pass kind):
/// the first bit-plane with a non-zero element gets a cleanup pass only
/// (D.3), every later plane the significance propagation, magnitude
/// refinement, cleanup triple (Table D.8 ordering).
fn pass_schedule(pass: u64) -> (u32, PassKind) {
    if pass == 0 {
        return (0, PassKind::Cleanup);
    }
    let offset = pass - 1;
    let plane = (offset / 3 + 1) as u32;
    let kind = match offset % 3 {
        0 => PassKind::SignificancePropagation,
        1 => PassKind::MagnitudeRefinement,
        _ => PassKind::Cleanup,
    };
    (plane, kind)
}

/// Bit position of relative plane `plane` (0 = first decoded plane) in the
/// output magnitude: MSB_i carries weight `Mb - i` (Equation (E-1)); the
/// first decoded plane is MSB_(missing+1). `None` once `missing + plane`
/// walks past the Mb magnitude bit-planes of (E-2).
fn plane_bit_weight(mb: u32, missing: u32, plane: u32) -> Option<u32> {
    let consumed = missing.checked_add(plane)?.checked_add(1)?;
    mb.checked_sub(consumed)
}

/// The D.1 scan: stripes of four rows anchored at the code-block's top,
/// each stripe visited column by column, top to bottom within a column;
/// a ragged final stripe simply has fewer rows (Figure D.1).
fn scan_positions(width: usize, height: usize) -> impl Iterator<Item = (usize, usize)> {
    (0..height.div_ceil(STRIPE_ROWS)).flat_map(move |stripe| {
        let top = stripe * STRIPE_ROWS;
        let rows = STRIPE_ROWS.min(height - top);
        (0..width).flat_map(move |x| (top..top + rows).map(move |y| (x, y)))
    })
}

/// Zero coding (Table D.1): maps the neighborhood significance sums to one
/// of the nine context labels; the mapping depends on the sub-band kind
/// (the HL column is the LL/LH column with the H and V roles exchanged,
/// the HH column keys on the diagonal sum).
fn zero_coding_context(kind: BandKind, sh: u32, sv: u32, sd: u32) -> usize {
    if kind == BandKind::Hh {
        // HH column of Table D.1, top to bottom.
        let hv = sh + sv;
        return if sd >= 3 {
            8
        } else if sd == 2 {
            if hv >= 1 {
                7
            } else {
                6
            }
        } else if sd == 1 {
            if hv >= 2 {
                5
            } else if hv == 1 {
                4
            } else {
                3
            }
        } else if hv >= 2 {
            2
        } else if hv == 1 {
            1
        } else {
            0
        };
    }
    // LL/LH column; the HL column is the same with the H and V roles
    // exchanged (Table D.1).
    let (h, v) = if kind == BandKind::Hl {
        (sv, sh)
    } else {
        (sh, sv)
    };
    if h >= 2 {
        8
    } else if h == 1 {
        if v >= 1 {
            7
        } else if sd >= 1 {
            6
        } else {
            5
        }
    } else if v >= 2 {
        4
    } else if v == 1 {
        3
    } else if sd >= 2 {
        2
    } else if sd == 1 {
        1
    } else {
        0
    }
}

/// Sign coding step one (Table D.2): a neighbor pair's contribution is 1
/// when at least one is significant positive and none is significant
/// negative, -1 mirrored, 0 when insignificant or cancelling; i.e. the
/// sign of the sum of the +1/0/-1 neighbor states.
fn sign_contribution(a: i32, b: i32) -> i32 {
    (a + b).signum()
}

/// Sign coding step two (Table D.3): reduces the horizontal and vertical
/// contributions to a context label and the XOR bit of Equation (D-1).
fn sign_context(h0: i32, h1: i32, v0: i32, v1: i32) -> (usize, u32) {
    let h = sign_contribution(h0, h1);
    let v = sign_contribution(v0, v1);
    match (h, v) {
        (1, 1) => (13, 0),
        (1, 0) => (12, 0),
        (1, -1) => (11, 0),
        (0, 1) => (10, 0),
        (0, 0) => (9, 0),
        (0, -1) => (10, 1),
        (-1, 1) => (11, 1),
        (-1, 0) => (12, 1),
        // (-1, -1); the contributions cannot leave -1..=1.
        _ => (13, 1),
    }
}

/// Magnitude refinement contexts (Table D.4): keyed on whether this is the
/// coefficient's first refinement bit and, if so, whether any of the eight
/// neighbors is significant.
fn refinement_context(neighbor_sum: u32, first: bool) -> usize {
    if !first {
        16
    } else if neighbor_sum >= 1 {
        15
    } else {
        14
    }
}

/// Fresh contexts per Table D.7: everything at index 0 with MPS 0, except
/// the all-zero-neighborhood zero coding context (label 0) at index 4, the
/// run-length context at index 3 and UNIFORM at index 46.
fn initial_contexts() -> [MqContext; CONTEXT_COUNT] {
    let mut contexts = [MqContext::new(0); CONTEXT_COUNT];
    contexts[0] = MqContext::new(4);
    contexts[CTX_RUN_LENGTH] = MqContext::new(3);
    contexts[CTX_UNIFORM] = MqContext::new(46);
    contexts
}

/// Raw (bypassed) bit reader for D.6: bits come MSB first straight from
/// the codeword segment; a byte following an 0xFF carries a stuffed bit in
/// its most significant position, which is discarded ("this routine throws
/// out the first bit after an 0xFF byte value"). Exhaustion is `None`:
/// running dry mid-pass is corruption, not padding.
struct RawBits<'a> {
    data: &'a [u8],
    position: usize,
    current: u8,
    remaining: u32,
    previous_was_ff: bool,
}

impl<'a> RawBits<'a> {
    fn new(data: &'a [u8]) -> Self {
        RawBits {
            data,
            position: 0,
            current: 0,
            remaining: 0,
            previous_was_ff: false,
        }
    }

    fn next_bit(&mut self) -> Option<u32> {
        if self.remaining == 0 {
            let byte = *self.data.get(self.position)?;
            self.position += 1;
            self.current = byte;
            // A stuffed bit occupies the MSB after an 0xFF byte (D.6);
            // starting at 7 remaining bits discards it.
            self.remaining = if self.previous_was_ff { 7 } else { 8 };
            self.previous_was_ff = byte == 255;
        }
        self.remaining -= 1;
        Some(u32::from(self.current >> self.remaining) & 1)
    }
}

/// One terminated codeword segment: the concatenated bytes of the Tier-2
/// contributions up to and including a terminated one, plus the number of
/// coding passes they carry (D.4, B.10.7).
struct TermSegment {
    bytes: Vec<u8>,
    passes: u64,
}

/// Groups Tier-2 contributions into terminated codeword segments: an
/// unterminated contribution concatenates with the following ones — packet
/// bodies split a segment's passes across layers (B.10.7) — and each
/// terminated group gets its own fresh entropy decoder (D.4). A
/// contribution pointing outside the tile bit stream is corrupt: its
/// entire half-assembled group is dropped and decoding stops at the last
/// cleanly assembled segment (leniency doctrine); the second return
/// reports whether that happened.
fn terminated_segments(
    contributions: &[CodeBlockSegment],
    bitstream: &[u8],
) -> (Vec<TermSegment>, bool) {
    let mut segments = Vec::new();
    let mut bytes: Vec<u8> = Vec::new();
    let mut passes = 0u64;
    for part in contributions {
        let data = part
            .start
            .checked_add(part.len)
            .and_then(|end| bitstream.get(part.start..end));
        let Some(data) = data else {
            return (segments, true);
        };
        bytes.extend_from_slice(data);
        passes += u64::from(part.passes);
        if part.terminated {
            segments.push(TermSegment {
                bytes: std::mem::take(&mut bytes),
                passes,
            });
            passes = 0;
        }
    }
    if passes > 0 {
        // Unterminated tail: decode it as the final segment — reads past
        // its end continue from synthesized 0xFF bytes (D.4.1).
        segments.push(TermSegment { bytes, passes });
    }
    (segments, false)
}

/// The bit source for one pass: the MQ coder (Annex C) or the raw
/// bypassed reader (D.6). Which one a pass uses follows Table D.9.
enum Coder<'a> {
    Mq(MqDecoder<'a>),
    Raw(RawBits<'a>),
}

/// Per-sample decoding state over one code-block, raster indexed.
struct BlockState {
    width: usize,
    height: usize,
    /// Vertically causal context formation (D.7) in force.
    causal: bool,
    /// Significance states (D.3), initialized insignificant.
    significant: Vec<bool>,
    /// Bit decoded by THIS plane's significance propagation pass: D.3.3
    /// excludes those coefficients from refinement and D.3.4 (decision D9
    /// of Table D.10) skips them in cleanup.
    visited: Vec<bool>,
    /// Coefficient already got a refinement bit (Table D.4 "first
    /// refinement" column).
    refined: Vec<bool>,
    negative: Vec<bool>,
    magnitudes: Vec<u32>,
    decoded_planes: Vec<u8>,
}

impl BlockState {
    fn new(width: usize, height: usize, causal: bool) -> Self {
        let n = width * height;
        BlockState {
            width,
            height,
            causal,
            significant: vec![false; n],
            visited: vec![false; n],
            refined: vec![false; n],
            negative: vec![false; n],
            magnitudes: vec![0; n],
            decoded_planes: vec![0; n],
        }
    }

    fn at(&self, x: usize, y: usize) -> usize {
        y * self.width + x
    }

    /// Exclusive row bound visible from a coefficient in row `y`: with
    /// vertically causal formation the next stripe's rows read as
    /// insignificant (D.7), otherwise everything in the block is visible.
    fn visibility_limit(&self, y: usize) -> isize {
        if self.causal {
            (y - y % STRIPE_ROWS + STRIPE_ROWS) as isize
        } else {
            isize::MAX
        }
    }

    /// Significance of the neighbor at (x, y) as 0/1: outside the block —
    /// or beyond the causal row limit — reads as insignificant (D.3, D.7).
    fn neighbor_significance(&self, x: isize, y: isize, row_limit: isize) -> u32 {
        if x < 0 || y < 0 || x >= self.width as isize || y >= (self.height as isize).min(row_limit)
        {
            return 0;
        }
        u32::from(self.significant[y as usize * self.width + x as usize])
    }

    /// (sum H, sum V, sum D) of Figure D.2 around (x, y).
    fn neighbor_sums(&self, x: usize, y: usize) -> (u32, u32, u32) {
        let limit = self.visibility_limit(y);
        let (xi, yi) = (x as isize, y as isize);
        let sh = self.neighbor_significance(xi - 1, yi, limit)
            + self.neighbor_significance(xi + 1, yi, limit);
        let sv = self.neighbor_significance(xi, yi - 1, limit)
            + self.neighbor_significance(xi, yi + 1, limit);
        let sd = self.neighbor_significance(xi - 1, yi - 1, limit)
            + self.neighbor_significance(xi + 1, yi - 1, limit)
            + self.neighbor_significance(xi - 1, yi + 1, limit)
            + self.neighbor_significance(xi + 1, yi + 1, limit);
        (sh, sv, sd)
    }

    /// A neighbor's sign state for Table D.2: +1 significant positive, -1
    /// significant negative, 0 insignificant (or invisible).
    fn sign_state(&self, x: isize, y: isize, row_limit: isize) -> i32 {
        if self.neighbor_significance(x, y, row_limit) == 0 {
            return 0;
        }
        if self.negative[y as usize * self.width + x as usize] {
            -1
        } else {
            1
        }
    }

    /// (H0, H1, V0, V1) sign states around (x, y) per Figure D.2.
    fn sign_neighbors(&self, x: usize, y: usize) -> (i32, i32, i32, i32) {
        let limit = self.visibility_limit(y);
        let (xi, yi) = (x as isize, y as isize);
        (
            self.sign_state(xi - 1, yi, limit),
            self.sign_state(xi + 1, yi, limit),
            self.sign_state(xi, yi - 1, limit),
            self.sign_state(xi, yi + 1, limit),
        )
    }

    /// Table D.1 label for the current neighborhood around (x, y).
    fn zero_coding_label(&self, band: BandKind, x: usize, y: usize) -> usize {
        let (sh, sv, sd) = self.neighbor_sums(x, y);
        zero_coding_context(band, sh, sv, sd)
    }

    /// D.3.4 run-length eligibility for the stripe column at `x`: all four
    /// coefficients remain to be decoded in this cleanup pass and every
    /// one currently has the all-zero context (Table D.5, first column).
    fn run_length_eligible(&self, band: BandKind, x: usize, stripe_top: usize) -> bool {
        (0..STRIPE_ROWS).all(|k| {
            let y = stripe_top + k;
            let i = self.at(x, y);
            !self.significant[i] && !self.visited[i] && self.zero_coding_label(band, x, y) == 0
        })
    }

    /// Records one decoded magnitude bit of the current plane: the bit
    /// lands at its (E-1) weight, and Nb(u, v) — which counts the
    /// signalled all-zero planes too (D.2.1) — advances to `planes`.
    fn record(&mut self, i: usize, bit: u32, weight: u32, planes: u8) {
        // Weights of 32 and above are unrepresentable in the u32 seam and
        // only reachable through hostile Mb values (Equation (E-2) caps
        // honest ones well below); those bits are dropped, not decoded
        // around, so the coder stays in sync.
        if bit == 1 && weight < 32 {
            self.magnitudes[i] |= 1u32 << weight;
        }
        self.decoded_planes[i] = planes;
    }
}

/// One significance/cleanup bit: from the MQ coder in the given context,
/// or straight from the raw segment in a bypassed pass (D.6).
fn significance_bit(
    coder: &mut Coder<'_>,
    contexts: &mut [MqContext; CONTEXT_COUNT],
    label: usize,
) -> Option<u32> {
    match coder {
        Coder::Mq(mq) => Some(mq.decode(&mut contexts[label])),
        Coder::Raw(raw) => raw.next_bit(),
    }
}

/// Decodes the sign of a just-significant coefficient: Tables D.2/D.3 and
/// the Equation (D-1) XOR through the arithmetic coder, or a raw bit that
/// IS the sign per Equation (D-2) in a bypassed pass.
fn decode_sign(
    coder: &mut Coder<'_>,
    contexts: &mut [MqContext; CONTEXT_COUNT],
    state: &BlockState,
    x: usize,
    y: usize,
) -> Option<bool> {
    match coder {
        Coder::Mq(mq) => {
            let (h0, h1, v0, v1) = state.sign_neighbors(x, y);
            let (label, xor) = sign_context(h0, h1, v0, v1);
            Some((mq.decode(&mut contexts[label]) ^ xor) == 1)
        }
        Coder::Raw(raw) => Some(raw.next_bit()? == 1),
    }
}

/// One magnitude refinement bit (Table D.4 context through the MQ coder,
/// or raw under bypass).
fn refinement_bit(
    coder: &mut Coder<'_>,
    contexts: &mut [MqContext; CONTEXT_COUNT],
    state: &BlockState,
    x: usize,
    y: usize,
    i: usize,
) -> Option<u32> {
    match coder {
        Coder::Mq(mq) => {
            let (sh, sv, sd) = state.neighbor_sums(x, y);
            let label = refinement_context(sh + sv + sd, !state.refined[i]);
            Some(mq.decode(&mut contexts[label]))
        }
        Coder::Raw(raw) => raw.next_bit(),
    }
}

/// D.3.1: decodes the significance propagation pass of one bit-plane.
/// Returns false when the (raw) segment ran dry mid-pass — corruption.
fn significance_pass(
    state: &mut BlockState,
    coder: &mut Coder<'_>,
    contexts: &mut [MqContext; CONTEXT_COUNT],
    band: BandKind,
    weight: u32,
    planes: u8,
) -> bool {
    // A new plane starts here: the visited flags describe THIS plane's
    // significance propagation from now on (D.3.3, D.3.4).
    state.visited.fill(false);
    for (x, y) in scan_positions(state.width, state.height) {
        let i = state.at(x, y);
        // Only insignificant coefficients with a non-zero context take
        // part; all others are skipped (D.3.1).
        if state.significant[i] {
            continue;
        }
        let label = state.zero_coding_label(band, x, y);
        if label == 0 {
            continue;
        }
        let Some(bit) = significance_bit(coder, contexts, label) else {
            return false;
        };
        state.visited[i] = true;
        state.record(i, bit, weight, planes);
        if bit == 1 {
            state.significant[i] = true;
            let Some(negative) = decode_sign(coder, contexts, state, x, y) else {
                return false;
            };
            state.negative[i] = negative;
        }
    }
    true
}

/// D.3.3: decodes the magnitude refinement pass of one bit-plane — every
/// coefficient already significant before this plane's significance
/// propagation pass gets one more magnitude bit.
fn refinement_pass(
    state: &mut BlockState,
    coder: &mut Coder<'_>,
    contexts: &mut [MqContext; CONTEXT_COUNT],
    weight: u32,
    planes: u8,
) -> bool {
    for (x, y) in scan_positions(state.width, state.height) {
        let i = state.at(x, y);
        if !state.significant[i] || state.visited[i] {
            continue;
        }
        let Some(bit) = refinement_bit(coder, contexts, state, x, y, i) else {
            return false;
        };
        state.refined[i] = true;
        state.record(i, bit, weight, planes);
    }
    true
}

/// D.3.4: decodes the cleanup pass of one bit-plane — everything not yet
/// handled this plane — with run-length coding on fully-quiet stripe
/// columns, then checks the segmentation symbol when one is in force
/// (D.5). Returns false on corruption (segmentation mismatch, or a raw
/// coder where Table D.9 demands the arithmetic one).
fn cleanup_pass(
    state: &mut BlockState,
    coder: &mut Coder<'_>,
    contexts: &mut [MqContext; CONTEXT_COUNT],
    band: BandKind,
    weight: u32,
    planes: u8,
    segmentation: bool,
) -> bool {
    let Coder::Mq(mq) = coder else {
        // Cleanup passes always stay arithmetic-coded, whatever the bypass
        // style says (D.6); a raw coder here means the terminated-segment
        // layout was corrupt.
        return false;
    };
    let mut stripe_top = 0;
    while stripe_top < state.height {
        let rows = STRIPE_ROWS.min(state.height - stripe_top);
        for x in 0..state.width {
            let mut from = 0;
            // Run-length mode needs the full four rows (D.3.4: "If there
            // are fewer than four rows remaining in a code-block, then no
            // run-length coding is used").
            if rows == STRIPE_ROWS && state.run_length_eligible(band, x, stripe_top) {
                if mq.decode(&mut contexts[CTX_RUN_LENGTH]) == 0 {
                    // Table D.5 first row: all four stay insignificant —
                    // their bit for this plane decoded to zero.
                    for k in 0..STRIPE_ROWS {
                        let i = state.at(x, stripe_top + k);
                        state.record(i, 0, weight, planes);
                    }
                    continue;
                }
                // Table D.5 second row: two UNIFORM bits, MSB then LSB,
                // locate the first significant coefficient of the column.
                let msb = mq.decode(&mut contexts[CTX_UNIFORM]);
                let lsb = mq.decode(&mut contexts[CTX_UNIFORM]);
                let first = ((msb << 1) | lsb) as usize;
                for k in 0..first {
                    let i = state.at(x, stripe_top + k);
                    state.record(i, 0, weight, planes);
                }
                let y = stripe_top + first;
                let i = state.at(x, y);
                state.record(i, 1, weight, planes);
                state.significant[i] = true;
                let (h0, h1, v0, v1) = state.sign_neighbors(x, y);
                let (label, xor) = sign_context(h0, h1, v0, v1);
                state.negative[i] = (mq.decode(&mut contexts[label]) ^ xor) == 1;
                from = first + 1;
            }
            // "The decoding of any remaining coefficients continues in the
            // manner described in D.3.1" (D.3.4), skipping coefficients
            // already significant or already coded this plane (D9).
            for k in from..rows {
                let y = stripe_top + k;
                let i = state.at(x, y);
                if state.significant[i] || state.visited[i] {
                    continue;
                }
                let label = state.zero_coding_label(band, x, y);
                let bit = mq.decode(&mut contexts[label]);
                state.record(i, bit, weight, planes);
                if bit == 1 {
                    state.significant[i] = true;
                    let (h0, h1, v0, v1) = state.sign_neighbors(x, y);
                    let (label, xor) = sign_context(h0, h1, v0, v1);
                    state.negative[i] = (mq.decode(&mut contexts[label]) ^ xor) == 1;
                }
            }
        }
        stripe_top += STRIPE_ROWS;
    }
    if segmentation {
        // D.5: the symbol 1010 decodes with the UNIFORM context at the end
        // of every cleanup pass; a mismatch flags bit errors in the plane.
        for want in [1u32, 0, 1, 0] {
            if mq.decode(&mut contexts[CTX_UNIFORM]) != want {
                return false;
            }
        }
    }
    true
}

/// Decodes one code-block from its codeword segments (byte ranges into
/// `bitstream`, the tile's concatenated bodies).
///
/// Contract: never panics on hostile data — corrupt entropy data ends the
/// affected pass early, leaving the remaining coefficients at their
/// current (zero-extended) state and raising
/// [`CodeBlockCoefficients::corrupt`] so the caller can warn once per
/// damaged block. A block with no segments decodes to all-zero
/// coefficients.
pub(crate) fn decode_code_block(
    input: &CodeBlockInput,
    bitstream: &[u8],
) -> Result<CodeBlockCoefficients> {
    let width = input.rect.width() as usize;
    let height = input.rect.height() as usize;
    if width as u64 * height as u64 > MAX_BLOCK_SAMPLES {
        return Err(JpxError::Malformed(format!(
            "code-block {width}x{height} exceeds the {MAX_BLOCK_SAMPLES}-sample bound of Table A.18"
        )));
    }
    let mut state = BlockState::new(width, height, input.style & STYLE_VCAUSAL != 0);
    let (segments, mut corrupt) = terminated_segments(&input.segments, bitstream);

    // How many passes the block can meaningfully carry: Mb magnitude
    // bit-planes (E-2) minus the signalled all-zero ones (B.10.5) leaves
    // `available` decodable planes — one cleanup plus a triple per further
    // plane (D.3). Passes signalled beyond that budget are corrupt and
    // simply never run.
    let mb = u32::from(input.magnitude_bits);
    let available = u64::from(mb.saturating_sub(input.missing_msbs));
    let budget = if available == 0 { 0 } else { 3 * available - 2 };
    let signalled: u64 = segments.iter().map(|segment| segment.passes).sum();
    if signalled > budget {
        corrupt = true;
    }
    let scheduled = signalled.min(budget);

    let bypass = input.style & STYLE_BYPASS != 0;
    let reset = input.style & STYLE_RESET != 0;
    let segmentation = input.style & STYLE_SEGSYM != 0;

    let mut contexts = initial_contexts();
    let mut coder: Option<Coder<'_>> = None;
    let mut segment_index = 0;
    let mut passes_left = 0u64;
    for pass in 0..scheduled {
        let (plane, kind) = pass_schedule(pass);
        let raw = bypass && pass >= FIRST_BYPASS_PASS && kind != PassKind::Cleanup;
        if passes_left == 0 {
            // The previous terminated segment is spent: the next pass
            // starts a fresh coder over the next segment (D.4), raw or
            // arithmetic per Table D.9.
            coder = None;
            while passes_left == 0 {
                let Some(segment) = segments.get(segment_index) else {
                    break;
                };
                segment_index += 1;
                passes_left = segment.passes;
                if passes_left > 0 {
                    coder = Some(if raw {
                        Coder::Raw(RawBits::new(&segment.bytes))
                    } else {
                        Coder::Mq(MqDecoder::new(&segment.bytes))
                    });
                }
            }
        }
        let Some(active) = coder.as_mut() else {
            corrupt = true;
            break;
        };
        if matches!(active, Coder::Raw(_)) != raw {
            // Every arithmetic<->raw switch coincides with a termination
            // in a valid stream (Table D.9); a mid-segment switch means
            // the pass counts and termination flags disagree — corrupt.
            corrupt = true;
            break;
        }
        passes_left -= 1;
        if reset {
            // Table A.19 bit 1: re-initialize the context probabilities
            // on every coding pass boundary, to the Table D.7 states.
            contexts = initial_contexts();
        }
        let Some(weight) = plane_bit_weight(mb, input.missing_msbs, plane) else {
            corrupt = true;
            break;
        };
        // Nb(u, v) after this plane counts the signalled all-zero planes
        // too (D.2.1); the u8 seam saturates on hostile counts.
        let planes = (u64::from(input.missing_msbs) + u64::from(plane) + 1).min(255) as u8;
        let clean = match kind {
            PassKind::SignificancePropagation => significance_pass(
                &mut state,
                active,
                &mut contexts,
                input.band,
                weight,
                planes,
            ),
            PassKind::MagnitudeRefinement => {
                refinement_pass(&mut state, active, &mut contexts, weight, planes)
            }
            PassKind::Cleanup => cleanup_pass(
                &mut state,
                active,
                &mut contexts,
                input.band,
                weight,
                planes,
                segmentation,
            ),
        };
        if !clean {
            // Corruption ends the block here; everything decoded so far
            // stays (the seam contract's leniency doctrine).
            corrupt = true;
            break;
        }
    }

    Ok(CodeBlockCoefficients {
        rect: input.rect,
        magnitudes: state.magnitudes,
        negative: state.negative,
        decoded_planes: state.decoded_planes,
        corrupt,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::Rect;

    /// Builds a one-band code-block input over a `width x height` LL rect
    /// anchored away from the origin (absolute coordinates must not
    /// matter for scanning, which is block-relative per D.1).
    fn block_input(
        width: u32,
        height: u32,
        magnitude_bits: u8,
        missing_msbs: u32,
        style: u8,
        segments: Vec<CodeBlockSegment>,
    ) -> CodeBlockInput {
        CodeBlockInput {
            rect: Rect {
                x0: 3,
                y0: 5,
                x1: 3 + width,
                y1: 5 + height,
            },
            band: BandKind::Ll,
            missing_msbs,
            magnitude_bits,
            style,
            segments,
        }
    }

    /// Finds, by exhaustive search over 1..=max_len byte streams, input
    /// bytes that make OUR MQ decoder (the only oracle this cleanroom
    /// has) produce a wanted decision sequence. `sim` gets a fresh
    /// decoder over the candidate plus a copy of `start` contexts and
    /// returns whether every wanted decision matched.
    fn search_bytes(
        min_len: usize,
        max_len: usize,
        start: &[MqContext; CONTEXT_COUNT],
        sim: impl Fn(&mut MqDecoder<'_>, &mut [MqContext; CONTEXT_COUNT]) -> bool,
    ) -> Vec<u8> {
        for len in min_len..=max_len {
            let mut bytes = vec![0u8; len];
            loop {
                let mut contexts = *start;
                let mut decoder = MqDecoder::new(&bytes);
                if sim(&mut decoder, &mut contexts) {
                    return bytes;
                }
                // Odometer increment; a wrap back to all-zero means the
                // whole length was searched.
                let mut k = len;
                loop {
                    if k == 0 {
                        break;
                    }
                    k -= 1;
                    if bytes[k] == 255 {
                        bytes[k] = 0;
                    } else {
                        bytes[k] += 1;
                        break;
                    }
                }
                if bytes.iter().all(|&b| b == 0) {
                    break;
                }
            }
        }
        panic!("no byte stream produces the wanted decision sequence");
    }

    /// Table D.1, LL and LH columns (rendered from the spec page): rows
    /// keyed on (sum H, sum V, sum D) with the don't-care columns pushed
    /// to both extremes. Hand transcription:
    ///   (2,x,x)->8  (1,>=1,x)->7  (1,0,>=1)->6  (1,0,0)->5  (0,2,x)->4
    ///   (0,1,x)->3  (0,0,>=2)->2  (0,0,1)->1    (0,0,0)->0
    #[test]
    fn table_d1_ll_and_lh_columns() {
        let rows = [
            (2, 0, 0, 8),
            (2, 2, 4, 8),
            (1, 1, 0, 7),
            (1, 2, 4, 7),
            (1, 0, 1, 6),
            (1, 0, 4, 6),
            (1, 0, 0, 5),
            (0, 2, 0, 4),
            (0, 2, 4, 4),
            (0, 1, 0, 3),
            (0, 1, 4, 3),
            (0, 0, 2, 2),
            (0, 0, 4, 2),
            (0, 0, 1, 1),
            (0, 0, 0, 0),
        ];
        for (sh, sv, sd, want) in rows {
            assert_eq!(
                zero_coding_context(BandKind::Ll, sh, sv, sd),
                want,
                "LL row ({sh},{sv},{sd})"
            );
        }
        // The table's first column serves "LL and LH sub-bands" alike.
        for sh in 0..=2 {
            for sv in 0..=2 {
                for sd in 0..=4 {
                    assert_eq!(
                        zero_coding_context(BandKind::Ll, sh, sv, sd),
                        zero_coding_context(BandKind::Lh, sh, sv, sd),
                        "LH must share the LL column at ({sh},{sv},{sd})"
                    );
                }
            }
        }
    }

    /// Table D.1, HL column: the same rows with the H and V roles
    /// exchanged (HL is horizontally high-pass). Hand rows:
    ///   (x,2,x)->8  (>=1,1,x)->7  (0,1,>=1)->6  (0,1,0)->5  (2,0,x)->4
    ///   (1,0,x)->3  (0,0,>=2)->2  (0,0,1)->1    (0,0,0)->0
    #[test]
    fn table_d1_hl_column_swaps_h_and_v() {
        let rows = [
            (0, 2, 0, 8),
            (2, 2, 4, 8),
            (1, 1, 0, 7),
            (2, 1, 4, 7),
            (0, 1, 1, 6),
            (0, 1, 4, 6),
            (0, 1, 0, 5),
            (2, 0, 0, 4),
            (2, 0, 4, 4),
            (1, 0, 0, 3),
            (1, 0, 4, 3),
            (0, 0, 2, 2),
            (0, 0, 1, 1),
            (0, 0, 0, 0),
        ];
        for (sh, sv, sd, want) in rows {
            assert_eq!(
                zero_coding_context(BandKind::Hl, sh, sv, sd),
                want,
                "HL row ({sh},{sv},{sd})"
            );
        }
        for sh in 0..=2 {
            for sv in 0..=2 {
                for sd in 0..=4 {
                    assert_eq!(
                        zero_coding_context(BandKind::Hl, sh, sv, sd),
                        zero_coding_context(BandKind::Ll, sv, sh, sd),
                        "HL must be LL with H and V exchanged at ({sh},{sv},{sd})"
                    );
                }
            }
        }
    }

    /// Table D.1, HH column, keyed on (sum(H+V), sum D). Hand rows:
    ///   (x,>=3)->8  (>=1,2)->7  (0,2)->6  (>=2,1)->5  (1,1)->4
    ///   (0,1)->3    (>=2,0)->2  (1,0)->1  (0,0)->0
    #[test]
    fn table_d1_hh_column() {
        let rows = [
            (0, 0, 3, 8),
            (2, 2, 4, 8),
            (1, 0, 2, 7),
            (2, 2, 2, 7),
            (0, 0, 2, 6),
            (1, 1, 1, 5),
            (2, 2, 1, 5),
            (1, 0, 1, 4),
            (0, 1, 1, 4),
            (0, 0, 1, 3),
            (1, 1, 0, 2),
            (2, 2, 0, 2),
            (1, 0, 0, 1),
            (0, 1, 0, 1),
            (0, 0, 0, 0),
        ];
        for (sh, sv, sd, want) in rows {
            assert_eq!(
                zero_coding_context(BandKind::Hh, sh, sv, sd),
                want,
                "HH row ({sh},{sv},{sd})"
            );
        }
    }

    /// Tables D.2 and D.3. The D.2 contribution of a neighbor pair (each
    /// +1 significant positive, -1 significant negative, 0 insignificant)
    /// is the sign of their sum: same signs reinforce, opposite signs
    /// cancel, a lone significant neighbor decides. D.3 then maps
    /// (H, V) -> (label, XORbit):
    ///   (1,1)->(13,0) (1,0)->(12,0) (1,-1)->(11,0) (0,1)->(10,0)
    ///   (0,0)->(9,0)  (0,-1)->(10,1) (-1,1)->(11,1) (-1,0)->(12,1)
    ///   (-1,-1)->(13,1)
    #[test]
    fn tables_d2_d3_sign_contexts() {
        // Table D.2 rows, checked through the contribution helper.
        assert_eq!(sign_contribution(1, 1), 1);
        assert_eq!(sign_contribution(-1, 1), 0);
        assert_eq!(sign_contribution(0, 1), 1);
        assert_eq!(sign_contribution(1, -1), 0);
        assert_eq!(sign_contribution(-1, -1), -1);
        assert_eq!(sign_contribution(0, -1), -1);
        assert_eq!(sign_contribution(1, 0), 1);
        assert_eq!(sign_contribution(-1, 0), -1);
        assert_eq!(sign_contribution(0, 0), 0);
        // Table D.3 rows, driven by (h0, h1, v0, v1) neighbor states.
        // H = 1, V = 1: both horizontals positive, lone positive vertical.
        assert_eq!(sign_context(1, 1, 1, 0), (13, 0));
        // H = 1 (lone positive), V = 0 (insignificant).
        assert_eq!(sign_context(1, 0, 0, 0), (12, 0));
        // H = 1, V = -1 (lone negative vertical).
        assert_eq!(sign_context(1, 1, -1, 0), (11, 0));
        // H = 0 (mixed signs cancel!), V = 1.
        assert_eq!(sign_context(1, -1, 0, 1), (10, 0));
        // Everything insignificant.
        assert_eq!(sign_context(0, 0, 0, 0), (9, 0));
        // H = 0 (mixed), V = -1 (both negative): XOR flips.
        assert_eq!(sign_context(1, -1, -1, -1), (10, 1));
        // H = -1, V = 1.
        assert_eq!(sign_context(-1, 0, 1, 1), (11, 1));
        // H = -1 (both negative), V = 0 (mixed).
        assert_eq!(sign_context(-1, -1, 1, -1), (12, 1));
        // H = -1, V = -1.
        assert_eq!(sign_context(-1, -1, -1, 0), (13, 1));
    }

    /// Table D.4: a first refinement bit takes context 15 when any of the
    /// eight neighbors is significant and 14 otherwise; every later
    /// refinement bit takes 16 whatever the neighborhood does.
    #[test]
    fn table_d4_refinement_contexts() {
        assert_eq!(refinement_context(0, true), 14);
        assert_eq!(refinement_context(1, true), 15);
        assert_eq!(refinement_context(8, true), 15);
        assert_eq!(refinement_context(0, false), 16);
        assert_eq!(refinement_context(3, false), 16);
    }

    /// Table D.7 initial states: label 0 (all-zero neighborhood) at index
    /// 4, run-length at 3, UNIFORM at 46, all nineteen with MPS 0 and
    /// everything else at index 0.
    #[test]
    fn table_d7_initial_states() {
        let contexts = initial_contexts();
        assert_eq!(contexts.len(), 19);
        for (label, cx) in contexts.iter().enumerate() {
            let want = match label {
                0 => 4,
                CTX_RUN_LENGTH => 3,
                CTX_UNIFORM => 46,
                _ => 0,
            };
            assert_eq!(cx.index, want, "context {label}");
            assert_eq!(cx.mps, 0, "context {label} MPS");
        }
    }

    /// D.1 / Figure D.1 scan for a 6 wide x 5 high block: stripe one is
    /// rows 0..4 walked column by column (four rows per column), the
    /// ragged stripe two is the single row 4. Hand-listed.
    #[test]
    fn scan_covers_stripes_column_by_column() {
        let got: Vec<(usize, usize)> = scan_positions(6, 5).collect();
        let want = vec![
            (0, 0),
            (0, 1),
            (0, 2),
            (0, 3),
            (1, 0),
            (1, 1),
            (1, 2),
            (1, 3),
            (2, 0),
            (2, 1),
            (2, 2),
            (2, 3),
            (3, 0),
            (3, 1),
            (3, 2),
            (3, 3),
            (4, 0),
            (4, 1),
            (4, 2),
            (4, 3),
            (5, 0),
            (5, 1),
            (5, 2),
            (5, 3),
            (0, 4),
            (1, 4),
            (2, 4),
            (3, 4),
            (4, 4),
            (5, 4),
        ];
        assert_eq!(got, want);
    }

    /// D.3 scheduling: pass 0 is the lone cleanup of the first non-zero
    /// bit-plane; every later plane runs significance propagation,
    /// magnitude refinement, cleanup (Table D.8 order). Hand-derived for
    /// the first ten passes; pass 10 must be the fifth plane's
    /// significance propagation — exactly where Table D.9 starts raw
    /// coding.
    #[test]
    fn pass_schedule_first_plane_cleanup_only() {
        use PassKind::{Cleanup, MagnitudeRefinement, SignificancePropagation};
        let got: Vec<(u32, PassKind)> = (0..10).map(pass_schedule).collect();
        let want = vec![
            (0, Cleanup),
            (1, SignificancePropagation),
            (1, MagnitudeRefinement),
            (1, Cleanup),
            (2, SignificancePropagation),
            (2, MagnitudeRefinement),
            (2, Cleanup),
            (3, SignificancePropagation),
            (3, MagnitudeRefinement),
            (3, Cleanup),
        ];
        assert_eq!(got, want);
        assert_eq!(pass_schedule(10), (4, SignificancePropagation));
        assert_eq!(FIRST_BYPASS_PASS, 10);
    }

    /// Bit weights per Equation (E-1): MSB_i sits at bit Mb - i, and the
    /// first decoded plane after `missing` all-zero planes is
    /// MSB_(missing+1). With Mb = 8 and missing = 2 the six decodable
    /// planes weigh 8-3=5 down to 8-8=0, and plane 6 does not exist.
    #[test]
    fn plane_bit_weights_follow_e1() {
        let got: Vec<Option<u32>> = (0..7).map(|p| plane_bit_weight(8, 2, p)).collect();
        assert_eq!(
            got,
            vec![Some(5), Some(4), Some(3), Some(2), Some(1), Some(0), None]
        );
        // A block with no missing planes and a single magnitude bit.
        assert_eq!(plane_bit_weight(1, 0, 0), Some(0));
        // All planes signalled away: nothing left to decode.
        assert_eq!(plane_bit_weight(1, 1, 0), None);
        // Hostile missing count beyond Mb must not underflow.
        assert_eq!(plane_bit_weight(4, 4000000000, 0), None);
    }

    /// D.6 raw bit reading with unstuffing, hand-assembled:
    ///   179 = 10110011 -> eight bits as-is
    ///   255 = 11111111 -> eight bits as-is
    ///    80 = 01010000 -> follows 0xFF: drop the stuffed MSB, 7 bits 1010000
    ///   255 = 11111111 -> follows 80: eight bits as-is
    ///   255 = 11111111 -> follows 0xFF: drop MSB, seven 1s
    ///    37 = 00100101 -> follows 0xFF: drop MSB, 0100101
    /// = 45 bits, then the segment is dry and reads yield None.
    #[test]
    fn raw_bits_unstuff_after_ff() {
        let data = [179u8, 255, 80, 255, 255, 37];
        let mut reader = RawBits::new(&data);
        let want = concat!("10110011", "11111111", "1010000", "11111111", "1111111", "0100101");
        let got: String = (0..45)
            .map(|_| match reader.next_bit() {
                Some(0) => '0',
                Some(1) => '1',
                got => panic!("raw bit was {got:?}"),
            })
            .collect();
        assert_eq!(got, want);
        assert_eq!(reader.next_bit(), None, "the 46th bit must not exist");
        assert_eq!(RawBits::new(&[]).next_bit(), None);
    }

    /// D.7 vertically causal formation: a 1 x 5 block puts row 4 in the
    /// second stripe, so from row 3 the significant neighbor below is
    /// hidden (label 0 instead of the LL (0,1,x) -> 3 row); without the
    /// style the same neighborhood reads sum V = 1.
    #[test]
    fn vertically_causal_hides_the_next_stripe() {
        let mut causal = BlockState::new(1, 5, true);
        let low = causal.at(0, 4);
        causal.significant[low] = true;
        assert_eq!(causal.zero_coding_label(BandKind::Ll, 0, 3), 0);

        let mut open = BlockState::new(1, 5, false);
        open.significant[low] = true;
        assert_eq!(open.zero_coding_label(BandKind::Ll, 0, 3), 3);
    }

    /// End to end on a 1 x 1 LL block, one cleanup pass, Mb = 1: the lone
    /// coefficient has no in-block neighbors, so cleanup decodes its
    /// significance with label 0 (initial index 4 per Table D.7; height 1
    /// < 4 rules run-length mode out) and, on a 1, its sign with the
    /// all-insignificant Table D.3 row — label 9, XORbit 0, so D = 1
    /// means negative. Wanted decisions: 1 on label 0, then 1 on label 9.
    /// The magnitude bit is MSB_1 at weight Mb - 1 = 0 and Nb becomes 1.
    #[test]
    fn one_sample_block_decodes_significance_and_sign() {
        let bytes = search_bytes(2, 3, &initial_contexts(), |decoder, contexts| {
            decoder.decode(&mut contexts[0]) == 1 && decoder.decode(&mut contexts[9]) == 1
        });
        let input = block_input(
            1,
            1,
            1,
            0,
            0,
            vec![CodeBlockSegment {
                start: 0,
                len: bytes.len(),
                passes: 1,
                terminated: true,
            }],
        );
        let block = decode_code_block(&input, &bytes).expect("clean block");
        assert_eq!(block.rect, input.rect);
        assert_eq!(block.magnitudes, vec![1]);
        assert_eq!(block.negative, vec![true]);
        assert_eq!(block.decoded_planes, vec![1]);
        assert!(!block.corrupt);

        // The same bytes split across an unterminated contribution and a
        // terminated one concatenate back into one codeword segment
        // (B.10.7) and must decode identically.
        let split = block_input(
            1,
            1,
            1,
            0,
            0,
            vec![
                CodeBlockSegment {
                    start: 0,
                    len: 1,
                    passes: 1,
                    terminated: false,
                },
                CodeBlockSegment {
                    start: 1,
                    len: bytes.len() - 1,
                    passes: 0,
                    terminated: true,
                },
            ],
        );
        let again = decode_code_block(&split, &bytes).expect("clean block");
        assert_eq!(again.magnitudes, vec![1]);
        assert_eq!(again.negative, vec![true]);
        assert_eq!(again.decoded_planes, vec![1]);
    }

    /// D.3.4 run-length mode on a 1 x 4 column, one cleanup pass, Mb = 1.
    /// All four coefficients start with the all-zero context, so the
    /// wanted decisions are: 1 on the run-length context (label 17,
    /// initial index 3), then UNIFORM bits 1,0 — MSB first, so the run
    /// interrupts at coefficient 2 — then that coefficient's sign on
    /// label 9 (insignificant neighbors) decoding 0 = positive, and
    /// finally row 3 rejoins D.3.1 decoding: its neighborhood now has
    /// sum V = 1 (the LL (0,1,x) -> 3 row), wanting bit 0.
    #[test]
    fn cleanup_run_length_interrupts_at_the_decoded_position() {
        let bytes = search_bytes(2, 3, &initial_contexts(), |decoder, contexts| {
            decoder.decode(&mut contexts[CTX_RUN_LENGTH]) == 1
                && decoder.decode(&mut contexts[CTX_UNIFORM]) == 1
                && decoder.decode(&mut contexts[CTX_UNIFORM]) == 0
                && decoder.decode(&mut contexts[9]) == 0
                && decoder.decode(&mut contexts[3]) == 0
        });
        let input = block_input(
            1,
            4,
            1,
            0,
            0,
            vec![CodeBlockSegment {
                start: 0,
                len: bytes.len(),
                passes: 1,
                terminated: true,
            }],
        );
        let block = decode_code_block(&input, &bytes).expect("clean block");
        assert_eq!(block.magnitudes, vec![0, 0, 1, 0]);
        assert_eq!(block.negative, vec![false; 4]);
        assert_eq!(block.decoded_planes, vec![1; 4]);
    }

    /// D.5 segmentation symbol, accepting path: a 1 x 1 block with Mb = 2
    /// and four passes. Plane 0's cleanup decodes 0 on label 0 and then
    /// the symbol 1010 with UNIFORM; plane 1's significance propagation
    /// and refinement decode nothing on a lone insignificant coefficient,
    /// and its cleanup then decodes the plane-1 bit — so Nb must reach 2,
    /// proving the correct symbol let decoding continue.
    #[test]
    fn segmentation_symbol_1010_lets_decoding_continue() {
        let bytes = search_bytes(2, 3, &initial_contexts(), |decoder, contexts| {
            decoder.decode(&mut contexts[0]) == 0
                && decoder.decode(&mut contexts[CTX_UNIFORM]) == 1
                && decoder.decode(&mut contexts[CTX_UNIFORM]) == 0
                && decoder.decode(&mut contexts[CTX_UNIFORM]) == 1
                && decoder.decode(&mut contexts[CTX_UNIFORM]) == 0
        });
        let input = block_input(
            1,
            1,
            2,
            0,
            STYLE_SEGSYM,
            vec![CodeBlockSegment {
                start: 0,
                len: bytes.len(),
                passes: 4,
                terminated: true,
            }],
        );
        let block = decode_code_block(&input, &bytes).expect("clean block");
        assert_eq!(block.decoded_planes, vec![2]);
    }

    /// D.5 segmentation symbol, corruption path: the first symbol bit
    /// decodes 0 where 1010 demands a 1, so decoding stops after plane
    /// 0's cleanup — the recorded plane count stays 1 and the remaining
    /// three passes never run. The block still comes back Ok: entropy
    /// corruption keeps what was decoded (the seam contract).
    #[test]
    fn segmentation_symbol_mismatch_stops_the_block() {
        let bytes = search_bytes(1, 3, &initial_contexts(), |decoder, contexts| {
            decoder.decode(&mut contexts[0]) == 0 && decoder.decode(&mut contexts[CTX_UNIFORM]) == 0
        });
        let input = block_input(
            1,
            1,
            2,
            0,
            STYLE_SEGSYM,
            vec![CodeBlockSegment {
                start: 0,
                len: bytes.len(),
                passes: 4,
                terminated: true,
            }],
        );
        let block = decode_code_block(&input, &bytes).expect("kept partial block");
        assert_eq!(block.magnitudes, vec![0]);
        assert_eq!(block.decoded_planes, vec![1]);
        assert!(block.corrupt, "the mismatch must flag the block corrupt");
    }

    /// D.6 selective bypass plumbing on a 1 x 1 block with Mb = 5 and 13
    /// passes (the full 3*5-2 budget). Table D.9: passes 0..=9 decode
    /// arithmetic and terminate at the fourth cleanup; the fifth plane's
    /// significance propagation and refinement (passes 10, 11) come raw
    /// in their own terminated segment — a lone insignificant coefficient
    /// gives them zero decisions, so that segment is empty — and pass 12
    /// is a fresh arithmetic cleanup. Wanted decisions: label 0 decodes
    /// 0 for planes 0..=3 (their cleanups; nothing else fires on a 1 x 1
    /// insignificant sample), then the plane-4 cleanup decodes 1 plus a
    /// negative sign on label 9. The magnitude bit is MSB_5 at weight
    /// 5 - 5 = 0 and Nb reaches 5.
    #[test]
    fn bypass_segments_switch_coders_at_terminations() {
        let head = search_bytes(2, 3, &initial_contexts(), |decoder, contexts| {
            (0..4).all(|_| decoder.decode(&mut contexts[0]) == 0)
        });
        // Replay the head to evolve the contexts the tail search starts
        // from — the tail's fresh MqDecoder shares the block's contexts.
        let mut evolved = initial_contexts();
        {
            let mut decoder = MqDecoder::new(&head);
            for _ in 0..4 {
                decoder.decode(&mut evolved[0]);
            }
        }
        let tail = search_bytes(2, 3, &evolved, |decoder, contexts| {
            decoder.decode(&mut contexts[0]) == 1 && decoder.decode(&mut contexts[9]) == 1
        });
        let mut bitstream = head.clone();
        bitstream.extend_from_slice(&tail);
        let input = block_input(
            1,
            1,
            5,
            0,
            STYLE_BYPASS,
            vec![
                CodeBlockSegment {
                    start: 0,
                    len: head.len(),
                    passes: 10,
                    terminated: true,
                },
                CodeBlockSegment {
                    start: head.len(),
                    len: 0,
                    passes: 2,
                    terminated: true,
                },
                CodeBlockSegment {
                    start: head.len(),
                    len: tail.len(),
                    passes: 1,
                    terminated: true,
                },
            ],
        );
        let block = decode_code_block(&input, &bitstream).expect("clean block");
        assert_eq!(block.magnitudes, vec![1]);
        assert_eq!(block.negative, vec![true]);
        assert_eq!(block.decoded_planes, vec![5]);
        assert!(!block.corrupt);
    }

    /// A block that never contributed to any packet decodes to all-zero
    /// coefficients (the seam contract).
    #[test]
    fn no_segments_decode_all_zero() {
        let input = block_input(3, 2, 6, 0, 0, Vec::new());
        let block = decode_code_block(&input, &[]).expect("empty block");
        assert_eq!(block.rect, input.rect);
        assert_eq!(block.magnitudes, vec![0; 6]);
        assert_eq!(block.negative, vec![false; 6]);
        assert_eq!(block.decoded_planes, vec![0; 6]);
        assert!(!block.corrupt, "an absent block is normal, not corrupt");
    }

    /// Table A.18 bounds code-blocks to 2^12 samples (xcb + ycb <= 12); a
    /// 70 x 70 rect (4900 samples) is structurally malformed, not
    /// something to allocate for.
    #[test]
    fn oversized_block_is_malformed() {
        let input = block_input(70, 70, 1, 0, 0, Vec::new());
        let got = decode_code_block(&input, &[]);
        assert!(
            matches!(got, Err(JpxError::Malformed(_))),
            "70x70 must be rejected, got {got:?}"
        );
    }

    /// A contribution whose byte range walks off the tile bit stream is
    /// dropped along with everything after it; with nothing decodable the
    /// block stays all zero — and never panics.
    #[test]
    fn out_of_range_segment_keeps_the_block_zero() {
        let input = block_input(
            2,
            2,
            3,
            0,
            0,
            vec![CodeBlockSegment {
                start: usize::MAX - 1,
                len: 5,
                passes: 1,
                terminated: true,
            }],
        );
        let block = decode_code_block(&input, &[1, 2, 3]).expect("kept zero block");
        assert_eq!(block.magnitudes, vec![0; 4]);
        assert_eq!(block.decoded_planes, vec![0; 4]);
        assert!(block.corrupt, "the dropped contribution must be flagged");
    }

    /// More passes than the 3 * (Mb - P) - 2 budget of D.3 can carry mean
    /// the pass counts disagree with the plane budget: the extra passes
    /// never run and the block is flagged corrupt.
    #[test]
    fn passes_beyond_the_plane_budget_flag_corruption() {
        // Mb = 1: budget is exactly one cleanup pass; four are signalled.
        let bytes = search_bytes(1, 3, &initial_contexts(), |decoder, contexts| {
            decoder.decode(&mut contexts[0]) == 0
        });
        let input = block_input(
            1,
            1,
            1,
            0,
            0,
            vec![CodeBlockSegment {
                start: 0,
                len: bytes.len(),
                passes: 4,
                terminated: true,
            }],
        );
        let block = decode_code_block(&input, &bytes).expect("kept the budgeted pass");
        assert_eq!(block.decoded_planes, vec![1]);
        assert!(block.corrupt);
    }
}
