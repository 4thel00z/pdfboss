//! Stroking: flattened segments expanded to offset quads, with the line
//! cap and join styles added at run ends and vertices, and dash patterns
//! applied at the flatten level.
//!
//! The pen is a circle in *user* space (ISO 32000-1 §8.4.3.2), so in device
//! space it is that circle carried through the current transformation — an
//! ellipse under anisotropic scaling. Reducing the matrix to one scalar
//! (say the square root of its determinant) mis-sizes every stroke the
//! moment the two axes scale differently: a matrix like
//! `0 2.0629 0.4848 0 0 0 cm` has determinant ~1, and scalar-width strokes
//! under it come out at half their true thickness, turning the stroked-line
//! gradients some producers emit into stripes. Everything here therefore
//! takes the matrix itself and offsets each segment by the device image of
//! the user-space pen radius.

use pdfboss_core::geom::{Matrix, Point};

use crate::path::Subpath;

/// Minimum stroke thickness in device pixels; thinner pens still leave a
/// visible hairline.
const MIN_WIDTH: f32 = 0.75;
/// Vertex count of the small fan approximating round joins and caps.
const FAN_SEGMENTS: usize = 12;
/// Upper bound on dash pieces produced per path, guarding pathological
/// patterns (e.g. many near-zero entries).
const MAX_DASH_PIECES: usize = 65_536;
/// Device distance below which consecutive points count as one: a
/// flattened curve or a dash cut on a vertex leaves such pairs, and they
/// have no direction to cap or join.
const MIN_SEGMENT: f32 = 1e-2;

/// Line cap style: the `J` operator and `/LC` (Table 54).
///
/// Covers ISO 32000-1 §8.4.3.3.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum LineCap {
    #[default]
    Butt,
    Round,
    Square,
}

impl LineCap {
    /// The style a `J` operand or `/LC` value names; a code outside the
    /// table leaves the initial butt cap.
    pub(crate) fn from_code(code: i32) -> LineCap {
        match code {
            1 => LineCap::Round,
            2 => LineCap::Square,
            _ => LineCap::Butt,
        }
    }
}

/// Line join style: the `j` operator and `/LJ` (Table 55).
///
/// Covers ISO 32000-1 §8.4.3.4.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub(crate) enum LineJoin {
    #[default]
    Miter,
    Round,
    Bevel,
}

impl LineJoin {
    /// The style a `j` operand or `/LJ` value names; a code outside the
    /// table leaves the initial miter join.
    pub(crate) fn from_code(code: i32) -> LineJoin {
        match code {
            1 => LineJoin::Round,
            2 => LineJoin::Bevel,
            _ => LineJoin::Miter,
        }
    }
}

/// The stroking parameters of the graphics state, all user-space
/// quantities (Table 52).
#[derive(Clone, Copy, Debug, PartialEq)]
pub(crate) struct StrokeStyle {
    /// The `/LineWidth`; the pen radius is half of it.
    pub(crate) width: f32,
    pub(crate) cap: LineCap,
    pub(crate) join: LineJoin,
    /// The ratio of miter length to line width above which a miter join
    /// is cut to a bevel; 1 or more.
    ///
    /// Covers ISO 32000-1 §8.4.3.5.
    pub(crate) miter_limit: f32,
}

fn lerp(a: Point, b: Point, t: f32) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

fn add(a: Point, b: Point) -> Point {
    Point::new(a.x + b.x, a.y + b.y)
}

fn sub(a: Point, b: Point) -> Point {
    Point::new(a.x - b.x, a.y - b.y)
}

fn scale(v: Point, k: f32) -> Point {
    Point::new(v.x * k, v.y * k)
}

/// `v` at unit length, or `None` when it has no usable length.
fn unit(v: Point) -> Option<Point> {
    let len = v.x.hypot(v.y);
    (len > 1e-6 && len.is_finite()).then(|| scale(v, 1.0 / len))
}

