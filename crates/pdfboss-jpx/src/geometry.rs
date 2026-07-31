//! Grid algebra from ITU-T T.800 Annex B: tiles, tile-components,
//! resolution levels, sub-bands, precincts and code-blocks.
//!
//! Everything here works in ABSOLUTE coordinates on the reference grid (or
//! the band's own absolute domain). Down-stream stages receive absolute
//! rects and must never renormalize them to zero: lifting parity in Annex F
//! is defined by the absolute coordinate values (see B.5 and F.3.3).

use crate::error::{JpxError, Result};
use crate::markers::{CodingStyle, Siz, SizComponent};

/// Half-open rectangle `[x0, x1) x [y0, y1)` in absolute coordinates
/// (reference grid, component grid, or a band domain — stated per use).
/// Width/height per T.800 Equation (B-2): `(x1 - x0, y1 - y0)`.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) struct Rect {
    pub x0: u32,
    pub y0: u32,
    pub x1: u32,
    pub y1: u32,
}

impl Rect {
    /// Width per Equation (B-2); zero when the rect is degenerate.
    pub(crate) fn width(&self) -> u32 {
        self.x1.saturating_sub(self.x0)
    }

    /// Height per Equation (B-2); zero when the rect is degenerate.
    pub(crate) fn height(&self) -> u32 {
        self.y1.saturating_sub(self.y0)
    }

    /// True when the rect contains no samples (B.6 note: empty precincts /
    /// tile-components exist and still occupy index space).
    pub(crate) fn is_empty(&self) -> bool {
        self.x1 <= self.x0 || self.y1 <= self.y0
    }
}

/// Ceiling division on u64: the corner-brackets operator used throughout
/// Annex B (e.g. Equations (B-5), (B-12), (B-14), (B-15)).
///
/// `denominator` must be nonzero; callers validate marker fields first.
pub(crate) fn ceil_div(numerator: u64, denominator: u64) -> u64 {
    numerator / denominator + u64::from(!numerator.is_multiple_of(denominator))
}

/// Tile counts per Equation (B-5):
/// `numXtiles = ceil((Xsiz - XTOsiz) / XTsiz)` and likewise for Y.
///
/// Errors with `Malformed` when the SIZ fields are inconsistent (zero tile
/// size, or tile/image offsets outside the constraints of (B-3)/(B-4))
/// instead of panicking — every header field is hostile input.
pub(crate) fn tile_grid(siz: &Siz) -> Result<(u32, u32)> {
    if siz.xtsiz == 0 || siz.ytsiz == 0 {
        return Err(JpxError::Malformed("SIZ: zero tile size".into()));
    }
    // (B-3): 0 <= XTOsiz <= XOsiz; (B-4): XTsiz + XTOsiz > XOsiz. Together
    // with Xsiz >= 1 (Table A.9) they guarantee at least one tile column.
    if siz.xtosiz > siz.xosiz || siz.ytosiz > siz.yosiz {
        return Err(JpxError::Malformed(
            "SIZ: tile offset exceeds image offset (B-3)".into(),
        ));
    }
    if siz.xsiz <= siz.xosiz || siz.ysiz <= siz.yosiz {
        return Err(JpxError::Malformed(
            "SIZ: empty image area (Xsiz <= XOsiz or Ysiz <= YOsiz)".into(),
        ));
    }
    let wide = ceil_div(
        u64::from(siz.xsiz) - u64::from(siz.xtosiz),
        u64::from(siz.xtsiz),
    );
    let high = ceil_div(
        u64::from(siz.ysiz) - u64::from(siz.ytosiz),
        u64::from(siz.ytsiz),
    );
    // (Xsiz - XTOsiz) / XTsiz <= u32::MAX because Xsiz <= u32::MAX and
    // XTsiz >= 1, so the casts cannot truncate.
    Ok((wide as u32, high as u32))
}

/// Tile rect on the reference grid per Equations (B-7)..(B-10):
/// `tx0 = max(XTOsiz + p*XTsiz, XOsiz)`, `tx1 = min(XTOsiz + (p+1)*XTsiz,
/// Xsiz)` (and the y analogues). Out-of-range `(p, q)` yields an empty rect
/// clamped to the image area rather than a panic.
pub(crate) fn tile_rect(siz: &Siz, p: u32, q: u32) -> Rect {
    let clamp_x = |v: u64| v.min(u64::from(siz.xsiz)) as u32;
    let clamp_y = |v: u64| v.min(u64::from(siz.ysiz)) as u32;
    let tx0 = clamp_x(
        (u64::from(siz.xtosiz) + u64::from(p) * u64::from(siz.xtsiz)).max(u64::from(siz.xosiz)),
    );
    let ty0 = clamp_y(
        (u64::from(siz.ytosiz) + u64::from(q) * u64::from(siz.ytsiz)).max(u64::from(siz.yosiz)),
    );
    let tx1 = clamp_x(u64::from(siz.xtosiz) + (u64::from(p) + 1) * u64::from(siz.xtsiz));
    let ty1 = clamp_y(u64::from(siz.ytosiz) + (u64::from(q) + 1) * u64::from(siz.ytsiz));
    Rect {
        x0: tx0,
        y0: ty0,
        x1: tx1,
        y1: ty1,
    }
}

/// Maps a reference-grid rect into one component's domain per Equation
/// (B-12): every corner is ceil-divided by the component's `(XRsiz, YRsiz)`
/// separations (A.5.1). Used both for tile-components and for the image
/// area itself (B-1).
pub(crate) fn component_rect(area: Rect, xrsiz: u8, yrsiz: u8) -> Result<Rect> {
    if xrsiz == 0 || yrsiz == 0 {
        return Err(JpxError::Malformed(
            "SIZ: zero component separation (XRsiz/YRsiz)".into(),
        ));
    }
    let dx = u64::from(xrsiz);
    let dy = u64::from(yrsiz);
    Ok(Rect {
        x0: ceil_div(u64::from(area.x0), dx) as u32,
        y0: ceil_div(u64::from(area.y0), dy) as u32,
        x1: ceil_div(u64::from(area.x1), dx) as u32,
        y1: ceil_div(u64::from(area.y1), dy) as u32,
    })
}

/// Sub-band kind. Determines the (xob, yob) offsets of Equation (B-15)
/// via Table B.1, the context tables of Annex D (Table D.1), and the
/// sub-band gain of Table E.1.
// Constructed by the geometry stage (tile_component_geometry); the scaffold
// only fixes the seam shape.
#[allow(dead_code)]
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum BandKind {
    /// nLL: (xob, yob) = (0, 0).
    Ll,
    /// nHL (horizontally high-pass): (xob, yob) = (1, 0).
    Hl,
    /// nLH (vertically high-pass): (xob, yob) = (0, 1).
    Lh,
    /// nHH: (xob, yob) = (1, 1).
    Hh,
}

/// Code-blocks of one band restricted to one precinct (B.7): the code-block
/// partition is anchored at (0, 0) of the band domain with span
/// `2^xcb' x 2^ycb'` where xcb'/ycb' obey Equations (B-17)/(B-18), and is
/// intersected with (band rect ∩ precinct mapped onto the band).
// Constructed by the geometry stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct PrecinctBand {
    /// Code-block columns spanned by this precinct within the band. The
    /// Tier-2 inclusion and zero-bit-plane tag trees are exactly
    /// `blocks_wide x blocks_high` leaves (B.10.2, B.10.4, B.10.5).
    pub blocks_wide: u32,
    /// Code-block rows spanned by this precinct within the band.
    pub blocks_high: u32,
    /// `blocks_wide * blocks_high` rects in raster order, each clipped to
    /// the band ∩ precinct intersection, in ABSOLUTE band coordinates.
    /// Fully clipped-away blocks appear as empty rects: they still occupy
    /// tag-tree leaf slots.
    pub blocks: Vec<Rect>,
}

