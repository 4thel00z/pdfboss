//! Inverse discrete wavelet transform (ITU-T T.800 Annex F): the IDWT
//! procedure (F.3.1) run level by level and in place over the interleaved
//! tile-component canvas.

use crate::dequant::{CoefficientCanvas, TileComponentCanvas};
use crate::error::{JpxError, Result};
use crate::geometry::{ceil_div, Rect};

// Table F.4 – lifting parameters for the 9-7 irreversible filter. Kept in
// f64 at the spec's full 15-decimal precision; the irreversible path casts
// them once to its f32 working domain.
const ALPHA: f64 = -1.586134342059924;
const BETA: f64 = -0.052980118572961;
const GAMMA: f64 = 0.882911075530934;
const DELTA: f64 = 0.443506852043971;
/// The scaling parameter K of Table F.4 (= 1/t0 of Table F.6).
const KAPPA: f64 = 1.230174104914001;

/// Runs the full inverse DWT in place: for lev = NL down to 1, one 2D_SR
/// sweep (F.3.2) over the sub-grid whose extent is the resolution rect
/// r = NL - lev + 1 of the canvas (Equation (B-14) applied to
/// `canvas.rect`), i.e. HOR_SR then VER_SR (F.3.4/F.3.5) built on 1D_SR
/// (F.3.6) with periodic symmetric extension (F.3.7 1D_EXTR) and the
/// 5-3R / 9-7I lifting filters (F.3.8.1/F.3.8.2).
///
/// Coordinate contract: all filtering parity derives from the ABSOLUTE
/// coordinates of `canvas.rect` (the classic bug is renormalizing odd
/// origins to zero — F.3.3's u0/v0 enter the lifting index math directly).
/// The canvas variant selects the arithmetic: `Reversible` lifts in i32
/// (bit-exact), `Irreversible` in f32. A canvas with `levels == 0` is a
/// no-op (Table A.15: NL = 0 means no transformation).
pub(crate) fn inverse(canvas: &mut TileComponentCanvas) -> Result<()> {
    let extent = canvas.rect;
    let area = u64::from(extent.width()) * u64::from(extent.height());
    let held = match &canvas.samples {
        CoefficientCanvas::Reversible(samples) => samples.len() as u64,
        CoefficientCanvas::Irreversible(samples) => samples.len() as u64,
    };
    if held != area {
        return Err(JpxError::Malformed(format!(
            "tile-component canvas holds {held} samples for a {}x{} extent",
            extent.width(),
            extent.height()
        )));
    }
    if canvas.levels == 0 || extent.is_empty() {
        return Ok(());
    }
    match &mut canvas.samples {
        CoefficientCanvas::Reversible(samples) => run_levels::<i64>(extent, canvas.levels, samples),
        CoefficientCanvas::Irreversible(samples) => {
            run_levels::<f32>(extent, canvas.levels, samples);
        }
    }
    Ok(())
}

/// One lifting arithmetic domain of F.3.8: i64 working values over the
/// reversible i32 canvas (headroom for the F-5/F-6 sums on hostile
/// extremes), f32 over the irreversible canvas.
trait LiftScalar: Copy {
    /// Canvas storage element this domain loads from and stores to.
    type Stored: Copy;
    /// 1D_EXTR margin applied on both sides: the Table F.2/F.3 maxima.
    /// F.3.7 states that extension counts equal to OR GREATER than the
    /// parity-dependent minima produce the same 1D_FILTR output, so one
    /// fixed margin per filter covers both parities.
    const MARGIN: i64;
    fn load(stored: Self::Stored) -> Self;
    fn store(self) -> Self::Stored;
    /// The F.3.6 length-one rule: X(i0) = Y(i0)/2 at odd i0.
    fn halve(self) -> Self;
    /// 1D_FILTR (F.3.8.1/F.3.8.2) over the extended signal: `ext[i - base]`
    /// holds Yext(i); X is written into `x` with the same indexing (it
    /// arrives as a copy of `ext`, and every position a step reads was
    /// written by an earlier step of the same procedure).
    fn filter(ext: &[Self], x: &mut [Self], base: i64, i0: i64, i1: i64);
}

impl LiftScalar for i64 {
    type Stored = i32;
    // Table F.2/F.3, 5-3 column: max(ileft) = 2 (odd i0), max(iright) = 2
    // (even i1).
    const MARGIN: i64 = 2;

    fn load(stored: i32) -> i64 {
        i64::from(stored)
    }

    fn store(self) -> i32 {
        // Genuine codestreams stay well inside i32 (Equation (E-4) bounds
        // Mb); clamp instead of wrapping so hostile extremes cannot panic
        // or alias.
        self.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
    }