/// `points` with each run of points closer than [`MIN_SEGMENT`] collapsed
/// into its first.
fn distinct(points: &[Point]) -> Vec<Point> {
    let mut out: Vec<Point> = Vec::with_capacity(points.len());
    for &p in points {
        let dup = out
            .last()
            .is_some_and(|last| (p.x - last.x).hypot(p.y - last.y) <= MIN_SEGMENT);
        if !dup {
            out.push(p);
        }
    }
    out
}

/// The linear part of `m` applied to a vector — a direction or offset,
/// which a translation never touches.
fn linear(m: Matrix, v: Point) -> Point {
    Point::new(m.a * v.x + m.c * v.y, m.b * v.x + m.d * v.y)
}

/// One painted run of a polyline, with the device direction of the
/// segment it starts on. A run of one point, which a zero-length dash
/// produces, has no direction of its own and takes its caps from that
/// segment.
struct Run {
    points: Vec<Point>,
    along: Point,
}

/// Splits a polyline of at least two device-space points into its painted
/// ("on") runs according to a dash pattern. The pattern and phase are
/// user-space quantities (ISO 32000-1 §8.4.3.6), so each segment is
/// measured through `inv`, the device-to-user matrix; the cut positions
/// themselves are fractions along the segment, which a linear map
/// preserves. An empty or degenerate pattern yields the whole polyline.
fn dash_split(points: &[Point], dash: &[f32], phase: f32, inv: Matrix) -> Vec<Run> {
    let first = sub(points[1], points[0]);
    let pattern: Vec<f32> = dash
        .iter()
        .copied()
        .filter(|d| d.is_finite() && *d >= 0.0)
        .collect();
    let total: f32 = pattern.iter().sum();
    if pattern.len() != dash.len() || pattern.is_empty() || total <= 0.0 {
        return vec![Run {
            points: points.to_vec(),
            along: first,
        }];
    }
    // Consume the phase to find the starting pattern position.
    let mut idx = 0usize;
    let mut rem = pattern[0];
    let mut ph = if phase.is_finite() && phase > 0.0 {
        phase % total
    } else {
        0.0
    };
    while ph > 0.0 {
        if ph >= rem {
            ph -= rem;
            idx = (idx + 1) % pattern.len();
            rem = pattern[idx];
        } else {
            rem -= ph;
            ph = 0.0;
        }
    }
    let mut on = idx.is_multiple_of(2);
    let mut runs: Vec<Run> = Vec::new();
    let mut cur: Vec<Point> = if on { vec![points[0]] } else { Vec::new() };
    let mut cur_along = first;
    let mut pieces = 0usize;
    for seg in points.windows(2) {
        let (a, b) = (seg[0], seg[1]);
        let seglen = {
            let u = linear(inv, sub(b, a));
            u.x.hypot(u.y)
        };
        let mut done = 0.0f32;
        while seglen - done > rem && pieces < MAX_DASH_PIECES {
            done += rem;
            let p = lerp(a, b, done / seglen);
            if on {
                cur.push(p);
                if cur.len() >= 2 {
                    runs.push(Run {
                        points: std::mem::take(&mut cur),
                        along: cur_along,
                    });
                } else {
                    cur.clear();
                }
            } else {
                cur = vec![p];
                cur_along = sub(b, a);
            }
            on = !on;
            idx = (idx + 1) % pattern.len();
            rem = pattern[idx];
            pieces += 1;
        }
        rem -= seglen - done;
        if on {
            cur.push(b);
        }
    }
    if on && cur.len() >= 2 {
        runs.push(Run {
            points: cur,
            along: cur_along,
        });
    }
    runs
}

