//! Dequantization (ITU-T T.800 Annex E) and ROI maxshift undo (H.1/H.2):
//! turns sign-magnitude Tier-1 output into the interleaved coefficient
//! canvas the inverse DWT consumes — the dequant → dwt seam.

use crate::error::{JpxError, Result};
use crate::geometry::{BandKind, Rect, TileComponentGeometry};
use crate::markers::{ComponentCoding, QuantizationStyle, SizComponent, WaveletKind};
use crate::t1::{BandCoefficients, CodeBlockCoefficients};
use crate::DecodeLimits;

/// The coefficient storage of one tile-component canvas.
// Constructed by the dequant stage; the variants are the frozen seam.
#[derive(Debug)]
pub(crate) enum CoefficientCanvas {
    /// 5-3 reversible path (Table A.20 = 1): i32 end to end; bit-exact
    /// reversibility is a test invariant.
    Reversible(Vec<i32>),
    /// 9-7 irreversible path (Table A.20 = 0): converted to f32 here at
    /// dequantization (Equation (E-6) with the r = 1/2 reconstruction
    /// adjustment for truncated code-blocks), lifted in f32.
    Irreversible(Vec<f32>),
}

/// One tile-component's interleaved coefficient canvas — the dequant → dwt
/// seam. Samples live at ABSOLUTE component-grid coordinates: sample
/// `(u, v)` of `rect` sits at index `(v - rect.y0) * rect.width() +
/// (u - rect.x0)`, and all NL levels are pre-interleaved per F.3.3
/// 2D_INTERLEAVE (band sample (u_b, v_b) of level nb lands where the
/// recursive interleave puts it, driven by the Table B.1 offsets).
/// Parity is defined by the absolute coordinates — never renormalize.
// Constructed by the dequant stage; the field list is the frozen seam.
#[derive(Debug)]
pub(crate) struct TileComponentCanvas {
    /// Absolute tile-component extent (Equation (B-12)).
    pub rect: Rect,
    /// Decomposition level count NL the canvas is interleaved for; the
    /// inverse DWT runs exactly this many 2D_SR sweeps (F.3.1).
    pub levels: u8,
    /// Interleaved coefficients, raster order over `rect`.
    pub samples: CoefficientCanvas,
}

/// Dequantizes one tile-component: applies Equation (E-2) `Mb = G +
/// epsilon_b - 1`, the reversible reconstruction (E.1.2, step size 1, with
/// the E-8 adjustment when Nb < Mb) or the irreversible one (E.1.1,
/// Equation (E-3) step from (epsilon_b, mu_b), Table E.1 gains, Equation
/// (E-5) for derived style, Equation (E-6) with r = 1/2), undoes the RGN
/// maxshift (H.1/H.2) when `coding.roi_shift` is set, and scatters every
/// code-block into the interleaved canvas (F.3.3).
///
/// `component` supplies RI (Table A.11 depth) for the Equation (E-4)
/// dynamic range; `limits.max_decoded_bytes` bounds the canvas allocation
/// before it happens.
pub(crate) fn dequantize_tile_component(
    geometry: &TileComponentGeometry,
    coding: &ComponentCoding,
    component: &SizComponent,
    bands: &[BandCoefficients],
    limits: &DecodeLimits,
) -> Result<TileComponentCanvas> {
    // RI (Table A.11) already contains any sign bit, so signedness never
    // enters the (E-4) dynamic range; it matters again at the G.1.2 level
    // shift downstream. Touch the seam field so it stays live until the
    // colour stage lands.
    let _ = component.signed;
    let rect = geometry.rect;
    // The canvas covers the (B-12) tile-component rect at 4 bytes per
    // coefficient (i32 or f32); the byte count is bounded by
    // max_decoded_bytes BEFORE the allocation happens.
    let bytes = (u64::from(rect.width()) * u64::from(rect.height()))
        .checked_mul(4)
        .ok_or_else(|| {
            JpxError::Malformed("tile-component canvas exceeds the address space".into())
        })?;
    if bytes > limits.max_decoded_bytes {
        return Err(JpxError::LimitExceeded {
            what: "max_decoded_bytes",
            actual: bytes,
            limit: limits.max_decoded_bytes,
        });
    }
    let count = usize::try_from(bytes / 4).map_err(|error| {
        JpxError::Malformed(format!(
            "tile-component canvas exceeds the address space: {error}"
        ))
    })?;
    // E.1.2 applies only when the 5-3 filter (Table A.20 value 1) is paired
    // with the no-quantization style (Table A.28): only then is the whole
    // dequant -> DWT pipeline integer-exact. Any other pairing dequantizes
    // per E.1.1 into f32.
    let reversible = coding.style.wavelet == WaveletKind::Reversible53
        && matches!(&coding.quant.style, QuantizationStyle::None { .. });
    let mut samples = if reversible {
        CoefficientCanvas::Reversible(vec![0; count])
    } else {
        CoefficientCanvas::Irreversible(vec![0.0; count])
    };
    for band in bands {
        let params = band_parameters(coding, component, geometry.levels, band)?;
        for block in &band.blocks {
            scatter_block(&mut samples, rect, band, block, &params, coding.roi_shift);
        }
    }
    Ok(TileComponentCanvas {
        rect,
        levels: geometry.levels,
        samples,
    })
}

/// Per-band dequantization parameters: `Mb` per Equation (E-2) and the step
/// size `Delta_b` per Equation (E-3) (or 1 for the E.1.2.1 reversible case).
struct BandParams {
    mb: i32,
    delta: f64,
}

/// Table E.1 sub-band gains as base-2 exponents: gain(levLL) = 1,
/// gain(levHL) = gain(levLH) = 2, gain(levHH) = 4.
fn gain_log2(kind: BandKind) -> i32 {
    match kind {
        BandKind::Ll => 0,
        BandKind::Hl | BandKind::Lh => 1,
        BandKind::Hh => 2,
    }
}

/// Index of a band in the codestream sub-band order of F.3.1 (the order the
/// QCD/QCC step-size lists follow, A.6.4): NLLL, NLHL, NLLH, NLHH,
/// (NL-1)HL, (NL-1)LH, (NL-1)HH, ..., 1HL, 1LH, 1HH.
fn subband_index(kind: BandKind, level: u8, levels: u8) -> Result<usize> {
    let orientation = match kind {
        BandKind::Ll => return Ok(0),
        BandKind::Hl => 1,
        BandKind::Lh => 2,
        BandKind::Hh => 3,
    };
    if level == 0 || level > levels {
        return Err(JpxError::Malformed(
            "sub-band decomposition level outside 1..=NL".into(),
        ));
    }
    Ok(3 * usize::from(levels - level) + orientation)
}

