//! The imperative painting surface. A `Canvas` accumulates
//! `pdfboss_core::content::Op` values — the exact IR the reader parses —
//! plus the fonts and images those operators reference. Nothing here
//! serializes; `finish` hands the parts to document assembly.
//!
//! Resource naming contract: the font first used gets resource name `F1`,
//! the next distinct font `F2`, …; image handles map to `Im1`, `Im2`, … in
//! [`add_image`](Canvas::add_image) order; groups registered with
//! [`group`](Canvas::group) map to `Gp1`, `Gp2`, …; distinct `/ExtGState`
//! entries pushed by the alpha/blend-mode setters map to `Gs1`, `Gs2`, ….
//! Assembly builds the matching `/Resources` dictionary from
//! [`CanvasParts`].

use pdfboss_core::content::Op;
use pdfboss_core::{Dict, Matrix, Name, Object, Point};

use crate::color::Color;
use crate::error::Result;
use crate::font::Standard14;
use crate::image::ImageData;

#[allow(clippy::excessive_precision)] // the contract names this exact literal
const KAPPA: f32 = 0.552284749831;

/// Line cap style (ISO 32000 §8.4.3.3).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineCap {
    /// Squared off at the endpoint.
    #[default]
    Butt,
    /// Semicircle around the endpoint.
    Round,
    /// Square projecting half a width beyond the endpoint.
    Square,
}

/// Line join style (ISO 32000 §8.4.3.4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum LineJoin {
    /// Outer edges extended to a point.
    #[default]
    Miter,
    /// Circular arc around the corner.
    Round,
    /// Cut off with a straight edge.
    Bevel,
}

/// Handle to an image registered on a canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ImageHandle(pub(crate) usize);

/// Handle to a sub-canvas registered with [`Canvas::group`], painted with
/// [`Canvas::draw_group`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GroupHandle(pub(crate) usize);

/// A separable blend mode (ISO 32000 §11.3.5, Table 136), set with
/// [`Canvas::set_blend_mode`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlendMode {
    /// Source color replaces the backdrop.
    Normal,
    /// Product of source and backdrop.
    Multiply,
    /// Complement of the product of the complements.
    Screen,
    /// Multiply or screen, chosen by the backdrop.
    Overlay,
    /// The darker of source and backdrop, per component.
    Darken,
    /// The lighter of source and backdrop, per component.
    Lighten,
    /// Brightens the backdrop toward the source.
    ColorDodge,
    /// Darkens the backdrop toward the source.
    ColorBurn,
    /// Multiply or screen, chosen by the source (Overlay with roles swapped).
    HardLight,
    /// A softer variant of `HardLight`.
    SoftLight,
    /// Absolute difference of source and backdrop.
    Difference,
    /// Like `Difference`, with lower contrast.
    Exclusion,
}

impl BlendMode {
    /// The `/BM` name for this mode.
    fn pdf_name(self) -> &'static str {
        match self {
            BlendMode::Normal => "Normal",
            BlendMode::Multiply => "Multiply",
            BlendMode::Screen => "Screen",
            BlendMode::Overlay => "Overlay",
            BlendMode::Darken => "Darken",
            BlendMode::Lighten => "Lighten",
            BlendMode::ColorDodge => "ColorDodge",
            BlendMode::ColorBurn => "ColorBurn",
            BlendMode::HardLight => "HardLight",
            BlendMode::SoftLight => "SoftLight",
            BlendMode::Difference => "Difference",
            BlendMode::Exclusion => "Exclusion",
        }
    }
}

/// One deduped `/ExtGState` entry. Each [`Canvas`] setter writes only its
/// own key, so at most one field is ever set — a `gs` referencing `/ca`
/// alone must leave `/CA` and `/BM` untouched (ISO 32000 §8.4.5).
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub(crate) struct GState {
    pub(crate) fill_alpha: Option<f32>,
    pub(crate) stroke_alpha: Option<f32>,
    pub(crate) blend_mode: Option<BlendMode>,
}

