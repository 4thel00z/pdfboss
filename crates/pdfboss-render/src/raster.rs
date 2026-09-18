//! Coverage rasterizer: per-pixel coverage from the signed area each
//! polygon edge cuts out of the pixels it crosses, nonzero and even-odd
//! fill rules, and coverage-mask clipping.

use crate::path::Subpath;
use crate::Pixmap;

/// Which interior rule decides what a path encloses.
///
/// Covers ISO 32000-1 §8.5.3.3.2 and §8.5.3.3.3.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum FillRule {
    /// Nonzero winding number.
    #[default]
    NonZero,
    /// Even-odd (parity) rule.
    EvenOdd,
}

/// Reusable rasterizer buffers, owned by the caller so a page of fills does
/// not re-allocate (and re-zero) them on every call.
#[derive(Debug, Default)]
pub(crate) struct RasterScratch {
    /// Per-row coverage, at least page-width long. Every column the sweep
    /// hands out is written before it is read, so it needs no clearing
    /// between rows the way `acc` does.
    row: Vec<f32>,
    /// Edge list of the path being rasterized.
    edges: Vec<Edge>,
    /// Active-edge indices for the current pixel row.
    active: Vec<usize>,
    /// Per-row difference array the sweep accumulates signed area into, two
    /// slots longer than the page because an edge in the last column hands
    /// a share to the column past it. All-zero between rows.
    acc: Vec<f32>,
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
    ///
    /// Covers ISO 32000-1 §11.6.4.2.
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
            // A path running off the right of the page has not closed inside
            // it, so the sweep carries its coverage out to the page edge,
            // past the bbox `xmax` fixed. Clamping here keeps the write
            // inside the stored region.
            let base = (y - by0) as usize * bw;
            let local_lo = lo.max(bx0 as usize) - bx0 as usize;
            let local_hi = hi.min(bx1 as usize).max(bx0 as usize) - bx0 as usize;
            if local_hi <= local_lo {
                return;
            }
            let dst = &mut mask.data[base + local_lo..base + local_hi];
            for (cov, out) in row[local_lo + bx0 as usize..local_hi + bx0 as usize]
                .iter()
                .zip(dst.iter_mut())
            {
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
    /// Horizontal movement per unit of y, so walking the edge down a row
    /// costs a multiply instead of a divide.
    dxdy: f32,
}

impl Edge {
    /// X coordinate where the edge crosses the horizontal line `y`
    /// (requires `y0 <= y <= y1`).
    #[inline]
    fn x_on(&self, y: f32) -> f32 {
        self.x0 + (y - self.y0) * self.dxdy
    }

    /// The part of this edge inside the pixel row starting at `ytop`, as
    /// `(x where it enters, x where it leaves, signed height)`; `None` when
    /// the edge misses the row. The height carries the winding direction,
    /// so summing pieces across edges sums winding.
    #[inline]
    fn piece_in_row(&self, ytop: f32) -> Option<(f32, f32, f32)> {
        let ya = self.y0.max(ytop);
        let yb = self.y1.min(ytop + 1.0);
        if yb <= ya {
            return None;
        }
        Some((self.x_on(ya), self.x_on(yb), (yb - ya) * self.dir as f32))
    }
}

/// Collects the non-horizontal edges of `polys` into `edges` (cleared
/// first), implicitly closing every subpath (fills always treat subpaths as
/// closed), then sorts them by top `y` so the active-edge sweep can bring
/// them in with a single forward-moving pointer as the scanline descends.
/// Edges with non-finite vertices are skipped.
///
/// The sort must stay STABLE: equal-`y0` ties keep build order, which fixes
/// the order edges enter the active list, which fixes the order their areas
/// are summed into a row. f32 addition is not associative, so that order
/// decides the output bytes.
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
                dxdy: (bot.x - top.x) / (bot.y - top.y),
            });
        }
    }
    edges.sort_by(|a, b| a.y0.total_cmp(&b.y0));
}

/// Adds one edge's crossing of a single pixel row to that row's difference
/// array. `dy` is the signed height the edge covers inside the row (positive
/// for a downward edge, negative for an upward one) and `x0`/`x1` are where
/// it enters and leaves the row band. Running the array up from the left
/// gives, at each pixel, the winding number weighted by how much of that
/// pixel lies to the right of every edge crossing it, which is the pixel's
/// signed covered area.
///
/// x moves linearly with y along an edge, so the share of `dy` falling in a
/// pixel column is the share of the x interval inside that column, and the
/// midpoint of the piece says how the column splits its own share with its
/// right neighbour.
fn accumulate(acc: &mut [f32], x0: f32, x1: f32, dy: f32, lo: &mut usize, hi: &mut usize) {
    let (mut xl, mut xr) = if x0 <= x1 { (x0, x1) } else { (x1, x0) };
    let right = (acc.len() - 2) as f32;
    // A piece of the edge left of the page still separates what is inside
    // the path from what is outside, so its whole height belongs to the
    // first column. Moving the endpoint alone would hand that height to
    // whatever columns the rest of the edge crosses instead.
    let mut dy = dy;
    if xr <= 0.0 {
        *lo = 0;
        *hi = (*hi).max(2);
        acc[0] += dy;
        return;
    }
    if xl >= right {
        // Only columns right of this edge see its winding, and the page has
        // none. It leaves the path open at the right edge, which the walk
        // reads off the running sum rather than from any column here.
        return;
    }
    if xl < 0.0 {
        let outside = -xl / (xr - xl);
        *lo = 0;
        *hi = (*hi).max(2);
        acc[0] += dy * outside;
        dy -= dy * outside;
        xl = 0.0;
    }
    if xr > right {
        dy *= (right - xl) / (xr - xl);
        xr = right;
    }
    let first = xl as usize;
    let last = xr as usize;
    *lo = (*lo).min(first);
    *hi = (*hi).max((last + 2).min(acc.len()));
    if first == last {
        let frac = (xl + xr) * 0.5 - first as f32;
        acc[first] += dy * (1.0 - frac);
        acc[first + 1] += dy * frac;
        return;
    }
    let span = xr - xl;
    for c in first..=last {
        let l = xl.max(c as f32);
        let r = xr.min(c as f32 + 1.0);
        if r <= l {
            continue;
        }
        let part = dy * (r - l) / span;
        let frac = (l + r) * 0.5 - c as f32;
        acc[c] += part * (1.0 - frac);
        acc[c + 1] += part * frac;
    }
}

/// Columns the coverage walk tests for emptiness in one go.
const COVERAGE_BLOCK: usize = 64;

/// Coverage below half of one 8-bit step, which every consumer rounds to
/// nothing. Winding that should cancel exactly can leave a residue this
/// small behind, and treating it as coverage would carry a row out to the
/// page edge for no visible pixel.
const QUANTUM: f32 = 0.5 / 255.0;

/// Coverage at least this close to full counts as full. A pixel the path
/// covers completely accumulates to one only up to the rounding of the f32
/// sum of its edges' areas, and the residue is many times smaller than this
/// gap, which is itself an eighth of one 8-bit step. Recovering the exact
/// one matters because the row painter fills a run of fully covered pixels
/// with the source color instead of blending each one.
const FULL: f32 = 1.0 - 1.0 / 2048.0;

/// Turns a running signed area into the coverage its fill rule asks for.
///
/// Nonzero keeps any winding at all, so the magnitude saturates at one
/// pixel. Even-odd alternates, so the magnitude folds back on itself: a
/// winding of two covers nothing, three covers everything again, and the
/// fractional area in between interpolates the same way.
///
/// Covers ISO 32000-1 §8.5.3.3.2 and §8.5.3.3.3.
#[inline(always)]
fn coverage_of<const EVEN_ODD: bool>(sum: f32) -> f32 {
    let a = sum.abs();
    if !EVEN_ODD {
        return saturate(a);
    }
    let m = a % 2.0;
    if m > 1.0 {
        return saturate(2.0 - m);
    }
    saturate(m)
}

/// Whether a block of the difference array holds nothing at all. Folding
/// the magnitudes together with a bitwise or asks the question without a
/// branch per column, which is what lets the compiler answer it several
/// columns at a time; a short-circuiting test cannot be vectorized and the
/// blocks it walks are mostly empty.
#[inline(always)]
fn all_zero(block: &[f32]) -> bool {
    block
        .iter()
        .fold(0u32, |a, &c| a | (c.to_bits() & 0x7fff_ffff))
        == 0
}