/// Equation (E-3): `Delta_b = 2^(Rb - eps_b) * (1 + mu_b / 2^11)`; the 2^11
/// denominator is the 11-bit mantissa allocation of Table A.30.
fn step_size(rb: i32, eps: i32, mantissa: u16) -> f64 {
    f64::from(rb - eps).exp2() * (1.0 + f64::from(mantissa) / 2048.0)
}

/// Resolves one band's (Mb, Delta_b) from the quantization marker data:
/// expounded steps are listed per band, derived steps follow Equation (E-5)
/// `(eps_b, mu_b) = (eps_0 - NL + nb, mu_0)`, and the no-quantization style
/// lists one reversible-ranging exponent per band with step size 1 (E.1.2.1).
fn band_parameters(
    coding: &ComponentCoding,
    component: &SizComponent,
    levels: u8,
    band: &BandCoefficients,
) -> Result<BandParams> {
    let guard = i32::from(coding.quant.guard_bits);
    // (E-4): Rb = RI + log2(gain_b); RI is the Table A.11 sample precision
    // (sign bit included for signed components).
    let rb = i32::from(component.depth) + gain_log2(band.kind);
    match &coding.quant.style {
        QuantizationStyle::None { exponents } => {
            let index = subband_index(band.kind, band.level, levels)?;
            let eps = exponents.get(index).copied().ok_or_else(|| {
                JpxError::Malformed("QCD/QCC: too few reversible sub-band exponents".into())
            })?;
            Ok(BandParams {
                mb: guard + i32::from(eps) - 1,
                delta: 1.0,
            })
        }
        QuantizationStyle::ScalarDerived { exponent, mantissa } => {
            let eps = i32::from(*exponent) - i32::from(levels) + i32::from(band.level);
            Ok(BandParams {
                mb: guard + eps - 1,
                delta: step_size(rb, eps, *mantissa),
            })
        }
        QuantizationStyle::ScalarExpounded { steps } => {
            let index = subband_index(band.kind, band.level, levels)?;
            let step = steps.get(index).ok_or_else(|| {
                JpxError::Malformed("QCD/QCC: too few expounded sub-band step sizes".into())
            })?;
            let eps = i32::from(step.exponent);
            Ok(BandParams {
                mb: guard + eps - 1,
                delta: step_size(rb, eps, step.mantissa),
            })
        }
    }
}

/// Undoes the Maxshift scaling of one sample per H.1. The encoder up-shifted
/// ROI coefficients by s (H-4), giving M'b = Mb + s coded bit-planes (H-3),
/// so in magnitude-value terms "at least one of the first Mb MSBs is
/// non-zero" (H.1 step 3) is `magnitude >= 2^s`: such an ROI sample keeps
/// its MSB indices, which re-weights its value down by s, and clamps
/// Nb to Mb when Nb >= Mb. A background sample (H.1 step 4, all first Mb
/// MSBs zero) has its MSBs shifted up s places (H-1) - exactly cancelling
/// the re-weighting, so its value is unchanged - and Nb = max(0, Nb - s)
/// (H-2). Samples with Nb < Mb take no modification (H.1 step 2).
fn undo_maxshift(magnitude: u32, planes: u8, shift: u8, mb: i32) -> (u32, u8) {
    // The shift is bounded: u32 magnitudes vanish under shifts >= 32, and
    // clamping the shift amount at 63 keeps the u64 shift defined for any
    // hostile SPrgn up to 255.
    let scaled = u64::from(magnitude) >> u32::from(shift).min(63);
    if scaled != 0 {
        // ROI sample; H.1 step 3 (or step 2 when Nb < Mb: Nb untouched).
        let planes = if i32::from(planes) >= mb {
            mb.clamp(0, 255) as u8
        } else {
            planes
        };
        (scaled as u32, planes)
    } else {
        // Background sample; H.1 step 4 (or the untouched step 2 case).
        let planes = if i32::from(planes) >= mb {
            planes.saturating_sub(shift)
        } else {
            planes
        };
        (magnitude, planes)
    }
}

/// Reversible reconstruction (E.1.2.2): exact when fully decoded (E-7);
/// a truncated non-zero sample gets the (E-8) adjustment with r = 1/2,
/// i.e. `+ 2^(Mb - Nb - 1)`, the midpoint of its uncertainty interval.
fn reconstruct_reversible(magnitude: u32, negative: bool, planes: u8, mb: i32) -> i32 {
    if magnitude == 0 {
        return 0;
    }
    let mut value = i64::from(magnitude);
    let missing = mb - i32::from(planes);
    if missing > 0 {
        // r * 2^(Mb - Nb) with r = 1/2; the clamp keeps hostile Mb values
        // from shifting past the i64 width (the result saturates below).
        value += 1i64 << (missing - 1).min(61);
    }
    if negative {
        value = -value;
    }
    value.clamp(i64::from(i32::MIN), i64::from(i32::MAX)) as i32
}

/// Irreversible reconstruction (E-6) with the r = 1/2 midpoint adjustment
/// for truncated samples: `Rq = (q +- r * 2^(Mb - Nb)) * Delta_b`, 0 for
/// zero magnitudes.
fn reconstruct_irreversible(
    magnitude: u32,
    negative: bool,
    planes: u8,
    mb: i32,
    delta: f64,
) -> f32 {
    if magnitude == 0 {
        return 0.0;
    }
    let mut value = f64::from(magnitude);
    let missing = mb - i32::from(planes);
    if missing > 0 {
        value += 0.5 * f64::from(missing).exp2();
    }
    if negative {
        value = -value;
    }
    (value * delta) as f32
}