/// One sub-band of one resolution level of one tile-component.
// Constructed by the geometry stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct BandGeometry {
    pub kind: BandKind,
    /// Decomposition level nb the band belongs to (B.5: for resolution
    /// r = 0 the single LL band has nb = NL; for r > 0 the HL/LH/HH bands
    /// have nb = NL - r + 1).
    pub level: u8,
    /// Band rect in ABSOLUTE band coordinates per Equation (B-15) with the
    /// Table B.1 (xob, yob) offsets. Never renormalized to zero.
    pub rect: Rect,
    /// Per-precinct code-block enumeration; outer index is the precinct's
    /// raster index in the resolution's precinct grid (B-16). The precinct
    /// partition maps onto band coordinates by the r > 0 halving rule
    /// (B.6/B.7: effective precinct exponents shrink by one because band
    /// coordinates are half the resolution-level coordinates).
    pub precincts: Vec<PrecinctBand>,
}

/// One resolution level r of one tile-component (B.5, B.6).
// Constructed by the geometry stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct ResolutionGeometry {
    /// Reduced-resolution rect per Equation (B-14):
    /// `ceil(tc / 2^(NL - r))` on every corner, absolute coordinates.
    pub rect: Rect,
    /// Precinct width exponent PPx effective at this resolution (Table
    /// A.21; 15 when the coding style carries no explicit precinct list).
    pub ppx: u8,
    /// Precinct height exponent PPy effective at this resolution.
    pub ppy: u8,
    /// Precinct columns per Equation (B-16) — zero when the resolution
    /// rect is degenerate, in which case the level has no packets.
    pub precincts_wide: u32,
    /// Precinct rows per Equation (B-16).
    pub precincts_high: u32,
    /// r = 0: exactly one LL band; r > 0: HL, LH, HH in that order (the
    /// packet ordering of B.9 and the SPqcd sub-band order both follow it).
    pub bands: Vec<BandGeometry>,
}

/// Complete geometry of one tile-component: what Tier-2 (packet), Tier-1,
/// dequantization and the inverse DWT all consume.
// Constructed by the geometry stage; the field list is the frozen seam.
#[allow(dead_code)]
#[derive(Debug)]
pub(crate) struct TileComponentGeometry {
    /// Tile-component rect on the component grid per Equation (B-12),
    /// absolute coordinates.
    pub rect: Rect,
    /// Decomposition level count NL of this tile-component (Table A.15).
    pub levels: u8,
    /// Resolution levels r = 0 ..= NL, index == r (length NL + 1).
    pub resolutions: Vec<ResolutionGeometry>,
}

/// Builds the full Annex B partition for one tile-component: resolution
/// rects (B-14), band rects (B-15 + Table B.1), precinct grids (B-16,
/// anchored at (0, 0) of the RESOLUTION grid), and per-(precinct ∩ band)
/// code-block enumeration (B-17/(B-18), partition anchored at (0, 0) of the
/// band domain).
///
/// `tile` is the reference-grid tile rect from [`tile_rect`]; the component
/// rect is derived here via Equation (B-12). All outputs are absolute.
pub(crate) fn tile_component_geometry(
    tile: Rect,
    component: &SizComponent,
    style: &CodingStyle,
) -> Result<TileComponentGeometry> {
    let rect = component_rect(tile, component.xrsiz, component.yrsiz)?;
    let levels = style.decomposition_levels;
    // Table A.15: 0 to 32 decomposition levels; anything larger would also
    // break the 2^(NL - r) shifts below.
    if levels > 32 {
        return Err(JpxError::Malformed(
            "COD/COC: more than 32 decomposition levels (Table A.15)".into(),
        ));
    }
    // Table A.15/A.21: when Scod/Scoc bit 0 signals explicit precincts,
    // exactly one (PPx, PPy) byte per resolution level, NL + 1 of them.
    if !style.precincts.is_empty() && style.precincts.len() != usize::from(levels) + 1 {
        return Err(JpxError::Malformed(
            "COD/COC: precinct size list is not NL + 1 entries (Table A.21)".into(),
        ));
    }
    // Table A.18: xcb and ycb range over 2..=10 (the marker parser applies
    // the + 2). Validated here because the shifts below depend on it.
    let xcb = style.code_block_width_exp;
    let ycb = style.code_block_height_exp;
    if !(2..=10).contains(&xcb) || !(2..=10).contains(&ycb) {
        return Err(JpxError::Malformed(
            "COD/COC: code-block exponent outside 2..=10 (Table A.18)".into(),
        ));
    }
    let mut resolutions = Vec::with_capacity(usize::from(levels) + 1);
    for r in 0..=levels {
        resolutions.push(resolution_geometry(rect, levels, r, style)?);
    }
    Ok(TileComponentGeometry {
        rect,
        levels,
        resolutions,
    })
}

/// Table B.1: the (xob, yob) band-orientation offsets of Equation (B-15).
fn band_offsets(kind: BandKind) -> (u64, u64) {
    match kind {
        BandKind::Ll => (0, 0),
        BandKind::Hl => (1, 0),
        BandKind::Lh => (0, 1),
        BandKind::Hh => (1, 1),
    }
}

/// One corner of Equation (B-15): `ceil((a - 2^(nb-1) * ob) / 2^nb)` with
/// `half = 2^(nb-1)` and `full = 2^nb`. For `ob = 1` the numerator can dip
/// below zero, so it is rewritten for unsigned arithmetic:
/// `ceil((a - h) / 2h) = floor((a + h - 1) / 2h)` for all `a >= 0` (both
/// count the same multiples of `2h` in the half-open interval).
fn band_coord(a: u32, ob: u64, half: u64, full: u64) -> u32 {
    let a = u64::from(a);
    if ob == 0 {
        ceil_div(a, full) as u32
    } else {
        ((a + half - 1) / full) as u32
    }
}

/// Sub-band rect per Equation (B-15) with the Table B.1 offsets, in
/// ABSOLUTE band coordinates. `level` is the band's decomposition level
/// nb; nb = 0 only occurs for the LL band of an untransformed
/// tile-component, where (B-15) degenerates to the identity.
fn band_rect(tc: Rect, level: u8, kind: BandKind) -> Rect {
    if level == 0 {
        return tc;
    }
    let half = 1u64 << (u32::from(level) - 1);
    let full = half << 1;
    let (xob, yob) = band_offsets(kind);
    Rect {
        x0: band_coord(tc.x0, xob, half, full),
        y0: band_coord(tc.y0, yob, half, full),
        x1: band_coord(tc.x1, xob, half, full),
        y1: band_coord(tc.y1, yob, half, full),
    }
}

/// Effective (PPx, PPy) for resolution level `r`: the signalled Table A.21
/// entry, or the Table A.13 maximal default 15 when the coding style
/// carries no precinct list. B.6 forbids zero exponents above r = 0.
fn precinct_exponents(style: &CodingStyle, r: u8) -> Result<(u8, u8)> {
    if style.precincts.is_empty() {
        return Ok((15, 15));
    }
    let entry = &style.precincts[usize::from(r)];
    if r > 0 && (entry.ppx == 0 || entry.ppy == 0) {
        return Err(JpxError::Malformed(
            "COD/COC: zero precinct exponent above resolution level 0 (B.6)".into(),
        ));
    }
    Ok((entry.ppx, entry.ppy))
}

/// One (B-16) dimension: `ceil(tr1 / 2^pp) - floor(tr0 / 2^pp)` when the
/// resolution rect is non-degenerate in this dimension, else zero.
fn precinct_count(tr0: u32, tr1: u32, pp: u8) -> u32 {
    if tr1 <= tr0 {
        return 0;
    }
    (ceil_div(u64::from(tr1), 1u64 << pp) - (u64::from(tr0) >> pp)) as u32
}

