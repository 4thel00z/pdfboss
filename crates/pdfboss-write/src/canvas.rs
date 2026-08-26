//! The imperative painting surface. A `Canvas` accumulates
//! `pdfboss_core::content::Op` values — the exact IR the reader parses —
//! plus the fonts and images those operators reference. Nothing here
//! serializes; `finish` hands the parts to document assembly.
//!
//! Resource naming contract: the font first used gets resource name `F1`,
//! the next distinct font `F2`, …; image handles map to `Im1`, `Im2`, … in
//! [`add_image`](Canvas::add_image) order. Assembly builds the matching
//! `/Resources` dictionary from [`CanvasParts`].

use pdfboss_core::content::Op;
use pdfboss_core::{Matrix, Point};

use crate::color::Color;
use crate::error::Result;
use crate::font::Standard14;
use crate::image::ImageData;

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

/// The accumulated output of a finished canvas.
#[derive(Debug)]
pub struct CanvasParts {
    /// Operators, in paint order.
    pub ops: Vec<Op>,
    /// Distinct fonts in first-use order; index `i` is resource `F{i+1}`.
    pub fonts: Vec<Standard14>,
    /// Images in registration order; index `i` is resource `Im{i+1}`.
    pub images: Vec<ImageData>,
}

/// An imperative painter producing content-stream operators.
#[derive(Debug, Default)]
pub struct Canvas {
    ops: Vec<Op>,
    fonts: Vec<Standard14>,
    images: Vec<ImageData>,
}

impl Canvas {
    /// Creates an empty canvas.
    pub fn new() -> Canvas {
        Canvas::default()
    }

    /// Pushes the graphics state (`q`).
    pub fn save(&mut self) {
        todo!("save state ({} ops)", self.ops.len())
    }

    /// Pops the graphics state (`Q`).
    pub fn restore(&mut self) {
        todo!("restore state ({} ops)", self.ops.len())
    }

    /// Concatenates `m` onto the current transformation matrix (`cm`).
    pub fn transform(&mut self, m: Matrix) {
        let unused = (&mut self.ops, m);
        todo!("transform: {unused:?}")
    }

    /// Sets the stroke line width (`w`).
    pub fn set_line_width(&mut self, width: f32) {
        let unused = (&mut self.ops, width);
        todo!("line width: {unused:?}")
    }

    /// Sets the line cap style (`J`).
    pub fn set_line_cap(&mut self, cap: LineCap) {
        let unused = (&mut self.ops, cap);
        todo!("line cap: {unused:?}")
    }

    /// Sets the line join style (`j`).
    pub fn set_line_join(&mut self, join: LineJoin) {
        let unused = (&mut self.ops, join);
        todo!("line join: {unused:?}")
    }

    /// Sets the miter limit (`M`).
    pub fn set_miter_limit(&mut self, limit: f32) {
        let unused = (&mut self.ops, limit);
        todo!("miter limit: {unused:?}")
    }

    /// Sets the dash pattern (`d`).
    pub fn set_dash(&mut self, pattern: &[f32], phase: f32) {
        let unused = (&mut self.ops, pattern, phase);
        todo!("dash: {unused:?}")
    }

    /// Sets the fill color (`g`/`rg`/`k`).
    pub fn set_fill(&mut self, color: Color) {
        let unused = (&mut self.ops, color);
        todo!("fill color: {unused:?}")
    }

    /// Sets the stroke color (`G`/`RG`/`K`).
    pub fn set_stroke(&mut self, color: Color) {
        let unused = (&mut self.ops, color);
        todo!("stroke color: {unused:?}")
    }

    /// Begins a new subpath at `(x, y)` (`m`).
    pub fn move_to(&mut self, x: f32, y: f32) {
        let unused = (&mut self.ops, x, y);
        todo!("move to: {unused:?}")
    }

    /// Straight segment to `(x, y)` (`l`).
    pub fn line_to(&mut self, x: f32, y: f32) {
        let unused = (&mut self.ops, x, y);
        todo!("line to: {unused:?}")
    }

