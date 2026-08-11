//! Scanline coverage rasterizer: per-pixel coverage accumulation from
//! polygon edges, nonzero and even-odd fill rules, and coverage-mask
//! clipping.

use crate::path::Subpath;
use crate::Pixmap;

/// Vertical subsamples per pixel row; horizontal coverage is analytic.
const SUBSAMPLES: u32 = 4;

/// Which interior rule decides what a path encloses.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FillRule {
    /// Nonzero winding number.
    NonZero,
    /// Even-odd (parity) rule.
    EvenOdd,
}

/// Reusable rasterizer buffers, owned by the caller so a page of fills does
/// not re-allocate (and re-zero) them on every call. `row` is all-zero
/// between calls; the sweep clears exactly the slots it dirtied.
#[derive(Debug, Default)]
pub(crate) struct RasterScratch {
    /// Per-row coverage accumulator, at least page-width long.
    row: Vec<f32>,
    /// Edge list of the path being rasterized.
    edges: Vec<Edge>,
    /// Active-edge indices for the current scanline.
    active: Vec<usize>,
    /// Scanline crossings as `(x, winding direction)`.
    crossings: Vec<(f32, i32)>,
}

/// A per-pixel coverage mask (0 = fully clipped out, 255 = fully visible)
/// over a page of `width * height` device pixels.
///
/// The coverage is stored only for its bounding box `[x0, x0+bbox_w) x
/// [y0, y0+bbox_h)`; every pixel outside that box reads as 0. A form field's
/// clip path is typically a small fraction of the page, so this keeps
/// `from_path`/`intersect` proportional to the clip's own size instead of
/// the whole page — real documents can carry hundreds of clips per page, so
/// an O(page) cost per clip (a naive full-page buffer) dominates render time
/// even though each clip only ever restricts a small region.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Mask {
    pub width: u32,
    pub height: u32,
    /// Left edge of the stored region, in device pixels.
    pub x0: u32,
    /// Top edge of the stored region, in device pixels.
    pub y0: u32,
    /// Width of the stored region (0 means the mask covers nothing).
    pub bbox_w: u32,
    /// Height of the stored region.
    pub bbox_h: u32,
    /// Row-major coverage values over the bbox, `bbox_w * bbox_h` bytes.
    pub data: Vec<u8>,
    /// Every stored byte is 255 (proven at construction, conservatively
    /// false otherwise). Lets a fill skip the per-pixel coverage multiply —
    /// scaling by `255/255.0 == 1.0` is exactly the identity — and treat the
    /// clip as pure bbox narrowing. Rectangular clips on integer device
    /// coordinates (the page-bounds reset clip most generators emit) are the
    /// common case.
    pub opaque: bool,
}

impl Mask {
    /// Creates an all-zero (fully clipped) mask covering the whole page.
    pub(crate) fn new(width: u32, height: u32) -> Mask {
        Mask {
            width,
            height,
            x0: 0,
            y0: 0,
            bbox_w: width,
            bbox_h: height,
            data: vec![0; width as usize * height as usize],
            opaque: false,
        }
    }

    /// A zero-cost mask that covers no pixels at all (every lookup is 0).
    fn empty(width: u32, height: u32) -> Mask {
        Mask {
            width,
            height,
            x0: 0,
            y0: 0,
            bbox_w: 0,
            bbox_h: 0,
            data: Vec::new(),
            opaque: false,
        }
    }