/// What the per-band precinct/code-block enumeration needs to know about
/// its resolution level.
struct ResolutionContext {
    /// Resolution-level rect (B-14): the precinct partition's domain.
    rect: Rect,
    /// Resolution level index r (selects the (B-17)/(B-18) branch and the
    /// B.7 halving of precinct exponents in band coordinates).
    r: u8,
    /// Signalled precinct exponents at this level.
    ppx: u8,
    ppy: u8,
    /// (B-16) grid dimensions.
    precincts_wide: u32,
    precincts_high: u32,
    /// Code-block exponents xcb'/ycb' per (B-17)/(B-18).
    xcb: u8,
    ycb: u8,
}

impl ResolutionContext {
    /// Precinct exponents in BAND coordinates: at r > 0 the band domain is
    /// half the resolution-level domain, so the effective exponents shrink
    /// by one (B.6/B.7); at r = 0 band and resolution coordinates agree.
    fn band_precinct_shifts(&self) -> (u32, u32) {
        if self.r == 0 {
            (u32::from(self.ppx), u32::from(self.ppy))
        } else {
            (u32::from(self.ppx) - 1, u32::from(self.ppy) - 1)
        }
    }
}

/// The code-blocks of one precinct restricted to one band (B.7): the
/// precinct's span in band coordinates intersected with the band rect,
/// then covered by the code-block partition anchored at (0, 0) of the
/// band, raster order. `(i, j)` indexes the precinct within the (B-16)
/// grid whose first column/row is `floor(tr0 / 2^PP)`.
fn precinct_band(band: Rect, ctx: &ResolutionContext, i: u32, j: u32) -> PrecinctBand {
    let (shift_x, shift_y) = ctx.band_precinct_shifts();
    // Absolute partition indices: the grid is anchored at coordinate 0 of
    // the resolution grid (B.6), and the same index reaches the band
    // domain because both corners of the span just halve at r > 0 (B.7).
    let column = u64::from(ctx.rect.x0 >> ctx.ppx) + u64::from(i);
    let row = u64::from(ctx.rect.y0 >> ctx.ppy) + u64::from(j);
    let ix0 = (column << shift_x).max(u64::from(band.x0));
    let ix1 = ((column + 1) << shift_x).min(u64::from(band.x1));
    let iy0 = (row << shift_y).max(u64::from(band.y0));
    let iy1 = ((row + 1) << shift_y).min(u64::from(band.y1));
    if ix0 >= ix1 || iy0 >= iy1 {
        // Empty precinct (B.6): it keeps its raster slot and its packets,
        // but contributes no code-blocks from this band (B.9).
        return PrecinctBand {
            blocks_wide: 0,
            blocks_high: 0,
            blocks: Vec::new(),
        };
    }
    // B.7: the code-block partition is anchored at (0, 0) of the band with
    // span 2^xcb' x 2^ycb'; enumerate the cells covering the intersection.
    let span_x = 1u64 << ctx.xcb;
    let span_y = 1u64 << ctx.ycb;
    let first_column = ix0 / span_x;
    let end_column = ceil_div(ix1, span_x);
    let first_row = iy0 / span_y;
    let end_row = ceil_div(iy1, span_y);
    let blocks_wide = (end_column - first_column) as u32;
    let blocks_high = (end_row - first_row) as u32;
    let mut blocks = Vec::with_capacity((blocks_wide as usize) * (blocks_high as usize));
    for block_row in first_row..end_row {
        for block_column in first_column..end_column {
            blocks.push(Rect {
                x0: (block_column * span_x).max(ix0) as u32,
                y0: (block_row * span_y).max(iy0) as u32,
                x1: ((block_column + 1) * span_x).min(ix1) as u32,
                y1: ((block_row + 1) * span_y).min(iy1) as u32,
            });
        }
    }
    PrecinctBand {
        blocks_wide,
        blocks_high,
        blocks,
    }
}

/// One band of one resolution level: the (B-15) rect plus one
/// [`PrecinctBand`] per (B-16) precinct in raster order.
fn band_geometry(tc: Rect, level: u8, kind: BandKind, ctx: &ResolutionContext) -> BandGeometry {
    let rect = band_rect(tc, level, kind);
    let count = (ctx.precincts_wide as usize) * (ctx.precincts_high as usize);
    let mut precincts = Vec::with_capacity(count);
    for j in 0..ctx.precincts_high {
        for i in 0..ctx.precincts_wide {
            precincts.push(precinct_band(rect, ctx, i, j));
        }
    }
    BandGeometry {
        kind,
        level,
        rect,
        precincts,
    }
}

