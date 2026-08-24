//! Stroking: flattened segments expanded to offset quads with approximated
//! round joins/caps, and dash patterns applied at the flatten level.
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

fn lerp(a: Point, b: Point, t: f32) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// The linear part of `m` applied to a vector — a direction or offset,
/// which a translation never touches.
fn linear(m: Matrix, v: Point) -> Point {
    Point::new(m.a * v.x + m.c * v.y, m.b * v.x + m.d * v.y)
}

/// Splits a polyline of device-space points into its painted ("on") runs
/// according to a dash pattern. The pattern and phase are user-space
/// quantities (ISO 32000-1 §8.4.3.6), so each segment is measured through
/// `inv`, the device-to-user matrix; the cut positions themselves are
/// fractions along the segment, which a linear map preserves. An empty or
/// degenerate pattern yields the whole polyline.
fn dash_split(points: &[Point], dash: &[f32], phase: f32, inv: Matrix) -> Vec<Vec<Point>> {
    let pattern: Vec<f32> = dash
        .iter()
        .copied()
        .filter(|d| d.is_finite() && *d >= 0.0)
        .collect();
    let total: f32 = pattern.iter().sum();
    if pattern.len() != dash.len() || pattern.is_empty() || total <= 0.0 {
        return vec![points.to_vec()];
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
    let mut runs: Vec<Vec<Point>> = Vec::new();
    let mut cur: Vec<Point> = if on { vec![points[0]] } else { Vec::new() };
    let mut pieces = 0usize;
    for seg in points.windows(2) {
        let (a, b) = (seg[0], seg[1]);
        let seglen = {
            let u = linear(inv, Point::new(b.x - a.x, b.y - a.y));
            u.x.hypot(u.y)
        };
        let mut done = 0.0f32;
        while seglen - done > rem && pieces < MAX_DASH_PIECES {
            done += rem;
            let p = lerp(a, b, done / seglen);
            if on {
                cur.push(p);
                if cur.len() >= 2 {
                    runs.push(std::mem::take(&mut cur));
                } else {
                    cur.clear();
                }
            } else {
                cur = vec![p];
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
        runs.push(cur);
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

/// The offset quad covering one stroked segment of device points. The
/// offset is the device image of the user-space pen radius perpendicular
/// to the segment *in user space* — generally not perpendicular to the
/// device segment; the parallelogram it spans is the transformed pen band.
/// All quads share the orientation [`Pen::winding`] names, so overlapping
/// pieces union under the nonzero rule. Returns `None` for zero-length
/// segments.
fn segment_quad(p: Point, q: Point, pen: Pen) -> Option<Subpath> {
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
    Some(Subpath {
        points: vec![
            Point::new(p.x + o.x, p.y + o.y),
            Point::new(q.x + o.x, q.y + o.y),
            Point::new(q.x - o.x, q.y - o.y),
            Point::new(p.x - o.x, p.y - o.y),
        ],
        closed: true,
    })
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
/// segment plus a fan at every vertex (round joins at interior vertices,
/// round caps at run ends). `width`, `dash` and `phase` are user-space
/// quantities carried into device space through `ctm` (only its linear
/// part matters to a pen); a stroke thinner than [`MIN_WIDTH`] device
/// pixels is widened to a visible hairline. A matrix that cannot be
/// inverted cannot carry the pen either way and is treated as the
/// identity, which keeps the stroke visible.
pub(crate) fn stroke_path(
    subpaths: &[Subpath],
    width: f32,
    ctm: Matrix,
    dash: &[f32],
    phase: f32,
) -> Vec<Subpath> {
    let width = if width.is_finite() {
        width.max(0.0)
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
    let dot = to_device.a * to_device.c + to_device.b * to_device.d;
    let scale = c1.max(c2);
    let uniform_r = ((c1 - c2).abs() <= scale * 1e-3 && dot.abs() <= scale * 1e-3)
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
    for sub in subpaths {
        if sub.points.is_empty() {
            continue;
        }
        let mut pts = sub.points.clone();
        if sub.closed && pts.last() != pts.first() {
            pts.push(pts[0]);
        }
        if pts.len() < 2 {
            continue;
        }
        for run in dash_split(&pts, dash, phase, to_user) {
            for seg in run.windows(2) {
                if let Some(quad) = segment_quad(seg[0], seg[1], pen) {
                    out.push(quad);
                }
            }
            for &v in &run {
                out.push(disc(v, pen));
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

    #[test]
    fn horizontal_line_paints_band_of_expected_thickness() {
        let mut pix = Pixmap::new(20, 10);
        let polys = stroke_path(
            &[line(&[(2.0, 5.0), (18.0, 5.0)])],
            4.0,
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

    #[test]
    fn round_caps_extend_past_endpoints() {
        let mut pix = Pixmap::new(20, 10);
        let polys = stroke_path(
            &[line(&[(4.0, 5.0), (16.0, 5.0)])],
            4.0,
            Matrix::identity(),
            &[],
            0.0,
        );
        paint(&mut pix, &polys);
        // The cap fan reaches ~2px left of x=4.
        assert!(alpha_at(&pix, 2, 5) > 127, "left cap");
        assert!(alpha_at(&pix, 17, 5) > 127, "right cap");
        assert_eq!(alpha_at(&pix, 0, 5), 0);
    }

    #[test]
    fn minimum_device_width_keeps_hairlines_visible() {
        let mut pix = Pixmap::new(20, 10);
        let polys = stroke_path(
            &[line(&[(2.0, 5.5), (18.0, 5.5)])],
            0.05,
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
            2.0,
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
        let polys = stroke_path(&[square], 2.0, Matrix::identity(), &[], 0.0);
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
        let polys = stroke_path(&[line(&[(2.0, 10.0), (38.0, 10.0)])], 4.26, ctm, &[], 0.0);
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
            1.5,
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