/// The pen carried into device space: the user-to-device matrix, its
/// inverse, and the sign of its determinant, which is the one orientation
/// every quad and fan must share for their union to survive the nonzero
/// rule (a negative determinant flips them all together).
#[derive(Clone, Copy)]
struct Pen {
    to_device: Matrix,
    to_user: Matrix,
    /// User-space pen radius (half the `/LineWidth`).
    r: f32,
    /// +1.0 or -1.0, the handedness of `to_device`.
    winding: f32,
    /// The device pen radius when `to_device` maps circles to circles —
    /// every scale/rotation/flip matrix, which is nearly every matrix a
    /// document sets. There the offset is the plain device perpendicular
    /// and the per-segment inverse mapping below is skipped.
    uniform_r: Option<f32>,
}

/// The half-width offset of the band around the device segment `p`→`q`:
/// the device image of the user-space pen radius perpendicular to the
/// segment *in user space* — generally not perpendicular to the device
/// segment; the parallelogram it spans is the transformed pen band. The
/// offset carries the sign [`Pen::winding`] names, so every quad built
/// from one shares an orientation and overlapping pieces union under the
/// nonzero rule. Returns `None` for zero-length segments.
fn offset(p: Point, q: Point, pen: Pen) -> Option<Point> {
    let dx = q.x - p.x;
    let dy = q.y - p.y;
    let len = dx.hypot(dy);
    if len <= 1e-6 || !len.is_finite() {
        return None;
    }
    let mut o = if let Some(r) = pen.uniform_r {
        Point::new(-dy / len * r * pen.winding, dx / len * r * pen.winding)
    } else {
        let u = linear(pen.to_user, Point::new(dx, dy));
        let ulen = u.x.hypot(u.y);
        if ulen > 0.0 && ulen.is_finite() {
            linear(
                pen.to_device,
                Point::new(-u.y / ulen * pen.r, u.x / ulen * pen.r),
            )
        } else {
            Point::new(0.0, 0.0)
        }
    };
    // The band's half-thickness across the device segment (the tangential
    // part of the offset skews the quad without thickening it). Clamping
    // it here rather than clamping the width keeps hairlines visible in
    // exactly the direction they are thin.
    let across = ((o.x * -dy + o.y * dx) / len).abs();
    if !across.is_finite() || across <= 0.0 {
        let h = MIN_WIDTH / 2.0 * pen.winding;
        o = Point::new(-dy / len * h, dx / len * h);
    } else if across < MIN_WIDTH / 2.0 {
        let k = MIN_WIDTH / 2.0 / across;
        o = Point::new(o.x * k, o.y * k);
    }
    Some(o)
}

/// The offset quad covering one stroked segment of device points, see
/// [`offset`]. Returns `None` for zero-length segments.
fn segment_quad(p: Point, q: Point, pen: Pen) -> Option<Subpath> {
    let o = offset(p, q, pen)?;
    Some(Subpath {
        points: vec![add(p, o), add(q, o), sub(q, o), sub(p, o)],
        closed: true,
    })
}

/// The device vector one user-space pen radius long in the direction of
/// the device vector `along`: how far a projecting square cap carries the
/// band past a run end, and half the side of the square a zero-length
/// dash paints. `None` when the pen or the direction has no length.
fn extension(along: Point, pen: Pen) -> Option<Point> {
    if let Some(r) = pen.uniform_r {
        return Some(scale(unit(along)?, r));
    }
    let u = unit(linear(pen.to_user, along))?;
    Some(linear(pen.to_device, scale(u, pen.r)))
}

/// The polygon a cap adds at the run end `e`, with `away` pointing out of
/// the run: nothing for a butt cap, the pen disc for a round cap, and for
/// a projecting square cap the band carried on by one pen radius.
///
/// Covers ISO 32000-1 §8.4.3.3.
fn cap(e: Point, away: Point, pen: Pen, cap: LineCap) -> Option<Subpath> {
    match cap {
        LineCap::Butt => None,
        LineCap::Round => Some(disc(e, pen)),
        LineCap::Square => segment_quad(e, add(e, extension(away, pen)?), pen),
    }
}

