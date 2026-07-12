//! Path building and flattening: move/line/cubic/close/rect collected and
//! flattened to polygons via adaptive cubic subdivision.

use pdfboss_core::geom::{Matrix, Point};

/// Maximum deviation, in device pixels, of a flattened cubic from the true
/// curve.
const TOLERANCE: f32 = 0.1;
/// Maximum subdivision depth for one cubic; caps the segment count per
/// curve at `2^10 = 1024`.
const MAX_DEPTH: u32 = 10;

/// A flattened subpath (polyline) in device space.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Subpath {
    /// The polyline vertices.
    pub points: Vec<Point>,
    /// Whether the subpath was explicitly closed; affects stroking (a
    /// closing segment joins the last point back to the first).
    pub closed: bool,
}

/// Collects path construction operators, applies a transform into device
/// space, and flattens Bezier curves to polylines.
#[derive(Debug, Clone)]
pub(crate) struct PathBuilder {
    ctm: Matrix,
    done: Vec<Subpath>,
    current: Vec<Point>,
    /// Start of the current subpath in user space (target of `close`).
    start_user: Point,
    /// Current point in user space (source for `v`/`y` curve forms).
    last_user: Point,
}

impl PathBuilder {
    /// Creates a builder that maps user-space input through `ctm`.
    pub(crate) fn new(ctm: Matrix) -> PathBuilder {
        PathBuilder {
            ctm,
            done: Vec::new(),
            current: Vec::new(),
            start_user: Point::new(0.0, 0.0),
            last_user: Point::new(0.0, 0.0),
        }
    }

    /// Current point in user space (origin before any operator).
    pub(crate) fn current_point(&self) -> Point {
        self.last_user
    }

    fn push_device(&mut self, p: Point) {
        if self.current.last() != Some(&p) {
            self.current.push(p);
        }
    }

    fn flush(&mut self, closed: bool) {
        if self.current.len() >= 2 {
            let points = std::mem::take(&mut self.current);
            self.done.push(Subpath { points, closed });
        } else {
            self.current.clear();
        }
    }

    /// Starts an open subpath if none is in progress, anchored at the
    /// current point (lenient handling of drawing before `moveto`).
    fn ensure_started(&mut self) {
        if self.current.is_empty() {
            self.start_user = self.last_user;
            let p = self.ctm.apply(self.last_user);
            self.current.push(p);
        }
    }

    /// Begins a new subpath at `(x, y)`.
    pub(crate) fn move_to(&mut self, x: f32, y: f32) {
        self.flush(false);
        self.start_user = Point::new(x, y);
        self.last_user = self.start_user;
        let p = self.ctm.apply(self.start_user);
        self.current.push(p);
    }

    /// Appends a straight segment to `(x, y)`.
    pub(crate) fn line_to(&mut self, x: f32, y: f32) {
        self.ensure_started();
        self.last_user = Point::new(x, y);
        let p = self.ctm.apply(self.last_user);
        self.push_device(p);
    }

    /// Appends a cubic Bezier segment with control points `(x1, y1)` and
    /// `(x2, y2)` ending at `(x3, y3)` (operator `c`).
    pub(crate) fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32) {
        self.ensure_started();
        let p0 = self.ctm.apply(self.last_user);
        let p1 = self.ctm.apply(Point::new(x1, y1));
        let p2 = self.ctm.apply(Point::new(x2, y2));
        let p3 = self.ctm.apply(Point::new(x3, y3));
        let mut pts = Vec::new();
        flatten_cubic(p0, p1, p2, p3, 0, &mut pts);
        for p in pts {
            self.push_device(p);
        }
        self.last_user = Point::new(x3, y3);
    }

    /// Cubic segment using the current point as the first control point
    /// (operator `v`).
    pub(crate) fn curve_to_v(&mut self, x2: f32, y2: f32, x3: f32, y3: f32) {
        let c = self.last_user;
        self.curve_to(c.x, c.y, x2, y2, x3, y3);
    }

    /// Cubic segment using the end point as the second control point
    /// (operator `y`).
    pub(crate) fn curve_to_y(&mut self, x1: f32, y1: f32, x3: f32, y3: f32) {
        self.curve_to(x1, y1, x3, y3, x3, y3);
    }

    /// Closes the current subpath; subsequent segments start from its
    /// starting point.
    pub(crate) fn close(&mut self) {
        self.flush(true);
        self.last_user = self.start_user;
    }

    /// Appends the axis-aligned rectangle `(x, y, w, h)` as a closed
    /// subpath.
    pub(crate) fn rect(&mut self, x: f32, y: f32, w: f32, h: f32) {
        self.move_to(x, y);
        self.line_to(x + w, y);
        self.line_to(x + w, y + h);
        self.line_to(x, y + h);
        self.close();
    }

    /// Returns the flattened subpaths accumulated so far, including any
    /// unfinished (open) subpath.
    pub(crate) fn finish(&self) -> Vec<Subpath> {
        let mut out = self.done.clone();
        if self.current.len() >= 2 {
            out.push(Subpath {
                points: self.current.clone(),
                closed: false,
            });
        }
        out
    }
}

/// Whether the control points of a cubic lie within [`TOLERANCE`] of the
/// chord `p0..p3`, meaning a single line segment approximates the curve.
fn cubic_is_flat(p0: Point, p1: Point, p2: Point, p3: Point) -> bool {
    let dx = p3.x - p0.x;
    let dy = p3.y - p0.y;
    let len2 = dx * dx + dy * dy;
    if len2 <= 1e-12 {
        // Degenerate chord: measure control-point distance from p0.
        let d1 = (p1.x - p0.x).hypot(p1.y - p0.y);
        let d2 = (p2.x - p0.x).hypot(p2.y - p0.y);
        return d1 <= TOLERANCE && d2 <= TOLERANCE;
    }
    let c1 = (p1.x - p0.x) * dy - (p1.y - p0.y) * dx;
    let c2 = (p2.x - p0.x) * dy - (p2.y - p0.y) * dx;
    let limit = TOLERANCE * TOLERANCE * len2;
    c1 * c1 <= limit && c2 * c2 <= limit
}