impl GState {
    /// The `/ExtGState` dictionary for this entry.
    pub(crate) fn ext_gstate_dict(self) -> Dict {
        let mut dict = Dict::new();
        if let Some(alpha) = self.fill_alpha {
            dict.insert(name("ca"), Object::Real(f64::from(alpha)));
        }
        if let Some(alpha) = self.stroke_alpha {
            dict.insert(name("CA"), Object::Real(f64::from(alpha)));
        }
        if let Some(mode) = self.blend_mode {
            dict.insert(name("BM"), Object::Name(name(mode.pdf_name())));
        }
        dict
    }
}

/// A `Name` from a string literal.
fn name(text: &str) -> Name {
    Name(text.to_string())
}

/// The accumulated output of a finished canvas.
#[derive(Debug)]
pub struct CanvasParts {
    /// Operators, in paint order.
    pub ops: Vec<Op>,
    /// Distinct fonts in first-use order; index `i` is resource `F{i+1}`.
    pub fonts: Vec<Standard14>,
    /// Images in registration order; index `i` is resource `Im{i+1}`.
    pub images: Vec<ImageData>,
    /// Sub-canvases registered with [`Canvas::group`], paired with their
    /// `/BBox`; index `i` is resource `Gp{i+1}`.
    pub(crate) groups: Vec<(CanvasParts, [f32; 4])>,
    /// Distinct `/ExtGState` entries in first-use order; index `i` is
    /// resource `Gs{i+1}`.
    pub(crate) gstates: Vec<GState>,
}

/// An imperative painter producing content-stream operators.
#[derive(Debug, Default)]
pub struct Canvas {
    ops: Vec<Op>,
    fonts: Vec<Standard14>,
    images: Vec<ImageData>,
    groups: Vec<(CanvasParts, [f32; 4])>,
    gstates: Vec<GState>,
}

impl Canvas {
    /// Creates an empty canvas.
    pub fn new() -> Canvas {
        Canvas::default()
    }

    /// Pushes the graphics state (`q`).
    pub fn save(&mut self) {
        self.ops.push(Op::Save);
    }

    /// Pops the graphics state (`Q`).
    pub fn restore(&mut self) {
        self.ops.push(Op::Restore);
    }

    /// Concatenates `m` onto the current transformation matrix (`cm`).
    pub fn transform(&mut self, m: Matrix) {
        self.ops.push(Op::Concat(m));
    }

    /// Sets the stroke line width (`w`).
    pub fn set_line_width(&mut self, width: f32) {
        self.ops.push(Op::SetLineWidth(width));
    }

    /// Sets the line cap style (`J`).
    pub fn set_line_cap(&mut self, cap: LineCap) {
        self.ops.push(Op::SetLineCap(cap as i32));
    }

    /// Sets the line join style (`j`).
    pub fn set_line_join(&mut self, join: LineJoin) {
        self.ops.push(Op::SetLineJoin(join as i32));
    }

    /// Sets the miter limit (`M`).
    pub fn set_miter_limit(&mut self, limit: f32) {
        self.ops.push(Op::SetMiterLimit(limit));
    }

    /// Sets the dash pattern (`d`).
    pub fn set_dash(&mut self, pattern: &[f32], phase: f32) {
        self.ops.push(Op::SetDash(pattern.to_vec(), phase));
    }

    /// Sets the fill color (`g`/`rg`/`k`).
    pub fn set_fill(&mut self, color: Color) {
        self.ops.push(color.fill_op());
    }

    /// Sets the stroke color (`G`/`RG`/`K`).
    pub fn set_stroke(&mut self, color: Color) {
        self.ops.push(color.stroke_op());
    }

    /// Begins a new subpath at `(x, y)` (`m`).
    pub fn move_to(&mut self, x: f32, y: f32) {
        self.ops.push(Op::MoveTo(x, y));
    }

    /// Straight segment to `(x, y)` (`l`).
    pub fn line_to(&mut self, x: f32, y: f32) {
        self.ops.push(Op::LineTo(x, y));
    }