    fn halve(self) -> i64 {
        // F.3.6 writes Y(i0)/2; reversible data is always even here (the
        // analysis side doubled it, F.4.6), floor keeps odd hostile values
        // total.
        self.div_euclid(2)
    }

    fn filter(ext: &[i64], x: &mut [i64], base: i64, i0: i64, i1: i64) {
        let at = |i: i64| (i - base) as usize;
        let nlo = i0.div_euclid(2);
        let nhi = i1.div_euclid(2);
        // (F-5): X(2n) = Yext(2n) - floor((Yext(2n-1) + Yext(2n+1) + 2)/4)
        // for floor(i0/2) <= n < floor(i1/2) + 1.
        for n in nlo..=nhi {
            let k = 2 * n;
            x[at(k)] = ext[at(k)] - (ext[at(k - 1)] + ext[at(k + 1)] + 2).div_euclid(4);
        }
        // (F-6): X(2n+1) = Yext(2n+1) + floor((X(2n) + X(2n+2))/2) for
        // floor(i0/2) <= n < floor(i1/2), using the (F-5) outputs.
        for n in nlo..nhi {
            let k = 2 * n + 1;
            x[at(k)] = ext[at(k)] + (x[at(k - 1)] + x[at(k + 1)]).div_euclid(2);
        }
    }
}

impl LiftScalar for f32 {
    type Stored = f32;
    // Table F.2/F.3, 9-7 column: max(ileft) = 4 (odd i0), max(iright) = 4
    // (even i1).
    const MARGIN: i64 = 4;

    fn load(stored: f32) -> f32 {
        stored
    }

    fn store(self) -> f32 {
        self
    }

    fn halve(self) -> f32 {
        self / 2.0
    }

    fn filter(ext: &[f32], x: &mut [f32], base: i64, i0: i64, i1: i64) {
        let at = |i: i64| (i - base) as usize;
        let nlo = i0.div_euclid(2);
        let nhi = i1.div_euclid(2);
        let scale_even = KAPPA as f32;
        let scale_odd = (1.0 / KAPPA) as f32;
        let delta = DELTA as f32;
        let gamma = GAMMA as f32;
        let beta = BETA as f32;
        let alpha = ALPHA as f32;
        // (F-7) STEP1: X(2n) = K*Yext(2n) for floor(i0/2) - 1 <= n <
        // floor(i1/2) + 2.
        for n in (nlo - 1)..(nhi + 2) {
            let k = 2 * n;
            x[at(k)] = scale_even * ext[at(k)];
        }
        // STEP2: X(2n+1) = (1/K)*Yext(2n+1) for floor(i0/2) - 2 <= n <
        // floor(i1/2) + 2.
        for n in (nlo - 2)..(nhi + 2) {
            let k = 2 * n + 1;
            x[at(k)] = scale_odd * ext[at(k)];
        }
        // STEP3: X(2n) -= delta*(X(2n-1) + X(2n+1)) for floor(i0/2) - 1 <=
        // n < floor(i1/2) + 2.
        for n in (nlo - 1)..(nhi + 2) {
            let k = 2 * n;
            x[at(k)] -= delta * (x[at(k - 1)] + x[at(k + 1)]);
        }
        // STEP4: X(2n+1) -= gamma*(X(2n) + X(2n+2)) for floor(i0/2) - 1 <=
        // n < floor(i1/2) + 1.
        for n in (nlo - 1)..(nhi + 1) {
            let k = 2 * n + 1;
            x[at(k)] -= gamma * (x[at(k - 1)] + x[at(k + 1)]);
        }
        // STEP5: X(2n) -= beta*(X(2n-1) + X(2n+1)) for floor(i0/2) <= n <
        // floor(i1/2) + 1.
        for n in nlo..(nhi + 1) {
            let k = 2 * n;
            x[at(k)] -= beta * (x[at(k - 1)] + x[at(k + 1)]);
        }
        // STEP6: X(2n+1) -= alpha*(X(2n) + X(2n+2)) for floor(i0/2) <= n <
        // floor(i1/2).
        for n in nlo..nhi {
            let k = 2 * n + 1;
            x[at(k)] -= alpha * (x[at(k - 1)] + x[at(k + 1)]);
        }
    }
}

/// Equation (F-4): the periodic symmetric extension origin,
/// PSEO(i, i0, i1) = i0 + min(mod(i - i0, 2(i1-i0-1)),
/// 2(i1-i0-1) - mod(i - i0, 2(i1-i0-1))) — mod taken non-negative. Only
/// called for signals of length >= 2, so the period is >= 2.
fn pseo(i: i64, i0: i64, i1: i64) -> i64 {
    let period = 2 * (i1 - i0 - 1);
    let m = (i - i0).rem_euclid(period);
    i0 + m.min(period - m)
}

