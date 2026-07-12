//! Stroking: flattened segments expanded to offset quads with approximated
//! round joins/caps, and dash patterns applied at the flatten level.

use pdfboss_core::geom::Point;

use crate::path::Subpath;

/// Minimum stroke width in device pixels; thinner pens still leave a
/// visible hairline.
const MIN_WIDTH: f32 = 0.75;
/// Vertex count of the small fan approximating round joins and caps.
const FAN_SEGMENTS: usize = 12;
/// Upper bound on dash pieces produced per path, guarding pathological
/// patterns (e.g. many near-zero entries).
const MAX_DASH_PIECES: usize = 65_536;

fn dist(a: Point, b: Point) -> f32 {
    (b.x - a.x).hypot(b.y - a.y)
}

fn lerp(a: Point, b: Point, t: f32) -> Point {
    Point::new(a.x + (b.x - a.x) * t, a.y + (b.y - a.y) * t)
}

/// Splits a polyline into its painted ("on") runs according to a dash
/// pattern. An empty or degenerate pattern yields the whole polyline.
fn dash_split(points: &[Point], dash: &[f32], phase: f32) -> Vec<Vec<Point>> {
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
        let seglen = dist(a, b);
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

/// The offset quad covering one stroked segment. All quads share one
/// intrinsic orientation, so overlapping pieces union under the nonzero
/// rule. Returns `None` for zero-length segments.
fn segment_quad(p: Point, q: Point, r: f32) -> Option<Subpath> {
    let dx = q.x - p.x;
    let dy = q.y - p.y;
    let len = dx.hypot(dy);
    if len <= 1e-6 || !len.is_finite() {
        return None;
    }
    let nx = -dy / len * r;
    let ny = dx / len * r;
    Some(Subpath {
        points: vec![
            Point::new(p.x + nx, p.y + ny),
            Point::new(q.x + nx, q.y + ny),
            Point::new(q.x - nx, q.y - ny),
            Point::new(p.x - nx, p.y - ny),
        ],
        closed: true,
    })
}

/// A small fan (regular polygon) of radius `r` around `c`, wound to match
/// [`segment_quad`]'s orientation; approximates round joins and caps.
fn disc(c: Point, r: f32) -> Subpath {
    let mut points = Vec::with_capacity(FAN_SEGMENTS);
    for i in 0..FAN_SEGMENTS {
        let theta = -(i as f32) * std::f32::consts::TAU / FAN_SEGMENTS as f32;
        points.push(Point::new(c.x + r * theta.cos(), c.y + r * theta.sin()));
    }
    Subpath {
        points,
        closed: true,
    }
}

/// Expands flattened device-space subpaths into closed polygons that,
/// filled with the nonzero rule, paint the stroke: one offset quad per
/// segment plus a fan at every vertex (round joins at interior vertices,
/// round caps at run ends). `width` is the device-space pen width (clamped
/// up to [`MIN_WIDTH`]); `dash`/`phase` split each subpath into painted
/// runs first (empty `dash` = solid).
pub(crate) fn stroke_path(
    subpaths: &[Subpath],
    width: f32,
    dash: &[f32],
    phase: f32,
) -> Vec<Subpath> {
    let width = if width.is_finite() { width } else { MIN_WIDTH };
    let r = width.max(MIN_WIDTH) / 2.0;
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
        for run in dash_split(&pts, dash, phase) {
            for seg in run.windows(2) {
                if let Some(quad) = segment_quad(seg[0], seg[1], r) {
                    out.push(quad);
                }
            }
            for &v in &run {
                out.push(disc(v, r));
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::raster::{fill_path, FillRule};
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
        fill_path(pix, polys, FillRule::NonZero, BLACK, 1.0, None);
    }

    #[test]
    fn horizontal_line_paints_band_of_expected_thickness() {
        let mut pix = Pixmap::new(20, 10);
        let polys = stroke_path(&[line(&[(2.0, 5.0), (18.0, 5.0)])], 4.0, &[], 0.0);
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
        let polys = stroke_path(&[line(&[(4.0, 5.0), (16.0, 5.0)])], 4.0, &[], 0.0);
        paint(&mut pix, &polys);
        // The cap fan reaches ~2px left of x=4.
        assert!(alpha_at(&pix, 2, 5) > 127, "left cap");
        assert!(alpha_at(&pix, 17, 5) > 127, "right cap");
        assert_eq!(alpha_at(&pix, 0, 5), 0);
    }

    #[test]
    fn minimum_device_width_keeps_hairlines_visible() {
        let mut pix = Pixmap::new(20, 10);
        let polys = stroke_path(&[line(&[(2.0, 5.5), (18.0, 5.5)])], 0.05, &[], 0.0);
        paint(&mut pix, &polys);
        let total: u32 = (0..10).map(|y| alpha_at(&pix, 10, y) as u32).sum();
        // Coverage ~0.75px of ink; an unclamped 0.05px pen would leave ~13.
        assert!(total >= 150, "hairline too faint: {total}");
    }

    #[test]
    fn dash_pattern_splits_into_runs() {
        let mut pix = Pixmap::new(21, 10);
        let polys = stroke_path(&[line(&[(1.0, 5.0), (19.0, 5.0)])], 2.0, &[4.0, 4.0], 0.0);
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
        assert_eq!(dash_split(&pts, &[2.0, 2.0], 0.0).len(), 5);
        assert_eq!(dash_split(&pts, &[2.0, 2.0], 2.0).len(), 5);
        assert_eq!(dash_split(&pts, &[2.0, 2.0], 1.0).len(), 6);
        // Empty or degenerate patterns are solid.
        assert_eq!(dash_split(&pts, &[], 0.0).len(), 1);
        assert_eq!(dash_split(&pts, &[0.0, 0.0], 0.0).len(), 1);
        assert_eq!(dash_split(&pts, &[-1.0, 2.0], 0.0).len(), 1);
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
        let polys = stroke_path(&[square], 2.0, &[], 0.0);
        paint(&mut pix, &polys);
        // The closing (left) edge is painted, the interior is not.
        assert_eq!(alpha_at(&pix, 2, 6), 255, "left edge");
        assert_eq!(alpha_at(&pix, 6, 6), 0, "interior clear");
    }

    #[test]
    fn zero_length_segments_are_skipped() {
        assert!(segment_quad(Point::new(1.0, 1.0), Point::new(1.0, 1.0), 2.0).is_none());
    }
}