    /// Cubic Bézier with two control points (`c`).
    #[allow(clippy::too_many_arguments)] // six coordinates is the operator's arity
    pub fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32) {
        self.ops.push(Op::CurveTo(x1, y1, x2, y2, x3, y3));
    }

    /// Closes the current subpath (`h`).
    pub fn close(&mut self) {
        self.ops.push(Op::ClosePath);
    }

    /// Appends a rectangle subpath (`re`).
    pub fn rect(&mut self, x: f32, y: f32, width: f32, height: f32) {
        self.ops.push(Op::Rect(x, y, width, height));
    }

    /// Appends a circle as four Bézier arcs, starting at the rightmost
    /// point `(cx + r, cy)` and running counter-clockwise.
    pub fn circle(&mut self, cx: f32, cy: f32, r: f32) {
        self.ellipse(cx, cy, r, r);
    }

    /// Appends an axis-aligned ellipse as four Bézier arcs, starting at the
    /// rightmost point `(cx + rx, cy)` and running counter-clockwise.
    pub fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32) {
        let ox = KAPPA * rx;
        let oy = KAPPA * ry;
        self.ops.push(Op::MoveTo(cx + rx, cy));
        self.ops
            .push(Op::CurveTo(cx + rx, cy + oy, cx + ox, cy + ry, cx, cy + ry));
        self.ops
            .push(Op::CurveTo(cx - ox, cy + ry, cx - rx, cy + oy, cx - rx, cy));
        self.ops
            .push(Op::CurveTo(cx - rx, cy - oy, cx - ox, cy - ry, cx, cy - ry));
        self.ops
            .push(Op::CurveTo(cx + ox, cy - ry, cx + rx, cy - oy, cx + rx, cy));
        self.ops.push(Op::ClosePath);
    }

    /// Appends a closed polygon through `points`. Fewer than three points
    /// appends nothing.
    pub fn polygon(&mut self, points: &[Point]) {
        if points.len() < 3 {
            return;
        }
        self.ops.push(Op::MoveTo(points[0].x, points[0].y));
        for point in &points[1..] {
            self.ops.push(Op::LineTo(point.x, point.y));
        }
        self.ops.push(Op::ClosePath);
    }

    /// Fills the current path, nonzero winding (`f`).
    pub fn fill(&mut self) {
        self.ops.push(Op::Fill);
    }

    /// Fills the current path, even-odd (`f*`).
    pub fn fill_even_odd(&mut self) {
        self.ops.push(Op::FillEvenOdd);
    }

    /// Strokes the current path (`S`).
    pub fn stroke(&mut self) {
        self.ops.push(Op::Stroke);
    }

    /// Closes and strokes the current path (`s`).
    pub fn close_stroke(&mut self) {
        self.ops.push(Op::CloseStroke);
    }

    /// Fills then strokes the current path (`B`).
    pub fn fill_stroke(&mut self) {
        self.ops.push(Op::FillStroke);
    }

    /// Intersects the clip with the current path, nonzero (`W n`). The
    /// path is consumed: it is ended without painting and a fresh path
    /// starts afterwards.
    pub fn clip(&mut self) {
        self.ops.push(Op::ClipNonZero);
        self.ops.push(Op::EndPath);
    }

    /// Intersects the clip with the current path, even-odd (`W* n`). The
    /// path is consumed: it is ended without painting and a fresh path
    /// starts afterwards.
    pub fn clip_even_odd(&mut self) {
        self.ops.push(Op::ClipEvenOdd);
        self.ops.push(Op::EndPath);
    }

    /// Ends the current path without painting (`n`).
    pub fn end_path(&mut self) {
        self.ops.push(Op::EndPath);
    }

    /// Shows one line of text with its baseline origin at `(x, y)`
    /// (`BT`/`Tf`/`Td`/`Tj`/`ET`). Errors on unencodable characters
    /// before any operator is pushed, leaving the canvas untouched.
    pub fn text(&mut self, text: &str, x: f32, y: f32, font: Standard14, size: f32) -> Result<()> {
        let encoded = font.encode(text)?;
        let index = match self.fonts.iter().position(|face| *face == font) {
            Some(index) => index,
            None => {
                self.fonts.push(font);
                self.fonts.len() - 1
            }
        };
        self.ops.push(Op::BeginText);
        self.ops
            .push(Op::SetFont(Name(format!("F{}", index + 1)), size));
        self.ops.push(Op::TextMove(x, y));
        self.ops.push(Op::ShowText(encoded));
        self.ops.push(Op::EndText);
        Ok(())
    }

    /// Registers an image for use with [`draw_image`](Canvas::draw_image).
    pub fn add_image(&mut self, image: ImageData) -> ImageHandle {
        let handle = ImageHandle(self.images.len());
        self.images.push(image);
        handle
    }

    /// Paints a registered image into the axis-aligned box at `(x, y)` with
    /// the given size (`q cm Do Q`).
    pub fn draw_image(&mut self, image: ImageHandle, x: f32, y: f32, width: f32, height: f32) {
        self.ops.push(Op::Save);
        self.ops.push(Op::Concat(Matrix {
            a: width,
            b: 0.0,
            c: 0.0,
            d: height,
            e: x,
            f: y,
        }));
        self.ops
            .push(Op::XObject(Name(format!("Im{}", image.0 + 1))));
        self.ops.push(Op::Restore);
    }

    /// Registers a finished sub-canvas as a form group, with `bbox`
    /// (`[llx, lly, urx, ury]`) becoming the form's `/BBox`. Paint it with
    /// [`draw_group`](Canvas::draw_group).
    pub fn group(&mut self, canvas: Canvas, bbox: [f32; 4]) -> GroupHandle {
        let handle = GroupHandle(self.groups.len());
        self.groups.push((canvas.into_parts(), bbox));
        handle
    }

    /// Paints a registered group under `matrix` (`q cm /GpN Do Q`). Two
    /// calls with the same `group` reference the same form resource.
    pub fn draw_group(&mut self, group: GroupHandle, matrix: Matrix) {
        self.ops.push(Op::Save);
        self.ops.push(Op::Concat(matrix));
        self.ops
            .push(Op::XObject(Name(format!("Gp{}", group.0 + 1))));
        self.ops.push(Op::Restore);
    }

    /// Sets the nonstroking alpha constant (`gs` referencing `/ca`).
    pub fn set_fill_alpha(&mut self, alpha: f32) {
        self.push_gstate(GState {
            fill_alpha: Some(alpha),
            ..GState::default()
        });
    }

    /// Sets the stroking alpha constant (`gs` referencing `/CA`).
    pub fn set_stroke_alpha(&mut self, alpha: f32) {
        self.push_gstate(GState {
            stroke_alpha: Some(alpha),
            ..GState::default()
        });
    }

    /// Sets the blend mode (`gs` referencing `/BM`).
    pub fn set_blend_mode(&mut self, mode: BlendMode) {
        self.push_gstate(GState {
            blend_mode: Some(mode),
            ..GState::default()
        });
    }

    /// Emits a `gs` referencing `state`'s resource, reusing an identical
    /// prior entry instead of adding a duplicate.
    fn push_gstate(&mut self, state: GState) {
        let index = match self.gstates.iter().position(|seen| *seen == state) {
            Some(index) => index,
            None => {
                self.gstates.push(state);
                self.gstates.len() - 1
            }
        };
        self.ops
            .push(Op::SetExtGState(Name(format!("Gs{}", index + 1))));
    }

    /// Pushes a raw operator — the escape hatch for anything the methods
    /// above don't cover. Resource-referencing operators are the caller's
    /// responsibility to keep consistent with the naming contract.
    pub fn op(&mut self, op: Op) {
        self.ops.push(op);
    }

    /// The operators accumulated so far, in paint order.
    pub fn ops(&self) -> &[Op] {
        &self.ops
    }

    /// Consumes the canvas into its parts for document assembly.
    pub(crate) fn into_parts(self) -> CanvasParts {
        CanvasParts {
            ops: self.ops,
            fonts: self.fonts,
            images: self.images,
            groups: self.groups,
            gstates: self.gstates,
        }
    }
}