    /// Rasterizes `polys` under `rule` into a fresh mask sized to `polys`'
    /// own bounding box (clamped to the page), not the full page.
    pub(crate) fn from_path(
        width: u32,
        height: u32,
        scratch: &mut RasterScratch,
        polys: &[Subpath],
        rule: FillRule,
    ) -> Mask {
        prepare_edges(&mut scratch.edges, polys);
        if scratch.edges.is_empty() || width == 0 || height == 0 {
            return Mask::empty(width, height);
        }
        let mut xmin = f32::MAX;
        let mut xmax = f32::MIN;
        let mut ymin = f32::MAX;
        let mut ymax = f32::MIN;
        for e in &scratch.edges {
            xmin = xmin.min(e.x0).min(e.x1);
            xmax = xmax.max(e.x0).max(e.x1);
            ymin = ymin.min(e.y0);
            ymax = ymax.max(e.y1);
        }
        let bx0 = xmin.floor().max(0.0) as u32;
        let bx1 = (xmax.ceil().max(0.0) as u32).min(width);
        let by0 = ymin.floor().max(0.0) as u32;
        let by1 = (ymax.ceil().max(0.0) as u32).min(height);
        if bx1 <= bx0 || by1 <= by0 {
            return Mask::empty(width, height);
        }
        let bbox_w = bx1 - bx0;
        let bbox_h = by1 - by0;
        let mut mask = Mask {
            width,
            height,
            x0: bx0,
            y0: by0,
            bbox_w,
            bbox_h,
            data: vec![0u8; bbox_w as usize * bbox_h as usize],
            opaque: false,
        };
        let bw = bbox_w as usize;
        sweep_rows(scratch, width, height, rule, |y, row, lo, hi| {
            // `lo`/`hi` are columns touched on this row, which `coverage_rows`
            // only ever derives from crossings between edges already bounded
            // by `[xmin, xmax]` — so they always fall within `[bx0, bx1)`.
            let base = (y - by0) as usize * bw;
            let local_lo = lo - bx0 as usize;
            let local_hi = hi - bx0 as usize;
            let dst = &mut mask.data[base + local_lo..base + local_hi];
            for (cov, out) in row[lo..hi].iter().zip(dst.iter_mut()) {
                *out = (cov.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
            }
        });
        mask.opaque = mask.data.iter().all(|&b| b == 255);
        mask
    }

    /// Coverage at device pixel `(x, y)`; 0 outside the stored bbox.
    #[inline]
    pub(crate) fn coverage(&self, x: u32, y: u32) -> u8 {
        if x < self.x0 || y < self.y0 {
            return 0;
        }
        let (lx, ly) = (x - self.x0, y - self.y0);
        if lx >= self.bbox_w || ly >= self.bbox_h {
            return 0;
        }
        self.data[ly as usize * self.bbox_w as usize + lx as usize]
    }

    /// Intersects this mask with `other` by taking the per-pixel minimum.
    /// The result is stored over just the overlap of the two bboxes (which
    /// can only shrink or stay the same size), not the full page — a chain
    /// of nested small clips stays cheap instead of re-touching every pixel
    /// on the page at each nesting level. The masks must belong to pages of
    /// identical dimensions.
    pub(crate) fn intersect(&mut self, other: &Mask) {
        *self = Mask::intersected(self, other);
    }

    /// Like [`Mask::intersect`], but takes both masks by reference and
    /// returns a fresh one — lets a caller holding `a` behind an `Arc` (e.g. a
    /// cached rasterization) compute the overlap without first cloning `a`'s
    /// full buffer just to shrink it back down.
    pub(crate) fn intersected(a: &Mask, b: &Mask) -> Mask {
        debug_assert_eq!((a.width, a.height), (b.width, b.height));
        let x0 = a.x0.max(b.x0);
        let y0 = a.y0.max(b.y0);
        let x1 = (a.x0 + a.bbox_w).min(b.x0 + b.bbox_w);
        let y1 = (a.y0 + a.bbox_h).min(b.y0 + b.bbox_h);
        if x1 <= x0 || y1 <= y0 {
            return Mask::empty(a.width, a.height);
        }
        let bbox_w = x1 - x0;
        let bbox_h = y1 - y0;
        let mut data = vec![0u8; bbox_w as usize * bbox_h as usize];
        for y in y0..y1 {
            let a_base = (y - a.y0) as usize * a.bbox_w as usize;
            let b_base = (y - b.y0) as usize * b.bbox_w as usize;
            let dst_base = (y - y0) as usize * bbox_w as usize;
            for x in x0..x1 {
                let av = a.data[a_base + (x - a.x0) as usize];
                let bv = b.data[b_base + (x - b.x0) as usize];
                data[dst_base + (x - x0) as usize] = av.min(bv);
            }
        }
        Mask {
            width: a.width,
            height: a.height,
            x0,
            y0,
            bbox_w,
            bbox_h,
            data,
            // Two everywhere-255 operands stay 255 across the overlap.
            opaque: a.opaque && b.opaque,
        }
    }
}

/// A non-horizontal polygon edge, stored top-to-bottom with its winding
/// direction.
#[derive(Debug)]
struct Edge {
    /// Top endpoint (smaller y).
    x0: f32,
    y0: f32,
    /// Bottom endpoint (larger y).
    x1: f32,
    y1: f32,
    /// +1 if the original edge pointed downward (increasing y), else -1.
    dir: i32,
}

impl Edge {
    /// X coordinate where the edge crosses the horizontal line `y`
    /// (requires `y0 <= y < y1`).
    fn x_at(&self, y: f32) -> f32 {
        self.x0 + (y - self.y0) * (self.x1 - self.x0) / (self.y1 - self.y0)
    }
}

/// Collects the non-horizontal edges of `polys` into `edges` (cleared
/// first), implicitly closing every subpath (fills always treat subpaths as
/// closed), then sorts them by top `y` so the active-edge sweep can bring
/// them in with a single forward-moving pointer as the scanline descends.
/// Edges with non-finite vertices are skipped.
///
/// The sort must stay STABLE: equal-`y0` ties keep build order, which fixes
/// the order crossings enter the scanline sort, which in turn fixes how
/// coincident crossings split spans — and span splits change the f32
/// accumulation order, i.e. the output bytes.
fn prepare_edges(edges: &mut Vec<Edge>, polys: &[Subpath]) {
    edges.clear();
    for sub in polys {
        let pts = &sub.points;
        if pts.len() < 2 {
            continue;
        }
        for i in 0..pts.len() {
            let p = pts[i];
            let q = pts[(i + 1) % pts.len()];
            if !(p.x.is_finite() && p.y.is_finite() && q.x.is_finite() && q.y.is_finite()) {
                continue;
            }
            if p.y == q.y {
                continue;
            }
            let (top, bot, dir) = if p.y < q.y { (p, q, 1) } else { (q, p, -1) };
            edges.push(Edge {
                x0: top.x,
                y0: top.y,
                x1: bot.x,
                y1: bot.y,
                dir,
            });
        }
    }
    edges.sort_by(|a, b| a.y0.total_cmp(&b.y0));
}

/// Adds the analytic horizontal coverage of the span `[x0, x1]`, scaled by
/// `weight`, to a row buffer, and widens `[dirty_lo, dirty_hi)` to cover the
/// pixels it wrote so the caller can restrict its work to the touched extent.
fn add_span(
    row: &mut [f32],
    x0: f32,
    x1: f32,
    weight: f32,
    dirty_lo: &mut usize,
    dirty_hi: &mut usize,
) {
    let w = row.len() as f32;
    let x0 = x0.max(0.0);
    let x1 = x1.min(w);
    if x1 <= x0 {
        return;
    }
    let first = x0.floor() as usize;
    let last = (x1.ceil() as usize).min(row.len());
    *dirty_lo = (*dirty_lo).min(first);
    *dirty_hi = (*dirty_hi).max(last);
    if last == first + 1 {
        row[first] += (x1.min(first as f32 + 1.0) - x0) * weight;
        return;
    }
    row[first] += (first as f32 + 1.0 - x0) * weight;
    // Interior pixels are fully covered: the old per-pixel min/max produced
    // exactly `(r - l) == 1.0` there, so this adds the identical value.
    for slot in &mut row[first + 1..last - 1] {
        *slot += weight;
    }
    row[last - 1] += (x1 - (last - 1) as f32) * weight;
}

/// Computes per-row anti-aliased coverage of the prepared `scratch.edges`
/// under `rule` and invokes `emit(y, row, x_lo, x_hi)` for every pixel row
/// the path touches, where `[x_lo, x_hi)` bounds the columns that received
/// coverage. Rows the path does not reach are never emitted (their coverage
/// is zero), and columns outside `[x_lo, x_hi)` in an emitted row are
/// guaranteed zero. The caller runs [`prepare_edges`] first; on return,
/// `scratch.row` is all-zero again.
fn sweep_rows<F: FnMut(u32, &[f32], usize, usize)>(
    scratch: &mut RasterScratch,
    width: u32,
    height: u32,
    rule: FillRule,
    mut emit: F,
) {
    if width == 0 || height == 0 {
        return;
    }
    let RasterScratch {
        row,
        edges,
        active,
        crossings,
    } = scratch;
    if edges.is_empty() {
        return;
    }
    let mut ymin = f32::MAX;
    let mut ymax = f32::MIN;
    for e in edges.iter() {
        ymin = ymin.min(e.y0);
        ymax = ymax.max(e.y1);
    }

    let row_start = ymin.floor().max(0.0) as u32;
    let row_end = (ymax.ceil().max(0.0) as u32).min(height);
    let full = width as usize;
    if row.len() < full {
        row.resize(full, 0.0);
    }
    // `add_span` clamps against the slice length, so hand it exactly the
    // page width even when the reused buffer is longer.
    let row = &mut row[..full];
    // Active-edge table: indices into `edges` for the edges that straddle the
    // current scanline. `ys` increases monotonically across the whole sweep
    // (rows outer, subsamples inner), so `next` only ever advances and expired
    // edges are dropped once and never revisited — turning the per-scanline
    // cost from O(all edges) into O(edges crossing this row). Activation in
    // index order plus order-preserving `retain` fixes the order crossings
    // are generated in, which the byte-identity of coincident-crossing span
    // splits depends on (see `prepare_edges`).
    active.clear();
    let mut next = 0usize;
    let weight = 1.0 / SUBSAMPLES as f32;
    // `[dirty_lo, dirty_hi)` is the range of `row` written for the row being
    // built; it is used both to bound `emit` and to clear only the touched
    // slice before the next row instead of re-zeroing the full width.
    let mut dirty_lo = full;
    let mut dirty_hi = 0usize;
    for y in row_start..row_end {
        if dirty_lo < dirty_hi {
            row[dirty_lo..dirty_hi].iter_mut().for_each(|c| *c = 0.0);
        }
        dirty_lo = full;
        dirty_hi = 0;
        for s in 0..SUBSAMPLES {
            let ys = y as f32 + (s as f32 + 0.5) / SUBSAMPLES as f32;
            while next < edges.len() && edges[next].y0 <= ys {
                active.push(next);
                next += 1;
            }
            active.retain(|&i| edges[i].y1 > ys);
            crossings.clear();
            for &i in active.iter() {
                // By construction `y0 <= ys` (activation) and `ys < y1`
                // (retain), so this edge genuinely crosses the scanline.
                crossings.push((edges[i].x_at(ys), edges[i].dir));
            }
            if crossings.len() < 2 {
                continue;
            }
            crossings.sort_by(|a, b| a.0.total_cmp(&b.0));
            let mut wind = 0i32;
            let mut span_start = 0.0f32;
            for &(x, dir) in crossings.iter() {
                let was_inside = inside(wind, rule);
                wind += dir;
                let is_inside = inside(wind, rule);
                if !was_inside && is_inside {
                    span_start = x;
                } else if was_inside && !is_inside {
                    add_span(row, span_start, x, weight, &mut dirty_lo, &mut dirty_hi);
                }
            }
        }
        if dirty_lo < dirty_hi {
            emit(y, row, dirty_lo, dirty_hi);
        }
    }
    // Restore the all-zero invariant for the next caller.
    if dirty_lo < dirty_hi {
        row[dirty_lo..dirty_hi].iter_mut().for_each(|c| *c = 0.0);
    }
}

/// Whether a winding count is "inside" under `rule`.
fn inside(wind: i32, rule: FillRule) -> bool {
    match rule {
        FillRule::NonZero => wind != 0,
        FillRule::EvenOdd => wind % 2 != 0,
    }
}

/// The separable blend modes this rasterizer paints (ISO 32000-1
/// §11.3.5.2), plus `Normal`. The non-separable four (Hue, Saturation,
/// Color, Luminosity) are recognized by the executor and reported instead.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum BlendMode {
    #[default]
    Normal,
    Multiply,
    Screen,
    Overlay,
    Darken,
    Lighten,
    ColorDodge,
    ColorBurn,
    HardLight,
    SoftLight,
    Difference,
    Exclusion,
}