/// 1D_SR (F.3.6) over one gathered lane whose ABSOLUTE interval is
/// [i0, i1): the length-one parity rule, else 1D_EXTR + 1D_FILTR. `ext`
/// and `x` are caller-owned scratch reused across lanes.
fn sr_lane<S: LiftScalar>(lane: &mut [S], i0: i64, i1: i64, ext: &mut Vec<S>, x: &mut Vec<S>) {
    if i1 - i0 == 1 {
        // F.3.6: "sets the value of X(i0) to Y(i0) if i0 is an even
        // integer, and to X(i0) = Y(i0)/2 if i0 is an odd integer".
        if i0.rem_euclid(2) == 1 {
            lane[0] = lane[0].halve();
        }
        return;
    }
    let base = i0 - S::MARGIN;
    ext.clear();
    ext.extend((base..i1 + S::MARGIN).map(|i| lane[(pseo(i, i0, i1) - i0) as usize]));
    x.clear();
    x.extend_from_slice(ext);
    S::filter(ext, x, base, i0, i1);
    let offset = S::MARGIN as usize;
    for (k, slot) in lane.iter_mut().enumerate() {
        *slot = x[offset + k];
    }
}

/// The IDWT loop of F.3.1 over one canvas: lev = NL down to 1, each
/// iteration a 2D_SR sweep (F.3.2) whose extent is the (lev-1)LL rect —
/// Equation (B-15) corners, i.e. `canvas.rect` ceil-divided by 2^(lev-1) —
/// and whose element (u, v) lives interleaved at canvas coordinate
/// (u*2^(lev-1), v*2^(lev-1)) (F.3.3 applied recursively).
fn run_levels<S: LiftScalar>(extent: Rect, levels: u8, data: &mut [S::Stored]) {
    let width = u64::from(extent.width());
    let x0 = u64::from(extent.x0);
    let y0 = u64::from(extent.y0);
    let longest = extent.width().max(extent.height()) as usize;
    let mut lane: Vec<S> = Vec::with_capacity(longest);
    let mut ext: Vec<S> = Vec::with_capacity(longest + 2 * S::MARGIN as usize);
    let mut x: Vec<S> = Vec::with_capacity(longest + 2 * S::MARGIN as usize);
    for lev in (1..=levels).rev() {
        // 2^(lev-1), exponent clamped: coordinates are u32, so every
        // denominator beyond 2^32 yields the same ceil-quotients (0 or 1).
        let stride = 1u64 << u32::from(lev - 1).min(32);
        let u0 = ceil_div(x0, stride);
        let u1 = ceil_div(u64::from(extent.x1), stride);
        let v0 = ceil_div(y0, stride);
        let v1 = ceil_div(u64::from(extent.y1), stride);
        if u0 >= u1 || v0 >= v1 {
            // The (lev-1)LL rect holds no samples at this depth; both
            // F.3.4/F.3.5 loops would be empty.
            continue;
        }
        let pos = |u: u64, v: u64| ((v * stride - y0) * width + (u * stride - x0)) as usize;
        // HOR_SR (F.3.4): 1D_SR over every row v, i0 = u0, i1 = u1.
        for v in v0..v1 {
            lane.clear();
            lane.extend((u0..u1).map(|u| S::load(data[pos(u, v)])));
            sr_lane(&mut lane, u0 as i64, u1 as i64, &mut ext, &mut x);
            for (u, value) in (u0..u1).zip(&lane) {
                data[pos(u, v)] = value.store();
            }
        }
        // VER_SR (F.3.5): 1D_SR over every column u, i0 = v0, i1 = v1.
        for u in u0..u1 {
            lane.clear();
            lane.extend((v0..v1).map(|v| S::load(data[pos(u, v)])));
            sr_lane(&mut lane, v0 as i64, v1 as i64, &mut ext, &mut x);
            for (v, value) in (v0..v1).zip(&lane) {
                data[pos(u, v)] = value.store();
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::inverse;
    use crate::dequant::{CoefficientCanvas, TileComponentCanvas};
    use crate::error::JpxError;
    use crate::geometry::{ceil_div, Rect};

    // ---- canvas plumbing --------------------------------------------------

    fn rect(x0: u32, y0: u32, x1: u32, y1: u32) -> Rect {
        Rect { x0, y0, x1, y1 }
    }

    fn reversible(extent: Rect, levels: u8, samples: Vec<i32>) -> TileComponentCanvas {
        TileComponentCanvas {
            rect: extent,
            levels,
            samples: CoefficientCanvas::Reversible(samples),
        }
    }

    fn irreversible(extent: Rect, levels: u8, samples: Vec<f32>) -> TileComponentCanvas {
        TileComponentCanvas {
            rect: extent,
            levels,
            samples: CoefficientCanvas::Irreversible(samples),
        }
    }

    fn int_samples(canvas: &TileComponentCanvas) -> &[i32] {
        match &canvas.samples {
            CoefficientCanvas::Reversible(samples) => samples,
            CoefficientCanvas::Irreversible(other) => {
                panic!(
                    "expected a reversible canvas, found {} f32 samples",
                    other.len()
                )
            }
        }
    }

    fn float_samples(canvas: &TileComponentCanvas) -> &[f32] {
        match &canvas.samples {
            CoefficientCanvas::Irreversible(samples) => samples,
            CoefficientCanvas::Reversible(other) => {
                panic!(
                    "expected an irreversible canvas, found {} i32 samples",
                    other.len()
                )
            }
        }
    }

    // ---- no-op and defence cases -------------------------------------------

    #[test]
    fn levels_zero_leaves_the_canvas_untouched() {
        // Table A.15: NL = 0 signals "no transformation" — inverse is a no-op.
        let mut canvas = reversible(rect(3, 5, 7, 7), 0, vec![9, -2, 4, 0, 1, 2, 3, 4]);
        inverse(&mut canvas).unwrap();
        assert_eq!(int_samples(&canvas), &[9, -2, 4, 0, 1, 2, 3, 4]);
    }

    #[test]
    fn empty_rect_is_a_no_op() {
        // B.6 note: empty tile-components exist; they hold no samples and
        // must not panic.
        let mut canvas = reversible(rect(5, 5, 5, 9), 2, Vec::new());
        inverse(&mut canvas).unwrap();
        assert!(int_samples(&canvas).is_empty());
    }

    #[test]
    fn mismatched_sample_count_is_malformed() {
        // Defensive seam check: a canvas whose buffer disagrees with its
        // rect would otherwise index out of bounds.
        let mut canvas = reversible(rect(0, 0, 2, 1), 1, vec![1, 2, 3]);
        assert!(matches!(
            inverse(&mut canvas),
            Err(JpxError::Malformed(detail)) if detail.contains('3')
        ));
    }

    // ---- spec-anchored hand computations ------------------------------------

    #[test]
    fn hand_computed_5_3_row_with_even_origin() {
        // One row at y = 0 (even): every VER_SR column is the F.3.6
        // length-one case with even i0, i.e. identity — this isolates
        // HOR_SR over the absolute interval [2, 7).
        //
        // 1D_SR over Y = [1, 2, 3, 4, 5] at i = 2..=6 (i0 = 2 even, i1 = 7
        // odd). 1D_EXTR (F-3/F-4, period 2*(7-2-1) = 8) reflects about the
        // boundary samples: Yext(1) = Y(3) = 2 and Yext(7) = Y(5) = 4.
        // (F-5), floor(2/2) <= n < floor(7/2) + 1, i.e. n = 1..=3:
        //   X(2) = Y(2) - floor((Yext(1) + Y(3) + 2)/4) = 1 - floor(6/4)  = 0
        //   X(4) = Y(4) - floor((Y(3) + Y(5) + 2)/4)    = 3 - floor(8/4)  = 1
        //   X(6) = Y(6) - floor((Y(5) + Yext(7) + 2)/4) = 5 - floor(10/4) = 3
        // (F-6), floor(2/2) <= n < floor(7/2), i.e. n = 1..=2:
        //   X(3) = Y(3) + floor((X(2) + X(4))/2) = 2 + floor(1/2) = 2
        //   X(5) = Y(5) + floor((X(4) + X(6))/2) = 4 + floor(4/2) = 6
        let mut canvas = reversible(rect(2, 0, 7, 1), 1, vec![1, 2, 3, 4, 5]);
        inverse(&mut canvas).unwrap();
        assert_eq!(int_samples(&canvas), &[0, 2, 1, 6, 3]);
    }

    #[test]
    fn hand_computed_5_3_row_with_odd_origin() {
        // The same five samples but at the odd absolute origin i0 = 3
        // (i1 = 8 even): every lifting index flips parity — the low-pass
        // positions are now 4 and 6. Renormalizing the buffer to a 0-based
        // origin would wrongly reproduce the even-origin answer above.
        //
        // 1D_EXTR (period 2*(8-3-1) = 8): Yext(2) = Y(4) = 2,
        // Yext(1) = Y(5) = 3, Yext(8) = Y(6) = 4, Yext(9) = Y(5) = 3.
        // (F-5), floor(3/2) <= n < floor(8/2) + 1, i.e. n = 1..=4:
        //   X(2) = Yext(2) - floor((Yext(1) + Y(3) + 2)/4)  = 2 - floor(6/4)  = 1
        //   X(4) = Y(4) - floor((Y(3) + Y(5) + 2)/4)        = 2 - floor(6/4)  = 1
        //   X(6) = Y(6) - floor((Y(5) + Y(7) + 2)/4)        = 4 - floor(10/4) = 2
        //   X(8) = Yext(8) - floor((Y(7) + Yext(9) + 2)/4)  = 4 - floor(10/4) = 2
        // (F-6), n = 1..=3:
        //   X(3) = Y(3) + floor((X(2) + X(4))/2) = 1 + 1 = 2
        //   X(5) = Y(5) + floor((X(4) + X(6))/2) = 3 + 1 = 4
        //   X(7) = Y(7) + floor((X(6) + X(8))/2) = 5 + 2 = 7
        // The output is X(3)..X(7).
        let mut canvas = reversible(rect(3, 0, 8, 1), 1, vec![1, 2, 3, 4, 5]);
        inverse(&mut canvas).unwrap();
        assert_eq!(int_samples(&canvas), &[2, 1, 4, 2, 7]);
    }

    #[test]
    fn length_one_lanes_follow_the_parity_rule() {
        // F.3.6: for i0 = i1 - 1 the output is X(i0) = Y(i0) when i0 is
        // even and Y(i0)/2 when i0 is odd. A 1x1 canvas at (1, 1) is halved
        // once by HOR_SR and once by VER_SR: 8 -> 4 -> 2.
        let mut odd = reversible(rect(1, 1, 2, 2), 1, vec![8]);
        inverse(&mut odd).unwrap();
        assert_eq!(int_samples(&odd), &[2]);
        // At (2, 2) both lanes take the even branch: untouched.
        let mut even = reversible(rect(2, 2, 3, 3), 1, vec![7]);
        inverse(&mut even).unwrap();
        assert_eq!(int_samples(&even), &[7]);
        // Same rule on the irreversible path: 3.0 -> 1.5 -> 0.75.
        let mut lossy = irreversible(rect(1, 1, 2, 2), 1, vec![3.0]);
        inverse(&mut lossy).unwrap();
        assert_eq!(float_samples(&lossy), &[0.75]);
    }

    #[test]
    fn hand_computed_two_level_2d_with_odd_origin() {
        // rect x in [3, 7), y in [1, 3), NL = 2, reversible. Raster input:
        //   y=1: [ 1, -2,  3,  0]
        //   y=2: [ 2, 10, -1,  4]
        // Recursive F.3.3 interleave: the lev = 2 array covers
        // u in [ceil(3/2), ceil(7/2)) = [2, 4), v in [ceil(1/2), ceil(3/2))
        // = [1, 2), and its element (u, v) sits at canvas (2u, 2v):
        // a(2,1) = canvas(4,2) = 10 (the 2LL sample), a(3,1) = canvas(6,2)
        // = 4 (2HL).
        //
        // lev = 2 sweep. HOR_SR row v = 1 over [2, 4): i0 = 2 even, i1 = 4
        // even, period 2*(4-2-1) = 2, so Yext(1) = Yext(3) = Yext(5) = Y(3)
        // and Yext(4) = Y(2).
        //   (F-5) X(2) = 10 - floor((4 + 4 + 2)/4) = 8, X(4) = 8
        //   (F-6) X(3) = 4 + floor((8 + 8)/2) = 12
        // VER_SR columns u = 2, 3 over [1, 2): length one at odd v0 = 1,
        // halve (F.3.6): canvas(4,2) = 8/2 = 4, canvas(6,2) = 12/2 = 6.
        //
        // lev = 1 sweep over u in [3, 7), v in [1, 3); the canvas is now
        //   y=1: [ 1, -2,  3,  0]
        //   y=2: [ 2,  4, -1,  6]
        // HOR_SR (i0 = 3 odd, i1 = 7 odd, period 6): Yext(1) = Y(5),
        // Yext(2) = Y(4), Yext(7) = Y(5).
        // Row y=1, Y(3..=6) = [1, -2, 3, 0]:
        //   X(2) = -2 - floor((3 + 1 + 2)/4) = -3
        //   X(4) = -2 - floor((1 + 3 + 2)/4) = -3
        //   X(6) =  0 - floor((3 + 3 + 2)/4) = -2
        //   X(3) =  1 + floor((-3 - 3)/2)    = -2
        //   X(5) =  3 + floor((-3 - 2)/2)    = 3 - 3 = 0   (floor of -2.5!)
        //   row -> [-2, -3, 0, -2]
        // Row y=2, Y(3..=6) = [2, 4, -1, 6]:
        //   X(2) = 4 - floor((-1 + 2 + 2)/4)  = 4
        //   X(4) = 4 - floor((2 - 1 + 2)/4)   = 4
        //   X(6) = 6 - floor((-1 - 1 + 2)/4)  = 6
        //   X(3) = 2 + floor((4 + 4)/2)       = 6
        //   X(5) = -1 + floor((4 + 6)/2)      = 4
        //   row -> [6, 4, 4, 6]
        // VER_SR columns over v in [1, 3): i0 = 1 odd, i1 = 3 odd, period
        // 2: Yext(-1) = Yext(3) = Y(1), Yext(0) = Y(2). With top t = Y(1),
        // bottom b = Y(2):
        //   (F-5) X(0) = b - floor((2t + 2)/4) and X(2) = X(0)
        //   (F-6) X(1) = t + floor((X(0) + X(2))/2) = t + X(0)
        // and the output rows are [X(1), X(2)]:
        //   u=3: t=-2, b=6: X(0) = 6 - floor(-2/4) = 7, X(1) = 5
        //   u=4: t=-3, b=4: X(0) = 4 - floor(-4/4) = 5, X(1) = 2
        //   u=5: t= 0, b=4: X(0) = 4 - floor(2/4)  = 4, X(1) = 4
        //   u=6: t=-2, b=6: X(0) = 7,                  X(1) = 5
        let mut canvas = reversible(rect(3, 1, 7, 3), 2, vec![1, -2, 3, 0, 2, 10, -1, 4]);
        inverse(&mut canvas).unwrap();
        assert_eq!(int_samples(&canvas), &[5, 2, 4, 5, 7, 5, 4, 7]);
    }

    #[test]
    fn constant_9_7_canvas_reconstructs_the_lifted_constants() {
        // One row of ones over [0, 8) x [0, 1). PSEO reflection about a
        // boundary index c maps i to 2c - i, which preserves parity, so
        // every extended even position holds 1 and every odd one holds 1;
        // each F-7 step then yields the same value at every position it
        // touches and the output collapses to a scalar chain over the
        // Table F.4 constants (worked in f64):
        //   STEP1  E1 = K   = 1.230174104914001
        //   STEP2  O2 = 1/K = 0.812893066115961      (= t0 of Table F.6)
        //   STEP3  E3 = E1 - delta*(2*O2)
        //             = 1.230174104914001 - 0.721047289602923
        //             = 0.509126815311078
        //   STEP4  O4 = O2 - gamma*(2*E3)
        //             = 0.812893066115961 - 0.899027408175886
        //             = -0.086134342059925
        //   STEP5  E5 = E3 - beta*(2*O4)
        //             = 0.509126815311078 - 0.009126815311078
        //             = 0.500000000000000
        //   STEP6  O6 = O4 - alpha*(2*E5) = O4 - alpha = 1.500000000000000
        // (O4 lands on alpha + 3/2 to within 1e-15 — the Table F.4
        // approximations are mutually consistent.) VER_SR columns are the
        // even length-one identity.
        let mut canvas = irreversible(rect(0, 0, 8, 1), 1, vec![1.0; 8]);
        inverse(&mut canvas).unwrap();
        for (k, value) in float_samples(&canvas).iter().enumerate() {
            let expected = if k % 2 == 0 { 0.5 } else { 1.5 };
            assert!(
                (value - expected).abs() < 1e-4,
                "sample {k}: {value} vs {expected}"
            );
        }
    }

    // ---- test-side FORWARD transform (F.4, informative) ---------------------
    //
    // The inverse is the normative procedure; the analysis filters below
    // exist only to state the reconstruction invariant of F.2.1. Two spots
    // where the printed (08/2002) F.4 text cannot be followed literally:
    //
    // * (F-9) prints "- floor((Xext(2n) + Xext(2n+2))/4)". With /4 the
    //   normative synthesis step (F-6), which adds floor((X(2n) +
    //   X(2n+2))/2) back, does not cancel it: x = [1, 3, 1] at [0, 3)
    //   would forward-transform to y = [3, 3, 3], which F-5/F-6
    //   reconstruct as [1, 4, 1]. Perfect reconstruction forces the /2
    //   used here.
    // * (F-11)'s printed step-5 range starts at ceil(i0/2), which would
    //   leave Y(i0) unscaled by K for odd i0 even though the normative
    //   F-7 STEP2 (floor ranges) divides every odd coefficient by K. The
    //   lifting windows below are therefore derived as the exact algebraic
    //   inverse of F-7: undo STEP6 first, applying each pass wherever both
    //   operands exist, one sample narrower per pass.

    /// Equation (F-4) PSEO — a private test copy so the analysis side
    /// stands on its own reading of the spec.
    fn pseo(i: i64, i0: i64, i1: i64) -> i64 {
        let period = 2 * (i1 - i0 - 1);
        let m = (i - i0).rem_euclid(period);
        i0 + m.min(period - m)
    }

    fn fwd_lane_53(lane: &mut [i64], i0: i64, i1: i64) {
        if i1 - i0 == 1 {
            // F.4.6: Y(i0) = X(i0) at even i0 and 2*X(i0) at odd i0.
            if i0.rem_euclid(2) == 1 {
                lane[0] *= 2;
            }
            return;
        }
        let base = i0 - 2;
        let ext: Vec<i64> = (base..i1 + 2)
            .map(|i| lane[(pseo(i, i0, i1) - i0) as usize])
            .collect();
        let mut y = ext.clone();
        let at = |i: i64| (i - base) as usize;
        let clo = (i0 + 1).div_euclid(2); // ceil(i0/2)
        let chi = (i1 + 1).div_euclid(2); // ceil(i1/2)
                                          // (F-9) over ceil(i0/2) - 1 <= n < ceil(i1/2), high-pass first.
        for n in (clo - 1)..chi {
            let k = 2 * n + 1;
            y[at(k)] = ext[at(k)] - (ext[at(k - 1)] + ext[at(k + 1)]).div_euclid(2);
        }
        // (F-10) over ceil(i0/2) <= n < ceil(i1/2).
        for n in clo..chi {
            let k = 2 * n;
            y[at(k)] = ext[at(k)] + (y[at(k - 1)] + y[at(k + 1)] + 2).div_euclid(4);
        }
        for (slot, k) in lane.iter_mut().zip(i0..i1) {
            *slot = y[at(k)];
        }
    }

    // Table F.4 lifting parameters (test-side copy).
    const ALPHA: f64 = -1.586134342059924;
    const BETA: f64 = -0.052980118572961;
    const GAMMA: f64 = 0.882911075530934;
    const DELTA: f64 = 0.443506852043971;
    const KAPPA: f64 = 1.230174104914001;

    /// One forward lifting pass: samples of `parity` in [lo, hi) gain
    /// `weight` times the sum of their direct neighbours.
    fn lift_forward(y: &mut [f64], base: i64, lo: i64, hi: i64, parity: i64, weight: f64) {
        let mut k = lo + (parity - lo).rem_euclid(2);
        while k < hi {
            y[(k - base) as usize] +=
                weight * (y[(k - 1 - base) as usize] + y[(k + 1 - base) as usize]);
            k += 2;
        }
    }

    fn fwd_lane_97(lane: &mut [f64], i0: i64, i1: i64) {
        if i1 - i0 == 1 {
            // F.4.6 again: double at odd i0 (the F.3.6 halving undoes it).
            if i0.rem_euclid(2) == 1 {
                lane[0] *= 2.0;
            }
            return;
        }
        let base = i0 - 4;
        let top = i1 + 4;
        let mut y: Vec<f64> = (base..top)
            .map(|i| lane[(pseo(i, i0, i1) - i0) as usize])
            .collect();
        lift_forward(&mut y, base, base + 1, top - 1, 1, ALPHA); // undoes F-7 STEP6
        lift_forward(&mut y, base, base + 2, top - 2, 0, BETA); // undoes STEP5
        lift_forward(&mut y, base, base + 3, top - 3, 1, GAMMA); // undoes STEP4
        lift_forward(&mut y, base, i0, i1, 0, DELTA); // undoes STEP3
        for (slot, k) in lane.iter_mut().zip(i0..i1) {
            let value = y[(k - base) as usize];
            // Undo the STEP1/STEP2 scaling: odd coefficients carry K.
            *slot = if k.rem_euclid(2) == 1 {
                KAPPA * value
            } else {
                value / KAPPA
            };
        }
    }

    /// The sweep bounds the inverse also uses: the lev sweep covers the
    /// (lev - 1)LL rect (Equation (B-15) with the (B-14) denominator
    /// 2^(lev-1)), one canvas stride 2^(lev-1) apart.
    fn level_bounds(extent: Rect, lev: u8) -> (u64, u64, u64, u64, u64) {
        let stride = 1u64 << u32::from(lev - 1).min(32);
        (
            stride,
            ceil_div(u64::from(extent.x0), stride),
            ceil_div(u64::from(extent.x1), stride),
            ceil_div(u64::from(extent.y0), stride),
            ceil_div(u64::from(extent.y1), stride),
        )
    }

    fn forward_53(extent: Rect, levels: u8, data: &mut [i32]) {
        let width = u64::from(extent.x1 - extent.x0);
        let x0 = u64::from(extent.x0);
        let y0 = u64::from(extent.y0);
        for lev in 1..=levels {
            let (stride, u0, u1, v0, v1) = level_bounds(extent, lev);
            if u0 >= u1 || v0 >= v1 {
                continue;
            }
            let pos = |u: u64, v: u64| ((v * stride - y0) * width + (u * stride - x0)) as usize;
            // F.4.2: VER_SD over every column, then HOR_SD over every row.
            for u in u0..u1 {
                let mut lane: Vec<i64> = (v0..v1).map(|v| i64::from(data[pos(u, v)])).collect();
                fwd_lane_53(&mut lane, v0 as i64, v1 as i64);
                for (v, value) in (v0..v1).zip(&lane) {
                    data[pos(u, v)] = *value as i32;
                }
            }
            for v in v0..v1 {
                let mut lane: Vec<i64> = (u0..u1).map(|u| i64::from(data[pos(u, v)])).collect();
                fwd_lane_53(&mut lane, u0 as i64, u1 as i64);
                for (u, value) in (u0..u1).zip(&lane) {
                    data[pos(u, v)] = *value as i32;
                }
            }
        }
    }

    fn forward_97(extent: Rect, levels: u8, data: &mut [f32]) {
        let width = u64::from(extent.x1 - extent.x0);
        let x0 = u64::from(extent.x0);
        let y0 = u64::from(extent.y0);
        for lev in 1..=levels {
            let (stride, u0, u1, v0, v1) = level_bounds(extent, lev);
            if u0 >= u1 || v0 >= v1 {
                continue;
            }
            let pos = |u: u64, v: u64| ((v * stride - y0) * width + (u * stride - x0)) as usize;
            for u in u0..u1 {
                let mut lane: Vec<f64> = (v0..v1).map(|v| f64::from(data[pos(u, v)])).collect();
                fwd_lane_97(&mut lane, v0 as i64, v1 as i64);
                for (v, value) in (v0..v1).zip(&lane) {
                    data[pos(u, v)] = *value as f32;
                }
            }
            for v in v0..v1 {
                let mut lane: Vec<f64> = (u0..u1).map(|u| f64::from(data[pos(u, v)])).collect();
                fwd_lane_97(&mut lane, u0 as i64, u1 as i64);
                for (u, value) in (u0..u1).zip(&lane) {
                    data[pos(u, v)] = *value as f32;
                }
            }
        }
    }

    /// Knuth's MMIX LCG — deterministic test inputs without a rand crate.
    struct Lcg(u64);

    impl Lcg {
        fn step(&mut self) -> u64 {
            self.0 = self
                .0
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            self.0
        }

        /// Uniform-ish integer in [-span, span].
        fn int(&mut self, span: i64) -> i64 {
            let raw = (self.step() >> 33) as i64;
            raw.rem_euclid(2 * span + 1) - span
        }
    }

    // ---- reconstruction invariants ------------------------------------------

    #[test]
    fn forward_then_inverse_5_3_is_bit_exact() {
        // The 5-3 path is reversible (F.2.1/F.2.3): inverse(forward(x))
        // must equal x EXACTLY for every extent, origin parity and level
        // count — the design-doc test invariant.
        let mut seed = 1u64;
        for x0 in [4u32, 5] {
            for y0 in [6u32, 7] {
                for width in 1..17u32 {
                    for height in 1..17u32 {
                        for levels in 1..4u8 {
                            seed += 1;
                            let extent = rect(x0, y0, x0 + width, y0 + height);
                            let mut lcg = Lcg(seed);
                            let original: Vec<i32> =
                                (0..width * height).map(|_| lcg.int(100) as i32).collect();
                            let mut data = original.clone();
                            forward_53(extent, levels, &mut data);
                            let mut canvas = reversible(extent, levels, data);
                            inverse(&mut canvas).unwrap();
                            assert_eq!(
                                int_samples(&canvas),
                                &original[..],
                                "x0={x0} y0={y0} {width}x{height} levels={levels}"
                            );
                        }
                    }
                }
            }
        }
    }

    #[test]
    fn forward_then_inverse_9_7_stays_within_tolerance() {
        // The 9-7 path is irreversible; the float round trip must still
        // land within 1e-4 of the source samples.
        let mut seed = 99u64;
        for x0 in [4u32, 5] {
            for y0 in [6u32, 7] {
                for width in 1..17u32 {
                    for height in 1..17u32 {
                        for levels in 1..4u8 {
                            seed += 1;
                            let extent = rect(x0, y0, x0 + width, y0 + height);
                            let mut lcg = Lcg(seed);
                            let original: Vec<f32> =
                                (0..width * height).map(|_| lcg.int(8) as f32).collect();
                            let mut data = original.clone();
                            forward_97(extent, levels, &mut data);
                            let mut canvas = irreversible(extent, levels, data);
                            inverse(&mut canvas).unwrap();
                            for (got, want) in float_samples(&canvas).iter().zip(&original) {
                                assert!(
                                    (got - want).abs() <= 1e-4,
                                    "x0={x0} y0={y0} {width}x{height} levels={levels}: \
                                     {got} vs {want}"
                                );
                            }
                        }
                    }
                }
            }
        }
    }
}