#[cfg(test)]
mod tests {
    use pdfboss_core::content::parse_content;
    use pdfboss_core::Name;

    use super::*;
    use crate::content::serialize_ops;
    use crate::error::Error;

    fn name(text: &str) -> Name {
        Name(text.into())
    }

    #[test]
    fn state_ops_push_single_operators() {
        let mut canvas = Canvas::new();
        canvas.save();
        canvas.restore();
        canvas.transform(Matrix {
            a: 1.0,
            b: 2.0,
            c: 3.0,
            d: 4.0,
            e: 5.0,
            f: 6.0,
        });
        canvas.set_line_width(2.5);
        canvas.set_miter_limit(4.0);
        canvas.set_dash(&[3.0, 1.0], 0.5);
        assert_eq!(
            canvas.ops(),
            [
                Op::Save,
                Op::Restore,
                Op::Concat(Matrix {
                    a: 1.0,
                    b: 2.0,
                    c: 3.0,
                    d: 4.0,
                    e: 5.0,
                    f: 6.0,
                }),
                Op::SetLineWidth(2.5),
                Op::SetMiterLimit(4.0),
                Op::SetDash(vec![3.0, 1.0], 0.5),
            ]
        );
    }

    #[test]
    fn line_cap_and_join_map_declaration_order() {
        let mut canvas = Canvas::new();
        canvas.set_line_cap(LineCap::Butt);
        canvas.set_line_cap(LineCap::Round);
        canvas.set_line_cap(LineCap::Square);
        canvas.set_line_join(LineJoin::Miter);
        canvas.set_line_join(LineJoin::Round);
        canvas.set_line_join(LineJoin::Bevel);
        assert_eq!(
            canvas.ops(),
            [
                Op::SetLineCap(0),
                Op::SetLineCap(1),
                Op::SetLineCap(2),
                Op::SetLineJoin(0),
                Op::SetLineJoin(1),
                Op::SetLineJoin(2),
            ]
        );
    }