impl BlendMode {
    /// The blended source color `B(Cb, Cs)` for one pixel, in RGBA8 terms.
    /// The page backdrop is always opaque here, so compositing stays
    /// `(1 − αs)·Cb + αs·B(Cb, Cs)` — the caller feeds this through the
    /// ordinary source-over composite in place of the raw source.
    pub(crate) fn blend(self, cb: [u8; 3], cs: [u8; 3]) -> [u8; 3] {
        if self == BlendMode::Normal {
            return cs;
        }
        let mut out = [0u8; 3];
        for i in 0..3 {
            let b = cb[i] as f32 / 255.0;
            let s = cs[i] as f32 / 255.0;
            let v = match self {
                BlendMode::Normal => s,
                BlendMode::Multiply => b * s,
                BlendMode::Screen => b + s - b * s,
                BlendMode::Overlay => hard_light(s, b),
                BlendMode::Darken => b.min(s),
                BlendMode::Lighten => b.max(s),
                BlendMode::ColorDodge => {
                    if s >= 1.0 {
                        1.0
                    } else {
                        (b / (1.0 - s)).min(1.0)
                    }
                }
                BlendMode::ColorBurn => {
                    if s <= 0.0 {
                        0.0
                    } else {
                        1.0 - ((1.0 - b) / s).min(1.0)
                    }
                }
                BlendMode::HardLight => hard_light(b, s),
                BlendMode::SoftLight => {
                    let d = if b <= 0.25 {
                        ((16.0 * b - 12.0) * b + 4.0) * b
                    } else {
                        b.sqrt()
                    };
                    if s <= 0.5 {
                        b - (1.0 - 2.0 * s) * b * (1.0 - b)
                    } else {
                        b + (2.0 * s - 1.0) * (d - b)
                    }
                }
                BlendMode::Difference => (b - s).abs(),
                BlendMode::Exclusion => b + s - 2.0 * b * s,
            };
            out[i] = (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        }
        out
    }
}

/// `HardLight(Cb, Cs)` per §11.3.5.2; `Overlay` is the same with the
/// operands swapped.
fn hard_light(b: f32, s: f32) -> f32 {
    if s <= 0.5 {
        b * (2.0 * s)
    } else {
        let s2 = 2.0 * s - 1.0;
        b + s2 - b * s2
    }
}

/// `UNIT[b]` is exactly `b as f32 / 255.0`, precomputed so per-pixel
/// coverage scaling replaces a hardware divide with a table load. Const
/// evaluation uses the same IEEE rounding as the runtime expression, so the
/// values are bit-identical to computing the division per pixel.
static UNIT: [f32; 256] = {
    let mut t = [0.0f32; 256];
    let mut i = 0;
    while i < 256 {
        t[i] = i as f32 / 255.0;
        i += 1;
    }
    t
};

/// Composites `rgb` at alpha `a` (0..=1) over one straight-alpha RGBA8
/// pixel using the source-over rule.
pub(crate) fn composite_over(dst: &mut [u8], rgb: [u8; 3], a: f32) {
    let da = dst[3] as f32 / 255.0;
    let oa = a + da * (1.0 - a);
    if oa <= 0.0 {
        dst.copy_from_slice(&[0, 0, 0, 0]);
        return;
    }
    for i in 0..3 {
        let s = rgb[i] as f32;
        let d = dst[i] as f32;
        let c = (s * a + d * da * (1.0 - a)) / oa;
        dst[i] = (c + 0.5) as u8;
    }
    dst[3] = (oa * 255.0 + 0.5) as u8;
}

/// Fills `polys` into `pix` under `rule` with the straight-alpha color
/// `rgba`, further scaled by the constant `alpha` (0..=1) and, when
/// present, the `clip` coverage mask. Anti-aliased coverage is composited
/// source-over.
pub(crate) fn fill_path(
    pix: &mut Pixmap,
    scratch: &mut RasterScratch,
    polys: &[Subpath],
    rule: FillRule,
    rgba: [u8; 4],
    alpha: f32,
    clip: Option<&Mask>,
    blend: BlendMode,
) {
    let alpha = if alpha.is_finite() {
        alpha.clamp(0.0, 1.0)
    } else {
        1.0
    };
    let base_a = rgba[3] as f32 / 255.0 * alpha;
    if base_a <= 0.0 {
        return;
    }
    let rgb = [rgba[0], rgba[1], rgba[2]];
    let w = pix.width as usize;
    prepare_edges(&mut scratch.edges, polys);
    sweep_rows(scratch, pix.width, pix.height, rule, |y, row, mut lo, mut hi| {
        let mask_row = match clip {
            None => None,
            Some(m) => {
                // Pixels outside the mask's stored bbox read coverage 0, so
                // the fill cannot touch them; narrow the span to the overlap
                // and hand the pixel loop the mask bytes for what remains.
                if y < m.y0 || y - m.y0 >= m.bbox_h {
                    return;
                }
                let mx0 = m.x0 as usize;
                lo = lo.max(mx0);
                hi = hi.min(mx0 + m.bbox_w as usize);
                if hi <= lo {
                    return;
                }
                if m.opaque {
                    // Every byte in range is 255 and scaling by 255/255.0
                    // == 1.0 is exactly the identity, so the clip reduces
                    // to the bbox narrowing above.
                    None
                } else {
                    let base = (y - m.y0) as usize * m.bbox_w as usize;
                    Some(&m.data[base + lo - mx0..base + hi - mx0])
                }
            }
        };
        let base = (y as usize * w + lo) * 4;
        let dst_row = &mut pix.data[base..base + (hi - lo) * 4];
        if blend == BlendMode::Normal {
            blend_row::<true>(dst_row, &row[lo..hi], mask_row, base_a, rgb, blend);
        } else {
            blend_row::<false>(dst_row, &row[lo..hi], mask_row, base_a, rgb, blend);
        }
    });
}

/// Paints one emitted coverage row into `dst_row` (4 bytes per pixel).
/// `NORMAL` mirrors `blend == BlendMode::Normal` so the mode test stays out
/// of the pixel loop.
#[inline(always)]
fn blend_row<const NORMAL: bool>(
    dst_row: &mut [u8],
    covs: &[f32],
    mask_row: Option<&[u8]>,
    base_a: f32,
    rgb: [u8; 3],
    blend: BlendMode,
) {
    let opaque = [rgb[0], rgb[1], rgb[2], 255];
    match mask_row {
        None => {
            for (dst, &cov) in dst_row.chunks_exact_mut(4).zip(covs) {
                let a = cov.clamp(0.0, 1.0) * base_a;
                paint_pixel::<NORMAL>(dst, a, rgb, opaque, blend);
            }
        }
        Some(mrow) => {
            for ((dst, &cov), &mb) in dst_row.chunks_exact_mut(4).zip(covs).zip(mrow) {
                let a = (cov.clamp(0.0, 1.0) * base_a) * UNIT[mb as usize];
                paint_pixel::<NORMAL>(dst, a, rgb, opaque, blend);
            }
        }
    }
}

/// Source-over paints one pixel at alpha `a`, honoring the blend mode.
#[inline(always)]
fn paint_pixel<const NORMAL: bool>(
    dst: &mut [u8],
    a: f32,
    rgb: [u8; 3],
    opaque: [u8; 4],
    blend: BlendMode,
) {
    if a <= 0.0 {
        return;
    }
    if NORMAL {
        if a >= 1.0 {
            // Fully covered by an opaque source: the source-over result
            // is exactly the source color, so skip the per-pixel divide.
            dst.copy_from_slice(&opaque);
        } else {
            composite_over(dst, rgb, a);
        }
        return;
    }
    // A non-Normal blend derives the effective source color from the
    // backdrop pixel, so neither branch below may shortcut it.
    let rgb = blend.blend([dst[0], dst[1], dst[2]], rgb);
    if a >= 1.0 {
        dst.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    } else {
        composite_over(dst, rgb, a);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfboss_core::geom::Point;

    /// Shadows the crate fn with a fresh-scratch wrapper so the tests stay
    /// focused on rasterization behavior, not buffer plumbing.
    fn fill_path(
        pix: &mut Pixmap,
        polys: &[Subpath],
        rule: FillRule,
        rgba: [u8; 4],
        alpha: f32,
        clip: Option<&Mask>,
        blend: BlendMode,
    ) {
        super::fill_path(
            pix,
            &mut RasterScratch::default(),
            polys,
            rule,
            rgba,
            alpha,
            clip,
            blend,
        );
    }

    fn mask_from_path(width: u32, height: u32, polys: &[Subpath], rule: FillRule) -> Mask {
        Mask::from_path(width, height, &mut RasterScratch::default(), polys, rule)
    }

    fn rect_poly(x0: f32, y0: f32, x1: f32, y1: f32) -> Subpath {
        Subpath {
            points: vec![
                Point::new(x0, y0),
                Point::new(x1, y0),
                Point::new(x1, y1),
                Point::new(x0, y1),
            ],
            closed: true,
        }
    }

    fn alpha_at(pix: &Pixmap, x: u32, y: u32) -> u8 {
        pix.data[((y * pix.width + x) * 4 + 3) as usize]
    }

    fn rgba_at(pix: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let off = ((y * pix.width + x) * 4) as usize;
        pix.data[off..off + 4].try_into().unwrap()
    }

    const RED: [u8; 4] = [255, 0, 0, 255];

    #[test]
    fn axis_aligned_rect_exact_interior() {
        let mut pix = Pixmap::new(10, 10);
        let polys = [rect_poly(2.0, 2.0, 8.0, 8.0)];
        fill_path(
            &mut pix,
            &polys,
            FillRule::NonZero,
            RED,
            1.0,
            None,
            BlendMode::Normal,
        );
        for y in 0..10 {
            for x in 0..10 {
                let inside = (2..8).contains(&x) && (2..8).contains(&y);
                if inside {
                    assert_eq!(rgba_at(&pix, x, y), RED, "pixel ({x},{y})");
                } else {
                    assert_eq!(alpha_at(&pix, x, y), 0, "pixel ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn half_pixel_horizontal_edge_antialiases() {
        let mut pix = Pixmap::new(10, 10);
        let polys = [rect_poly(2.5, 2.0, 8.0, 8.0)];
        fill_path(
            &mut pix,
            &polys,
            FillRule::NonZero,
            RED,
            1.0,
            None,
            BlendMode::Normal,
        );
        let a = alpha_at(&pix, 2, 4);
        assert!((127..=129).contains(&a), "edge alpha {a}");
        assert_eq!(alpha_at(&pix, 3, 4), 255);
        assert_eq!(alpha_at(&pix, 1, 4), 0);
    }

    #[test]
    fn half_pixel_vertical_edge_antialiases() {
        let mut pix = Pixmap::new(10, 10);
        let polys = [rect_poly(2.0, 2.5, 8.0, 8.0)];
        fill_path(
            &mut pix,
            &polys,
            FillRule::NonZero,
            RED,
            1.0,
            None,
            BlendMode::Normal,
        );
        let a = alpha_at(&pix, 4, 2);
        assert!((115..=140).contains(&a), "edge alpha {a}");
        assert_eq!(alpha_at(&pix, 4, 3), 255);
        assert_eq!(alpha_at(&pix, 4, 1), 0);
    }

    #[test]
    fn triangle_half_plane_sanity() {
        let mut pix = Pixmap::new(10, 10);
        let tri = Subpath {
            points: vec![
                Point::new(1.0, 1.0),
                Point::new(9.0, 1.0),
                Point::new(5.0, 9.0),
            ],
            closed: true,
        };
        fill_path(
            &mut pix,
            &[tri],
            FillRule::NonZero,
            RED,
            1.0,
            None,
            BlendMode::Normal,
        );
        assert_eq!(alpha_at(&pix, 5, 4), 255, "interior");
        assert_eq!(alpha_at(&pix, 4, 2), 255, "interior near top");
        assert_eq!(alpha_at(&pix, 0, 5), 0, "left of triangle");
        assert_eq!(alpha_at(&pix, 9, 8), 0, "right of apex");
        assert_eq!(alpha_at(&pix, 5, 0), 0, "above");
    }

    #[test]
    fn even_odd_donut_has_hole() {
        let mut pix = Pixmap::new(12, 12);
        let polys = [
            rect_poly(1.0, 1.0, 11.0, 11.0),
            rect_poly(4.0, 4.0, 8.0, 8.0),
        ];
        fill_path(
            &mut pix,
            &polys,
            FillRule::EvenOdd,
            RED,
            1.0,
            None,
            BlendMode::Normal,
        );
        assert_eq!(alpha_at(&pix, 6, 6), 0, "hole must be empty");
        assert_eq!(alpha_at(&pix, 2, 6), 255, "ring left");
        assert_eq!(alpha_at(&pix, 9, 6), 255, "ring right");
        assert_eq!(alpha_at(&pix, 6, 2), 255, "ring top");
        assert_eq!(alpha_at(&pix, 0, 6), 0, "outside");
    }

    #[test]
    fn nonzero_same_winding_donut_fills_solid() {
        let mut pix = Pixmap::new(12, 12);
        // Both rects share the same winding direction.
        let polys = [
            rect_poly(1.0, 1.0, 11.0, 11.0),
            rect_poly(4.0, 4.0, 8.0, 8.0),
        ];
        fill_path(
            &mut pix,
            &polys,
            FillRule::NonZero,
            RED,
            1.0,
            None,
            BlendMode::Normal,
        );
        assert_eq!(alpha_at(&pix, 6, 6), 255, "center filled under nonzero");
        assert_eq!(alpha_at(&pix, 2, 6), 255, "ring");
        assert_eq!(alpha_at(&pix, 0, 6), 0, "outside");
    }

    #[test]
    fn nonzero_opposite_winding_donut_has_hole() {
        let mut pix = Pixmap::new(12, 12);
        let inner = Subpath {
            points: vec![
                Point::new(4.0, 4.0),
                Point::new(4.0, 8.0),
                Point::new(8.0, 8.0),
                Point::new(8.0, 4.0),
            ],
            closed: true,
        };
        let polys = [rect_poly(1.0, 1.0, 11.0, 11.0), inner];
        fill_path(
            &mut pix,
            &polys,
            FillRule::NonZero,
            RED,
            1.0,
            None,
            BlendMode::Normal,
        );
        assert_eq!(alpha_at(&pix, 6, 6), 0, "reversed inner rect punches hole");
        assert_eq!(alpha_at(&pix, 2, 6), 255, "ring");
    }

    #[test]
    fn clip_mask_restricts_fill() {
        let mut pix = Pixmap::new(10, 10);
        let clip = mask_from_path(10, 10, &[rect_poly(0.0, 0.0, 5.0, 10.0)], FillRule::NonZero);
        let polys = [rect_poly(0.0, 0.0, 10.0, 10.0)];
        fill_path(
            &mut pix,
            &polys,
            FillRule::NonZero,
            RED,
            1.0,
            Some(&clip),
            BlendMode::Normal,
        );
        for y in 0..10 {
            for x in 0..10 {
                if x < 5 {
                    assert_eq!(alpha_at(&pix, x, y), 255, "inside clip ({x},{y})");
                } else {
                    assert_eq!(alpha_at(&pix, x, y), 0, "outside clip untouched ({x},{y})");
                }
            }
        }
    }

    #[test]
    fn mask_intersect_takes_minimum() {
        let mut a = mask_from_path(8, 8, &[rect_poly(0.0, 0.0, 6.0, 8.0)], FillRule::NonZero);
        let b = mask_from_path(8, 8, &[rect_poly(4.0, 0.0, 8.0, 8.0)], FillRule::NonZero);
        a.intersect(&b);
        assert_eq!(a.coverage(2, 4), 0, "only in a");
        assert_eq!(a.coverage(7, 4), 0, "only in b");
        assert_eq!(a.coverage(5, 4), 255, "in both");
    }

    #[test]
    fn mask_from_path_bbox_is_tight_not_full_page() {
        // A small clip rect on a large page should only allocate its own
        // bounding box, not the whole page — this is the whole point of the
        // fix (O(clip area), not O(page area), per clip operation).
        let mask = mask_from_path(
            1000,
            1000,
            &[rect_poly(10.0, 20.0, 30.0, 50.0)],
            FillRule::NonZero,
        );
        assert_eq!(mask.x0, 10);
        assert_eq!(mask.y0, 20);
        assert_eq!(mask.bbox_w, 20);
        assert_eq!(mask.bbox_h, 30);
        assert_eq!(mask.data.len(), 20 * 30);
        assert_eq!(mask.coverage(15, 25), 255, "inside clip");
        assert_eq!(mask.coverage(500, 500), 0, "far outside clip bbox");
        assert_eq!(mask.coverage(0, 0), 0, "outside clip bbox but inside page");
    }

    #[test]
    fn mask_intersect_disjoint_bboxes_is_empty() {
        let mut a = mask_from_path(
            100,
            100,
            &[rect_poly(0.0, 0.0, 10.0, 10.0)],
            FillRule::NonZero,
        );
        let b = mask_from_path(
            100,
            100,
            &[rect_poly(50.0, 50.0, 60.0, 60.0)],
            FillRule::NonZero,
        );
        a.intersect(&b);
        assert_eq!(a.bbox_w, 0);
        assert_eq!(a.bbox_h, 0);
        for y in 0..100 {
            for x in 0..100 {
                assert_eq!(a.coverage(x, y), 0, "disjoint clips leave nothing visible");
            }
        }
    }

    #[test]
    fn mask_intersect_shrinks_bbox_to_overlap() {
        let mut a = mask_from_path(
            100,
            100,
            &[rect_poly(0.0, 0.0, 20.0, 20.0)],
            FillRule::NonZero,
        );
        let b = mask_from_path(
            100,
            100,
            &[rect_poly(10.0, 10.0, 30.0, 30.0)],
            FillRule::NonZero,
        );
        a.intersect(&b);
        assert_eq!(a.x0, 10);
        assert_eq!(a.y0, 10);
        assert_eq!(a.bbox_w, 10);
        assert_eq!(a.bbox_h, 10);
        assert_eq!(a.coverage(15, 15), 255, "in overlap");
        assert_eq!(a.coverage(5, 5), 0, "only in a");
        assert_eq!(a.coverage(25, 25), 0, "only in b");
    }

    #[test]
    fn opaque_clip_shortcut_matches_full_mask_math() {
        let polys = [rect_poly(1.5, 1.5, 9.5, 9.5)];
        let clip = mask_from_path(12, 12, &[rect_poly(2.0, 0.0, 8.0, 12.0)], FillRule::NonZero);
        assert!(clip.opaque, "integer-coordinate rect clip is fully opaque");
        let mut dull = clip.clone();
        dull.opaque = false;
        let mut a = Pixmap::new(12, 12);
        let mut b = Pixmap::new(12, 12);
        fill_path(
            &mut a,
            &polys,
            FillRule::NonZero,
            RED,
            0.7,
            Some(&clip),
            BlendMode::Normal,
        );
        fill_path(
            &mut b,
            &polys,
            FillRule::NonZero,
            RED,
            0.7,
            Some(&dull),
            BlendMode::Normal,
        );
        assert_eq!(a.data, b.data, "opaque shortcut must not change pixels");
    }

    #[test]
    fn fractional_clip_is_not_marked_opaque() {
        let m = mask_from_path(12, 12, &[rect_poly(2.5, 2.0, 8.0, 10.0)], FillRule::NonZero);
        assert!(!m.opaque, "partial edge coverage forbids the opaque flag");
    }

    #[test]
    fn mask_new_is_full_page_and_directly_indexable() {
        // `Mask::new` stays a full-page buffer (unlike `from_path`): a few
        // tests (and image.rs's) build a synthetic mask by hand via direct
        // `.data` indexing, which relies on this.
        let mask = Mask::new(8, 8);
        assert_eq!(mask.bbox_w, 8);
        assert_eq!(mask.bbox_h, 8);
        assert_eq!(mask.data.len(), 64);
        assert!(mask.data.iter().all(|&b| b == 0));
    }

    #[test]
    fn constant_alpha_composites_over_white() {
        let mut pix = Pixmap::new(4, 4);
        pix.fill([255, 255, 255, 255]);
        let polys = [rect_poly(0.0, 0.0, 4.0, 4.0)];
        fill_path(
            &mut pix,
            &polys,
            FillRule::NonZero,
            RED,
            0.5,
            None,
            BlendMode::Normal,
        );
        let px = rgba_at(&pix, 2, 2);
        assert_eq!(px[0], 255);
        assert!((127..=129).contains(&px[1]), "green {}", px[1]);
        assert!((127..=129).contains(&px[2]), "blue {}", px[2]);
        assert_eq!(px[3], 255);
    }

    #[test]
    fn open_subpath_is_implicitly_closed_for_fill() {
        let mut pix = Pixmap::new(10, 10);
        let tri = Subpath {
            points: vec![
                Point::new(1.0, 1.0),
                Point::new(9.0, 1.0),
                Point::new(5.0, 9.0),
            ],
            closed: false,
        };
        fill_path(
            &mut pix,
            &[tri],
            FillRule::NonZero,
            RED,
            1.0,
            None,
            BlendMode::Normal,
        );
        assert_eq!(alpha_at(&pix, 5, 4), 255);
    }
}
