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
    let _ = (rect, style.decomposition_levels);
    Err(JpxError::Unsupported("decoder scaffold"))
}

#[cfg(test)]
mod tests {
    use super::*;

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