/// Canvas position of band sample (ub, vb) per the F.3.3 2D_INTERLEAVE
/// recursion, collapsed to a closed form: the first interleave step places
/// a level-nb sample at (2u + xob, 2v + yob) of the (nb-1)LL grid
/// (Figure F.8, offsets per Table B.1), and each remaining LL step doubles,
/// so the absolute canvas coordinate is
/// `(ub * 2^nb + xob * 2^(nb-1), vb * 2^nb + yob * 2^(nb-1))`. The deepest
/// LL band (nb = NL) seeds the recursion with the plain doubling (xob = 0);
/// nb = 0 (no transformation, Table A.15) is the identity. Positions outside
/// the canvas rect yield `None` and are dropped by the caller.
fn canvas_index(rect: Rect, level: u8, kind: BandKind, ub: u32, vb: u32) -> Option<usize> {
    if level > 32 {
        return None;
    }
    let (x, y) = if level == 0 {
        (u64::from(ub), u64::from(vb))
    } else {
        let (xob, yob) = match kind {
            BandKind::Ll => (0u64, 0u64),
            BandKind::Hl => (1, 0),
            BandKind::Lh => (0, 1),
            BandKind::Hh => (1, 1),
        };
        let shift = u32::from(level);
        (
            (u64::from(ub) << shift) + (xob << (shift - 1)),
            (u64::from(vb) << shift) + (yob << (shift - 1)),
        )
    };
    if x < u64::from(rect.x0)
        || x >= u64::from(rect.x1)
        || y < u64::from(rect.y0)
        || y >= u64::from(rect.y1)
    {
        return None;
    }
    let index = (y - u64::from(rect.y0)) * u64::from(rect.width()) + (x - u64::from(rect.x0));
    usize::try_from(index).ok()
}