    #[test]
    fn fill_and_stroke_colors() {
        let mut canvas = Canvas::new();
        canvas.set_fill(Color::Gray(0.5));
        canvas.set_fill(Color::Rgb(0.1, 0.2, 0.3));
        canvas.set_fill(Color::Cmyk(0.1, 0.2, 0.3, 0.4));
        canvas.set_stroke(Color::Gray(0.5));
        canvas.set_stroke(Color::Rgb(0.1, 0.2, 0.3));
        canvas.set_stroke(Color::Cmyk(0.1, 0.2, 0.3, 0.4));
        assert_eq!(
            canvas.ops(),
            [
                Op::SetFillGray(0.5),
                Op::SetFillRGB(0.1, 0.2, 0.3),
                Op::SetFillCMYK(0.1, 0.2, 0.3, 0.4),
                Op::SetStrokeGray(0.5),
                Op::SetStrokeRGB(0.1, 0.2, 0.3),
                Op::SetStrokeCMYK(0.1, 0.2, 0.3, 0.4),
            ]
        );
    }

    #[test]
    fn path_construction_ops() {
        let mut canvas = Canvas::new();
        canvas.move_to(1.0, 2.0);
        canvas.line_to(3.0, 4.0);
        canvas.curve_to(1.0, 2.0, 3.0, 4.0, 5.0, 6.0);
        canvas.close();
        canvas.rect(10.0, 20.0, 30.0, 40.0);
        assert_eq!(
            canvas.ops(),
            [
                Op::MoveTo(1.0, 2.0),
                Op::LineTo(3.0, 4.0),
                Op::CurveTo(1.0, 2.0, 3.0, 4.0, 5.0, 6.0),
                Op::ClosePath,
                Op::Rect(10.0, 20.0, 30.0, 40.0),
            ]
        );
    }

    #[test]
    fn circle_appends_four_arcs_and_close() {
        let mut canvas = Canvas::new();
        canvas.circle(0.0, 0.0, 1.0);
        assert_eq!(
            canvas.ops(),
            [
                Op::MoveTo(1.0, 0.0),
                Op::CurveTo(1.0, KAPPA, KAPPA, 1.0, 0.0, 1.0),
                Op::CurveTo(-KAPPA, 1.0, -1.0, KAPPA, -1.0, 0.0),
                Op::CurveTo(-1.0, -KAPPA, -KAPPA, -1.0, 0.0, -1.0),
                Op::CurveTo(KAPPA, -1.0, 1.0, -KAPPA, 1.0, 0.0),
                Op::ClosePath,
            ]
        );
    }

