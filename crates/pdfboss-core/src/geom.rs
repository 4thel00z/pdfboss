//! Planar geometry shared by text extraction and rendering: points,
//! axis-aligned rectangles, and 2-D affine transformation matrices using the
//! PDF row-vector convention `[x y 1] · M`.

/// A point in user or device space.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    /// Creates a point from its coordinates.
    pub const fn new(x: f32, y: f32) -> Point {
        Point { x, y }
    }
}

/// An axis-aligned rectangle described by two opposite corners
/// `(x0, y0)` and `(x1, y1)`.
///
/// A rectangle is *normalized* when `x0 <= x1` and `y0 <= y1`. Operations
/// that depend on orientation ([`Rect::union`], [`Rect::intersect`],
/// [`Rect::contains`]) treat their inputs as if normalized.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Rect {
    pub x0: f32,
    pub y0: f32,
    pub x1: f32,
    pub y1: f32,
}

impl Rect {
    /// Creates a rectangle from two opposite corners.
    pub const fn new(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
        Rect { x0, y0, x1, y1 }
    }

    /// Signed width, `x1 - x0` (negative if not normalized).
    pub fn width(self) -> f32 {
        self.x1 - self.x0
    }

    /// Signed height, `y1 - y0` (negative if not normalized).
    pub fn height(self) -> f32 {
        self.y1 - self.y0
    }

    /// Returns the same area with `x0 <= x1` and `y0 <= y1`.
    pub fn normalize(self) -> Rect {
        Rect {
            x0: self.x0.min(self.x1),
            y0: self.y0.min(self.y1),
            x1: self.x0.max(self.x1),
            y1: self.y0.max(self.y1),
        }
    }

    /// Smallest rectangle containing both `self` and `other`.
    pub fn union(self, other: Rect) -> Rect {
        let a = self.normalize();
        let b = other.normalize();
        Rect {
            x0: a.x0.min(b.x0),
            y0: a.y0.min(b.y0),
            x1: a.x1.max(b.x1),
            y1: a.y1.max(b.y1),
        }
    }

    /// Overlapping area of `self` and `other`, or `None` if they are
    /// disjoint. Rectangles that merely touch yield a degenerate
    /// (zero-width or zero-height) rectangle.
    pub fn intersect(self, other: Rect) -> Option<Rect> {
        let a = self.normalize();
        let b = other.normalize();
        let r = Rect {
            x0: a.x0.max(b.x0),
            y0: a.y0.max(b.y0),
            x1: a.x1.min(b.x1),
            y1: a.y1.min(b.y1),
        };
        if r.x0 <= r.x1 && r.y0 <= r.y1 {
            Some(r)
        } else {
            None
        }
    }

    /// Whether `p` lies inside the (normalized) rectangle, borders included.
    pub fn contains(self, p: Point) -> bool {
        let r = self.normalize();
        p.x >= r.x0 && p.x <= r.x1 && p.y >= r.y0 && p.y <= r.y1
    }

    /// Axis-aligned bounding box of the rectangle's four corners mapped
    /// through `m`. The result is normalized.
    pub fn transform(self, m: Matrix) -> Rect {
        let corners = [
            m.apply(Point::new(self.x0, self.y0)),
            m.apply(Point::new(self.x1, self.y0)),
            m.apply(Point::new(self.x1, self.y1)),
            m.apply(Point::new(self.x0, self.y1)),
        ];
        let mut r = Rect::new(corners[0].x, corners[0].y, corners[0].x, corners[0].y);
        for c in &corners[1..] {
            r.x0 = r.x0.min(c.x);
            r.y0 = r.y0.min(c.y);
            r.x1 = r.x1.max(c.x);
            r.y1 = r.y1.max(c.y);
        }
        r
    }
}

/// A 2-D affine transformation `[a b c d e f]` in the PDF convention:
///
/// ```text
/// x' = a·x + c·y + e
/// y' = b·x + d·y + f
/// ```
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix {
    pub a: f32,
    pub b: f32,
    pub c: f32,
    pub d: f32,
    pub e: f32,
    pub f: f32,
}

impl Matrix {
    /// The identity transformation.
    pub const fn identity() -> Matrix {
        Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Translation by `(tx, ty)`.
    pub const fn translate(tx: f32, ty: f32) -> Matrix {
        Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: tx,
            f: ty,
        }
    }