fn midpoint(a: Point, b: Point) -> Point {
    Point::new((a.x + b.x) * 0.5, (a.y + b.y) * 0.5)
}

/// Adaptively subdivides a cubic in device space, appending the vertices
/// after `p0` (exclusive) up to `p3` (inclusive) to `out`.
fn flatten_cubic(p0: Point, p1: Point, p2: Point, p3: Point, depth: u32, out: &mut Vec<Point>) {
    if depth >= MAX_DEPTH || cubic_is_flat(p0, p1, p2, p3) {
        out.push(p3);
        return;
    }
    // de Casteljau split at t = 0.5.
    let ab = midpoint(p0, p1);
    let bc = midpoint(p1, p2);
    let cd = midpoint(p2, p3);
    let abc = midpoint(ab, bc);
    let bcd = midpoint(bc, cd);
    let mid = midpoint(abc, bcd);
    flatten_cubic(p0, ab, abc, mid, depth + 1, out);
    flatten_cubic(mid, bcd, cd, p3, depth + 1, out);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rect_produces_closed_quad() {
        let mut b = PathBuilder::new(Matrix::identity());
        b.rect(1.0, 2.0, 10.0, 5.0);
        let subs = b.finish();
        assert_eq!(subs.len(), 1);
        assert!(subs[0].closed);
        assert_eq!(
            subs[0].points,
            vec![
                Point::new(1.0, 2.0),
                Point::new(11.0, 2.0),
                Point::new(11.0, 7.0),
                Point::new(1.0, 7.0),
            ]
        );
    }

    #[test]
    fn matrix_transformed_rect() {
        let m = Matrix::scale(2.0, 3.0).concat(Matrix::translate(10.0, 20.0));
        let mut b = PathBuilder::new(m);
        b.rect(0.0, 0.0, 4.0, 4.0);
        let subs = b.finish();
        assert_eq!(subs[0].points[0], Point::new(10.0, 20.0));
        assert_eq!(subs[0].points[2], Point::new(18.0, 32.0));
    }

    #[test]
    fn straight_cubic_flattens_to_endpoint() {
        let mut b = PathBuilder::new(Matrix::identity());
        b.move_to(0.0, 0.0);
        b.curve_to(1.0, 0.0, 2.0, 0.0, 3.0, 0.0);
        let subs = b.finish();
        assert_eq!(subs[0].points.len(), 2);
        assert_eq!(*subs[0].points.last().unwrap(), Point::new(3.0, 0.0));
    }

    #[test]
    fn curved_cubic_stays_within_tolerance() {
        let mut b = PathBuilder::new(Matrix::identity());
        b.move_to(0.0, 0.0);
        b.curve_to(0.0, 100.0, 100.0, 100.0, 100.0, 0.0);
        let subs = b.finish();
        let pts = &subs[0].points;
        assert!(pts.len() > 4, "curve should subdivide, got {}", pts.len());
        // Compare each vertex against the analytic curve at the parameter
        // recovered from a dense sampling: every vertex must lie on the
        // curve to within a small epsilon of the tolerance guarantee.
        for p in pts {
            let mut best = f32::MAX;
            for i in 0..=1000 {
                let t = i as f32 / 1000.0;
                let mt = 1.0 - t;
                let x = 3.0 * mt * mt * t * 0.0 + 3.0 * mt * t * t * 100.0 + t * t * t * 100.0;
                let y = 3.0 * mt * mt * t * 100.0 + 3.0 * mt * t * t * 100.0;
                let d = (p.x - x).hypot(p.y - y);
                best = best.min(d);
            }
            assert!(best < 0.2, "vertex {p:?} off-curve by {best}");
        }
    }

    #[test]
    fn cubic_segment_cap_holds() {
        // A wild curve spanning a huge range must not exceed 2^10 segments.
        let mut b = PathBuilder::new(Matrix::identity());
        b.move_to(0.0, 0.0);
        b.curve_to(1e6, 1e6, -1e6, 1e6, 0.0, 0.0);
        let subs = b.finish();
        assert!(subs[0].points.len() <= 1025);
    }

    #[test]
    fn close_then_line_starts_at_subpath_start() {
        let mut b = PathBuilder::new(Matrix::identity());
        b.move_to(5.0, 5.0);
        b.line_to(10.0, 5.0);
        b.close();
        b.line_to(5.0, 9.0);
        let subs = b.finish();
        assert_eq!(subs.len(), 2);
        assert!(subs[0].closed);
        assert_eq!(subs[1].points[0], Point::new(5.0, 5.0));
        assert!(!subs[1].closed);
    }

    #[test]
    fn v_and_y_curve_forms() {
        let mut b = PathBuilder::new(Matrix::identity());
        b.move_to(0.0, 0.0);
        b.curve_to_v(10.0, 10.0, 20.0, 0.0);
        assert_eq!(b.current_point(), Point::new(20.0, 0.0));
        b.curve_to_y(30.0, 10.0, 40.0, 0.0);
        assert_eq!(b.current_point(), Point::new(40.0, 0.0));
    }

    #[test]
    fn line_before_move_starts_at_origin() {
        let mut b = PathBuilder::new(Matrix::identity());
        b.line_to(3.0, 4.0);
        let subs = b.finish();
        assert_eq!(subs[0].points[0], Point::new(0.0, 0.0));
        assert_eq!(subs[0].points[1], Point::new(3.0, 4.0));
    }
}