/// What a run collapsed to the single point `c` paints: a filled disc
/// under round caps, nothing under butt caps, and under projecting square
/// caps a square along the underlying path's direction `along` when there
/// is one (a zero-length dash) and nothing when there is not (a
/// degenerate subpath, whose caps have no orientation).
///
/// Covers ISO 32000-1 §8.5.3.2.
fn dot(c: Point, along: Option<Point>, pen: Pen, cap: LineCap) -> Option<Subpath> {
    match cap {
        LineCap::Butt => None,
        LineCap::Round => Some(disc(c, pen)),
        LineCap::Square => {
            let ext = extension(along?, pen)?;
            segment_quad(sub(c, ext), add(c, ext), pen)
        }
    }
}

/// The polygon that fills the corner at the vertex `v` between two
/// segments: the pen disc, for every join style until miter and bevel
/// joins are built.
fn join(v: Point, pen: Pen) -> Option<Subpath> {
    Some(disc(v, pen))
}

/// A small fan around `c` approximating the pen's own shape — the device
/// image of the user-space circle of radius `pen.r`, an ellipse — used for
/// round joins and caps. Wound to match [`segment_quad`]'s orientation.
/// A vertex that would land inside the minimum hairline disc is pushed out
/// to it radially, which keeps sub-hairline caps visible and preserves the
/// winding.
fn disc(c: Point, pen: Pen) -> Subpath {
    let mut points = Vec::with_capacity(FAN_SEGMENTS);
    for i in 0..FAN_SEGMENTS {
        let theta = -(i as f32) * std::f32::consts::TAU / FAN_SEGMENTS as f32;
        let (sin, cos) = theta.sin_cos();
        // Mapping through the matrix flips the fan's orientation exactly
        // when it flips the quads', so no correction is needed here; only
        // the unmapped fallback below must be reflected to match.
        let v = linear(pen.to_device, Point::new(pen.r * cos, pen.r * sin));
        let vlen = v.x.hypot(v.y);
        let v = if !vlen.is_finite() || vlen <= 0.0 {
            Point::new(MIN_WIDTH / 2.0 * cos, MIN_WIDTH / 2.0 * sin * pen.winding)
        } else if vlen < MIN_WIDTH / 2.0 {
            let k = MIN_WIDTH / 2.0 / vlen;
            Point::new(v.x * k, v.y * k)
        } else {
            v
        };
        points.push(Point::new(c.x + v.x, c.y + v.y));
    }
    Subpath {
        points,
        closed: true,
    }
}

