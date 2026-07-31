//! Dequantization (ITU-T T.800 Annex E) and ROI maxshift undo (H.1/H.2):
//! turns sign-magnitude Tier-1 output into the interleaved coefficient
//! canvas the inverse DWT consumes — the dequant → dwt seam.

use crate::error::{JpxError, Result};
use crate::geometry::{Rect, TileComponentGeometry};
use crate::markers::{ComponentCoding, SizComponent};
use crate::t1::BandCoefficients;

/// The coefficient storage of one tile-component canvas.
// Constructed by the dequant stage; the variants are the frozen seam.
#[allow(dead_code)]
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
#[allow(dead_code)]
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
/// dynamic range.
pub(crate) fn dequantize_tile_component(
    geometry: &TileComponentGeometry,
    coding: &ComponentCoding,
    component: &SizComponent,
    bands: &[BandCoefficients],
) -> Result<TileComponentCanvas> {
    // RI (Table A.11) enters the Equation (E-4) dynamic range; signedness
    // matters again at the G.1.2 level shift downstream.
    let _ = (geometry, coding, component.depth, component.signed, bands);
    Err(JpxError::Unsupported("decoder scaffold"))
}

#[cfg(test)]
mod tests {}
