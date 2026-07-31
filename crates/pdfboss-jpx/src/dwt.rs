//! Inverse discrete wavelet transform (ITU-T T.800 Annex F): the IDWT
//! procedure (F.3.1) run level by level and in place over the interleaved
//! tile-component canvas.

use crate::dequant::TileComponentCanvas;
use crate::error::{JpxError, Result};

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
    let _ = (canvas.rect, canvas.levels, &canvas.samples);
    Err(JpxError::Unsupported("decoder scaffold"))
}

#[cfg(test)]
mod tests {}