/// One pixel of area at most, and exactly one for anything within [`FULL`].
#[inline(always)]
fn saturate(area: f32) -> f32 {
    if area >= FULL {
        return 1.0;
    }
    area
}

/// Runs one row's difference array up from the left into coverage under the
/// fill rule, and returns the column range it wrote. `acc` comes back
/// all-zero, ready for the next row.
///
/// Coverage only bends at a column an edge wrote to, and most columns of a
/// wide row have nothing written, so the walk asks a whole block of the
/// difference array at once whether anything did: an empty block repeats
/// the coverage the running sum already holds.
fn resolve_row<const EVEN_ODD: bool>(
    acc: &mut [f32],
    row: &mut [f32],
    lo: usize,
    hi: usize,
) -> (usize, usize) {
    let full = row.len();
    let end = hi.min(full);
    // The walk stops at the page edge; anything an edge wrote past it still
    // has to go back to zero for the next row.
    acc[end..hi].fill(0.0);
    if lo >= end {
        return (0, 0);
    }
    let mut sum = 0.0f32;
    let mut x = lo;
    let mut start = lo;
    let mut finish = lo;
    while x < end {
        let stop = (x + COVERAGE_BLOCK).min(end);
        if !all_zero(&acc[x..stop]) {
            for slot in x..stop {
                sum += acc[slot];
                acc[slot] = 0.0;
                row[slot] = coverage_of::<EVEN_ODD>(sum);
            }
            finish = stop;
            x = stop;
            continue;
        }
        let cov = coverage_of::<EVEN_ODD>(sum);
        // Nothing visible here. A subpath that falls entirely outside the
        // page still pulls the row's column range out to meet it, and a
        // winding that cancels shows nothing across the gap, so ahead of
        // the row's first painted column this moves its start. Past that
        // column the coverage still has to be written, because a later
        // column may be painted and would otherwise be handed this row's
        // predecessor, but the row need not carry on beyond it.
        if cov < QUANTUM {
            if start == x {
                start = stop;
                x = stop;
                continue;
            }
            row[x..stop].fill(cov);
            x = stop;
            continue;
        }
        row[x..stop].fill(cov);
        finish = stop;
        x = stop;
    }
    // Past the last column an edge wrote to, coverage no longer changes. A
    // path that closed inside the page leaves a winding of zero there and
    // the row ends; one that ran off the right is still open, and carries
    // its coverage to the page edge.
    let tail = coverage_of::<EVEN_ODD>(sum);
    if tail >= QUANTUM {
        row[end..full].fill(tail);
        finish = full;
    }
    if start >= finish {
        return (0, 0);
    }
    (start, finish)
}

/// Brings into `active` every edge whose top lies above the bottom of the
/// row starting at `ytop`, and drops every edge that ended above its top,
/// so `active` holds exactly the edges crossing the row. `next` walks the
/// y-sorted edge list forward only, so a whole sweep costs one pass.
fn advance_active(edges: &[Edge], active: &mut Vec<usize>, next: &mut usize, ytop: f32) {
    let ybot = ytop + 1.0;
    while *next < edges.len() && edges[*next].y0 < ybot {
        active.push(*next);
        *next += 1;
    }
    active.retain(|&i| edges[i].y1 > ytop);
}

/// [`sweep_rows_with`] with the fill rule chosen once for the whole sweep
/// instead of per pixel.
fn sweep_rows<F: FnMut(u32, &[f32], usize, usize)>(
    scratch: &mut RasterScratch,
    width: u32,
    height: u32,
    rule: FillRule,
    emit: F,
) {
    if rule == FillRule::EvenOdd {
        sweep_rows_with::<F, true>(scratch, width, height, emit);
        return;
    }
    sweep_rows_with::<F, false>(scratch, width, height, emit);
}

/// Computes per-row anti-aliased coverage of the prepared `scratch.edges`
/// and invokes `emit(y, row, x_lo, x_hi)` for every pixel row the path
/// touches, where `[x_lo, x_hi)` bounds the columns whose coverage the row
/// buffer holds. Columns outside it are not written and must not be read.
/// The caller runs [`prepare_edges`] first.
///
/// Covers ISO 32000-1 §10.6.4 and §11.6.4.2.
fn sweep_rows_with<F: FnMut(u32, &[f32], usize, usize), const EVEN_ODD: bool>(
    scratch: &mut RasterScratch,
    width: u32,
    height: u32,
    mut emit: F,
) {
    if width == 0 || height == 0 {
        return;
    }
    let RasterScratch {
        edges,
        active,
        acc,
        row,
        ..
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
    if acc.len() < full + 2 {
        acc.resize(full + 2, 0.0);
    }
    let row = &mut row[..full];
    let acc = &mut acc[..full + 2];
    active.clear();
    let mut next = 0usize;
    for y in row_start..row_end {
        let ytop = y as f32;
        advance_active(edges, active, &mut next, ytop);
        let mut lo = acc.len();
        let mut hi = 0usize;
        for &i in active.iter() {
            // Unclamped: `accumulate` needs to know where the edge leaves
            // the page to know how much of its height belongs to the first
            // column.
            let Some((xa, xb, dy)) = edges[i].piece_in_row(ytop) else {
                continue;
            };
            accumulate(acc, xa, xb, dy, &mut lo, &mut hi);
        }
        let (lo, end) = resolve_row::<EVEN_ODD>(acc, row, lo, hi);
        if lo >= end {
            continue;
        }
        emit(y, row, lo, end);
    }
}

/// Anti-aliased coverage of one path, captured as the per-row edge pieces
/// the sweep cuts out of it instead of painted pixels, so a glyph repeated
/// along a baseline sweeps its edges once: a repeat replays the recorded
/// pieces shifted by its own device offset, skipping edge preparation, the
/// active-edge walk and the row clipping. Coordinates are the swept
/// geometry's own (unclamped by any page); the pixel-row index `r` maps to
/// device row `y0 + r + iy` at fill time.
///
/// A piece's signed height is fixed by which pixel row it falls in, so a
/// replay may shift the capture horizontally by any amount and vertically
/// by whole rows only. The horizontal shift is exact: [`accumulate`] reads
/// x positions and nothing else.
#[derive(Debug, Default)]
pub(crate) struct SpanSet {
    /// Topmost pixel row the geometry touches, in its own frame.
    y0: i64,
    /// Number of pixel rows captured.
    rows: usize,
    /// Which interior rule the replay resolves the accumulated area under.
    /// Winding is still summed at fill time, so the capture itself holds
    /// for either rule.
    rule: FillRule,
    /// Prefix offsets into `pieces`, one slot per row plus a terminator:
    /// row `r` owns `pieces[offs[r]..offs[r + 1]]`.
    offs: Vec<u32>,
    /// `(x where the edge enters the row, x where it leaves, signed
    /// height)` per edge piece, in the order the sweep accumulated them.
    pieces: Vec<(f32, f32, f32)>,
}

/// Row-count and piece-count bounds on a capture: geometry taller or busier
/// than any honest glyph refuses to be captured (the caller falls back to a
/// direct fill, which page bounds keep proportional), so a hostile stream
/// cannot mint an arbitrarily large [`SpanSet`].
const MAX_CAPTURE_ROWS: i64 = 8192;
const MAX_CAPTURE_PIECES: usize = 1 << 16;

/// Sweeps `polys` with the same edge preparation, activation order and row
/// clipping as [`fill_path`]'s rasterizer, recording each edge's piece of
/// each row instead of resolving it into coverage. No page clamps apply:
/// the capture is in the geometry's own frame, and [`fill_spans`] clamps to
/// the page it paints, which is where [`accumulate`] wants the clamping
/// anyway. `None` means the geometry exceeded a capture bound.
pub(crate) fn capture_spans(
    scratch: &mut RasterScratch,
    polys: &[Subpath],
    rule: FillRule,
) -> Option<SpanSet> {
    let RasterScratch { edges, active, .. } = scratch;
    prepare_edges(edges, polys);
    if edges.is_empty() {
        return Some(SpanSet::default());
    }
    let mut ymin = f32::MAX;
    let mut ymax = f32::MIN;
    for e in edges.iter() {
        ymin = ymin.min(e.y0);
        ymax = ymax.max(e.y1);
    }
    if !ymin.is_finite() || !ymax.is_finite() {
        return None;
    }
    let row_start = ymin.floor() as i64;
    let row_end = ymax.ceil() as i64;
    let rows = row_end.saturating_sub(row_start);
    if rows <= 0 || rows > MAX_CAPTURE_ROWS {
        return None;
    }
    let rows = rows as usize;
    let mut set = SpanSet {
        y0: row_start,
        rows,
        rule,
        offs: Vec::with_capacity(rows + 1),
        pieces: Vec::new(),
    };
    set.offs.push(0);
    active.clear();
    let mut next = 0usize;
    for r in 0..rows {
        let ytop = (row_start + r as i64) as f32;
        advance_active(edges, active, &mut next, ytop);
        for &i in active.iter() {
            let Some(piece) = edges[i].piece_in_row(ytop) else {
                continue;
            };
            set.pieces.push(piece);
        }
        if set.pieces.len() > MAX_CAPTURE_PIECES {
            return None;
        }
        set.offs.push(set.pieces.len() as u32);
    }
    Some(set)
}

/// Paints a captured [`SpanSet`] onto `pix` at horizontal offset `dx` and
/// integer row offset `iy`, with the color, alpha, clip and blend semantics
/// of [`fill_path`]. The area accumulation, the coverage walk and the row
/// painting are the same code, so a capture-and-fill of a path gives the
/// same pixels as a direct fill of it.
#[allow(clippy::too_many_arguments)]
pub(crate) fn fill_spans(
    pix: &mut Pixmap,
    scratch: &mut RasterScratch,
    set: &SpanSet,
    dx: f32,
    iy: i64,
    rgba: [u8; 4],
    alpha: f32,
    clip: Option<&Mask>,
    blend: BlendMode,
) {
    if set.rule == FillRule::EvenOdd {
        fill_spans_with::<true>(pix, scratch, set, dx, iy, rgba, alpha, clip, blend);
        return;
    }
    fill_spans_with::<false>(pix, scratch, set, dx, iy, rgba, alpha, clip, blend);
}

/// [`fill_spans`] with the fill rule chosen once for the whole replay
/// instead of per pixel.
#[allow(clippy::too_many_arguments)]
fn fill_spans_with<const EVEN_ODD: bool>(
    pix: &mut Pixmap,
    scratch: &mut RasterScratch,
    set: &SpanSet,
    dx: f32,
    iy: i64,
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
    if base_a <= 0.0 || pix.width == 0 || pix.height == 0 {
        return;
    }
    let rgb = [rgba[0], rgba[1], rgba[2]];
    let full = pix.width as usize;
    if scratch.row.len() < full {
        scratch.row.resize(full, 0.0);
    }
    if scratch.acc.len() < full + 2 {
        scratch.acc.resize(full + 2, 0.0);
    }
    let row = &mut scratch.row[..full];
    let acc = &mut scratch.acc[..full + 2];
    for r in 0..set.rows {
        let page_y = set.y0 + r as i64 + iy;
        if page_y < 0 || page_y >= pix.height as i64 {
            continue;
        }
        let mut lo = acc.len();
        let mut hi = 0usize;
        let from = set.offs[r] as usize;
        let to = set.offs[r + 1] as usize;
        for &(xa, xb, dy) in &set.pieces[from..to] {
            accumulate(acc, xa + dx, xb + dx, dy, &mut lo, &mut hi);
        }
        let (lo, end) = resolve_row::<EVEN_ODD>(acc, row, lo, hi);
        if lo >= end {
            continue;
        }
        paint_row(pix, page_y as u32, row, lo, end, clip, base_a, rgb, blend);
    }
}

/// The blend modes this rasterizer paints: `Normal`, the separable modes
/// (ISO 32000-2 §11.3.5.2) and the non-separable four (§11.3.5.3).
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
    Hue,
    Saturation,
    Color,
    Luminosity,
}