    #[test]
    fn ellipse_appends_four_arcs_and_close() {
        let (cx, cy, rx, ry) = (10.0f32, 20.0f32, 4.0f32, 2.0f32);
        let ox = KAPPA * rx;
        let oy = KAPPA * ry;
        let mut canvas = Canvas::new();
        canvas.ellipse(cx, cy, rx, ry);
        assert_eq!(
            canvas.ops(),
            [
                Op::MoveTo(cx + rx, cy),
                Op::CurveTo(cx + rx, cy + oy, cx + ox, cy + ry, cx, cy + ry),
                Op::CurveTo(cx - ox, cy + ry, cx - rx, cy + oy, cx - rx, cy),
                Op::CurveTo(cx - rx, cy - oy, cx - ox, cy - ry, cx, cy - ry),
                Op::CurveTo(cx + ox, cy - ry, cx + rx, cy - oy, cx + rx, cy),
                Op::ClosePath,
            ]
        );
    }

    #[test]
    fn polygon_appends_closed_path() {
        let mut canvas = Canvas::new();
        canvas.polygon(&[
            Point::new(0.0, 0.0),
            Point::new(10.0, 0.0),
            Point::new(5.0, 8.0),
        ]);
        assert_eq!(
            canvas.ops(),
            [
                Op::MoveTo(0.0, 0.0),
                Op::LineTo(10.0, 0.0),
                Op::LineTo(5.0, 8.0),
                Op::ClosePath,
            ]
        );
    }

    #[test]
    fn polygon_under_three_points_is_no_op() {
        let mut canvas = Canvas::new();
        canvas.polygon(&[]);
        canvas.polygon(&[Point::new(1.0, 2.0)]);
        canvas.polygon(&[Point::new(1.0, 2.0), Point::new(3.0, 4.0)]);
        assert_eq!(canvas.ops(), []);
    }

    #[test]
    fn paint_verbs_push_single_operators() {
        let mut canvas = Canvas::new();
        canvas.fill();
        canvas.fill_even_odd();
        canvas.stroke();
        canvas.close_stroke();
        canvas.fill_stroke();
        canvas.end_path();
        assert_eq!(
            canvas.ops(),
            [
                Op::Fill,
                Op::FillEvenOdd,
                Op::Stroke,
                Op::CloseStroke,
                Op::FillStroke,
                Op::EndPath,
            ]
        );
    }

    #[test]
    fn clip_pairs_consume_the_path() {
        let mut canvas = Canvas::new();
        canvas.clip();
        canvas.clip_even_odd();
        assert_eq!(
            canvas.ops(),
            [Op::ClipNonZero, Op::EndPath, Op::ClipEvenOdd, Op::EndPath,]
        );
    }

    #[test]
    fn text_pushes_five_op_sequence() {
        let mut canvas = Canvas::new();
        canvas
            .text("Hi", 72.0, 720.0, Standard14::Helvetica, 12.0)
            .unwrap();
        assert_eq!(
            canvas.ops(),
            [
                Op::BeginText,
                Op::SetFont(name("F1"), 12.0),
                Op::TextMove(72.0, 720.0),
                Op::ShowText(b"Hi".to_vec()),
                Op::EndText,
            ]
        );
        let parts = canvas.into_parts();
        assert_eq!(parts.fonts, [Standard14::Helvetica]);
    }

    #[test]
    fn texts_in_the_same_font_share_f1() {
        let mut canvas = Canvas::new();
        canvas
            .text("one", 0.0, 0.0, Standard14::TimesRoman, 10.0)
            .unwrap();
        canvas
            .text("two", 0.0, 20.0, Standard14::TimesRoman, 10.0)
            .unwrap();
        let font_names: Vec<&Op> = canvas
            .ops()
            .iter()
            .filter(|op| matches!(op, Op::SetFont(..)))
            .collect();
        assert_eq!(
            font_names,
            [
                &Op::SetFont(name("F1"), 10.0),
                &Op::SetFont(name("F1"), 10.0),
            ]
        );
        assert_eq!(canvas.into_parts().fonts, [Standard14::TimesRoman]);
    }