/// Dequantizes one code-block and scatters it onto the canvas: per sample,
/// the H.1 ROI undo (when signalled), then the E.1.1.2/E.1.2.2
/// reconstruction, then the F.3.3 interleave placement. Zero magnitudes
/// keep the canvas zero fill; samples mapping outside the canvas (hostile
/// or degenerate rects) are dropped rather than trusted.
fn scatter_block(
    canvas: &mut CoefficientCanvas,
    rect: Rect,
    band: &BandCoefficients,
    block: &CodeBlockCoefficients,
    params: &BandParams,
    roi_shift: Option<u8>,
) {
    let width = block.rect.width() as usize;
    let height = block.rect.height() as usize;
    if width == 0 || height == 0 {
        return;
    }
    for (i, &raw) in block.magnitudes.iter().take(width * height).enumerate() {
        if raw == 0 {
            continue;
        }
        let ub = block.rect.x0 + (i % width) as u32;
        let vb = block.rect.y0 + (i / width) as u32;
        let negative = block.negative.get(i).copied().unwrap_or(false);
        let planes = block.decoded_planes.get(i).copied().unwrap_or(0);
        let (magnitude, planes) = match roi_shift {
            Some(shift) => undo_maxshift(raw, planes, shift, params.mb),
            None => (raw, planes),
        };
        let Some(index) = canvas_index(rect, band.level, band.kind, ub, vb) else {
            continue;
        };
        match canvas {
            CoefficientCanvas::Reversible(values) => {
                values[index] = reconstruct_reversible(magnitude, negative, planes, params.mb);
            }
            CoefficientCanvas::Irreversible(values) => {
                values[index] =
                    reconstruct_irreversible(magnitude, negative, planes, params.mb, params.delta);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::geometry::tile_component_geometry;
    use crate::markers::{CodingStyle, QuantStep, Quantization};

    /// A minimal Table A.15 coding style: no precinct list (maximal
    /// precincts), 64 x 64 code-blocks, no style flags.
    fn coding_style(levels: u8, wavelet: WaveletKind) -> CodingStyle {
        CodingStyle {
            decomposition_levels: levels,
            code_block_width_exp: 6,
            code_block_height_exp: 6,
            code_block_style: 0,
            wavelet,
            precincts: Vec::new(),
        }
    }

    /// An unsigned component with RI = `depth` (Table A.11) and no
    /// sub-sampling, so the tile-component rect equals the tile rect (B-12).
    fn component(depth: u8) -> SizComponent {
        SizComponent {
            depth,
            signed: false,
            xrsiz: 1,
            yrsiz: 1,
        }
    }

    fn coding(
        levels: u8,
        wavelet: WaveletKind,
        guard_bits: u8,
        style: QuantizationStyle,
        roi_shift: Option<u8>,
    ) -> ComponentCoding {
        ComponentCoding {
            style: coding_style(levels, wavelet),
            quant: Quantization { guard_bits, style },
            roi_shift,
        }
    }

    fn geometry_for(tile: Rect, levels: u8, wavelet: WaveletKind) -> TileComponentGeometry {
        tile_component_geometry(tile, &component(8), &coding_style(levels, wavelet)).unwrap()
    }

    /// Looks up the Annex B band rect the geometry stage computed.
    fn band_rect_of(geometry: &TileComponentGeometry, kind: BandKind, level: u8) -> Rect {
        for resolution in &geometry.resolutions {
            for band in &resolution.bands {
                if band.kind == kind && band.level == level {
                    return band.rect;
                }
            }
        }
        panic!("band {kind:?} level {level} not in geometry");
    }

    /// An all-zero decoded code-block covering `rect`.
    fn zero_block(rect: Rect) -> CodeBlockCoefficients {
        let count = (rect.width() * rect.height()) as usize;
        CodeBlockCoefficients {
            rect,
            magnitudes: vec![0; count],
            negative: vec![false; count],
            decoded_planes: vec![0; count],
            corrupt: false,
        }
    }

    /// Writes one sample (absolute band coordinates) into a block.
    fn set_sample(
        block: &mut CodeBlockCoefficients,
        ub: u32,
        vb: u32,
        magnitude: u32,
        negative: bool,
        planes: u8,
    ) {
        let index = ((vb - block.rect.y0) * block.rect.width() + (ub - block.rect.x0)) as usize;
        block.magnitudes[index] = magnitude;
        block.negative[index] = negative;
        block.decoded_planes[index] = planes;
    }

    fn one_band(
        kind: BandKind,
        level: u8,
        rect: Rect,
        block: CodeBlockCoefficients,
    ) -> BandCoefficients {
        BandCoefficients {
            kind,
            level,
            rect,
            blocks: vec![block],
        }
    }

    fn floats(canvas: &TileComponentCanvas) -> &[f32] {
        match &canvas.samples {
            CoefficientCanvas::Irreversible(values) => values,
            CoefficientCanvas::Reversible(values) => {
                panic!("expected the f32 canvas, got i32 x {}", values.len())
            }
        }
    }

    fn ints(canvas: &TileComponentCanvas) -> &[i32] {
        match &canvas.samples {
            CoefficientCanvas::Reversible(values) => values,
            CoefficientCanvas::Irreversible(values) => {
                panic!("expected the i32 canvas, got f32 x {}", values.len())
            }
        }
    }

    #[test]
    fn delta_steps_follow_equation_e3() {
        // (E-3): Delta_b = 2^(Rb - eps_b) * (1 + mu_b / 2^11), with (E-4)
        // Rb = RI + log2(gain_b); the single 0LL band (NL = 0) has gain 1
        // (Table E.1), so Rb = RI here. Hand-computed step sizes:
        //   RI = 8,  eps = 8,  mu = 0:    2^(8-8)   * (1 +    0/2048) = 1    * 1    = 1.0
        //   RI = 9,  eps = 7,  mu = 1024: 2^(9-7)   * (1 + 1024/2048) = 4    * 1.5  = 6.0
        //   RI = 10, eps = 12, mu = 512:  2^(10-12) * (1 +  512/2048) = 0.25 * 1.25 = 0.3125
        //   RI = 8,  eps = 5,  mu = 2047: 2^(8-5)   * (1 + 2047/2048) = 8 * 4095/2048
        //                                                             = 15.99609375
        // A fully decoded magnitude of 1 (Nb = Mb = G + eps - 1, (E-2))
        // reconstructs to exactly 1 * Delta_b: (E-6) adds no midpoint term.
        let tile = Rect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        let cases: [(u8, u8, u16, f32); 4] = [
            (8, 8, 0, 1.0),
            (9, 7, 1024, 6.0),
            (10, 12, 512, 0.3125),
            // 8 * 4095/2048 = 4095/256 = 15.99609375, exact in f32
            (8, 5, 2047, 4095.0 / 256.0),
        ];
        for (depth, exponent, mantissa, expected) in cases {
            let geometry = geometry_for(tile, 0, WaveletKind::Irreversible97);
            let coding = coding(
                0,
                WaveletKind::Irreversible97,
                2,
                QuantizationStyle::ScalarExpounded {
                    steps: vec![QuantStep { exponent, mantissa }],
                },
                None,
            );
            let mb = 2 + exponent - 1; // (E-2) with G = 2
            let mut block = zero_block(tile);
            set_sample(&mut block, 0, 0, 1, false, mb);
            let bands = vec![one_band(BandKind::Ll, 0, tile, block)];
            let canvas = dequantize_tile_component(
                &geometry,
                &coding,
                &component(depth),
                &bands,
                &DecodeLimits::default(),
            )
            .unwrap();
            assert_eq!(
                floats(&canvas),
                &[expected],
                "RI={depth} eps={exponent} mu={mantissa}"
            );
        }
    }

    #[test]
    fn derived_exponents_follow_equation_e5() {
        // ScalarDerived signals (eps_0, mu_0) for the NL-LL band only; (E-5):
        // (eps_b, mu_b) = (eps_0 - NL + nb, mu_0). With NL = 3, eps_0 = 10,
        // mu_0 = 1024 (step factor 1 + 1024/2048 = 1.5), RI = 8, and the
        // Table E.1 gains in (E-4), the full ladder is:
        //   nb = 3: eps = 10 - 3 + 3 = 10:
        //     3LL (Rb = 8):      2^(8-10)  * 1.5 = 0.375
        //     3HL/3LH (Rb = 9):  2^(9-10)  * 1.5 = 0.75
        //     3HH (Rb = 10):     2^(10-10) * 1.5 = 1.5
        //   nb = 2: eps = 10 - 3 + 2 = 9:
        //     2HL/2LH: 2^(9-9)  * 1.5 = 1.5
        //     2HH:     2^(10-9) * 1.5 = 3.0
        //   nb = 1: eps = 10 - 3 + 1 = 8:
        //     1HL/1LH: 2^(9-8)  * 1.5 = 3.0
        //     1HH:     2^(10-8) * 1.5 = 6.0
        // Mb = G + eps_b - 1 (E-2) with G = 2: 11 / 10 / 9 by level; each
        // band's magnitude-1 sample is fully decoded (Nb = Mb) so the canvas
        // holds exactly Delta_b. Band sample (0, 0) of level nb with offsets
        // (xob, yob) lands at canvas (xob * 2^(nb-1), yob * 2^(nb-1)) by the
        // F.3.3 interleave recursion; the canvas is 16 wide.
        let tile = Rect {
            x0: 0,
            y0: 0,
            x1: 16,
            y1: 16,
        };
        let geometry = geometry_for(tile, 3, WaveletKind::Irreversible97);
        let coding = coding(
            3,
            WaveletKind::Irreversible97,
            2,
            QuantizationStyle::ScalarDerived {
                exponent: 10,
                mantissa: 1024,
            },
            None,
        );
        let cases: [(BandKind, u8, u32, u32, f32); 10] = [
            (BandKind::Ll, 3, 0, 0, 0.375),
            (BandKind::Hl, 3, 4, 0, 0.75),
            (BandKind::Lh, 3, 0, 4, 0.75),
            (BandKind::Hh, 3, 4, 4, 1.5),
            (BandKind::Hl, 2, 2, 0, 1.5),
            (BandKind::Lh, 2, 0, 2, 1.5),
            (BandKind::Hh, 2, 2, 2, 3.0),
            (BandKind::Hl, 1, 1, 0, 3.0),
            (BandKind::Lh, 1, 0, 1, 3.0),
            (BandKind::Hh, 1, 1, 1, 6.0),
        ];
        let mut bands = Vec::new();
        for (kind, level, ..) in cases {
            let rect = band_rect_of(&geometry, kind, level);
            let mut block = zero_block(rect);
            let mb = 2 + (10 - 3 + level) - 1; // (E-2) over the (E-5) exponent
            set_sample(&mut block, rect.x0, rect.y0, 1, false, mb);
            bands.push(one_band(kind, level, rect, block));
        }
        let canvas = dequantize_tile_component(
            &geometry,
            &coding,
            &component(8),
            &bands,
            &DecodeLimits::default(),
        )
        .unwrap();
        let values = floats(&canvas);
        for (kind, level, x, y, delta) in cases {
            assert_eq!(
                values[(y * 16 + x) as usize],
                delta,
                "{kind:?} level {level}"
            );
        }
        assert_eq!(values.iter().filter(|value| **value != 0.0).count(), 10);
    }

    #[test]
    fn reversible_reconstruction_is_exact_or_midpoint() {
        // Reversible path (5-3 wavelet + no-quantization style): Delta_b = 1
        // (E.1.2.1), Mb = G + eps - 1 = 1 + 8 - 1 = 8 (E-2).
        //   Fully decoded (Nb = Mb = 8): (E-7) Rq = q exactly: m = 5 -> +5,
        //     and -5 with the sign flag set.
        //   Truncated (Nb = 5 < Mb): (E-8) with r = 1/2 adds
        //     r * 2^(Mb - Nb) = 0.5 * 2^3 = 4: m = 8 -> 8 + 4 = 12, the
        //     midpoint of the uncertainty interval [8, 16); sign after: -12.
        //   Zero magnitude reconstructs to 0 regardless of Nb.
        let tile = Rect {
            x0: 0,
            y0: 0,
            x1: 6,
            y1: 1,
        };
        let geometry = geometry_for(tile, 0, WaveletKind::Reversible53);
        let coding = coding(
            0,
            WaveletKind::Reversible53,
            1,
            QuantizationStyle::None { exponents: vec![8] },
            None,
        );
        let mut block = zero_block(tile);
        set_sample(&mut block, 0, 0, 5, false, 8);
        set_sample(&mut block, 1, 0, 5, true, 8);
        set_sample(&mut block, 2, 0, 8, false, 5);
        set_sample(&mut block, 3, 0, 8, true, 5);
        set_sample(&mut block, 4, 0, 0, false, 5);
        set_sample(&mut block, 5, 0, 0, true, 0);
        let bands = vec![one_band(BandKind::Ll, 0, tile, block)];
        let canvas = dequantize_tile_component(
            &geometry,
            &coding,
            &component(8),
            &bands,
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!(ints(&canvas), &[5, -5, 12, -12, 0, 0]);
    }

    #[test]
    fn irreversible_reconstruction_applies_delta_and_midpoint() {
        // Delta = 2^(8-8) * (1 + 1024/2048) = 1.5 (E-3); Mb = 1 + 8 - 1 = 8.
        //   Fully decoded m = 5: (E-6), no midpoint: 5 * 1.5 = 7.5 (both signs).
        //   Truncated m = 8, Nb = 5: (8 + 0.5 * 2^3) * 1.5 = 12 * 1.5 = 18.0
        //     (both signs).
        //   Zero magnitude -> 0.
        let tile = Rect {
            x0: 0,
            y0: 0,
            x1: 6,
            y1: 1,
        };
        let geometry = geometry_for(tile, 0, WaveletKind::Irreversible97);
        let coding = coding(
            0,
            WaveletKind::Irreversible97,
            1,
            QuantizationStyle::ScalarExpounded {
                steps: vec![QuantStep {
                    exponent: 8,
                    mantissa: 1024,
                }],
            },
            None,
        );
        let mut block = zero_block(tile);
        set_sample(&mut block, 0, 0, 5, false, 8);
        set_sample(&mut block, 1, 0, 5, true, 8);
        set_sample(&mut block, 2, 0, 8, false, 5);
        set_sample(&mut block, 3, 0, 8, true, 5);
        set_sample(&mut block, 4, 0, 0, false, 5);
        set_sample(&mut block, 5, 0, 0, true, 0);
        let bands = vec![one_band(BandKind::Ll, 0, tile, block)];
        let canvas = dequantize_tile_component(
            &geometry,
            &coding,
            &component(8),
            &bands,
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!(floats(&canvas), &[7.5, -7.5, 18.0, -18.0, 0.0, 0.0]);
    }

    #[test]
    fn band_gains_follow_table_e1() {
        // Table E.1 sub-band gains: LL 1, HL 2, LH 2, HH 4 (log2 = 0/1/1/2),
        // entering (E-4) Rb = RI + log2(gain_b). With RI = 8 and the same
        // (eps = 8, mu = 0) step for every band, (E-3) gives Delta = 2^(Rb-8):
        //   LL 2^0 = 1.0, HL 2^1 = 2.0, LH 2^1 = 2.0, HH 2^2 = 4.0.
        // NL = 1 over tile [0,4)^2: every band rect is [0,2)^2 and band sample
        // (0, 0) lands at canvas (xob, yob) (F.3.3 with nb = 1); the canvas is
        // 4 wide, so the indices are LL 0, HL 1, LH 4, HH 5.
        let tile = Rect {
            x0: 0,
            y0: 0,
            x1: 4,
            y1: 4,
        };
        let geometry = geometry_for(tile, 1, WaveletKind::Irreversible97);
        let step = QuantStep {
            exponent: 8,
            mantissa: 0,
        };
        let coding = coding(
            1,
            WaveletKind::Irreversible97,
            2,
            QuantizationStyle::ScalarExpounded {
                steps: vec![step; 4],
            },
            None,
        );
        let mb = 2 + 8 - 1; // (E-2)
        let cases: [(BandKind, usize, f32); 4] = [
            (BandKind::Ll, 0, 1.0),
            (BandKind::Hl, 1, 2.0),
            (BandKind::Lh, 4, 2.0),
            (BandKind::Hh, 5, 4.0),
        ];
        let mut bands = Vec::new();
        for (kind, ..) in cases {
            let rect = band_rect_of(&geometry, kind, 1);
            let mut block = zero_block(rect);
            set_sample(&mut block, rect.x0, rect.y0, 1, false, mb);
            bands.push(one_band(kind, 1, rect, block));
        }
        let canvas = dequantize_tile_component(
            &geometry,
            &coding,
            &component(8),
            &bands,
            &DecodeLimits::default(),
        )
        .unwrap();
        let values = floats(&canvas);
        for (kind, index, expected) in cases {
            assert_eq!(values[index], expected, "{kind:?}");
        }
    }

    #[test]
    fn expounded_steps_resolve_in_codestream_band_order() {
        // Same NL = 1 layout as band_gains_follow_table_e1 but with distinct
        // mantissas to pin the sub-band order of the expounded step list
        // (F.3.1 / Table A.29: 1LL, 1HL, 1LH, 1HH). Hand-computed (E-3):
        //   LL: 2^(8-8)  * (1 +    0/2048) = 1.0
        //   HL: 2^(9-8)  * (1 + 1024/2048) = 2 * 1.5  = 3.0
        //   LH: 2^(9-8)  * (1 +  512/2048) = 2 * 1.25 = 2.5
        //   HH: 2^(10-8) * (1 + 2047/2048) = 4 * 4095/2048 = 7.998046875
        let tile = Rect {
            x0: 0,
            y0: 0,
            x1: 4,
            y1: 4,
        };
        let geometry = geometry_for(tile, 1, WaveletKind::Irreversible97);
        let mantissas: [u16; 4] = [0, 1024, 512, 2047];
        let coding = coding(
            1,
            WaveletKind::Irreversible97,
            2,
            QuantizationStyle::ScalarExpounded {
                steps: mantissas
                    .iter()
                    .map(|&mantissa| QuantStep {
                        exponent: 8,
                        mantissa,
                    })
                    .collect(),
            },
            None,
        );
        let mb = 2 + 8 - 1; // (E-2)
        let cases: [(BandKind, usize, f32); 4] = [
            (BandKind::Ll, 0, 1.0),
            (BandKind::Hl, 1, 3.0),
            (BandKind::Lh, 4, 2.5),
            // 4 * 4095/2048 = 4095/512 = 7.998046875, exact in f32
            (BandKind::Hh, 5, 4095.0 / 512.0),
        ];
        let mut bands = Vec::new();
        for (kind, ..) in cases {
            let rect = band_rect_of(&geometry, kind, 1);
            let mut block = zero_block(rect);
            set_sample(&mut block, rect.x0, rect.y0, 1, false, mb);
            bands.push(one_band(kind, 1, rect, block));
        }
        let canvas = dequantize_tile_component(
            &geometry,
            &coding,
            &component(8),
            &bands,
            &DecodeLimits::default(),
        )
        .unwrap();
        let values = floats(&canvas);
        for (kind, index, expected) in cases {
            assert_eq!(values[index], expected, "{kind:?}");
        }
    }

    #[test]
    fn maxshift_undo_follows_h1() {
        // H.1 (decoding of ROI), scaling value s from SPrgn: "2) If
        // Nb(u,v) < Mb (...), then no modification takes place. 3) If
        // Nb(u,v) >= Mb and if at least one of the first Mb MSBs
        // (i = 1, ..., Mb) is non-zero, then the value of Nb(u,v) is updated
        // as Nb(u,v) = Mb. 4) If Nb(u,v) >= Mb and if all first Mb MSBs are
        // equal to zero, then (...) discard the first s MSBs and shift the
        // remaining MSBs s places, as described in Equation (H-1) (...)
        // update the value of Nb(u,v) as given in Equation (H-2)":
        // Nb = max(0, Nb - s).
        //
        // The encoder up-shifted ROI coefficients by s (H-4), making
        // M'b = Mb + s coded planes (H-3). In magnitude-value terms (Tier-1
        // emits every decoded bit at its coded weight): a sample with any of
        // its first Mb MSBs set has magnitude >= 2^(M'b - Mb) = 2^s and is an
        // ROI sample whose value comes back DOWN by s; a background sample
        // (all first Mb MSBs zero <=> magnitude < 2^s) keeps its value - its
        // MSB indices shift UP by s (H-1), exactly compensating the (E-1)
        // reweighting from M'b to Mb - and loses s decoded planes (H-2).
        //
        // Mb = 1 + 3 - 1 = 3 (E-2), s = 3, M'b = 6; reversible so Delta = 1:
        //   ROI, fully decoded: original 5 was coded as 5 * 2^3 = 40;
        //     Nb = 6 >= Mb: magnitude 40 >> 3 = 5, Nb := Mb = 3 -> exact +5.
        //   Background, fully decoded: m = 5 < 2^3, Nb = 6 -> Nb = 3 = Mb ->
        //     exact +5.
        //   Background, truncated: m = 4, Nb = 4 -> Nb = 4 - 3 = 1 < Mb:
        //     (E-8) midpoint 4 + 0.5 * 2^(3-1) = 6.
        //   ROI, truncated: m = 32 (4 * 2^3 with only Nb = 2 planes),
        //     32 >= 2^3 -> m = 4, Nb stays 2 (step 2, Nb < Mb):
        //     4 + 0.5 * 2^(3-2) = 5, negative -> -5.
        //   Zero magnitude stays 0.
        let tile = Rect {
            x0: 0,
            y0: 0,
            x1: 5,
            y1: 1,
        };
        let geometry = geometry_for(tile, 0, WaveletKind::Reversible53);
        let coding = coding(
            0,
            WaveletKind::Reversible53,
            1,
            QuantizationStyle::None { exponents: vec![3] },
            Some(3),
        );
        let mut block = zero_block(tile);
        set_sample(&mut block, 0, 0, 40, false, 6);
        set_sample(&mut block, 1, 0, 5, false, 6);
        set_sample(&mut block, 2, 0, 4, false, 4);
        set_sample(&mut block, 3, 0, 32, true, 2);
        set_sample(&mut block, 4, 0, 0, false, 6);
        let bands = vec![one_band(BandKind::Ll, 0, tile, block)];
        let canvas = dequantize_tile_component(
            &geometry,
            &coding,
            &component(8),
            &bands,
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!(ints(&canvas), &[5, 5, 6, -5, 0]);
    }

    #[test]
    fn interleave_places_every_band_at_its_f33_canvas_position() {
        // Tile-component rect [3,11) x [5,9), NL = 2 - the odd origins
        // exercise the absolute-coordinate parity of F.3.3 (u0/v0 enter the
        // interleave directly; see also B.5). Band rects per (B-15) with the
        // Table B.1 offsets, hand-computed:
        //   nb=1: 1HL [1,5)x[3,5)  x: ceil((3-1)/2)=1, ceil((11-1)/2)=5;
        //                          y: ceil(5/2)=3, ceil(9/2)=5
        //         1LH [2,6)x[2,4)  x: ceil(3/2)=2, ceil(11/2)=6;
        //                          y: ceil((5-1)/2)=2, ceil((9-1)/2)=4
        //         1HH [1,5)x[2,4)
        //   nb=2: 2LL [1,3)x[2,3)  x: ceil(3/4)=1, ceil(11/4)=3;
        //                          y: ceil(5/4)=2, ceil(9/4)=3
        //         2HL [1,3)x[2,3)  x: ceil((3-2)/4)=1, ceil((11-2)/4)=3
        //         2LH [1,3)x[1,2)  y: ceil((5-2)/4)=1, ceil((9-2)/4)=2
        //         2HH [1,3)x[1,2)
        // F.3.3 applied level by level (the deepest LL seeds the recursion):
        // level nb sample (ub, vb) lands at canvas
        // (ub * 2^nb + xob * 2^(nb-1), vb * 2^nb + yob * 2^(nb-1)).
        // One hand-picked sample per band; the canvas is 8 wide with
        // index = (y - 5) * 8 + (x - 3):
        //   2LL (1,2) -> (1*4+0, 2*4+0) = (4,8): index 3*8+1 = 25
        //   2HL (1,2) -> (1*4+2, 2*4+0) = (6,8): index 3*8+3 = 27
        //   2LH (1,1) -> (1*4+0, 1*4+2) = (4,6): index 1*8+1 = 9
        //   2HH (1,1) -> (1*4+2, 1*4+2) = (6,6): index 1*8+3 = 11
        //   1HL (1,3) -> (1*2+1, 3*2+0) = (3,6): index 1*8+0 = 8
        //   1LH (2,2) -> (2*2+0, 2*2+1) = (4,5): index 0*8+1 = 1
        //   1HH (1,2) -> (1*2+1, 2*2+1) = (3,5): index 0*8+0 = 0
        let tile = Rect {
            x0: 3,
            y0: 5,
            x1: 11,
            y1: 9,
        };
        let geometry = geometry_for(tile, 2, WaveletKind::Reversible53);
        // Seven sub-bands (2LL, 2HL, 2LH, 2HH, 1HL, 1LH, 1HH), one reversible
        // exponent each; Mb = 1 + 8 - 1 = 8, every sample fully decoded.
        let coding = coding(
            2,
            WaveletKind::Reversible53,
            1,
            QuantizationStyle::None {
                exponents: vec![8; 7],
            },
            None,
        );
        let cases: [(BandKind, u8, Rect, u32, u32, usize, i32); 7] = [
            (
                BandKind::Ll,
                2,
                Rect {
                    x0: 1,
                    y0: 2,
                    x1: 3,
                    y1: 3,
                },
                1,
                2,
                25,
                1,
            ),
            (
                BandKind::Hl,
                2,
                Rect {
                    x0: 1,
                    y0: 2,
                    x1: 3,
                    y1: 3,
                },
                1,
                2,
                27,
                2,
            ),
            (
                BandKind::Lh,
                2,
                Rect {
                    x0: 1,
                    y0: 1,
                    x1: 3,
                    y1: 2,
                },
                1,
                1,
                9,
                3,
            ),
            (
                BandKind::Hh,
                2,
                Rect {
                    x0: 1,
                    y0: 1,
                    x1: 3,
                    y1: 2,
                },
                1,
                1,
                11,
                4,
            ),
            (
                BandKind::Hl,
                1,
                Rect {
                    x0: 1,
                    y0: 3,
                    x1: 5,
                    y1: 5,
                },
                1,
                3,
                8,
                5,
            ),
            (
                BandKind::Lh,
                1,
                Rect {
                    x0: 2,
                    y0: 2,
                    x1: 6,
                    y1: 4,
                },
                2,
                2,
                1,
                6,
            ),
            (
                BandKind::Hh,
                1,
                Rect {
                    x0: 1,
                    y0: 2,
                    x1: 5,
                    y1: 4,
                },
                1,
                2,
                0,
                7,
            ),
        ];
        let mut bands = Vec::new();
        for (kind, level, rect, ub, vb, _, marker) in cases {
            // the hand-computed band rect must agree with the Annex B geometry
            assert_eq!(
                band_rect_of(&geometry, kind, level),
                rect,
                "{kind:?} level {level}"
            );
            let mut block = zero_block(rect);
            set_sample(&mut block, ub, vb, marker as u32, false, 8);
            bands.push(one_band(kind, level, rect, block));
        }
        let canvas = dequantize_tile_component(
            &geometry,
            &coding,
            &component(8),
            &bands,
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!(canvas.rect, tile);
        assert_eq!(canvas.levels, 2);
        let values = ints(&canvas);
        assert_eq!(values.len(), 32);
        for (kind, level, _, _, _, index, marker) in cases {
            assert_eq!(values[index], marker, "{kind:?} level {level}");
        }
        // every other canvas cell keeps its zero fill: the seven markers sum
        // to 1 + 2 + ... + 7 = 28 and are the only non-zero samples
        assert_eq!(
            values.iter().map(|value| i64::from(*value)).sum::<i64>(),
            28
        );
        assert_eq!(values.iter().filter(|value| **value != 0).count(), 7);
    }

    #[test]
    fn canvas_variant_needs_both_reversible_wavelet_and_no_quantization() {
        // The i32 path is only bit-exact when BOTH the 5-3 wavelet
        // (Table A.20 value 1) and the no-quantization style (Table A.28)
        // are in effect; every other combination dequantizes to f32.
        let tile = Rect {
            x0: 0,
            y0: 0,
            x1: 1,
            y1: 1,
        };
        // 5-3 wavelet + expounded steps -> f32: Delta = 2^(8-8) * 1 = 1.0,
        // m = 3 fully decoded (Mb = 2 + 8 - 1 = 9) -> 3.0.
        let geometry = geometry_for(tile, 0, WaveletKind::Reversible53);
        let coding_mixed = coding(
            0,
            WaveletKind::Reversible53,
            2,
            QuantizationStyle::ScalarExpounded {
                steps: vec![QuantStep {
                    exponent: 8,
                    mantissa: 0,
                }],
            },
            None,
        );
        let mut block = zero_block(tile);
        set_sample(&mut block, 0, 0, 3, false, 9);
        let bands = vec![one_band(BandKind::Ll, 0, tile, block)];
        let canvas = dequantize_tile_component(
            &geometry,
            &coding_mixed,
            &component(8),
            &bands,
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!(floats(&canvas), &[3.0]);

        // 9-7 wavelet + no-quantization style -> f32 with Delta = 1
        // (E.1.2.1): truncated m = 8, Nb = 5, Mb = 1 + 8 - 1 = 8:
        // 8 + 0.5 * 2^3 = 12.0.
        let geometry = geometry_for(tile, 0, WaveletKind::Irreversible97);
        let coding_ranged = coding(
            0,
            WaveletKind::Irreversible97,
            1,
            QuantizationStyle::None { exponents: vec![8] },
            None,
        );
        let mut block = zero_block(tile);
        set_sample(&mut block, 0, 0, 8, false, 5);
        let bands = vec![one_band(BandKind::Ll, 0, tile, block)];
        let canvas = dequantize_tile_component(
            &geometry,
            &coding_ranged,
            &component(8),
            &bands,
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!(floats(&canvas), &[12.0]);
    }

    #[test]
    fn missing_expounded_step_is_malformed() {
        // NL = 1 has four sub-bands in the codestream order; a one-entry
        // expounded list cannot serve band 1HH (index 3).
        let tile = Rect {
            x0: 0,
            y0: 0,
            x1: 4,
            y1: 4,
        };
        let geometry = geometry_for(tile, 1, WaveletKind::Irreversible97);
        let coding = coding(
            1,
            WaveletKind::Irreversible97,
            2,
            QuantizationStyle::ScalarExpounded {
                steps: vec![QuantStep {
                    exponent: 8,
                    mantissa: 0,
                }],
            },
            None,
        );
        let rect = band_rect_of(&geometry, BandKind::Hh, 1);
        let bands = vec![one_band(BandKind::Hh, 1, rect, zero_block(rect))];
        assert!(matches!(
            dequantize_tile_component(
                &geometry,
                &coding,
                &component(8),
                &bands,
                &DecodeLimits::default()
            ),
            Err(JpxError::Malformed(_))
        ));
    }

    #[test]
    fn samples_outside_the_canvas_are_dropped_not_panicked() {
        // A hostile block rect that overhangs the canvas: in-range samples
        // land, the out-of-range one is discarded without panicking.
        let tile = Rect {
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 1,
        };
        let geometry = geometry_for(tile, 0, WaveletKind::Reversible53);
        let coding = coding(
            0,
            WaveletKind::Reversible53,
            1,
            QuantizationStyle::None { exponents: vec![8] },
            None,
        );
        let hostile = Rect {
            x0: 0,
            y0: 0,
            x1: 3,
            y1: 1,
        };
        let mut block = zero_block(hostile);
        set_sample(&mut block, 0, 0, 1, false, 8);
        set_sample(&mut block, 1, 0, 2, false, 8);
        set_sample(&mut block, 2, 0, 3, false, 8);
        let bands = vec![one_band(BandKind::Ll, 0, hostile, block)];
        let canvas = dequantize_tile_component(
            &geometry,
            &coding,
            &component(8),
            &bands,
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!(ints(&canvas), &[1, 2]);
    }

    /// `max_decoded_bytes` bounds the coefficient canvas BEFORE it is
    /// allocated: a 5 x 1 tile-component needs 20 bytes of i32.
    #[test]
    fn max_decoded_bytes_bounds_the_canvas_allocation() {
        let tile = Rect {
            x0: 0,
            y0: 0,
            x1: 5,
            y1: 1,
        };
        let geometry = geometry_for(tile, 0, WaveletKind::Reversible53);
        let coding = coding(
            0,
            WaveletKind::Reversible53,
            1,
            QuantizationStyle::None { exponents: vec![8] },
            None,
        );
        let bands = vec![one_band(BandKind::Ll, 0, tile, zero_block(tile))];
        let tight = DecodeLimits {
            max_decoded_bytes: 19,
            ..DecodeLimits::default()
        };
        assert!(matches!(
            dequantize_tile_component(&geometry, &coding, &component(8), &bands, &tight),
            Err(JpxError::LimitExceeded {
                what: "max_decoded_bytes",
                actual: 20,
                limit: 19,
            })
        ));
        let exact = DecodeLimits {
            max_decoded_bytes: 20,
            ..DecodeLimits::default()
        };
        dequantize_tile_component(&geometry, &coding, &component(8), &bands, &exact).unwrap();
    }

    /// The RGN composition across the seams (H.1/H.2): Tier-2 hands
    /// Tier-1 the plane budget WITH the maxshift folded in — M'b = Mb + s
    /// per (H-3), `packet::band_magnitude_bits` — so Tier-1 magnitudes
    /// arrive at the coded weights, while this stage resolves the
    /// UNSHIFTED Mb from the very same markers ((E-2), `band_parameters`)
    /// and undoes the shift per H.1. Fully decoded (Nb = M'b) ROI and
    /// background samples must therefore reconstruct exactly. (There is
    /// no RGN fixture in the committed zoo, so the composition is pinned
    /// here by construction.)
    #[test]
    fn rgn_streams_compose_across_the_tier2_and_dequant_seams() {
        let shift = 3u8;
        let coding = coding(
            0,
            WaveletKind::Reversible53,
            1,
            QuantizationStyle::None { exponents: vec![3] },
            Some(shift),
        );
        // Tier-2's coded budget for every block of the band:
        // M'b = (G + eps - 1) + s = (1 + 3 - 1) + 3 = 6 (E-2, H-3).
        let coded_planes = crate::packet::band_magnitude_bits(&coding, 0, 0, 0);
        assert_eq!(coded_planes, 6);
        let tile = Rect {
            x0: 0,
            y0: 0,
            x1: 2,
            y1: 1,
        };
        let geometry = geometry_for(tile, 0, WaveletKind::Reversible53);
        let mut block = zero_block(tile);
        // An ROI sample of value 5, up-shifted by the encoder per (H-4)
        // and fully decoded through all M'b coded planes...
        set_sample(&mut block, 0, 0, 5 << shift, false, coded_planes);
        // ...and a background sample of value -5, coded unshifted (its
        // magnitude stays below 2^s by the maxshift construction).
        set_sample(&mut block, 1, 0, 5, true, coded_planes);
        let bands = vec![one_band(BandKind::Ll, 0, tile, block)];
        let canvas = dequantize_tile_component(
            &geometry,
            &coding,
            &component(8),
            &bands,
            &DecodeLimits::default(),
        )
        .unwrap();
        assert_eq!(ints(&canvas), &[5, -5]);
    }
}