impl BlendMode {
    /// The blended source color `B(Cb, Cs)` for one pixel, in RGBA8 terms.
    /// The page backdrop is always opaque here, so compositing stays
    /// `(1 − αs)·Cb + αs·B(Cb, Cs)` — the caller feeds this through the
    /// ordinary source-over composite in place of the raw source.
    ///
    /// Covers ISO 32000-1 §11.3.4 and §11.3.5.
    pub(crate) fn blend(self, cb: [u8; 3], cs: [u8; 3]) -> [u8; 3] {
        if self == BlendMode::Normal {
            return cs;
        }
        let b3 = [
            UNIT[cb[0] as usize],
            UNIT[cb[1] as usize],
            UNIT[cb[2] as usize],
        ];
        let s3 = [
            UNIT[cs[0] as usize],
            UNIT[cs[1] as usize],
            UNIT[cs[2] as usize],
        ];
        let v3 = match self {
            BlendMode::Hue => set_lum(set_sat(s3, sat(b3)), lum(b3)),
            BlendMode::Saturation => set_lum(set_sat(b3, sat(s3)), lum(b3)),
            BlendMode::Color => set_lum(s3, lum(b3)),
            BlendMode::Luminosity => set_lum(b3, lum(s3)),
            separable => {
                let mut v3 = [0f32; 3];
                for i in 0..3 {
                    v3[i] = blend_channel(separable, b3[i], s3[i]);
                }
                v3
            }
        };
        let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
        [q(v3[0]), q(v3[1]), q(v3[2])]
    }
}

/// One channel of a separable blend function (§11.3.5.2).
///
/// Covers ISO 32000-2 §11.3.5.2.
fn blend_channel(mode: BlendMode, b: f32, s: f32) -> f32 {
    match mode {
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
        // Normal returns early and the non-separable four never come here.
        _ => s,
    }
}

/// `Lum(C)` per §11.3.5.3.
///
/// Covers ISO 32000-1 §10.3.2 and §11.3.5.
fn lum(c: [f32; 3]) -> f32 {
    0.3 * c[0] + 0.59 * c[1] + 0.11 * c[2]
}

/// `ClipColor(C)` per §11.3.5.3: pulls out-of-range components back toward
/// the color's luminosity. `n` and `x` are taken once, before either fixup,
/// exactly as the spec's pseudocode reads them.
///
/// Covers ISO 32000-2 §11.3.5.3.
fn clip_color(mut c: [f32; 3]) -> [f32; 3] {
    let l = lum(c);
    let n = c[0].min(c[1]).min(c[2]);
    let x = c[0].max(c[1]).max(c[2]);
    if n < 0.0 {
        for v in &mut c {
            *v = l + ((*v - l) * l) / (l - n);
        }
    }
    if x > 1.0 {
        for v in &mut c {
            *v = l + ((*v - l) * (1.0 - l)) / (x - l);
        }
    }
    c
}

/// `SetLum(C, l)` per §11.3.5.3.
fn set_lum(c: [f32; 3], l: f32) -> [f32; 3] {
    let d = l - lum(c);
    clip_color([c[0] + d, c[1] + d, c[2] + d])
}

/// `Sat(C)` per §11.3.5.3.
fn sat(c: [f32; 3]) -> f32 {
    c[0].max(c[1]).max(c[2]) - c[0].min(c[1]).min(c[2])
}

/// `SetSat(C, s)` per §11.3.5.3, on the components sorted into min/mid/max
/// slots. Ties order arbitrarily — tied slots compute the same value.
fn set_sat(c: [f32; 3], s: f32) -> [f32; 3] {
    let mut order = [0usize, 1, 2];
    order.sort_by(|&a, &b| c[a].total_cmp(&c[b]));
    let [imin, imid, imax] = order;
    let mut out = [0f32; 3];
    if c[imax] > c[imin] {
        out[imid] = ((c[imid] - c[imin]) * s) / (c[imax] - c[imin]);
        out[imax] = s;
    }
    out
}

/// `HardLight(Cb, Cs)` per §11.3.5.2; `Overlay` is the same with the
/// operands swapped.
///
/// Covers ISO 32000-2 §11.3.5.2.
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
///
/// Covers ISO 32000-1 §11.3.2, §11.3.3, §11.3.7 and §11.3.7.3.
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
///
/// Covers ISO 32000-1 §11.3.6, §11.3.7, §11.3.7.2, §11.6.4, §8.5.3.3.1 and §8.5.3.3.2.
#[allow(clippy::too_many_arguments)]
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
    prepare_edges(&mut scratch.edges, polys);
    sweep_rows(scratch, pix.width, pix.height, rule, |y, row, lo, hi| {
        paint_row(pix, y, row, lo, hi, clip, base_a, rgb, blend)
    });
}