/// One resolution level of the Annex B partition: the (B-14) rect, the
/// (B-16) precinct grid and the level's bands (B.9 order).
fn resolution_geometry(
    tc: Rect,
    levels: u8,
    r: u8,
    style: &CodingStyle,
) -> Result<ResolutionGeometry> {
    // (B-14): every corner ceil-divided by 2^(NL - r).
    let divisor = 1u64 << u32::from(levels - r);
    let rect = Rect {
        x0: ceil_div(u64::from(tc.x0), divisor) as u32,
        y0: ceil_div(u64::from(tc.y0), divisor) as u32,
        x1: ceil_div(u64::from(tc.x1), divisor) as u32,
        y1: ceil_div(u64::from(tc.y1), divisor) as u32,
    };
    let (ppx, ppy) = precinct_exponents(style, r)?;
    let ctx = ResolutionContext {
        rect,
        r,
        ppx,
        ppy,
        precincts_wide: precinct_count(rect.x0, rect.x1, ppx),
        precincts_high: precinct_count(rect.y0, rect.y1, ppy),
        // (B-17)/(B-18): the code-block size is bounded by the precinct
        // size, which halves in band coordinates above r = 0.
        xcb: if r == 0 {
            style.code_block_width_exp.min(ppx)
        } else {
            style.code_block_width_exp.min(ppx - 1)
        },
        ycb: if r == 0 {
            style.code_block_height_exp.min(ppy)
        } else {
            style.code_block_height_exp.min(ppy - 1)
        },
    };
    // B.5: r = 0 is the NL-LL band alone; every later level adds its HL,
    // LH, HH triple at nb = NL - r + 1 (also the B.9 packet band order).
    let (level, kinds): (u8, &[BandKind]) = if r == 0 {
        (levels, &[BandKind::Ll])
    } else {
        (levels - r + 1, &[BandKind::Hl, BandKind::Lh, BandKind::Hh])
    };
    let bands = kinds
        .iter()
        .map(|kind| band_geometry(tc, level, *kind, &ctx))
        .collect();
    Ok(ResolutionGeometry {
        rect,
        ppx,
        ppy,
        precincts_wide: ctx.precincts_wide,
        precincts_high: ctx.precincts_high,
        bands,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::markers::{PrecinctExponents, WaveletKind};

    fn rect(x0: u32, y0: u32, x1: u32, y1: u32) -> Rect {
        Rect { x0, y0, x1, y1 }
    }

    fn unit_component() -> SizComponent {
        SizComponent {
            depth: 8,
            signed: false,
            xrsiz: 1,
            yrsiz: 1,
        }
    }

    fn coding_style(levels: u8, xcb: u8, ycb: u8, precincts: &[(u8, u8)]) -> CodingStyle {
        CodingStyle {
            decomposition_levels: levels,
            code_block_width_exp: xcb,
            code_block_height_exp: ycb,
            code_block_style: 0,
            wavelet: WaveletKind::Reversible53,
            precincts: precincts
                .iter()
                .map(|&(ppx, ppy)| PrecinctExponents { ppx, ppy })
                .collect(),
        }
    }

    /// Structural invariants every partition must satisfy: NL + 1
    /// resolutions (B.5), band count and order per B.9, `precincts` sized
    /// by the (B-16) grid, `blocks` sized by `blocks_wide * blocks_high`.
    fn check_invariants(geometry: &TileComponentGeometry) {
        assert_eq!(geometry.resolutions.len(), usize::from(geometry.levels) + 1);
        for (r, resolution) in geometry.resolutions.iter().enumerate() {
            let expected_kinds: &[BandKind] = if r == 0 {
                &[BandKind::Ll]
            } else {
                &[BandKind::Hl, BandKind::Lh, BandKind::Hh]
            };
            let kinds: Vec<BandKind> = resolution.bands.iter().map(|b| b.kind).collect();
            assert_eq!(kinds, expected_kinds, "band order at r={r} (B.9)");
            // B.5: nb = NL for the r = 0 LL band, NL - r + 1 above it.
            let expected_level = if r == 0 {
                geometry.levels
            } else {
                geometry.levels - r as u8 + 1
            };
            let precinct_count =
                resolution.precincts_wide as usize * resolution.precincts_high as usize;
            for band in &resolution.bands {
                assert_eq!(band.level, expected_level, "band level at r={r}");
                assert_eq!(band.precincts.len(), precinct_count, "precincts at r={r}");
                for precinct in &band.precincts {
                    assert_eq!(
                        precinct.blocks.len(),
                        precinct.blocks_wide as usize * precinct.blocks_high as usize
                    );
                    for block in &precinct.blocks {
                        assert!(!block.is_empty(), "clipped-empty block emitted at r={r}");
                        assert!(block.x0 >= band.rect.x0 && block.x1 <= band.rect.x1);
                        assert!(block.y0 >= band.rect.y0 && block.y1 <= band.rect.y1);
                    }
                }
            }
        }
    }

    /// Every precinct of every band holds exactly this list of rects, one
    /// block per precinct, raster order.
    fn single_blocks(band: &BandGeometry) -> Vec<Rect> {
        band.precincts
            .iter()
            .map(|p| {
                assert_eq!((p.blocks_wide, p.blocks_high), (1, 1));
                p.blocks[0]
            })
            .collect()
    }

    /// The fixture zoo's `rgb-tiled` SIZ (its actual marker values):
    /// 523 x 311 RGB, no offsets, 128 x 128 tiles.
    fn tiled_zoo_siz() -> Siz {
        Siz {
            rsiz: 0,
            xsiz: 523,
            ysiz: 311,
            xosiz: 0,
            yosiz: 0,
            xtsiz: 128,
            ytsiz: 128,
            xtosiz: 0,
            ytosiz: 0,
            components: vec![unit_component(); 3],
        }
    }

    #[test]
    fn tiled_zoo_interior_tile_partition() {
        // rgb-tiled COD: NL = 5, 64 x 64 code-blocks (xcb = ycb = 6), no
        // precinct list so PPx = PPy = 15 everywhere (Table A.13).
        let siz = tiled_zoo_siz();
        // (B-5): numXtiles = ceil(523/128) = ceil(4.09) = 5,
        //        numYtiles = ceil(311/128) = ceil(2.43) = 3.
        assert_eq!(tile_grid(&siz).unwrap(), (5, 3));
        // Tile (0, 0) per (B-7)..(B-10): [0, 128) x [0, 128); XRsiz = 1 so
        // the tile-component rect (B-12) is the same.
        let tile = tile_rect(&siz, 0, 0);
        assert_eq!(tile, rect(0, 0, 128, 128));
        let style = coding_style(5, 6, 6, &[]);
        let geometry = tile_component_geometry(tile, &siz.components[0], &style).unwrap();
        check_invariants(&geometry);
        assert_eq!(geometry.rect, rect(0, 0, 128, 128));
        assert_eq!(geometry.levels, 5);

        // (B-14): trx = ceil(tcx / 2^(5-r)); 128 = 2^7 divides exactly.
        // r = 0: /32 -> [0, 4)^2; r = 1: /16 -> [0, 8)^2; r = 2: [0, 16)^2;
        // r = 3: [0, 32)^2; r = 4: [0, 64)^2; r = 5: [0, 128)^2.
        let sides = [4u32, 8, 16, 32, 64, 128];
        for (r, side) in sides.iter().enumerate() {
            let resolution = &geometry.resolutions[r];
            assert_eq!(resolution.rect, rect(0, 0, *side, *side), "r={r}");
            // No precinct list: PPx = PPy = 15 (Table A.13) and the whole
            // resolution fits one precinct: (B-16) gives
            // ceil(side/32768) - floor(0/32768) = 1.
            assert_eq!((resolution.ppx, resolution.ppy), (15, 15));
            assert_eq!(
                (resolution.precincts_wide, resolution.precincts_high),
                (1, 1)
            );
        }
        // (B-15) at r = 0: LL, nb = 5: ceil(0/32) = 0, ceil(128/32) = 4.
        assert_eq!(geometry.resolutions[0].bands[0].rect, rect(0, 0, 4, 4));
        // r = 1, nb = 5: HL x-corners subtract 2^4 = 16 before /32:
        // x0 = ceil((0-16)/32) = 0, x1 = ceil((128-16)/32) = ceil(3.5) = 4;
        // untouched corners: ceil(0/32) = 0, ceil(128/32) = 4. All three
        // bands land on [0, 4)^2; halving continues down the pyramid:
        // r = 2 (nb = 4): ceil((128-8)/16) = ceil(7.5) = 8 -> [0, 8)^2;
        // r = 3: ceil((128-4)/8) = ceil(15.5) = 16; r = 4: ceil(126/4) =
        // ceil(31.5) = 32; r = 5 (nb = 1): ceil(127/2) = 64 -> [0, 64)^2.
        for (r, side) in [(1u32, 4u32), (2, 8), (3, 16), (4, 32), (5, 64)] {
            for band in &geometry.resolutions[r as usize].bands {
                assert_eq!(band.rect, rect(0, 0, side, side), "r={r}");
                // xcb' = min(6, 15 or 14) = 6 (B-17): one 64 x 64 partition
                // cell [0, 64)^2 covers every band, so each band is a
                // single block clipped to the band rect.
                assert_eq!(single_blocks(band), vec![band.rect], "r={r}");
            }
        }
        assert_eq!(
            single_blocks(&geometry.resolutions[0].bands[0]),
            vec![rect(0, 0, 4, 4)]
        );
    }

    #[test]
    fn tiled_zoo_edge_tile_partition() {
        // Bottom-right partial tile (4, 2) of rgb-tiled:
        // tx0 = max(4*128, 0) = 512, tx1 = min(5*128, 523) = 523,
        // ty0 = 256, ty1 = min(384, 311) = 311 -> [512, 523) x [256, 311).
        let siz = tiled_zoo_siz();
        let tile = tile_rect(&siz, 4, 2);
        assert_eq!(tile, rect(512, 256, 523, 311));
        let style = coding_style(5, 6, 6, &[]);
        let geometry = tile_component_geometry(tile, &siz.components[0], &style).unwrap();
        check_invariants(&geometry);

        // (B-14), all corners ceil-divided by 2^(5-r) and NOT renormalized:
        // r=0: x: ceil(512/32)=16, ceil(523/32)=ceil(16.34)=17;
        //      y: ceil(256/32)=8,  ceil(311/32)=ceil(9.72)=10.
        // r=1: x: 32, ceil(523/16)=ceil(32.69)=33; y: 16, ceil(19.44)=20.
        // r=2: x: 64, ceil(523/8)=ceil(65.375)=66; y: 32, ceil(38.875)=39.
        // r=3: x: 128, ceil(523/4)=ceil(130.75)=131; y: 64, ceil(77.75)=78.
        // r=4: x: 256, ceil(523/2)=ceil(261.5)=262; y: 128, ceil(155.5)=156.
        // r=5: the tile-component rect itself.
        let expected_resolutions = [
            rect(16, 8, 17, 10),
            rect(32, 16, 33, 20),
            rect(64, 32, 66, 39),
            rect(128, 64, 131, 78),
            rect(256, 128, 262, 156),
            rect(512, 256, 523, 311),
        ];
        for (r, expected) in expected_resolutions.iter().enumerate() {
            assert_eq!(&geometry.resolutions[r].rect, expected, "r={r}");
            assert_eq!(
                (
                    geometry.resolutions[r].precincts_wide,
                    geometry.resolutions[r].precincts_high
                ),
                (1, 1),
                "r={r}"
            );
        }

        // (B-15) band rects. r=1, nb=5 (offset 2^4 = 16, divisor 32):
        // HL x: ceil((512-16)/32) = ceil(15.5) = 16, ceil((523-16)/32) =
        //       ceil(15.84) = 16 -> [16, 16) EMPTY (width 0);
        //    y: ceil(256/32) = 8, ceil(311/32) = 10.
        // LH x: ceil(512/32) = 16, ceil(523/32) = 17;
        //    y: ceil((256-16)/32) = ceil(7.5) = 8, ceil((311-16)/32) =
        //       ceil(9.22) = 10.
        // HH: empty x like HL, y like LH.
        let bands1 = &geometry.resolutions[1].bands;
        assert_eq!(bands1[0].rect, rect(16, 8, 16, 10));
        assert!(bands1[0].rect.is_empty());
        assert_eq!(bands1[1].rect, rect(16, 8, 17, 10));
        assert_eq!(bands1[2].rect, rect(16, 8, 16, 10));
        // Empty bands still carry one PrecinctBand per (B-16) precinct,
        // holding zero code-blocks (B.9: no representation in the packet).
        assert_eq!(bands1[0].precincts.len(), 1);
        assert_eq!(
            (
                bands1[0].precincts[0].blocks_wide,
                bands1[0].precincts[0].blocks_high
            ),
            (0, 0)
        );
        assert!(bands1[0].precincts[0].blocks.is_empty());
        // The LH band's single 64 x 64 partition cell is clipped to the
        // band: floor(16/64) = 0, ceil(17/64) = 1 -> one block, the rect.
        assert_eq!(single_blocks(&bands1[1]), vec![rect(16, 8, 17, 10)]);

        // r=2, nb=4 (offset 8, divisor 16):
        // HL x: ceil((512-8)/16) = ceil(31.5) = 32,
        //       ceil((523-8)/16) = ceil(32.19) = 33; y: 16, 20.
        // LH x: 32, 33; y: ceil((256-8)/16) = ceil(15.5) = 16,
        //       ceil((311-8)/16) = ceil(18.94) = 19.
        let bands2 = &geometry.resolutions[2].bands;
        assert_eq!(bands2[0].rect, rect(32, 16, 33, 20));
        assert_eq!(bands2[1].rect, rect(32, 16, 33, 19));
        assert_eq!(bands2[2].rect, rect(32, 16, 33, 19));

        // r=5, nb=1 (offset 1, divisor 2):
        // HL x: ceil(511/2) = 256, ceil(522/2) = 261; y: 128, ceil(155.5) = 156.
        // LH x: 256, ceil(261.5) = 262; y: ceil(255/2) = 128, ceil(310/2) = 155.
        let bands5 = &geometry.resolutions[5].bands;
        assert_eq!(bands5[0].rect, rect(256, 128, 261, 156));
        assert_eq!(bands5[1].rect, rect(256, 128, 262, 155));
        assert_eq!(bands5[2].rect, rect(256, 128, 261, 155));
        // Code-block partition anchored at (0, 0) of the BAND (B.7): the
        // HL band spans partition cell x: floor(256/64) = 4 to
        // ceil(261/64) = 5, y: floor(128/64) = 2 to ceil(156/64) = 3 ->
        // one block, clipped to the absolute band rect.
        assert_eq!(single_blocks(&bands5[0]), vec![rect(256, 128, 261, 156)]);
        assert_eq!(single_blocks(&bands5[1]), vec![rect(256, 128, 262, 155)]);
    }

    #[test]
    fn offset_odd_tile_component_partition() {
        // Hand-built offset case: image area offset (5, 3), odd-ended
        // tile-component [5, 101) x [3, 69), NL = 2, 8 x 4 code-blocks
        // (xcb = 3, ycb = 2), explicit per-resolution precincts
        // r0: (3, 3), r1: (4, 4), r2: (4, 3) (Table A.21).
        let tile = rect(5, 3, 101, 69);
        let style = coding_style(2, 3, 2, &[(3, 3), (4, 4), (4, 3)]);
        let geometry = tile_component_geometry(tile, &unit_component(), &style).unwrap();
        check_invariants(&geometry);
        assert_eq!(geometry.rect, rect(5, 3, 101, 69));

        // (B-14):
        // r=0 (/4): x: ceil(5/4)=2,  ceil(101/4)=ceil(25.25)=26;
        //           y: ceil(3/4)=1,  ceil(69/4)=ceil(17.25)=18.
        // r=1 (/2): x: ceil(5/2)=3,  ceil(101/2)=51; y: 2, ceil(34.5)=35.
        // r=2 (/1): the tile-component rect.
        assert_eq!(geometry.resolutions[0].rect, rect(2, 1, 26, 18));
        assert_eq!(geometry.resolutions[1].rect, rect(3, 2, 51, 35));
        assert_eq!(geometry.resolutions[2].rect, rect(5, 3, 101, 69));

        // (B-16) precinct grids:
        // r=0, 2^3=8: wide = ceil(26/8) - floor(2/8) = 4 - 0 = 4;
        //             high = ceil(18/8) - floor(1/8) = 3 - 0 = 3.
        // r=1, 2^4=16: wide = ceil(51/16) - floor(3/16) = 4 - 0 = 4;
        //              high = ceil(35/16) - floor(2/16) = 3 - 0 = 3.
        // r=2, x 2^4=16: wide = ceil(101/16) - floor(5/16) = 7 - 0 = 7;
        //      y 2^3=8:  high = ceil(69/8)  - floor(3/8)  = 9 - 0 = 9.
        let grids = [(3u8, 3u8, 4u32, 3u32), (4, 4, 4, 3), (4, 3, 7, 9)];
        for (r, (ppx, ppy, wide, high)) in grids.iter().enumerate() {
            let resolution = &geometry.resolutions[r];
            assert_eq!((resolution.ppx, resolution.ppy), (*ppx, *ppy), "r={r}");
            assert_eq!(
                (resolution.precincts_wide, resolution.precincts_high),
                (*wide, *high),
                "r={r}"
            );
        }

        // (B-15) band rects at every level.
        // r=0 LL (nb=2): the resolution rect.
        assert_eq!(geometry.resolutions[0].bands[0].rect, rect(2, 1, 26, 18));
        // r=1, nb=2 (offset 2^1=2, divisor 4):
        // HL x: ceil((5-2)/4)=ceil(0.75)=1, ceil((101-2)/4)=ceil(24.75)=25;
        //    y: ceil(3/4)=1, ceil(69/4)=18.
        // LH x: ceil(5/4)=2, ceil(101/4)=26;
        //    y: ceil((3-2)/4)=ceil(0.25)=1, ceil((69-2)/4)=ceil(16.75)=17.
        // HH: x from HL, y from LH.
        let bands1 = &geometry.resolutions[1].bands;
        assert_eq!(bands1[0].rect, rect(1, 1, 25, 18));
        assert_eq!(bands1[1].rect, rect(2, 1, 26, 17));
        assert_eq!(bands1[2].rect, rect(1, 1, 25, 17));
        // r=2, nb=1 (offset 1, divisor 2):
        // HL x: ceil(4/2)=2, ceil(100/2)=50; y: ceil(3/2)=2, ceil(69/2)=35.
        // LH x: ceil(5/2)=3, ceil(101/2)=51; y: ceil(2/2)=1, ceil(68/2)=34.
        // HH: [2, 50) x [1, 34).
        let bands2 = &geometry.resolutions[2].bands;
        assert_eq!(bands2[0].rect, rect(2, 2, 50, 35));
        assert_eq!(bands2[1].rect, rect(3, 1, 51, 34));
        assert_eq!(bands2[2].rect, rect(2, 1, 50, 34));

        // r=0 LL code-blocks: xcb' = min(3, PPx=3) = 3, ycb' = min(2, 3)
        // = 2 (B-17/B-18) -> 8 x 4 blocks anchored at (0, 0) of the band.
        // Precinct (0, 0) covers [0, 8) x [0, 8), clipped to the band ->
        // [2, 8) x [1, 8); block rows floor(1/4)=0 to ceil(8/4)=2 -> two
        // blocks stacked: [2, 8) x [1, 4) and [2, 8) x [4, 8).
        let ll = &geometry.resolutions[0].bands[0];
        let p0 = &ll.precincts[0];
        assert_eq!((p0.blocks_wide, p0.blocks_high), (1, 2));
        assert_eq!(p0.blocks, vec![rect(2, 1, 8, 4), rect(2, 4, 8, 8)]);
        // Interior precinct (1, 1) (raster index 5): [8, 16) x [8, 16) is
        // fully inside the band; rows floor(8/4)=2 to ceil(16/4)=4.
        let p5 = &ll.precincts[5];
        assert_eq!((p5.blocks_wide, p5.blocks_high), (1, 2));
        assert_eq!(p5.blocks, vec![rect(8, 8, 16, 12), rect(8, 12, 16, 16)]);
        // Last precinct (3, 2) (raster index 11): [24, 32) x [16, 24)
        // clipped to [24, 26) x [16, 18) -> a single clipped block.
        let p11 = &ll.precincts[11];
        assert_eq!((p11.blocks_wide, p11.blocks_high), (1, 1));
        assert_eq!(p11.blocks, vec![rect(24, 16, 26, 18)]);

        // r=1 HL: the r > 0 halving rule (B.6/B.7): PPx = 4 on the
        // resolution grid becomes 2^3 = 8 in band coordinates, anchored at
        // 0. xcb' = min(3, 4-1) = 3, ycb' = min(2, 3) = 2 -> 8 x 4 blocks.
        // Precinct (0, 0): [0, 8) x [0, 8) ∩ [1, 25) x [1, 18) =
        // [1, 8) x [1, 8) -> blocks [1, 8) x [1, 4), [1, 8) x [4, 8).
        let hl1 = &bands1[0];
        assert_eq!(
            hl1.precincts[0].blocks,
            vec![rect(1, 1, 8, 4), rect(1, 4, 8, 8)]
        );
        // Precinct (3, 2) (raster 11): [24, 32) x [16, 24) ∩ band =
        // [24, 25) x [16, 18) -> one clipped block.
        assert_eq!(hl1.precincts[11].blocks, vec![rect(24, 16, 25, 18)]);

        // r=2 HL: band-domain precinct spans 2^(4-1)=8 wide, 2^(3-1)=4
        // high. xcb' = min(3, 3) = 3, ycb' = min(2, 2) = 2: every span
        // equals one block. Precinct (0, 0): [0, 8) x [0, 4) ∩
        // [2, 50) x [2, 35) = [2, 8) x [2, 4).
        let hl2 = &bands2[0];
        assert_eq!(hl2.precincts[0].blocks, vec![rect(2, 2, 8, 4)]);
        // Last precinct (6, 8) (raster 62): [48, 56) x [32, 36) ∩ band =
        // [48, 50) x [32, 35).
        assert_eq!(hl2.precincts[62].blocks, vec![rect(48, 32, 50, 35)]);
        // LH last precinct: [48, 56) x [32, 36) ∩ [3, 51) x [1, 34) =
        // [48, 51) x [32, 34).
        assert_eq!(bands2[1].precincts[62].blocks, vec![rect(48, 32, 51, 34)]);
        // Every r=2 precinct of every band holds exactly one block.
        for band in bands2 {
            assert_eq!(single_blocks(band).len(), 63);
        }
    }

    #[test]
    fn precinct_zoo_partition() {
        // The fixture zoo's rgb-precinct COD: single 523 x 311 tile,
        // NL = 5, xcb = ycb = 6, per-resolution precinct exponents
        // (PPx, PPy) = (2,2), (3,3), (4,4), (5,5), (6,6), (7,7).
        let tile = rect(0, 0, 523, 311);
        let exps = [(2, 2), (3, 3), (4, 4), (5, 5), (6, 6), (7, 7)];
        let style = coding_style(5, 6, 6, &exps);
        let geometry = tile_component_geometry(tile, &unit_component(), &style).unwrap();
        check_invariants(&geometry);

        // (B-14) resolution rects (origin 0, so only the far corners move):
        // r=0: ceil(523/32)=17, ceil(311/32)=10; r=1: 33, 20; r=2: 66, 39;
        // r=3: 131, 78; r=4: 262, 156; r=5: 523, 311.
        // (B-16): every level spans 5 x 3 precincts:
        // r=0: ceil(17/4)=ceil(4.25)=5,   ceil(10/4)=ceil(2.5)=3.
        // r=1: ceil(33/8)=ceil(4.125)=5,  ceil(20/8)=ceil(2.5)=3.
        // r=2: ceil(66/16)=ceil(4.125)=5, ceil(39/16)=ceil(2.44)=3.
        // r=3: ceil(131/32)=ceil(4.09)=5, ceil(78/32)=ceil(2.44)=3.
        // r=4: ceil(262/64)=ceil(4.09)=5, ceil(156/64)=ceil(2.44)=3.
        // r=5: ceil(523/128)=ceil(4.09)=5, ceil(311/128)=ceil(2.43)=3.
        let far = [
            (17u32, 10u32),
            (33, 20),
            (66, 39),
            (131, 78),
            (262, 156),
            (523, 311),
        ];
        for (r, (x1, y1)) in far.iter().enumerate() {
            let resolution = &geometry.resolutions[r];
            assert_eq!(resolution.rect, rect(0, 0, *x1, *y1), "r={r}");
            let (ppx, ppy) = exps[r];
            assert_eq!((resolution.ppx, resolution.ppy), (ppx, ppy), "r={r}");
            assert_eq!(
                (resolution.precincts_wide, resolution.precincts_high),
                (5, 3),
                "r={r}"
            );
        }

        // r=0 LL [0, 17) x [0, 10): xcb' = min(6, 2) = 2 -> 4 x 4 blocks,
        // exactly one per 4 x 4 precinct. Precinct (4, 0) (raster 4):
        // [16, 20) x [0, 4) clipped to [16, 17) x [0, 4); precinct (4, 2)
        // (raster 14): [16, 17) x [8, 10).
        let ll = &geometry.resolutions[0].bands[0];
        let ll_blocks = single_blocks(ll);
        assert_eq!(ll_blocks.len(), 15);
        assert_eq!(ll_blocks[0], rect(0, 0, 4, 4));
        assert_eq!(ll_blocks[4], rect(16, 0, 17, 4));
        assert_eq!(ll_blocks[14], rect(16, 8, 17, 10));

        // r=1, nb=5: HL [0, ceil((523-16)/32)=ceil(15.84)=16) x [0, 10);
        // LH [0, 17) x [0, ceil((311-16)/32)=ceil(9.22)=10);
        // HH [0, 16) x [0, 10).
        let bands1 = &geometry.resolutions[1].bands;
        assert_eq!(bands1[0].rect, rect(0, 0, 16, 10));
        assert_eq!(bands1[1].rect, rect(0, 0, 17, 10));
        assert_eq!(bands1[2].rect, rect(0, 0, 16, 10));
        // Halving rule: PPx = 3 -> 4-wide spans in band coordinates. The
        // fifth precinct column spans [16, 20), which misses the 16-wide
        // HL and HH bands entirely: raster indices 4, 9, 14 are empty for
        // HL/HH but hold one block for the 17-wide LH.
        for band in [&bands1[0], &bands1[2]] {
            for idx in [4usize, 9, 14] {
                assert_eq!(band.precincts[idx].blocks_wide, 0, "idx={idx}");
                assert!(band.precincts[idx].blocks.is_empty(), "idx={idx}");
            }
            // Precinct (3, 2) (raster 13): [12, 16) x [8, 12) ∩ band =
            // [12, 16) x [8, 10) -> one 4 x 4 partition cell, clipped.
            assert_eq!(band.precincts[13].blocks, vec![rect(12, 8, 16, 10)]);
        }
        assert_eq!(bands1[1].precincts[4].blocks, vec![rect(16, 0, 17, 4)]);
        assert_eq!(bands1[1].precincts[14].blocks, vec![rect(16, 8, 17, 10)]);

        // r=5, nb=1: HL [0, ceil(522/2)=261) x [0, ceil(311/2)=156);
        // LH [0, ceil(523/2)=262) x [0, ceil(310/2)=155); HH 261 x 155.
        // PPx = 7 halves to 64-wide band spans; xcb' = min(6, 6) = 6 ->
        // 64 x 64 blocks, one per precinct. Raster 14 = column 4, row 2:
        // [256, 320) x [128, 192) clipped to each band.
        let bands5 = &geometry.resolutions[5].bands;
        assert_eq!(bands5[0].rect, rect(0, 0, 261, 156));
        assert_eq!(bands5[1].rect, rect(0, 0, 262, 155));
        assert_eq!(bands5[2].rect, rect(0, 0, 261, 155));
        assert_eq!(
            bands5[0].precincts[14].blocks,
            vec![rect(256, 128, 261, 156)]
        );
        assert_eq!(
            bands5[1].precincts[14].blocks,
            vec![rect(256, 128, 262, 155)]
        );
        assert_eq!(
            bands5[2].precincts[14].blocks,
            vec![rect(256, 128, 261, 155)]
        );
    }

    #[test]
    fn precinct_anchor_is_absolute() {
        // The precinct partition anchors at (0, 0) of the resolution grid
        // (B.6), NOT at the tile-component corner. NL = 0 keeps r = 0 the
        // only level, so the LL band is the tile-component [100, 120) x
        // [70, 80) itself. PPx = PPy = 3: (B-16) gives
        // wide = ceil(120/8) - floor(100/8) = 15 - 12 = 3,
        // high = ceil(80/8) - floor(70/8) = 10 - 8 = 2.
        let tile = rect(100, 70, 120, 80);
        let style = coding_style(0, 2, 2, &[(3, 3)]);
        let geometry = tile_component_geometry(tile, &unit_component(), &style).unwrap();
        check_invariants(&geometry);
        assert_eq!(geometry.levels, 0);
        assert_eq!(geometry.resolutions.len(), 1);
        let resolution = &geometry.resolutions[0];
        assert_eq!(resolution.rect, rect(100, 70, 120, 80));
        assert_eq!(
            (resolution.precincts_wide, resolution.precincts_high),
            (3, 2)
        );
        let ll = &resolution.bands[0];
        assert_eq!(ll.kind, BandKind::Ll);
        assert_eq!(ll.level, 0);
        assert_eq!(ll.rect, rect(100, 70, 120, 80));
        // xcb' = min(2, 3) = 2 -> 4 x 4 blocks anchored at (0, 0) too.
        // Precinct (12, 8) in partition indices covers [96, 104) x
        // [64, 72), clipped to [100, 104) x [70, 72): block columns
        // floor(100/4) = 25 to ceil(104/4) = 26 -> one clipped block.
        assert_eq!(ll.precincts[0].blocks, vec![rect(100, 70, 104, 72)]);
        // Precinct (13, 8): [104, 112) x [70, 72): columns floor(104/4) =
        // 26 to ceil(112/4) = 28 -> two blocks side by side.
        assert_eq!(
            (ll.precincts[1].blocks_wide, ll.precincts[1].blocks_high),
            (2, 1)
        );
        assert_eq!(
            ll.precincts[1].blocks,
            vec![rect(104, 70, 108, 72), rect(108, 70, 112, 72)]
        );
        // Precinct (14, 9): [112, 120) x [72, 80) sits fully inside the
        // band: a full 2 x 2 block grid in raster order.
        assert_eq!(
            ll.precincts[5].blocks,
            vec![
                rect(112, 72, 116, 76),
                rect(116, 72, 120, 76),
                rect(112, 76, 116, 80),
                rect(116, 76, 120, 80),
            ]
        );
    }

    #[test]
    fn degenerate_single_sample_lands_in_hh() {
        // A 1 x 1 tile-component at odd absolute coordinates (5, 3): the
        // absolute parity decides band membership (B.5/F.3.3). NL = 2.
        // r=0 (/4): x: ceil(5/4)=2 = ceil(6/4) -> [2, 2) EMPTY;
        //           y: ceil(3/4)=1 = ceil(4/4) -> empty. (B-16): 0
        //           precincts -> no packets at r = 0 (B.6).
        // r=1 (/2): x: ceil(5/2)=3 = ceil(6/2) -> empty; same for bands.
        // r=2: [5, 6) x [3, 4). HL x: ceil(4/2)=2, ceil(5/2)=3 -> [2, 3);
        //      y: ceil(3/2)=2 = ceil(4/2) -> EMPTY. LH x empty. HH:
        //      x [2, 3), y: ceil(2/2)=1, ceil(3/2)=2 -> [1, 2): the one
        //      sample is an HH coefficient at absolute (2, 1).
        let tile = rect(5, 3, 6, 4);
        let style = coding_style(2, 6, 6, &[]);
        let geometry = tile_component_geometry(tile, &unit_component(), &style).unwrap();
        check_invariants(&geometry);
        assert_eq!(geometry.resolutions[0].rect, rect(2, 1, 2, 1));
        assert!(geometry.resolutions[0].rect.is_empty());
        assert_eq!(
            (
                geometry.resolutions[0].precincts_wide,
                geometry.resolutions[0].precincts_high
            ),
            (0, 0)
        );
        assert!(geometry.resolutions[0].bands[0].precincts.is_empty());
        assert_eq!(
            (
                geometry.resolutions[1].precincts_wide,
                geometry.resolutions[1].precincts_high
            ),
            (0, 0)
        );
        for band in &geometry.resolutions[1].bands {
            assert!(band.rect.is_empty());
            assert!(band.precincts.is_empty());
        }
        let resolution2 = &geometry.resolutions[2];
        assert_eq!(resolution2.rect, rect(5, 3, 6, 4));
        assert_eq!(
            (resolution2.precincts_wide, resolution2.precincts_high),
            (1, 1)
        );
        let bands2 = &resolution2.bands;
        assert_eq!(bands2[0].rect, rect(2, 2, 3, 2));
        assert!(bands2[0].rect.is_empty());
        assert!(bands2[1].rect.is_empty());
        assert_eq!(bands2[2].rect, rect(2, 1, 3, 2));
        assert_eq!(bands2[0].precincts[0].blocks_wide, 0);
        assert!(bands2[0].precincts[0].blocks.is_empty());
        assert_eq!(bands2[2].precincts[0].blocks, vec![rect(2, 1, 3, 2)]);
    }

    #[test]
    fn extreme_coordinates_do_not_overflow() {
        // Corners near u32::MAX must go through checked u64 arithmetic.
        // Tile-component [4294967000, 4294967295) x [4294967293,
        // 4294967295), NL = 5, default maximal precincts, 64 x 64 blocks.
        // r=5 HL (nb=1): x0 = ceil((4294967000-1)/2) = ceil(2147483499.5)
        // = 2147483500; x1 = ceil((4294967295-1)/2) = 2147483647;
        // y0 = ceil(4294967293/2) = 2147483647, y1 = ceil(4294967295/2) =
        // 2147483648. Block columns floor(2147483500/64) = 33554429
        // (33554429*64 = 2147483456) to ceil(2147483647/64) = 33554432 ->
        // three blocks; one row (floor(2147483647/64) = 33554431,
        // ceil(2147483648/64) = 33554432).
        let tile = rect(4294967000, 4294967293, 4294967295, 4294967295);
        let style = coding_style(5, 6, 6, &[]);
        let geometry = tile_component_geometry(tile, &unit_component(), &style).unwrap();
        check_invariants(&geometry);
        let hl5 = &geometry.resolutions[5].bands[0];
        assert_eq!(
            hl5.rect,
            rect(2147483500, 2147483647, 2147483647, 2147483648)
        );
        let p0 = &hl5.precincts[0];
        assert_eq!((p0.blocks_wide, p0.blocks_high), (3, 1));
        assert_eq!(
            p0.blocks,
            vec![
                rect(2147483500, 2147483647, 2147483520, 2147483648),
                rect(2147483520, 2147483647, 2147483584, 2147483648),
                rect(2147483584, 2147483647, 2147483647, 2147483648),
            ]
        );
    }

    #[test]
    fn rejects_zero_precinct_exponent_above_r0() {
        // B.6: PPx and PPy must be at least 1 for every r except r = 0.
        let tile = rect(0, 0, 64, 64);
        let style = coding_style(2, 6, 6, &[(0, 0), (0, 3), (3, 3)]);
        let outcome = tile_component_geometry(tile, &unit_component(), &style);
        assert!(matches!(outcome, Err(JpxError::Malformed(msg)) if msg.contains("precinct")));
    }

    #[test]
    fn rejects_short_precinct_list() {
        // Table A.15/A.21: one precinct-size byte per resolution level,
        // NL + 1 of them, when Scod bit 0 signals explicit precincts.
        let tile = rect(0, 0, 64, 64);
        let style = coding_style(2, 6, 6, &[(3, 3)]);
        let outcome = tile_component_geometry(tile, &unit_component(), &style);
        assert!(matches!(outcome, Err(JpxError::Malformed(msg)) if msg.contains("precinct")));
    }

    /// The worked example of T.800 B.4: reference grid (1432, 954), image
    /// offset (152, 234), tile size (396, 297), tile offset (0, 0), two
    /// components with separations (1, 1) and (2, 2).
    fn example_siz() -> Siz {
        Siz {
            rsiz: 0,
            xsiz: 1432,
            ysiz: 954,
            xosiz: 152,
            yosiz: 234,
            xtsiz: 396,
            ytsiz: 297,
            xtosiz: 0,
            ytosiz: 0,
            components: vec![
                SizComponent {
                    depth: 8,
                    signed: false,
                    xrsiz: 1,
                    yrsiz: 1,
                },
                SizComponent {
                    depth: 8,
                    signed: false,
                    xrsiz: 2,
                    yrsiz: 2,
                },
            ],
        }
    }

    #[test]
    fn ceil_div_matches_the_b4_example() {
        // B.4: numXtiles = ceil(1432/396) = 4, numYtiles = ceil(954/297) = 4.
        assert_eq!(ceil_div(1432, 396), 4);
        assert_eq!(ceil_div(954, 297), 4);
        // Exact division has no remainder to round: 792/396 = 2.
        assert_eq!(ceil_div(792, 396), 2);
        assert_eq!(ceil_div(0, 7), 0);
        // Near the u64 top: (2^64 - 1) / 1 must not overflow.
        assert_eq!(ceil_div(u64::MAX, 1), u64::MAX);
    }

    #[test]
    fn rect_dimensions_follow_b2() {
        // (B-2): width = x1 - x0 = 396 - 152 = 244, height = 297 - 234 = 63
        // (the B.4 example's tile (0, 0) in component 0).
        let rect = Rect {
            x0: 152,
            y0: 234,
            x1: 396,
            y1: 297,
        };
        assert_eq!(rect.width(), 244);
        assert_eq!(rect.height(), 63);
        assert!(!rect.is_empty());
        let degenerate = Rect {
            x0: 5,
            y0: 5,
            x1: 5,
            y1: 9,
        };
        assert_eq!(degenerate.width(), 0);
        assert!(degenerate.is_empty());
    }

    #[test]
    fn tile_grid_matches_the_b4_example() {
        // B.4: numXtiles = 4, numYtiles = 4 (16 tiles).
        assert_eq!(tile_grid(&example_siz()).unwrap(), (4, 4));
    }

    #[test]
    fn tile_grid_rejects_hostile_siz_fields() {
        let mut siz = example_siz();
        siz.xtsiz = 0;
        assert!(matches!(tile_grid(&siz), Err(JpxError::Malformed(msg)) if msg.contains("tile")));
    }

    #[test]
    fn tile_rects_match_the_b4_example() {
        // B.4 lists tx0(0:3) = {152, 396, 792, 1188}, tx1(0:3) = {396, 792,
        // 1188, 1432}, ty0(0:3) = {234, 297, 594, 891}, ty1(0:3) = {297,
        // 594, 891, 954}.
        let siz = example_siz();
        assert_eq!(
            tile_rect(&siz, 0, 0),
            Rect {
                x0: 152,
                y0: 234,
                x1: 396,
                y1: 297
            }
        );
        assert_eq!(
            tile_rect(&siz, 1, 1),
            Rect {
                x0: 396,
                y0: 297,
                x1: 792,
                y1: 594
            }
        );
        assert_eq!(
            tile_rect(&siz, 3, 3),
            Rect {
                x0: 1188,
                y0: 891,
                x1: 1432,
                y1: 954
            }
        );
    }

    #[test]
    fn component_rect_matches_the_b4_example() {
        // B.4, component 1 with (XRsiz, YRsiz) = (2, 2):
        // image area maps to (ceil(152/2), ceil(234/2)) .. (ceil(1432/2),
        // ceil(954/2)) = (76, 117) .. (716, 477): 640 x 360 samples.
        let siz = example_siz();
        let image = Rect {
            x0: siz.xosiz,
            y0: siz.yosiz,
            x1: siz.xsiz,
            y1: siz.ysiz,
        };
        let comp1 = component_rect(image, 2, 2).unwrap();
        assert_eq!(
            comp1,
            Rect {
                x0: 76,
                y0: 117,
                x1: 716,
                y1: 477
            }
        );
        assert_eq!(comp1.width(), 640);
        assert_eq!(comp1.height(), 360);
        // Tile (0, 0) in component 1 is 122 x 32 anchored at (76, 117)
        // (Figure B.7): ceil(396/2) = 198, ceil(297/2) = 149.
        let tile00 = component_rect(tile_rect(&siz, 0, 0), 2, 2).unwrap();
        assert_eq!(
            tile00,
            Rect {
                x0: 76,
                y0: 117,
                x1: 198,
                y1: 149
            }
        );
        // Tile (1, 2) in component 1 is 198 x 149 (Figure B.7):
        // y range ceil(594/2) = 297 .. ceil(891/2) = 446.
        let tile12 = component_rect(tile_rect(&siz, 1, 2), 2, 2).unwrap();
        assert_eq!(
            tile12,
            Rect {
                x0: 198,
                y0: 297,
                x1: 396,
                y1: 446
            }
        );
        assert_eq!(tile12.width(), 198);
        assert_eq!(tile12.height(), 149);
    }

    #[test]
    fn component_rect_rejects_zero_separation() {
        let area = Rect {
            x0: 0,
            y0: 0,
            x1: 8,
            y1: 8,
        };
        assert!(component_rect(area, 0, 1).is_err());
    }
}