/// Expands flattened device-space subpaths into closed polygons that,
/// filled with the nonzero rule, paint the stroke: one offset quad per
/// segment, a join at every interior vertex (and at the start of a closed
/// subpath), a cap at both ends of every open run, and for a degenerate
/// subpath the dot its cap style allows. `style`, `dash` and `phase` are
/// user-space quantities carried into device space through `ctm` (only
/// its linear part matters to a pen); a stroke thinner than [`MIN_WIDTH`]
/// device pixels is widened to a visible hairline. A matrix that cannot
/// be inverted cannot carry the pen either way and is treated as the
/// identity, which keeps the stroke visible.
///
/// Covers ISO 32000-1 §8.4.3.2, §8.4.3.3 and §8.5.3.2.
pub(crate) fn stroke_path(
    subpaths: &[Subpath],
    style: StrokeStyle,
    ctm: Matrix,
    dash: &[f32],
    phase: f32,
) -> Vec<Subpath> {
    let width = if style.width.is_finite() {
        style.width.max(0.0)
    } else {
        0.0
    };
    let (to_device, to_user) = match ctm.invert() {
        Some(inv) => (ctm, inv),
        None => (Matrix::identity(), Matrix::identity()),
    };
    // Circle-preserving means orthogonal columns of equal length; the
    // tolerance forgives the drift a chain of concatenations leaves.
    let c1 = to_device.a * to_device.a + to_device.b * to_device.b;
    let c2 = to_device.c * to_device.c + to_device.d * to_device.d;
    let skew = to_device.a * to_device.c + to_device.b * to_device.d;
    let largest = c1.max(c2);
    let uniform_r = ((c1 - c2).abs() <= largest * 1e-3 && skew.abs() <= largest * 1e-3)
        .then(|| width / 2.0 * c1.sqrt());
    let pen = Pen {
        to_device,
        to_user,
        r: width / 2.0,
        winding: if to_device.a * to_device.d - to_device.b * to_device.c < 0.0 {
            -1.0
        } else {
            1.0
        },
        uniform_r,
    };
    let mut out = Vec::new();
    for subpath in subpaths {
        let mut pts = distinct(&subpath.points);
        if subpath.closed && pts.len() >= 2 {
            let (first, last) = (pts[0], pts[pts.len() - 1]);
            if (first.x - last.x).hypot(first.y - last.y) <= MIN_SEGMENT {
                pts.pop();
            }
        }
        match pts.len() {
            0 => continue,
            1 => {
                out.extend(dot(pts[0], None, pen, style.cap));
                continue;
            }
            _ => {}
        }
        if subpath.closed {
            pts.push(pts[0]);
        }
        let runs = dash_split(&pts, dash, phase, to_user);
        // A closed subpath the dash pattern never cuts joins its two ends
        // instead of capping them.
        let closed = subpath.closed && runs.len() == 1 && runs[0].points.len() == pts.len();
        for run in runs {
            let rp = distinct(&run.points);
            let n = rp.len();
            if n == 0 {
                continue;
            }
            if n == 1 {
                out.extend(dot(rp[0], Some(run.along), pen, style.cap));
                continue;
            }
            for seg in rp.windows(2) {
                out.extend(segment_quad(seg[0], seg[1], pen));
            }
            for w in rp.windows(3) {
                out.extend(join(w[1], pen));
            }
            if closed {
                out.extend(join(rp[0], pen));
            } else {
                out.extend(cap(rp[0], sub(rp[0], rp[1]), pen, style.cap));
                out.extend(cap(rp[n - 1], sub(rp[n - 1], rp[n - 2]), pen, style.cap));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::{fill_path, BlendMode, FillRule, RasterScratch};
    use crate::Pixmap;

    fn line(points: &[(f32, f32)]) -> Subpath {
        Subpath {
            points: points.iter().map(|&(x, y)| Point::new(x, y)).collect(),
            closed: false,
        }
    }

    fn alpha_at(pix: &Pixmap, x: u32, y: u32) -> u8 {
        pix.data[((y * pix.width + x) * 4 + 3) as usize]
    }

    /// The initial graphics state's stroke parameters at `width`: butt
    /// caps, miter joins, miter limit 10.
    fn solid(width: f32) -> StrokeStyle {
        StrokeStyle {
            width,
            cap: LineCap::Butt,
            join: LineJoin::Miter,
            miter_limit: 10.0,
        }
    }

    const BLACK: [u8; 4] = [0, 0, 0, 255];

    fn paint(pix: &mut Pixmap, polys: &[Subpath]) {
        fill_path(
            pix,
            &mut RasterScratch::default(),
            polys,
            FillRule::NonZero,
            BLACK,
            1.0,
            None,
            BlendMode::Normal,
        );
    }

    // Covers ISO 32000-1 §8.5.3.2.
    #[test]
    fn horizontal_line_paints_band_of_expected_thickness() {
        let mut pix = Pixmap::new(20, 10);
        let polys = stroke_path(
            &[line(&[(2.0, 5.0), (18.0, 5.0)])],
            solid(4.0),
            Matrix::identity(),
            &[],
            0.0,
        );
        paint(&mut pix, &polys);
        let thick = (0..10).filter(|&y| alpha_at(&pix, 10, y) > 127).count();
        assert!((3..=5).contains(&thick), "band thickness {thick}");
        assert_eq!(alpha_at(&pix, 10, 5), 255, "band core solid");
        assert_eq!(alpha_at(&pix, 10, 0), 0, "above band clear");
        assert_eq!(alpha_at(&pix, 10, 9), 0, "below band clear");
    }

    // Covers ISO 32000-1 §8.4.3.3.
    #[test]
    fn round_caps_extend_past_endpoints() {
        let mut pix = Pixmap::new(20, 10);
        let polys = stroke_path(
            &[line(&[(4.0, 5.0), (16.0, 5.0)])],
            StrokeStyle {
                cap: LineCap::Round,
                ..solid(4.0)
            },
            Matrix::identity(),
            &[],
            0.0,
        );
        paint(&mut pix, &polys);
        // The cap fan reaches ~2px left of x=4.
        assert!(alpha_at(&pix, 2, 5) > 127, "left cap");
        assert!(alpha_at(&pix, 17, 5) > 127, "right cap");
        assert_eq!(alpha_at(&pix, 0, 5), 0);
        // The corner a square cap would fill lies outside the half disc.
        assert!(alpha_at(&pix, 2, 3) < 200, "round cap corner");
    }

    // Covers ISO 32000-1 §8.4.3.3.
    #[test]
    fn butt_caps_stop_at_the_endpoints() {
        let mut pix = Pixmap::new(20, 10);
        let polys = stroke_path(
            &[line(&[(4.0, 5.0), (16.0, 5.0)])],
            solid(4.0),
            Matrix::identity(),
            &[],
            0.0,
        );
        paint(&mut pix, &polys);
        assert_eq!(alpha_at(&pix, 2, 5), 0, "past the left end");
        assert_eq!(alpha_at(&pix, 17, 5), 0, "past the right end");
        assert_eq!(alpha_at(&pix, 5, 5), 255, "band");
    }

    // Covers ISO 32000-1 §8.4.3.3.
    #[test]
    fn square_caps_extend_the_band_by_half_the_width() {
        let mut pix = Pixmap::new(20, 10);
        let polys = stroke_path(
            &[line(&[(4.0, 5.0), (16.0, 5.0)])],
            StrokeStyle {
                cap: LineCap::Square,
                ..solid(4.0)
            },
            Matrix::identity(),
            &[],
            0.0,
        );
        paint(&mut pix, &polys);
        // The band runs from x=2 to x=18 and keeps its full height there.
        assert_eq!(alpha_at(&pix, 2, 5), 255, "left extension");
        assert_eq!(alpha_at(&pix, 2, 3), 255, "left corner");
        assert_eq!(alpha_at(&pix, 17, 6), 255, "right corner");
        assert_eq!(alpha_at(&pix, 1, 5), 0, "beyond the extension");
    }

    // Covers ISO 32000-1 §8.5.3.2.
    #[test]
    fn degenerate_subpaths_paint_a_dot_only_under_round_caps() {
        let closed_point = Subpath {
            points: vec![Point::new(5.0, 5.0)],
            closed: true,
        };
        let coincident = Subpath {
            points: vec![Point::new(14.0, 5.0), Point::new(14.0, 5.0)],
            closed: false,
        };
        for cap in [LineCap::Butt, LineCap::Round, LineCap::Square] {
            let mut pix = Pixmap::new(20, 10);
            let polys = stroke_path(
                &[closed_point.clone(), coincident.clone()],
                StrokeStyle { cap, ..solid(4.0) },
                Matrix::identity(),
                &[],
                0.0,
            );
            paint(&mut pix, &polys);
            let want = if cap == LineCap::Round { 255 } else { 0 };
            assert_eq!(alpha_at(&pix, 5, 5), want, "closed point under {cap:?}");
            assert_eq!(
                alpha_at(&pix, 14, 5),
                want,
                "coincident points under {cap:?}"
            );
        }
    }

    // Covers ISO 32000-1 §8.4.3.6 and §8.5.3.2.
    #[test]
    fn zero_length_dashes_take_the_cap_style() {
        // [0 8] puts a zero-length dash at x = 1, 9 and 17.
        let stroke = |cap| {
            let mut pix = Pixmap::new(20, 10);
            let polys = stroke_path(
                &[line(&[(1.0, 5.0), (19.0, 5.0)])],
                StrokeStyle { cap, ..solid(4.0) },
                Matrix::identity(),
                &[0.0, 8.0],
                0.0,
            );
            paint(&mut pix, &polys);
            pix
        };
        let pix = stroke(LineCap::Round);
        assert_eq!(alpha_at(&pix, 9, 5), 255, "round dot");
        assert_eq!(alpha_at(&pix, 5, 5), 0, "gap");
        assert!(alpha_at(&pix, 7, 3) < 200, "round dot corner");
        let pix = stroke(LineCap::Square);
        assert_eq!(alpha_at(&pix, 9, 5), 255, "square dot");
        assert_eq!(alpha_at(&pix, 7, 3), 255, "square dot corner");
        assert_eq!(alpha_at(&pix, 5, 5), 0, "gap");
        let pix = stroke(LineCap::Butt);
        assert_eq!(alpha_at(&pix, 9, 5), 0, "butt caps paint nothing");
    }

    // Covers ISO 32000-1 §8.4.3.2.
    #[test]
    fn minimum_device_width_keeps_hairlines_visible() {
        let mut pix = Pixmap::new(20, 10);
        let polys = stroke_path(
            &[line(&[(2.0, 5.5), (18.0, 5.5)])],
            solid(0.05),
            Matrix::identity(),
            &[],
            0.0,
        );
        paint(&mut pix, &polys);
        let total: u32 = (0..10).map(|y| alpha_at(&pix, 10, y) as u32).sum();
        // Coverage ~0.75px of ink; an unclamped 0.05px pen would leave ~13.
        assert!(total >= 150, "hairline too faint: {total}");
    }

    #[test]
    fn dash_pattern_splits_into_runs() {
        let mut pix = Pixmap::new(21, 10);
        let polys = stroke_path(
            &[line(&[(1.0, 5.0), (19.0, 5.0)])],
            solid(2.0),
            Matrix::identity(),
            &[4.0, 4.0],
            0.0,
        );
        paint(&mut pix, &polys);
        let mut runs = 0;
        let mut prev_on = false;
        for x in 0..21 {
            let on = alpha_at(&pix, x, 4) > 127;
            if on && !prev_on {
                runs += 1;
            }
            prev_on = on;
        }
        assert_eq!(runs, 3, "expected 3 painted runs");
    }

    // Covers ISO 32000-1 §8.4.3.6.
    #[test]
    fn dash_split_counts_and_phase() {
        let pts = [Point::new(0.0, 0.0), Point::new(20.0, 0.0)];
        assert_eq!(
            dash_split(&pts, &[2.0, 2.0], 0.0, Matrix::identity()).len(),
            5
        );
        assert_eq!(
            dash_split(&pts, &[2.0, 2.0], 2.0, Matrix::identity()).len(),
            5
        );
        assert_eq!(
            dash_split(&pts, &[2.0, 2.0], 1.0, Matrix::identity()).len(),
            6
        );
        // Empty or degenerate patterns are solid.
        assert_eq!(dash_split(&pts, &[], 0.0, Matrix::identity()).len(), 1);
        assert_eq!(
            dash_split(&pts, &[0.0, 0.0], 0.0, Matrix::identity()).len(),
            1
        );
        assert_eq!(
            dash_split(&pts, &[-1.0, 2.0], 0.0, Matrix::identity()).len(),
            1
        );
    }

    // Covers ISO 32000-1 §8.5.3.2.
    #[test]
    fn closed_subpath_strokes_closing_segment() {
        let mut pix = Pixmap::new(12, 12);
        let square = Subpath {
            points: vec![
                Point::new(2.0, 2.0),
                Point::new(10.0, 2.0),
                Point::new(10.0, 10.0),
                Point::new(2.0, 10.0),
            ],
            closed: true,
        };
        let polys = stroke_path(&[square], solid(2.0), Matrix::identity(), &[], 0.0);
        paint(&mut pix, &polys);
        // The closing (left) edge is painted, the interior is not.
        assert_eq!(alpha_at(&pix, 2, 6), 255, "left edge");
        assert_eq!(alpha_at(&pix, 6, 6), 0, "interior clear");
    }

    #[test]
    fn zero_length_segments_are_skipped() {
        let pen = Pen {
            to_device: Matrix::identity(),
            to_user: Matrix::identity(),
            r: 2.0,
            winding: 1.0,
            uniform_r: Some(2.0),
        };
        assert!(segment_quad(Point::new(1.0, 1.0), Point::new(1.0, 1.0), pen).is_none());
    }

    /// The hand-made gradient in the corpus's 049466.pdf: vertical user-space
    /// lines under `0 2.0629 0.4848 0 0 0 cm`, stroked `4.26 w`. The pen is a
    /// user-space circle, so under this matrix the device band across these
    /// (device-horizontal) lines is 4.26 * 2.0629 ~ 8.8 pixels — not the
    /// 4.26 * sqrt(|det|) ~ 4.26 a scalar width yields, which leaves gaps
    /// between strokes spaced 8.1 apart and stripes every such gradient.
    // Covers ISO 32000-1 §8.4.3.2.
    #[test]
    fn anisotropic_ctm_widens_the_pen_across_the_stroke() {
        let ctm = Matrix {
            a: 0.0,
            b: 2.0629,
            c: 0.4848,
            d: 0.0,
            e: 0.0,
            f: 0.0,
        };
        let mut pix = Pixmap::new(40, 20);
        let polys = stroke_path(
            &[line(&[(2.0, 10.0), (38.0, 10.0)])],
            solid(4.26),
            ctm,
            &[],
            0.0,
        );
        paint(&mut pix, &polys);
        let thick = (0..20).filter(|&y| alpha_at(&pix, 20, y) > 127).count();
        assert!(
            (8..=10).contains(&thick),
            "band thickness {thick}, want ~8.8"
        );
    }

    /// Dash lengths are user-space quantities (ISO 32000-1 §8.4.3.6). Under
    /// the same anisotropic matrix, a device-horizontal line maps back to a
    /// user-space length 1/0.4848 times its device length, so a [4 4] pattern
    /// cuts runs every 8 * 0.4848 ~ 3.9 device pixels — not every 8.
    #[test]
    fn dash_pattern_is_measured_in_user_space() {
        let ctm = Matrix {
            a: 0.0,
            b: 2.0629,
            c: 0.4848,
            d: 0.0,
            e: 0.0,
            f: 0.0,
        };
        let mut pix = Pixmap::new(40, 20);
        let polys = stroke_path(
            &[line(&[(1.0, 10.0), (39.0, 10.0)])],
            solid(1.5),
            ctm,
            &[4.0, 4.0],
            0.0,
        );
        paint(&mut pix, &polys);
        let mut runs = 0;
        let mut prev_on = false;
        for x in 0..40 {
            let on = alpha_at(&pix, x, 10) > 127;
            if on && !prev_on {
                runs += 1;
            }
            prev_on = on;
        }
        // 38 device px = ~78 user units = ~9.8 pattern periods, so 10 painted
        // runs (9 whole plus the partial each end); a device-measured pattern
        // would paint only 5.
        assert!((9..=11).contains(&runs), "painted runs {runs}, want ~10");
    }
}