/// Paints one coverage row's `[lo, hi)` columns onto `pix` at row `y`,
/// narrowing to the clip mask's stored bbox and dispatching to the blend
/// loop — the single row-painting path behind both [`fill_path`] and
/// [`fill_spans`].
///
/// Covers ISO 32000-1 §11.3.7.2.
#[allow(clippy::too_many_arguments)]
fn paint_row(
    pix: &mut Pixmap,
    y: u32,
    row: &[f32],
    mut lo: usize,
    mut hi: usize,
    clip: Option<&Mask>,
    base_a: f32,
    rgb: [u8; 3],
    blend: BlendMode,
) {
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
    let base = (y as usize * pix.width as usize + lo) * 4;
    let dst_row = &mut pix.data[base..base + (hi - lo) * 4];
    if blend == BlendMode::Normal {
        blend_row::<true>(dst_row, &row[lo..hi], mask_row, base_a, rgb, blend);
    } else {
        blend_row::<false>(dst_row, &row[lo..hi], mask_row, base_a, rgb, blend);
    }
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
    // `base_a` is clamped to [0, 1], so `>= 1.0` means exactly 1.0: a fully
    // covered pixel then writes exactly the source color, and a run of them
    // becomes a plain pattern fill instead of per-pixel arithmetic.
    let solid_src = NORMAL && base_a >= 1.0;
    let n = covs.len();
    let dst_row = &mut dst_row[..n * 4];
    match mask_row {
        None => {
            let mut x = 0;
            while x < n {
                let cov = covs[x];
                if solid_src && cov >= 1.0 {
                    let start = x;
                    x += 1;
                    while x < n && covs[x] >= 1.0 {
                        x += 1;
                    }
                    fill_run(&mut dst_row[start * 4..x * 4], opaque);
                    continue;
                }
                // Anti-aliased stretches composite four pixels per step on
                // vector lanes where the arithmetic is bit-identical to
                // [`composite_over`]; lanes at full coverage or zero take
                // the same shortcuts the scalar form takes.
                if NORMAL {
                    while x + 4 <= n && !(solid_src && covs[x..x + 4].iter().any(|&c| c >= 1.0)) {
                        blend_normal4(
                            &mut dst_row[x * 4..x * 4 + 16],
                            &covs[x..x + 4],
                            base_a,
                            rgb,
                            opaque,
                        );
                        x += 4;
                    }
                    if x >= n {
                        break;
                    }
                    let cov = covs[x];
                    if solid_src && cov >= 1.0 {
                        continue;
                    }
                    let a = cov.clamp(0.0, 1.0) * base_a;
                    paint_pixel::<NORMAL>(&mut dst_row[x * 4..(x + 1) * 4], a, rgb, opaque, blend);
                    x += 1;
                    continue;
                }
                let a = cov.clamp(0.0, 1.0) * base_a;
                paint_pixel::<NORMAL>(&mut dst_row[x * 4..(x + 1) * 4], a, rgb, opaque, blend);
                x += 1;
            }
        }
        Some(mrow) => {
            let mrow = &mrow[..n];
            let mut x = 0;
            while x < n {
                let cov = covs[x];
                if solid_src && cov >= 1.0 && mrow[x] == 255 {
                    let start = x;
                    x += 1;
                    while x < n && covs[x] >= 1.0 && mrow[x] == 255 {
                        x += 1;
                    }
                    fill_run(&mut dst_row[start * 4..x * 4], opaque);
                    continue;
                }
                let a = (cov.clamp(0.0, 1.0) * base_a) * UNIT[mrow[x] as usize];
                paint_pixel::<NORMAL>(&mut dst_row[x * 4..(x + 1) * 4], a, rgb, opaque, blend);
                x += 1;
            }
        }
    }
}

/// Source-over composites four adjacent pixels of one row: the vector
/// twin of four [`paint_pixel::<true>`] calls, byte-identical because the
/// lanes perform [`composite_over`]'s exact f32 operations (no fused
/// multiply-add, truncating converts) and the full/zero-coverage lanes
/// reproduce its shortcuts.
fn blend_normal4(dst: &mut [u8], covs: &[f32], base_a: f32, rgb: [u8; 3], opaque: [u8; 4]) {
    #[cfg(target_arch = "aarch64")]
    {
        // SAFETY: NEON is baseline on aarch64.
        unsafe { blend_hw::normal4(dst, covs, base_a, rgb, opaque) };
        return;
    }
    #[cfg(target_arch = "x86_64")]
    {
        // SAFETY: SSE2 is baseline on x86_64.
        unsafe { blend_hw::normal4(dst, covs, base_a, rgb, opaque) };
        return;
    }
    #[allow(unreachable_code)]
    for (i, &cov) in covs.iter().enumerate() {
        let a = cov.clamp(0.0, 1.0) * base_a;
        paint_pixel::<true>(
            &mut dst[i * 4..i * 4 + 4],
            a,
            rgb,
            opaque,
            BlendMode::Normal,
        );
    }
}

#[cfg(target_arch = "aarch64")]
mod blend_hw {
    use core::arch::aarch64::{
        vaddq_f32, vbslq_u32, vceqq_f32, vcgeq_f32, vcleq_f32, vcvtq_f32_u32, vcvtq_u32_f32,
        vdivq_f32, vdupq_n_f32, vdupq_n_u32, vld1q_f32, vld1q_u8, vmaxq_f32, vminq_f32, vmulq_f32,
        vreinterpretq_u32_u8, vreinterpretq_u8_u32, vst1q_u8, vsubq_f32,
    };

    /// # Safety
    /// NEON is baseline on aarch64; `dst` must hold 16 bytes and `covs`
    /// four coverages.
    pub unsafe fn normal4(
        dst: &mut [u8],
        covs: &[f32],
        base_a: f32,
        rgb: [u8; 3],
        opaque: [u8; 4],
    ) {
        let one = vdupq_n_f32(1.0);
        let zero = vdupq_n_f32(0.0);
        // a = clamp(cov, 0, 1) * base_a, per pixel.
        let cov = vld1q_f32(covs.as_ptr());
        let a = vmulq_f32(vminq_f32(vmaxq_f32(cov, zero), one), vdupq_n_f32(base_a));

        // Load 4 interleaved RGBA pixels and split channels via shifts.
        let px = vreinterpretq_u32_u8(vld1q_u8(dst.as_ptr()));
        let byte = |lane_shift: u32| {
            use core::arch::aarch64::{vandq_u32, vdupq_n_u32, vshrq_n_u32};
            let shifted = match lane_shift {
                0 => px,
                8 => vshrq_n_u32::<8>(px),
                16 => vshrq_n_u32::<16>(px),
                _ => vshrq_n_u32::<24>(px),
            };
            vcvtq_f32_u32(vandq_u32(shifted, vdupq_n_u32(0xff)))
        };
        let (dr, dg, db, da_bytes) = (byte(0), byte(8), byte(16), byte(24));

        let da = vdivq_f32(da_bytes, vdupq_n_f32(255.0));
        let one_minus_a = vsubq_f32(one, a);
        let oa = vaddq_f32(a, vmulq_f32(da, one_minus_a));
        let dw = vmulq_f32(da, one_minus_a);

        let half = vdupq_n_f32(0.5);
        let channel = |s: u8, d: core::arch::aarch64::float32x4_t| {
            let c = vdivq_f32(
                vaddq_f32(vmulq_f32(vdupq_n_f32(s as f32), a), vmulq_f32(d, dw)),
                oa,
            );
            vcvtq_u32_f32(vaddq_f32(c, half))
        };
        let r = channel(rgb[0], dr);
        let g = channel(rgb[1], dg);
        let b = channel(rgb[2], db);
        let out_a = vcvtq_u32_f32(vaddq_f32(vmulq_f32(oa, vdupq_n_f32(255.0)), half));

        use core::arch::aarch64::{vorrq_u32, vshlq_n_u32};
        let packed = vorrq_u32(
            vorrq_u32(r, vshlq_n_u32::<8>(g)),
            vorrq_u32(vshlq_n_u32::<16>(b), vshlq_n_u32::<24>(out_a)),
        );

        // Per-lane shortcuts, exactly the scalar branches: a <= 0 leaves
        // the pixel; a >= 1 writes the opaque source; oa <= 0 writes
        // transparent black.
        let src_lane = vdupq_n_u32(u32::from_le_bytes(opaque));
        let zero_lane = vdupq_n_u32(0);
        let keep = vcleq_f32(a, zero);
        let full = vcgeq_f32(a, one);
        let clear = vceqq_f32(vminq_f32(oa, zero), oa); // oa <= 0
        let mut out = vbslq_u32(clear, zero_lane, packed);
        out = vbslq_u32(full, src_lane, out);
        out = vbslq_u32(keep, px, out);
        vst1q_u8(dst.as_mut_ptr(), vreinterpretq_u8_u32(out));
    }
}