    #[test]
    fn second_face_gets_f2() {
        let mut canvas = Canvas::new();
        canvas
            .text("one", 0.0, 0.0, Standard14::Helvetica, 10.0)
            .unwrap();
        canvas
            .text("two", 0.0, 20.0, Standard14::CourierBold, 10.0)
            .unwrap();
        canvas
            .text("three", 0.0, 40.0, Standard14::Helvetica, 10.0)
            .unwrap();
        let font_names: Vec<&Op> = canvas
            .ops()
            .iter()
            .filter(|op| matches!(op, Op::SetFont(..)))
            .collect();
        assert_eq!(
            font_names,
            [
                &Op::SetFont(name("F1"), 10.0),
                &Op::SetFont(name("F2"), 10.0),
                &Op::SetFont(name("F1"), 10.0),
            ]
        );
        assert_eq!(
            canvas.into_parts().fonts,
            [Standard14::Helvetica, Standard14::CourierBold]
        );
    }

    #[test]
    fn text_error_leaves_canvas_untouched() {
        let mut canvas = Canvas::new();
        canvas.save();
        let result = canvas.text("\u{2318}", 0.0, 0.0, Standard14::Helvetica, 12.0);
        assert!(matches!(
            result,
            Err(Error::Unencodable { ch: '\u{2318}', .. })
        ));
        assert_eq!(canvas.ops(), [Op::Save]);
        assert!(canvas.into_parts().fonts.is_empty());
    }

    #[test]
    fn draw_image_emits_save_concat_xobject_restore() {
        let mut canvas = Canvas::new();
        let first = canvas.add_image(ImageData::gray8(1, 1, vec![0]).unwrap());
        let second = canvas.add_image(ImageData::gray8(1, 1, vec![255]).unwrap());
        canvas.draw_image(first, 10.0, 20.0, 100.0, 50.0);
        canvas.draw_image(second, 0.0, 0.0, 5.0, 5.0);
        assert_eq!(
            canvas.ops(),
            [
                Op::Save,
                Op::Concat(Matrix {
                    a: 100.0,
                    b: 0.0,
                    c: 0.0,
                    d: 50.0,
                    e: 10.0,
                    f: 20.0,
                }),
                Op::XObject(name("Im1")),
                Op::Restore,
                Op::Save,
                Op::Concat(Matrix {
                    a: 5.0,
                    b: 0.0,
                    c: 0.0,
                    d: 5.0,
                    e: 0.0,
                    f: 0.0,
                }),
                Op::XObject(name("Im2")),
                Op::Restore,
            ]
        );
        assert_eq!(canvas.into_parts().images.len(), 2);
    }

    #[test]
    fn group_registers_subcanvas_parts_and_bbox() {
        let mut sub = Canvas::new();
        sub.rect(1.0, 2.0, 3.0, 4.0);
        sub.fill();
        let mut canvas = Canvas::new();
        let handle = canvas.group(sub, [0.0, 0.0, 50.0, 20.0]);
        assert_eq!(handle, GroupHandle(0));
        let parts = canvas.into_parts();
        assert_eq!(parts.groups.len(), 1);
        let (group_parts, bbox) = &parts.groups[0];
        assert_eq!(*bbox, [0.0, 0.0, 50.0, 20.0]);
        assert_eq!(group_parts.ops, [Op::Rect(1.0, 2.0, 3.0, 4.0), Op::Fill]);
    }

    #[test]
    fn second_group_gets_index_one() {
        let mut canvas = Canvas::new();
        let first = canvas.group(Canvas::new(), [0.0, 0.0, 1.0, 1.0]);
        let second = canvas.group(Canvas::new(), [0.0, 0.0, 1.0, 1.0]);
        assert_eq!(first, GroupHandle(0));
        assert_eq!(second, GroupHandle(1));
    }

    #[test]
    fn draw_group_emits_save_cm_xobject_restore_and_round_trips() {
        let mut canvas = Canvas::new();
        let handle = canvas.group(Canvas::new(), [0.0, 0.0, 10.0, 10.0]);
        let matrix = Matrix {
            a: 1.0,
            b: 0.0,
            c: 0.0,
            d: 1.0,
            e: 72.0,
            f: 144.0,
        };
        canvas.draw_group(handle, matrix);
        assert_eq!(
            canvas.ops(),
            [
                Op::Save,
                Op::Concat(matrix),
                Op::XObject(name("Gp1")),
                Op::Restore,
            ]
        );
        let bytes = serialize_ops(canvas.ops());
        let parsed = parse_content(&bytes).unwrap();
        assert_eq!(parsed, canvas.ops());
    }