    /// Scaling by `(sx, sy)` about the origin.
    pub const fn scale(sx: f32, sy: f32) -> Matrix {
        Matrix {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Counter-clockwise rotation by `deg` degrees about the origin.
    pub fn rotate_deg(deg: f32) -> Matrix {
        let (sin, cos) = deg.to_radians().sin_cos();
        Matrix {
            a: cos,
            b: sin,
            c: -sin,
            d: cos,
            e: 0.0,
            f: 0.0,
        }
    }

    /// Composition "apply `self` first, then `other`".
    ///
    /// With the row-vector convention this is the matrix product
    /// `self × other`, so `p.apply(self.concat(other)) ==
    /// other.apply(self.apply(p))`.
    pub fn concat(self, other: Matrix) -> Matrix {
        Matrix {
            a: self.a * other.a + self.b * other.c,
            b: self.a * other.b + self.b * other.d,
            c: self.c * other.a + self.d * other.c,
            d: self.c * other.b + self.d * other.d,
            e: self.e * other.a + self.f * other.c + other.e,
            f: self.e * other.b + self.f * other.d + other.f,
        }
    }

    /// Transforms a point.
    pub fn apply(self, p: Point) -> Point {
        Point {
            x: self.a * p.x + self.c * p.y + self.e,
            y: self.b * p.x + self.d * p.y + self.f,
        }
    }

    /// The inverse transformation, or `None` if `self` is singular
    /// (determinant zero or non-finite).
    pub fn invert(self) -> Option<Matrix> {
        let det = self.a * self.d - self.b * self.c;
        if det == 0.0 || !det.is_finite() {
            return None;
        }
        let inv = 1.0 / det;
        Some(Matrix {
            a: self.d * inv,
            b: -self.b * inv,
            c: -self.c * inv,
            d: self.a * inv,
            e: (self.c * self.f - self.d * self.e) * inv,
            f: (self.b * self.e - self.a * self.f) * inv,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPS: f32 = 1e-4;

    fn assert_point_eq(got: Point, want: Point) {
        assert!(
            (got.x - want.x).abs() < EPS && (got.y - want.y).abs() < EPS,
            "expected {want:?}, got {got:?}"
        );
    }

    fn assert_matrix_eq(got: Matrix, want: Matrix) {
        for (g, w) in [
            (got.a, want.a),
            (got.b, want.b),
            (got.c, want.c),
            (got.d, want.d),
            (got.e, want.e),
            (got.f, want.f),
        ] {
            assert!((g - w).abs() < EPS, "expected {want:?}, got {got:?}");
        }
    }

    fn assert_rect_eq(got: Rect, want: Rect) {
        for (g, w) in [
            (got.x0, want.x0),
            (got.y0, want.y0),
            (got.x1, want.x1),
            (got.y1, want.y1),
        ] {
            assert!((g - w).abs() < EPS, "expected {want:?}, got {got:?}");
        }
    }

    #[test]
    fn identity_is_neutral() {
        let id = Matrix::identity();
        let m = Matrix::translate(3.0, -4.0).concat(Matrix::scale(2.0, 0.5));
        assert_matrix_eq(id.concat(m), m);
        assert_matrix_eq(m.concat(id), m);
        assert_point_eq(id.apply(Point::new(7.5, -2.0)), Point::new(7.5, -2.0));
    }

    #[test]
    fn apply_translate_scale_rotate() {
        let p = Point::new(1.0, 2.0);
        assert_point_eq(
            Matrix::translate(10.0, 20.0).apply(p),
            Point::new(11.0, 22.0),
        );
        assert_point_eq(Matrix::scale(2.0, 3.0).apply(p), Point::new(2.0, 6.0));
        assert_point_eq(
            Matrix::rotate_deg(90.0).apply(Point::new(1.0, 0.0)),
            Point::new(0.0, 1.0),
        );
        assert_point_eq(
            Matrix::rotate_deg(180.0).apply(Point::new(1.0, 2.0)),
            Point::new(-1.0, -2.0),
        );
    }

    #[test]
    fn concat_applies_self_then_other() {
        // Translate by (10, 0), then scale by 2: (1, 2) -> (11, 2) -> (22, 4).
        let m = Matrix::translate(10.0, 0.0).concat(Matrix::scale(2.0, 2.0));
        assert_point_eq(m.apply(Point::new(1.0, 2.0)), Point::new(22.0, 4.0));
        // Opposite order: scale first, then translate: (1, 2) -> (2, 4) -> (12, 4).
        let m = Matrix::scale(2.0, 2.0).concat(Matrix::translate(10.0, 0.0));
        assert_point_eq(m.apply(Point::new(1.0, 2.0)), Point::new(12.0, 4.0));
    }

    #[test]
    fn concat_matches_sequential_application() {
        let m1 = Matrix::rotate_deg(30.0).concat(Matrix::translate(5.0, -3.0));
        let m2 = Matrix::scale(1.5, 0.25).concat(Matrix::rotate_deg(-45.0));
        let p = Point::new(-2.0, 7.0);
        assert_point_eq(m1.concat(m2).apply(p), m2.apply(m1.apply(p)));
    }

    #[test]
    fn invert_round_trips() {
        let m = Matrix::translate(4.0, -1.0)
            .concat(Matrix::rotate_deg(37.0))
            .concat(Matrix::scale(2.0, 5.0));
        let inv = m.invert().expect("matrix should be invertible");
        assert_matrix_eq(m.concat(inv), Matrix::identity());
        assert_matrix_eq(inv.concat(m), Matrix::identity());

        let p = Point::new(3.25, -9.5);
        assert_point_eq(inv.apply(m.apply(p)), p);
    }

    #[test]
    fn invert_translation() {
        let inv = Matrix::translate(10.0, -2.0).invert().unwrap();
        assert_matrix_eq(inv, Matrix::translate(-10.0, 2.0));
    }

    #[test]
    fn invert_singular_is_none() {
        assert!(Matrix::scale(0.0, 0.0).invert().is_none());
        assert!(Matrix::scale(1.0, 0.0).invert().is_none());
        // Collinear basis vectors: determinant zero.
        let m = Matrix {
            a: 1.0,
            b: 2.0,
            c: 2.0,
            d: 4.0,
            e: 5.0,
            f: 6.0,
        };
        assert!(m.invert().is_none());
    }

    #[test]
    fn rect_width_height_normalize() {
        let r = Rect::new(10.0, 20.0, 4.0, 2.0);
        assert!((r.width() - -6.0).abs() < EPS);
        assert!((r.height() - -18.0).abs() < EPS);
        let n = r.normalize();
        assert_rect_eq(n, Rect::new(4.0, 2.0, 10.0, 20.0));
        assert!((n.width() - 6.0).abs() < EPS);
        assert!((n.height() - 18.0).abs() < EPS);
    }

    #[test]
    fn rect_union() {
        let a = Rect::new(0.0, 0.0, 2.0, 2.0);
        let b = Rect::new(1.0, -1.0, 3.0, 1.0);
        assert_rect_eq(a.union(b), Rect::new(0.0, -1.0, 3.0, 2.0));
        // Union treats inputs as normalized.
        let flipped = Rect::new(3.0, 1.0, 1.0, -1.0);
        assert_rect_eq(a.union(flipped), Rect::new(0.0, -1.0, 3.0, 2.0));
    }

    #[test]
    fn rect_intersect() {
        let a = Rect::new(0.0, 0.0, 4.0, 4.0);
        let b = Rect::new(2.0, 1.0, 6.0, 3.0);
        assert_rect_eq(a.intersect(b).unwrap(), Rect::new(2.0, 1.0, 4.0, 3.0));
        // Disjoint.
        assert!(a.intersect(Rect::new(5.0, 5.0, 6.0, 6.0)).is_none());
        // Touching edges produce a degenerate rectangle, not None.
        let edge = a.intersect(Rect::new(4.0, 0.0, 8.0, 4.0)).unwrap();
        assert!((edge.width() - 0.0).abs() < EPS);
    }

    #[test]
    fn rect_contains() {
        let r = Rect::new(0.0, 0.0, 10.0, 5.0);
        assert!(r.contains(Point::new(5.0, 2.5)));
        assert!(r.contains(Point::new(0.0, 0.0))); // corner inclusive
        assert!(r.contains(Point::new(10.0, 5.0))); // corner inclusive
        assert!(!r.contains(Point::new(10.1, 2.0)));
        assert!(!r.contains(Point::new(5.0, -0.1)));
        // Un-normalized rectangle behaves like its normalized form.
        let f = Rect::new(10.0, 5.0, 0.0, 0.0);
        assert!(f.contains(Point::new(5.0, 2.5)));
    }

    #[test]
    fn rect_transform() {
        let r = Rect::new(0.0, 0.0, 2.0, 1.0);
        assert_rect_eq(
            r.transform(Matrix::translate(5.0, 6.0)),
            Rect::new(5.0, 6.0, 7.0, 7.0),
        );
        assert_rect_eq(
            r.transform(Matrix::scale(2.0, 3.0)),
            Rect::new(0.0, 0.0, 4.0, 3.0),
        );
        // Rotation by 90 degrees maps [0,2]x[0,1] onto [-1,0]x[0,2].
        assert_rect_eq(
            r.transform(Matrix::rotate_deg(90.0)),
            Rect::new(-1.0, 0.0, 0.0, 2.0),
        );
    }
}