#[cfg(target_arch = "x86_64")]
mod blend_hw {
    use core::arch::x86_64::{
        __m128, __m128i, _mm_add_ps, _mm_and_si128, _mm_andnot_si128, _mm_castps_si128,
        _mm_cmpge_ps, _mm_cmple_ps, _mm_cvtepi32_ps, _mm_cvttps_epi32, _mm_div_ps, _mm_loadu_ps,
        _mm_loadu_si128, _mm_max_ps, _mm_min_ps, _mm_mul_ps, _mm_or_si128, _mm_set1_epi32,
        _mm_set1_ps, _mm_slli_epi32, _mm_srli_epi32, _mm_storeu_si128, _mm_sub_ps,
    };

    /// # Safety
    /// SSE2 is baseline on x86_64; `dst` must hold 16 bytes and `covs`
    /// four coverages.
    pub unsafe fn normal4(
        dst: &mut [u8],
        covs: &[f32],
        base_a: f32,
        rgb: [u8; 3],
        opaque: [u8; 4],
    ) {
        let one = _mm_set1_ps(1.0);
        let zero = _mm_set1_ps(0.0);
        let cov = _mm_loadu_ps(covs.as_ptr());
        let a = _mm_mul_ps(_mm_min_ps(_mm_max_ps(cov, zero), one), _mm_set1_ps(base_a));

        let px = _mm_loadu_si128(dst.as_ptr().cast::<__m128i>());
        let mask = _mm_set1_epi32(0xff);
        let byte = |shift: i32| -> __m128 {
            let shifted = match shift {
                0 => px,
                8 => _mm_srli_epi32(px, 8),
                16 => _mm_srli_epi32(px, 16),
                _ => _mm_srli_epi32(px, 24),
            };
            _mm_cvtepi32_ps(_mm_and_si128(shifted, mask))
        };
        let (dr, dg, db, da_bytes) = (byte(0), byte(8), byte(16), byte(24));

        let da = _mm_div_ps(da_bytes, _mm_set1_ps(255.0));
        let one_minus_a = _mm_sub_ps(one, a);
        let oa = _mm_add_ps(a, _mm_mul_ps(da, one_minus_a));
        let dw = _mm_mul_ps(da, one_minus_a);

        let half = _mm_set1_ps(0.5);
        let channel = |s: u8, d: __m128| -> __m128i {
            let c = _mm_div_ps(
                _mm_add_ps(_mm_mul_ps(_mm_set1_ps(s as f32), a), _mm_mul_ps(d, dw)),
                oa,
            );
            _mm_cvttps_epi32(_mm_add_ps(c, half))
        };
        let r = channel(rgb[0], dr);
        let g = channel(rgb[1], dg);
        let b = channel(rgb[2], db);
        let out_a = _mm_cvttps_epi32(_mm_add_ps(_mm_mul_ps(oa, _mm_set1_ps(255.0)), half));

        let packed = _mm_or_si128(
            _mm_or_si128(r, _mm_slli_epi32(g, 8)),
            _mm_or_si128(_mm_slli_epi32(b, 16), _mm_slli_epi32(out_a, 24)),
        );

        let src_lane = _mm_set1_epi32(i32::from_le_bytes(opaque));
        let keep = _mm_castps_si128(_mm_cmple_ps(a, zero));
        let full = _mm_castps_si128(_mm_cmpge_ps(a, one));
        let clear = _mm_castps_si128(_mm_cmple_ps(oa, zero));
        let mut out = _mm_or_si128(
            _mm_andnot_si128(clear, packed),
            _mm_and_si128(clear, _mm_set1_epi32(0)),
        );
        out = _mm_or_si128(_mm_and_si128(full, src_lane), _mm_andnot_si128(full, out));
        out = _mm_or_si128(_mm_and_si128(keep, px), _mm_andnot_si128(keep, out));
        _mm_storeu_si128(dst.as_mut_ptr().cast::<__m128i>(), out);
    }
}

/// Fills a run of pixels with one RGBA value (plain repeated 4-byte
/// pattern; the loop lowers to wide stores).
fn fill_run(dst: &mut [u8], px: [u8; 4]) {
    for chunk in dst.as_chunks_mut::<4>().0 {
        *chunk = px;
    }
}

/// Source-over paints one pixel at alpha `a`, honoring the blend mode.
///
/// Covers ISO 32000-1 §11.2, §11.3.3 and §11.3.8.
#[inline(always)]
pub(crate) fn paint_pixel<const NORMAL: bool>(
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
    let rgb = blended(blend, dst, rgb);
    if a >= 1.0 {
        dst.copy_from_slice(&[rgb[0], rgb[1], rgb[2], 255]);
    } else {
        composite_over(dst, rgb, a);
    }
}

/// The source color a non-Normal blend paints with: `B(Cb, Cs)` weighted
/// by the backdrop alpha, so a transparent backdrop (a group's fresh
/// buffer) passes the source color through unchanged and an opaque one
/// (the page) blends fully.
///
/// Covers ISO 32000-1 §11.3.8.
fn blended(blend: BlendMode, dst: &[u8], rgb: [u8; 3]) -> [u8; 3] {
    let b = blend.blend([dst[0], dst[1], dst[2]], rgb);
    let ab = UNIT[dst[3] as usize];
    if ab >= 1.0 {
        return b;
    }
    let mix = |s: u8, t: u8| ((1.0 - ab) * s as f32 + ab * t as f32 + 0.5) as u8;
    [mix(rgb[0], b[0]), mix(rgb[1], b[1]), mix(rgb[2], b[2])]
}