    #[test]
    fn two_draws_of_one_group_reference_the_same_resource_name() {
        let mut canvas = Canvas::new();
        let handle = canvas.group(Canvas::new(), [0.0, 0.0, 10.0, 10.0]);
        canvas.draw_group(handle, Matrix::identity());
        canvas.draw_group(handle, Matrix::translate(5.0, 5.0));
        let names: Vec<&Op> = canvas
            .ops()
            .iter()
            .filter(|op| matches!(op, Op::XObject(_)))
            .collect();
        assert_eq!(
            names,
            [&Op::XObject(name("Gp1")), &Op::XObject(name("Gp1"))]
        );
    }

    #[test]
    fn second_registered_group_gets_gp2() {
        let mut canvas = Canvas::new();
        let first = canvas.group(Canvas::new(), [0.0, 0.0, 1.0, 1.0]);
        let second = canvas.group(Canvas::new(), [0.0, 0.0, 1.0, 1.0]);
        canvas.draw_group(first, Matrix::identity());
        canvas.draw_group(second, Matrix::identity());
        let names: Vec<&Op> = canvas
            .ops()
            .iter()
            .filter(|op| matches!(op, Op::XObject(_)))
            .collect();
        assert_eq!(
            names,
            [&Op::XObject(name("Gp1")), &Op::XObject(name("Gp2"))]
        );
    }

    #[test]
    fn nested_groups_recurse() {
        let mut innermost = Canvas::new();
        innermost.fill();
        let mut outer = Canvas::new();
        let inner_handle = outer.group(innermost, [0.0, 0.0, 1.0, 1.0]);
        outer.draw_group(inner_handle, Matrix::identity());
        let mut canvas = Canvas::new();
        let outer_handle = canvas.group(outer, [0.0, 0.0, 2.0, 2.0]);
        canvas.draw_group(outer_handle, Matrix::identity());
        let parts = canvas.into_parts();
        let (outer_parts, _) = &parts.groups[0];
        assert_eq!(outer_parts.groups.len(), 1);
        let (inner_parts, _) = &outer_parts.groups[0];
        assert_eq!(inner_parts.ops, [Op::Fill]);
    }

    #[test]
    fn set_fill_alpha_pushes_gs_referencing_gs1() {
        let mut canvas = Canvas::new();
        canvas.set_fill_alpha(0.5);
        assert_eq!(canvas.ops(), [Op::SetExtGState(name("Gs1"))]);
        let parts = canvas.into_parts();
        assert_eq!(parts.gstates.len(), 1);
        assert_eq!(parts.gstates[0].fill_alpha, Some(0.5));
        assert_eq!(parts.gstates[0].stroke_alpha, None);
        assert_eq!(parts.gstates[0].blend_mode, None);
    }

    #[test]
    fn set_stroke_alpha_and_blend_mode_get_distinct_resources() {
        let mut canvas = Canvas::new();
        canvas.set_fill_alpha(0.5);
        canvas.set_stroke_alpha(0.5);
        canvas.set_blend_mode(BlendMode::Multiply);
        assert_eq!(
            canvas.ops(),
            [
                Op::SetExtGState(name("Gs1")),
                Op::SetExtGState(name("Gs2")),
                Op::SetExtGState(name("Gs3")),
            ]
        );
        assert_eq!(canvas.into_parts().gstates.len(), 3);
    }

    #[test]
    fn identical_setter_calls_dedupe_to_one_resource() {
        let mut canvas = Canvas::new();
        canvas.set_fill_alpha(0.5);
        canvas.set_fill_alpha(0.5);
        assert_eq!(
            canvas.ops(),
            [Op::SetExtGState(name("Gs1")), Op::SetExtGState(name("Gs1"))]
        );
        assert_eq!(canvas.into_parts().gstates.len(), 1);
    }

    #[test]
    fn gstate_setters_round_trip() {
        let mut canvas = Canvas::new();
        canvas.set_fill_alpha(0.25);
        canvas.set_stroke_alpha(0.75);
        canvas.set_blend_mode(BlendMode::Screen);
        let bytes = serialize_ops(canvas.ops());
        let parsed = parse_content(&bytes).unwrap();
        assert_eq!(parsed, canvas.ops());
    }
}