    /// Cubic Bézier with two control points (`c`).
    #[allow(clippy::too_many_arguments)] // six coordinates is the operator's arity
    pub fn curve_to(&mut self, x1: f32, y1: f32, x2: f32, y2: f32, x3: f32, y3: f32) {
        let unused = (&mut self.ops, x1, y1, x2, y2, x3, y3);
        todo!("curve to: {unused:?}")
    }

    /// Closes the current subpath (`h`).
    pub fn close(&mut self) {
        todo!("close path ({} ops)", self.ops.len())
    }

    /// Appends a rectangle subpath (`re`).
    pub fn rect(&mut self, x: f32, y: f32, width: f32, height: f32) {
        let unused = (&mut self.ops, x, y, width, height);
        todo!("rect: {unused:?}")
    }

    /// Appends a circle as four Bézier arcs.
    pub fn circle(&mut self, cx: f32, cy: f32, r: f32) {
        let unused = (&mut self.ops, cx, cy, r);
        todo!("circle: {unused:?}")
    }

    /// Appends an axis-aligned ellipse as four Bézier arcs.
    pub fn ellipse(&mut self, cx: f32, cy: f32, rx: f32, ry: f32) {
        let unused = (&mut self.ops, cx, cy, rx, ry);
        todo!("ellipse: {unused:?}")
    }

    /// Appends a closed polygon through `points`.
    pub fn polygon(&mut self, points: &[Point]) {
        let unused = (&mut self.ops, points);
        todo!("polygon: {unused:?}")
    }

    /// Fills the current path, nonzero winding (`f`).
    pub fn fill(&mut self) {
        todo!("fill ({} ops)", self.ops.len())
    }

    /// Fills the current path, even-odd (`f*`).
    pub fn fill_even_odd(&mut self) {
        todo!("fill even-odd ({} ops)", self.ops.len())
    }

    /// Strokes the current path (`S`).
    pub fn stroke(&mut self) {
        todo!("stroke ({} ops)", self.ops.len())
    }

    /// Closes and strokes the current path (`s`).
    pub fn close_stroke(&mut self) {
        todo!("close-stroke ({} ops)", self.ops.len())
    }

    /// Fills then strokes the current path (`B`).
    pub fn fill_stroke(&mut self) {
        todo!("fill-stroke ({} ops)", self.ops.len())
    }

    /// Intersects the clip with the current path, nonzero (`W n`).
    pub fn clip(&mut self) {
        todo!("clip ({} ops)", self.ops.len())
    }

    /// Intersects the clip with the current path, even-odd (`W* n`).
    pub fn clip_even_odd(&mut self) {
        todo!("clip even-odd ({} ops)", self.ops.len())
    }

    /// Ends the current path without painting (`n`).
    pub fn end_path(&mut self) {
        todo!("end path ({} ops)", self.ops.len())
    }

    /// Shows one line of text with its baseline origin at `(x, y)`
    /// (`BT`/`Tf`/`Td`/`Tj`/`ET`). Errors on unencodable characters.
    pub fn text(&mut self, text: &str, x: f32, y: f32, font: Standard14, size: f32) -> Result<()> {
        let unused = (&mut self.ops, text, x, y, font, size);
        todo!("text: {unused:?}")
    }

    /// Registers an image for use with [`draw_image`](Canvas::draw_image).
    pub fn add_image(&mut self, image: ImageData) -> ImageHandle {
        let unused = (&mut self.images, image);
        todo!("add image: {unused:?}")
    }

    /// Paints a registered image into the axis-aligned box at `(x, y)` with
    /// the given size (`q cm Do Q`).
    pub fn draw_image(&mut self, image: ImageHandle, x: f32, y: f32, width: f32, height: f32) {
        let unused = (&mut self.ops, image, x, y, width, height);
        todo!("draw image: {unused:?}")
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
        }
    }
}