/// Composites an isolated transparency group's offscreen render `src`
/// onto `dst` as one object: each group pixel's alpha, scaled by the
/// constant `alpha` and the `soft_mask` coverage, is the source alpha, and
/// its color blends against the backdrop through `blend`. Only `region`'s
/// bounding box is visited (the group's clip: it painted nowhere else);
/// `None` is the whole page. This is the isolated case of the group
/// compositing formulas: with a transparent initial backdrop the group's
/// result is its own render, and the mask and constant alpha apply once.
///
/// Covers ISO 32000-1 §11.6.6, §11.4.7, §11.6.4.4, §11.4.4, §11.4.5 and §11.4.8.
pub(crate) fn composite_group(
    dst: &mut Pixmap,
    src: &Pixmap,
    alpha: f32,
    blend: BlendMode,
    soft_mask: Option<&Mask>,
    region: Option<&Mask>,
) {
    let alpha = if alpha.is_finite() {
        alpha.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if alpha <= 0.0 {
        return;
    }
    let Some((x0, y0, x1, y1)) = region_bounds(dst, region) else {
        return;
    };
    let stride = dst.width as usize;
    for y in y0..y1 {
        let row = y as usize * stride;
        for x in x0..x1 {
            let i = (row + x as usize) * 4;
            let s = &src.data[i..i + 4];
            if s[3] == 0 {
                continue;
            }
            let cover = match soft_mask {
                Some(m) => UNIT[m.coverage(x, y) as usize],
                None => 1.0,
            };
            let a = UNIT[s[3] as usize] * alpha * cover;
            if a <= 0.0 {
                continue;
            }
            let d = &mut dst.data[i..i + 4];
            let rgb = match blend {
                BlendMode::Normal => [s[0], s[1], s[2]],
                _ => blended(blend, d, [s[0], s[1], s[2]]),
            };
            composite_over(d, rgb, a);
        }
    }
}

/// Composites a non-isolated transparency group rendered over a copy of
/// its backdrop (see [`copy_region`]) back onto that backdrop `dst`. With
/// the backdrop's contribution removed and the group re-composited at the
/// constant `alpha` (§11.4.7), the result reduces to a mix of backdrop and
/// group render weighted by `alpha` and the `soft_mask` coverage: a pixel
/// the group never touched is unchanged, and a Normal outer blend mode is
/// assumed. Only `region`'s bounding box is visited.
///
/// Covers ISO 32000-1 §11.6.6, §11.4.7, §11.6.4.4, §11.4.2 and §11.4.4.
pub(crate) fn mix_group(
    dst: &mut Pixmap,
    src: &Pixmap,
    alpha: f32,
    soft_mask: Option<&Mask>,
    region: Option<&Mask>,
) {
    let alpha = if alpha.is_finite() {
        alpha.clamp(0.0, 1.0)
    } else {
        1.0
    };
    if alpha <= 0.0 {
        return;
    }
    let Some((x0, y0, x1, y1)) = region_bounds(dst, region) else {
        return;
    };
    let stride = dst.width as usize;
    for y in y0..y1 {
        let row = y as usize * stride;
        for x in x0..x1 {
            let i = (row + x as usize) * 4;
            let s = &src.data[i..i + 4];
            let d = &mut dst.data[i..i + 4];
            if s == d {
                continue;
            }
            let cover = match soft_mask {
                Some(m) => UNIT[m.coverage(x, y) as usize],
                None => 1.0,
            };
            let w = alpha * cover;
            if w <= 0.0 {
                continue;
            }
            if w >= 1.0 {
                d.copy_from_slice(s);
                continue;
            }
            let (da, sa) = (UNIT[d[3] as usize], UNIT[s[3] as usize]);
            let oa = da + (sa - da) * w;
            if oa <= 0.0 {
                d.copy_from_slice(&[0, 0, 0, 0]);
                continue;
            }
            for c in 0..3 {
                let pd = d[c] as f32 * da;
                let ps = s[c] as f32 * sa;
                d[c] = ((pd + (ps - pd) * w) / oa + 0.5) as u8;
            }
            d[3] = (oa * 255.0 + 0.5) as u8;
        }
    }
}

/// Copies `region`'s bounding box of `src` into `dst` (the whole page for
/// `None`): the group backdrop a non-isolated group starts from.
///
/// Covers ISO 32000-1 §11.4.7 and §11.4.3.
pub(crate) fn copy_region(dst: &mut Pixmap, src: &Pixmap, region: Option<&Mask>) {
    let Some((x0, y0, x1, y1)) = region_bounds(dst, region) else {
        return;
    };
    let stride = dst.width as usize * 4;
    for y in y0..y1 {
        let from = y as usize * stride + x0 as usize * 4;
        let to = y as usize * stride + x1 as usize * 4;
        dst.data[from..to].copy_from_slice(&src.data[from..to]);
    }
}

/// The device-pixel box `[x0, x1) x [y0, y1)` a group composite visits:
/// the clip mask's stored bbox clamped to the page, or the whole page
/// without a mask; `None` when nothing is inside.
fn region_bounds(pix: &Pixmap, region: Option<&Mask>) -> Option<(u32, u32, u32, u32)> {
    let (x0, y0, x1, y1) = match region {
        Some(m) => (m.x0, m.y0, m.x0 + m.bbox_w, m.y0 + m.bbox_h),
        None => (0, 0, pix.width, pix.height),
    };
    let (x1, y1) = (x1.min(pix.width), y1.min(pix.height));
    if x1 <= x0 || y1 <= y0 {
        return None;
    }
    Some((x0, y0, x1, y1))
}

#[cfg(test)]
mod blend_hw_tests {
    use super::*;

    /// Over a transparent backdrop the blend function contributes nothing:
    /// a Multiply of red onto transparent black paints red, not black, and
    /// onto an opaque backdrop it multiplies as before.
    // Covers ISO 32000-1 §11.3.3 and §11.3.8.
    #[test]
    fn blend_over_a_transparent_backdrop_passes_the_source_through() {
        let mut clear = [0u8, 0, 0, 0];
        paint_pixel::<false>(
            &mut clear,
            1.0,
            [255, 0, 0],
            [255, 0, 0, 255],
            BlendMode::Multiply,
        );
        assert_eq!(clear, [255, 0, 0, 255]);
        let mut white = [255u8, 255, 255, 255];
        paint_pixel::<false>(
            &mut white,
            1.0,
            [255, 0, 0],
            [255, 0, 0, 255],
            BlendMode::Multiply,
        );
        assert_eq!(white, [255, 0, 0, 255]);
        let mut grey = [128u8, 128, 128, 255];
        paint_pixel::<false>(
            &mut grey,
            1.0,
            [255, 255, 255],
            [255, 255, 255, 255],
            BlendMode::Multiply,
        );
        assert_eq!(grey, [128, 128, 128, 255]);
        let mut half = [0u8, 0, 0, 128];
        paint_pixel::<false>(
            &mut half,
            1.0,
            [200, 200, 200],
            [200, 200, 200, 255],
            BlendMode::Multiply,
        );
        assert_eq!(half, [100, 100, 100, 255]);
    }

    /// The vector composite must be byte-identical to four scalar
    /// [`paint_pixel`] calls across every lane-shortcut combination:
    /// zero coverage, full coverage, transparent destinations, and
    /// anti-aliased fractions, mixed within one vector.
    // Covers ISO 32000-1 §11.3.3.
    #[test]
    fn vector_blend_matches_scalar_bytes() {
        let mut state = 0x2468aceu32;
        let mut next = move || {
            state = state.wrapping_mul(1664525).wrapping_add(1013904223);
            state
        };
        for _ in 0..2000 {
            let covs: Vec<f32> = (0..4)
                .map(|_| match next() % 5 {
                    0 => 0.0,
                    1 => 1.0,
                    2 => 1.5,
                    3 => -0.25,
                    _ => (next() % 1000) as f32 / 1000.0,
                })
                .collect();
            let base_a = match next() % 3 {
                0 => 1.0,
                1 => 0.0,
                _ => (next() % 1000) as f32 / 1000.0,
            };
            let rgb = [next() as u8, next() as u8, next() as u8];
            let opaque = [rgb[0], rgb[1], rgb[2], 255];
            let mut dst: Vec<u8> = (0..16).map(|_| next() as u8).collect();
            if next() % 4 == 0 {
                for px in dst.as_chunks_mut::<4>().0 {
                    px[3] = 0;
                }
            }
            let mut scalar = dst.clone();
            for (i, &cov) in covs.iter().enumerate() {
                let a = cov.clamp(0.0, 1.0) * base_a;
                paint_pixel::<true>(
                    &mut scalar[i * 4..i * 4 + 4],
                    a,
                    rgb,
                    opaque,
                    BlendMode::Normal,
                );
            }
            blend_normal4(&mut dst, &covs, base_a, rgb, opaque);
            assert_eq!(dst, scalar, "covs {covs:?} base_a {base_a}");
        }
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

    /// Clips `poly` to the half-plane where `keep` is non-negative,
    /// interpolating the crossings. Sutherland-Hodgman, and exact for the
    /// convex subjects this reference clips.
    fn clip_half(poly: &[(f64, f64)], keep: impl Fn((f64, f64)) -> f64) -> Vec<(f64, f64)> {
        let mut out = Vec::new();
        for i in 0..poly.len() {
            let a = poly[i];
            let b = poly[(i + 1) % poly.len()];
            let (da, db) = (keep(a), keep(b));
            if da >= 0.0 {
                out.push(a);
            }
            if (da >= 0.0) == (db >= 0.0) {
                continue;
            }
            let t = da / (da - db);
            out.push((a.0 + (b.0 - a.0) * t, a.1 + (b.1 - a.1) * t));
        }
        out
    }

    /// The exact area a convex `poly` cuts out of the pixel square at
    /// `(x, y)`: clip to the square, then the shoelace formula. This is the
    /// quantity the sweep claims to compute, worked out independently.
    fn cell_area(poly: &[Point], x: u32, y: u32) -> f64 {
        let (lx, ly) = (x as f64, y as f64);
        let mut p: Vec<(f64, f64)> = poly.iter().map(|q| (q.x as f64, q.y as f64)).collect();
        p = clip_half(&p, |v| v.0 - lx);
        p = clip_half(&p, |v| lx + 1.0 - v.0);
        p = clip_half(&p, |v| v.1 - ly);
        p = clip_half(&p, |v| ly + 1.0 - v.1);
        if p.len() < 3 {
            return 0.0;
        }
        let mut twice = 0.0;
        for i in 0..p.len() {
            let a = p[i];
            let b = p[(i + 1) % p.len()];
            twice += a.0 * b.1 - b.0 * a.1;
        }
        twice.abs() * 0.5
    }

    fn poly(pts: &[(f32, f32)]) -> Vec<Point> {
        pts.iter().map(|&(x, y)| Point::new(x, y)).collect()
    }

    /// Every pixel's coverage is the area the path cuts out of it, to
    /// within the mask's own rounding: a slanted edge, a wedge whose edges
    /// are nearly horizontal, a rectangle on fractional coordinates, and a
    /// diamond whose vertices fall inside rows. Sampling a row at fixed
    /// heights integrates the covered width exactly only while that width
    /// runs linearly down the row, which any row holding a vertex or an
    /// edge endpoint breaks, and flattened curves and glyph outlines are
    /// mostly such rows.
    ///
    /// Covers ISO 32000-1 §10.6.4.
    #[test]
    fn coverage_is_the_area_the_path_cuts_from_each_pixel() {
        let shapes = [
            (
                "slanted triangle",
                poly(&[(2.0, 2.0), (9.5, 3.5), (3.5, 9.0)]),
            ),
            (
                "near-horizontal wedge",
                poly(&[(1.0, 4.0), (11.0, 4.2), (11.0, 4.9)]),
            ),
            (
                "fractional rect",
                poly(&[(1.3, 2.7), (8.6, 2.7), (8.6, 9.1), (1.3, 9.1)]),
            ),
            (
                "diamond",
                poly(&[(6.0, 1.5), (10.5, 6.0), (6.0, 10.5), (1.5, 6.0)]),
            ),
        ];
        for (name, pts) in shapes {
            let polys = [Subpath {
                points: pts.clone(),
                closed: true,
            }];
            let mask = mask_from_path(12, 12, &polys, FillRule::NonZero);
            for y in 0..12 {
                for x in 0..12 {
                    let want = cell_area(&pts, x, y);
                    let got = mask.coverage(x, y) as f64 / 255.0;
                    assert!(
                        (got - want).abs() <= 1.0 / 255.0,
                        "{name} at ({x},{y}): got {got:.4}, area is {want:.4}"
                    );
                }
            }
        }
    }

    /// Two shapes far apart in one path, each row's gap between them
    /// wider than the block the coverage walk skips in one go. The gap
    /// carries no coverage, and the shapes narrow at different rates, so a
    /// row that took its gap from the row before it would show the earlier
    /// row's edges. Coverage is the sum of the two shapes' areas because
    /// they never overlap.
    ///
    /// Covers ISO 32000-1 §10.6.4.
    #[test]
    fn a_gap_between_two_shapes_carries_no_coverage_from_the_row_above() {
        let left = poly(&[(1.0, 1.0), (9.0, 1.0), (2.0, 19.0)]);
        let right = poly(&[(191.0, 1.0), (199.0, 1.0), (199.0, 19.0)]);
        let polys = [
            Subpath {
                points: left.clone(),
                closed: true,
            },
            Subpath {
                points: right.clone(),
                closed: true,
            },
        ];
        let mask = mask_from_path(200, 20, &polys, FillRule::NonZero);
        for y in 0..20 {
            for x in 0..200 {
                let want = cell_area(&left, x, y) + cell_area(&right, x, y);
                let got = mask.coverage(x, y) as f64 / 255.0;
                assert!(
                    (got - want).abs() <= 1.0 / 255.0,
                    "({x},{y}): got {got:.4}, area is {want:.4}"
                );
            }
        }
    }

    /// The same exactness where the path leaves the page. An edge crossing
    /// x = 0 inside a row gives the first column the height it cuts off
    /// there, not the height its clamped endpoint would suggest; a path
    /// running off the right has not closed inside the page, so its
    /// coverage has to reach the last column.
    ///
    /// Covers ISO 32000-1 §10.6.4.
    #[test]
    fn coverage_is_exact_where_the_path_leaves_the_page() {
        let shapes = [
            (
                "off the left",
                poly(&[(-6.0, 1.0), (7.5, 3.0), (-6.0, 10.0)]),
            ),
            (
                "off the right",
                poly(&[(18.0, 1.0), (4.5, 3.0), (18.0, 10.0)]),
            ),
            ("off the top", poly(&[(2.0, -6.0), (9.0, -6.0), (5.5, 7.5)])),
            (
                "off the bottom",
                poly(&[(2.0, 18.0), (9.0, 18.0), (5.5, 4.5)]),
            ),
            (
                "wider than the page",
                poly(&[(-9.0, 2.5), (21.0, 3.5), (21.0, 8.5), (-9.0, 9.5)]),
            ),
        ];
        for (name, pts) in shapes {
            let polys = [Subpath {
                points: pts.clone(),
                closed: true,
            }];
            let mask = mask_from_path(12, 12, &polys, FillRule::NonZero);
            for y in 0..12 {
                for x in 0..12 {
                    let want = cell_area(&pts, x, y);
                    let got = mask.coverage(x, y) as f64 / 255.0;
                    assert!(
                        (got - want).abs() <= 1.0 / 255.0,
                        "{name} at ({x},{y}): got {got:.4}, area is {want:.4}"
                    );
                }
            }
        }
    }

    const RED: [u8; 4] = [255, 0, 0, 255];

    // Covers ISO 32000-1 §10.6.4.
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

    // Covers ISO 32000-1 §10.6.4 and §11.3.7.2.
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

    // Covers ISO 32000-1 §10.6.4 and §11.6.4.2.
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

    // Covers ISO 32000-1 §10.6.4 and §8.5.3.3.3.
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

    // Covers ISO 32000-1 §8.5.3.3.2.
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

    // Covers ISO 32000-1 §10.6.4 and §8.5.3.3.2.
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

    // Covers ISO 32000-1 §8.5.4.
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

    // Covers ISO 32000-1 §8.5.4.
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

    // Covers ISO 32000-1 §11.3.3, §11.3.7 and §11.6.4.4.
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

    // Non-separable blend vectors, hand-computed from the ISO 32000-2
    // §11.3.5.3 formulas (Lum weights 0.3/0.59/0.11).

    // Covers ISO 32000-2 §11.3.5.3.
    #[test]
    fn hue_takes_source_hue_at_backdrop_luminosity() {
        // B(red, blue) = SetLum(SetSat(blue, Sat(red)=1) = blue, Lum(red)=0.3):
        // d = 0.3 − 0.11 → [0.19, 0.19, 1.19]; ClipColor's x>1 branch maps
        // to [0.213483, 0.213483, 1.0] → bytes [54, 54, 255].
        let got = BlendMode::Hue.blend([255, 0, 0], [0, 0, 255]);
        assert_eq!(got, [54, 54, 255]);
    }

    #[test]
    fn hue_with_a_gray_source_paints_backdrop_luminosity_gray() {
        // SetSat's min == max branch: a gray source zeroes out, then
        // SetLum lifts it to Lum(yellow) = 0.89 → bytes [227, 227, 227].
        let got = BlendMode::Hue.blend([255, 255, 0], [128, 128, 128]);
        assert_eq!(got, [227, 227, 227]);
    }

    // Covers ISO 32000-2 §11.3.5.3.
    #[test]
    fn saturation_of_a_gray_source_desaturates_the_backdrop() {
        // Sat(gray) = 0, so SetSat(yellow, 0) = [0, 0, 0]; SetLum lifts it
        // to Lum(yellow) = 0.89 → bytes [227, 227, 227].
        let got = BlendMode::Saturation.blend([255, 255, 0], [128, 128, 128]);
        assert_eq!(got, [227, 227, 227]);
    }

    // Covers ISO 32000-2 §11.3.5.3.
    #[test]
    fn color_takes_source_color_at_backdrop_luminosity() {
        // SetLum(red, Lum(mid-gray) = 128/255): d = 0.201961 →
        // [1.201961, 0.201961, 0.201961]; ClipColor's x>1 branch maps to
        // [1.0, 0.288515, 0.288515] → bytes [255, 74, 74].
        let got = BlendMode::Color.blend([128, 128, 128], [255, 0, 0]);
        assert_eq!(got, [255, 74, 74]);
    }

    // Covers ISO 32000-2 §11.3.5.3.
    #[test]
    fn luminosity_takes_source_luminosity_at_backdrop_color() {
        // SetLum(red, Lum(mid-gray)) — the Color vector with the operands
        // swapped lands on the same clipped result [255, 74, 74].
        let got = BlendMode::Luminosity.blend([255, 0, 0], [128, 128, 128]);
        assert_eq!(got, [255, 74, 74]);
    }

    // Covers ISO 32000-2 §11.3.5.3.
    #[test]
    fn luminosity_darkening_exercises_the_negative_clip() {
        // SetLum(red, Lum(dark gray) = 64/255): d = −0.049020 →
        // [0.950980, −0.049020, −0.049020]; ClipColor's n<0 branch maps to
        // [0.836601, 0, 0] → bytes [213, 0, 0].
        let got = BlendMode::Luminosity.blend([255, 0, 0], [64, 64, 64]);
        assert_eq!(got, [213, 0, 0]);
    }

    // Covers ISO 32000-1 §10.6.4 and §8.5.3.3.1.
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

#[cfg(test)]
mod stage_times {
    use super::*;
    use pdfboss_core::geom::Point;

    fn glyph_like(cx: f32, cy: f32) -> Vec<Subpath> {
        // An 8-point closed blob roughly the size of a 12pt glyph.
        let pts = [
            (0.0, 0.0),
            (4.0, -1.0),
            (7.0, 2.0),
            (8.0, 6.0),
            (6.0, 9.0),
            (3.0, 10.0),
            (0.5, 8.0),
            (-1.0, 4.0),
        ];
        vec![Subpath {
            points: pts
                .iter()
                .map(|&(x, y)| Point::new(cx + x, cy + y))
                .collect(),
            closed: true,
        }]
    }

    /// Not a correctness test: prints where a text-page-shaped fill
    /// workload spends its time. Run explicitly:
    /// `cargo test --release -p pdfboss-render raster::stage_times -- --nocapture --ignored`
    #[test]
    #[ignore]
    fn print_stage_times() {
        let mut pix = Pixmap::new(612, 792);
        pix.fill([255, 255, 255, 255]);
        let mut scratch = RasterScratch::default();
        let glyphs: Vec<Vec<Subpath>> = (0..2000)
            .map(|i| glyph_like(20.0 + (i % 60) as f32 * 9.5, 20.0 + (i / 60) as f32 * 12.0))
            .collect();
        let reps = 20;

        // Whole fill (edges + sweep + blend).
        let t0 = std::time::Instant::now();
        for _ in 0..reps {
            for polys in &glyphs {
                fill_path(
                    &mut pix,
                    &mut scratch,
                    polys,
                    FillRule::NonZero,
                    [20, 20, 20, 255],
                    1.0,
                    None,
                    BlendMode::Normal,
                );
            }
        }
        println!("fill (2000 glyphs): {:?}/page", t0.elapsed() / reps);

        // Sweep only: coverage accumulation with a no-op emit.
        let t0 = std::time::Instant::now();
        for _ in 0..reps {
            for polys in &glyphs {
                prepare_edges(&mut scratch.edges, polys);
                sweep_rows(&mut scratch, 612, 792, FillRule::NonZero, |_, _, _, _| {});
            }
        }
        println!("sweep only:         {:?}/page", t0.elapsed() / reps);

        // Edge preparation alone.
        let t0 = std::time::Instant::now();
        for _ in 0..reps {
            for polys in &glyphs {
                prepare_edges(&mut scratch.edges, polys);
            }
        }
        println!("prepare_edges:      {:?}/page", t0.elapsed() / reps);

        // One big fill: a page-sized rounded blob, 40 of them.
        let big: Vec<Subpath> = vec![Subpath {
            points: (0..64)
                .map(|i| {
                    let a = i as f32 / 64.0 * std::f32::consts::TAU;
                    Point::new(306.0 + 280.0 * a.cos(), 396.0 + 370.0 * a.sin())
                })
                .collect(),
            closed: true,
        }];
        let t0 = std::time::Instant::now();
        for _ in 0..reps {
            for _ in 0..40 {
                fill_path(
                    &mut pix,
                    &mut scratch,
                    &big,
                    FillRule::NonZero,
                    [40, 80, 120, 200],
                    0.8,
                    None,
                    BlendMode::Normal,
                );
            }
        }
        println!("fill (40 big):      {:?}/page", t0.elapsed() / reps);
    }
}

#[cfg(test)]
mod span_tests {
    use super::*;
    use pdfboss_core::geom::Point;

    /// A self-intersecting star on subpixel coordinates: exercises the
    /// nonzero winding rule, coincident-crossing ordering and fractional
    /// coverage at every edge.
    fn star(cx: f32, cy: f32, r: f32) -> Subpath {
        Subpath {
            points: (0..5)
                .map(|i| {
                    let a = (i * 2) as f32 / 5.0 * std::f32::consts::TAU - 0.37;
                    Point::new(cx + r * a.cos(), cy + r * a.sin())
                })
                .collect(),
            closed: true,
        }
    }

    fn subpixel_rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Subpath {
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

    const INK: [u8; 4] = [30, 60, 200, 255];

    fn direct(polys: &[Subpath], alpha: f32, clip: Option<&Mask>, blend: BlendMode) -> Pixmap {
        let mut pix = Pixmap::new(24, 24);
        fill_path(
            &mut pix,
            &mut RasterScratch::default(),
            polys,
            FillRule::NonZero,
            INK,
            alpha,
            clip,
            blend,
        );
        pix
    }

    fn replayed(polys: &[Subpath], alpha: f32, clip: Option<&Mask>, blend: BlendMode) -> Pixmap {
        let mut scratch = RasterScratch::default();
        let set = capture_spans(&mut scratch, polys, FillRule::NonZero).expect("captured");
        let mut pix = Pixmap::new(24, 24);
        fill_spans(
            &mut pix,
            &mut scratch,
            &set,
            0.0,
            0,
            INK,
            alpha,
            clip,
            blend,
        );
        pix
    }

    /// Capturing a path's spans and replaying them at offset zero paints
    /// exactly the pixels a direct fill paints.
    #[test]
    fn captured_spans_rebuild_the_exact_coverage() {
        let polys = [star(11.3, 12.7, 9.4), subpixel_rect(1.2, 1.7, 5.8, 4.3)];
        let a = direct(&polys, 1.0, None, BlendMode::Normal);
        let b = replayed(&polys, 1.0, None, BlendMode::Normal);
        assert!(a.data.iter().any(|&v| v != 0), "the direct fill painted");
        assert_eq!(a.data, b.data);
    }

    /// Alpha scaling, a coverage clip and a non-normal blend mode reach the
    /// replay through the same row-painting path as a direct fill.
    #[test]
    fn span_fill_applies_alpha_clip_and_blend_like_fill_path() {
        let polys = [star(11.3, 12.7, 9.4)];
        let clip = Mask::from_path(
            24,
            24,
            &mut RasterScratch::default(),
            &[subpixel_rect(3.4, 2.2, 19.7, 21.3)],
            FillRule::NonZero,
        );
        let a = direct(&polys, 0.6, Some(&clip), BlendMode::Multiply);
        let b = replayed(&polys, 0.6, Some(&clip), BlendMode::Multiply);
        assert!(a.data.iter().any(|&v| v != 0), "the direct fill painted");
        assert_eq!(a.data, b.data);
    }

    /// A replay shifted by integer offsets clamps to the page exactly like a
    /// direct fill of the same translated rectangle: a rectangle's vertical
    /// edges interpolate to their stored x, so translating the geometry and
    /// translating the spans round identically and the comparison is exact.
    #[test]
    fn span_fill_shifts_and_clips_to_the_page_like_a_direct_fill() {
        let rect = subpixel_rect(2.3, 2.6, 9.7, 8.4);
        let translated = subpixel_rect(2.3 + 16.0, 2.6 + 19.0, 9.7 + 16.0, 8.4 + 19.0);
        let a = direct(&[translated], 1.0, None, BlendMode::Normal);
        let mut scratch = RasterScratch::default();
        let set = capture_spans(&mut scratch, &[rect], FillRule::NonZero).expect("captured");
        let mut b = Pixmap::new(24, 24);
        fill_spans(
            &mut b,
            &mut scratch,
            &set,
            16.0,
            19,
            INK,
            1.0,
            None,
            BlendMode::Normal,
        );
        assert!(a.data.iter().any(|&v| v != 0), "the direct fill painted");
        assert_eq!(a.data, b.data);
    }

    /// Rows shifted entirely off the page are skipped, not wrapped or
    /// painted at clamped rows.
    #[test]
    fn span_fill_skips_rows_off_the_page() {
        let rect = subpixel_rect(2.3, 2.6, 9.7, 8.4);
        let mut scratch = RasterScratch::default();
        let set = capture_spans(&mut scratch, &[rect], FillRule::NonZero).expect("captured");
        let mut pix = Pixmap::new(24, 24);
        fill_spans(
            &mut pix,
            &mut scratch,
            &set,
            0.0,
            -100,
            INK,
            1.0,
            None,
            BlendMode::Normal,
        );
        assert!(
            pix.data.iter().all(|&v| v == 0),
            "everything above the page"
        );
        fill_spans(
            &mut pix,
            &mut scratch,
            &set,
            0.0,
            100,
            INK,
            1.0,
            None,
            BlendMode::Normal,
        );
        assert!(
            pix.data.iter().all(|&v| v == 0),
            "everything below the page"
        );
    }
}
