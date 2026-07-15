//! Content-op execution against a graphics state stack: transforms, colors,
//! clipping, ExtGState, form XObject recursion, and paint dispatch.
//!
//! Limitations (v0.1): only embedded-TrueType glyph outlines are painted
//! (other fonts are positioned but not drawn); `sh` shadings are skipped;
//! pattern fills paint mid-gray.

use std::collections::HashMap;
use std::rc::Rc;

use pdfboss_core::content::{parse_content, ImageParams, Op, TextItem};
use pdfboss_core::filters::decode_stream;
use pdfboss_core::geom::Matrix;
use pdfboss_core::{Dict, Document, Name, Object, Page, Result, Stream};

use crate::color::ColorSpace;
use crate::glyph::GlyphFont;
use crate::image::{self, DrawParams};
use crate::path::PathBuilder;
use crate::raster::{fill_path, FillRule, Mask};
use crate::stroke::stroke_path;
use crate::truetype::Seg;
use crate::type3::Type3Font;
use crate::{GlyphPainting, Pixmap, RenderOptions};

/// Maximum `q`/`Q` nesting depth.
const MAX_GSTATE_DEPTH: usize = 64;
/// Maximum form XObject recursion depth.
const MAX_FORM_DEPTH: u32 = 16;
/// Maximum pixmap side length, guarding malformed boxes and huge scales.
const MAX_SIDE: f32 = 16384.0;

/// The graphics state carried across operators and saved/restored by
/// `q`/`Q`.
#[derive(Debug, Clone)]
struct GState {
    /// Current transformation matrix, user space to device pixels.
    ctm: Matrix,
    fill_space: ColorSpace,
    stroke_space: ColorSpace,
    /// Fill color already converted to RGB in 0..=1.
    fill_rgb: [f32; 3],
    stroke_rgb: [f32; 3],
    /// A `/Pattern` fill space is active: paint mid-gray instead.
    fill_pattern: bool,
    stroke_pattern: bool,
    /// Line width in user space.
    line_width: f32,
    /// Stored but unused: stroking approximates round caps (v0.1).
    #[allow(dead_code)]
    line_cap: i32,
    /// Stored but unused: stroking approximates round joins (v0.1).
    #[allow(dead_code)]
    line_join: i32,
    /// Stored but unused: joins are round, so the miter limit never cuts.
    #[allow(dead_code)]
    miter_limit: f32,
    /// Dash pattern lengths in user space (empty = solid).
    dash: Vec<f32>,
    dash_phase: f32,
    /// Constant fill alpha (`ca`).
    fill_alpha: f32,
    /// Constant stroke alpha (`CA`).
    stroke_alpha: f32,
    /// Active clip as a device-space coverage mask. Shared behind an `Rc` so
    /// that saving state (`q`) and entering a form clone the graphics state
    /// without copying the full-page mask buffer; a new clip always builds a
    /// fresh `Mask`, so this is effectively clone-on-write.
    clip: Option<Rc<Mask>>,
}

impl GState {
    fn new(ctm: Matrix) -> GState {
        GState {
            ctm,
            fill_space: ColorSpace::DeviceGray,
            stroke_space: ColorSpace::DeviceGray,
            fill_rgb: [0.0; 3],
            stroke_rgb: [0.0; 3],
            fill_pattern: false,
            stroke_pattern: false,
            line_width: 1.0,
            line_cap: 0,
            line_join: 0,
            miter_limit: 10.0,
            dash: Vec::new(),
            dash_phase: 0.0,
            fill_alpha: 1.0,
            stroke_alpha: 1.0,
            clip: None,
        }
    }

    /// The fill color as RGBA8 (patterns paint mid-gray, documented v0.1
    /// approximation).
    fn fill_rgba8(&self) -> [u8; 4] {
        rgba8(if self.fill_pattern {
            [0.5; 3]
        } else {
            self.fill_rgb
        })
    }

    /// The stroke color as RGBA8.
    fn stroke_rgba8(&self) -> [u8; 4] {
        rgba8(if self.stroke_pattern {
            [0.5; 3]
        } else {
            self.stroke_rgb
        })
    }
}

/// Text-showing state within a `BT`/`ET` block. Held per content stream (not
/// saved by `q`/`Q`), matching how the extractor tracks text.
struct TextState {
    /// Text matrix and line matrix.
    tm: Matrix,
    tlm: Matrix,
    font: Option<Rc<GlyphFont>>,
    /// A `/Type3` font whose glyphs paint by re-entering the executor per
    /// CharProc (ISO 32000-1 §9.6.5). Invariant: at most one of `font`
    /// (outline) / `type3` is `Some`.
    type3: Option<Rc<Type3Font>>,
    size: f32,
    char_spacing: f32,
    word_spacing: f32,
    /// Horizontal scale as a fraction (`Tz` / 100).
    horiz: f32,
    leading: f32,
    rise: f32,
}

impl Default for TextState {
    fn default() -> TextState {
        TextState {
            tm: Matrix::identity(),
            tlm: Matrix::identity(),
            font: None,
            type3: None,
            size: 0.0,
            char_spacing: 0.0,
            word_spacing: 0.0,
            horiz: 1.0,
            leading: 0.0,
            rise: 0.0,
        }
    }
}

/// Converts unit-range RGB to opaque RGBA8.
fn rgba8(rgb: [f32; 3]) -> [u8; 4] {
    let q = |v: f32| (v.clamp(0.0, 1.0) * 255.0 + 0.5) as u8;
    [q(rgb[0]), q(rgb[1]), q(rgb[2]), 255]
}

/// Approximate device scale of `m`: the square root of the absolute
/// determinant (exact for uniform scaling), used to size stroke widths and
/// dash lengths in device space.
fn ctm_scale(m: Matrix) -> f32 {
    let det = (m.a * m.d - m.b * m.c).abs();
    if det.is_finite() && det > 0.0 {
        det.sqrt()
    } else {
        1.0
    }
}

/// True when every value is finite (NaN/Inf operands skip the op).
fn all_finite(vals: &[f32]) -> bool {
    vals.iter().all(|v| v.is_finite())
}

/// True when all six matrix entries are finite.
fn finite_matrix(m: &Matrix) -> bool {
    all_finite(&[m.a, m.b, m.c, m.d, m.e, m.f])
}

/// Flattens a glyph outline (font-unit segments) into device-space subpaths via
/// `to_device`, promoting each quadratic to an equivalent cubic so the shared
/// cubic flattener can subdivide it.
fn build_glyph(segs: &[Seg], to_device: Matrix) -> Vec<crate::path::Subpath> {
    let mut pb = PathBuilder::new(to_device);
    for seg in segs {
        match *seg {
            Seg::Move(x, y) => pb.move_to(x, y),
            Seg::Line(x, y) => pb.line_to(x, y),
            Seg::Quad(cx, cy, x, y) => {
                let p0 = pb.current_point();
                let c1x = p0.x + 2.0 / 3.0 * (cx - p0.x);
                let c1y = p0.y + 2.0 / 3.0 * (cy - p0.y);
                let c2x = x + 2.0 / 3.0 * (cx - x);
                let c2y = y + 2.0 / 3.0 * (cy - y);
                pb.curve_to(c1x, c1y, c2x, c2y, x, y);
            }
            Seg::Cubic(c1x, c1y, c2x, c2y, x, y) => pb.curve_to(c1x, c1y, c2x, c2y, x, y),
            Seg::Close => pb.close(),
        }
    }
    pb.finish()
}

/// The base transform mapping the (normalized) crop box to device pixels:
/// translate the crop origin away, apply `/Rotate` clockwise into the
/// display quadrant, then flip y and scale so the display top-left lands
/// on pixel (0, 0).
fn base_ctm(crop: pdfboss_core::Rect, rotate: i32, scale: f32) -> Matrix {
    let (cw, ch) = (crop.width(), crop.height());
    let spin = match rotate {
        90 => Matrix {
            a: 0.0,
            b: -1.0,
            c: 1.0,
            d: 0.0,
            e: 0.0,
            f: cw,
        },
        180 => Matrix {
            a: -1.0,
            b: 0.0,
            c: 0.0,
            d: -1.0,
            e: cw,
            f: ch,
        },
        270 => Matrix {
            a: 0.0,
            b: 1.0,
            c: -1.0,
            d: 0.0,
            e: ch,
            f: 0.0,
        },
        _ => Matrix::identity(),
    };
    let disp_h = if rotate == 90 || rotate == 270 {
        cw
    } else {
        ch
    };
    let flip = Matrix {
        a: scale,
        b: 0.0,
        c: 0.0,
        d: -scale,
        e: 0.0,
        f: disp_h * scale,
    };
    Matrix::translate(-crop.x0, -crop.y0)
        .concat(spin)
        .concat(flip)
}

/// Renders `page` from `doc` at `scale` onto a white background. The pixel
/// size is `ceil(crop_w * scale) x ceil(crop_h * scale)` after `/Rotate`.
/// Content errors are lenient: an unreadable stream renders blank.
pub(crate) fn render_page_with_options(
    doc: &Document,
    page: &Page,
    scale: f32,
    opts: &RenderOptions,
) -> Result<Pixmap> {
    let scale = if scale.is_finite() && scale > 0.0 {
        scale
    } else {
        1.0
    };
    let (w_pt, h_pt) = page.size();
    let pw = (w_pt * scale).ceil().clamp(1.0, MAX_SIDE) as u32;
    let ph = (h_pt * scale).ceil().clamp(1.0, MAX_SIDE) as u32;
    let mut pix = Pixmap::new(pw, ph);
    pix.fill([255, 255, 255, 255]);
    let content = page.content(doc).unwrap_or_default();
    let ops = parse_content(&content).unwrap_or_default();
    let ctm = base_ctm(page.crop_box.normalize(), page.rotate, scale);
    let mut exec = Executor {
        doc,
        pix,
        painting: opts.glyph_painting,
        color_locked: false,
    };
    exec.run(&ops, &[&page.resources], GState::new(ctm), 0);
    Ok(exec.pix)
}

/// Executes parsed content operators against a shared pixmap; forms
/// recurse through [`Executor::run`] with their own resource chain.
struct Executor<'a> {
    doc: &'a Document,
    pix: Pixmap,
    painting: GlyphPainting,
    /// Set while painting a `d1` (uncolored) Type3 CharProc: ISO 32000-1
    /// §9.6.5.2 says such a glyph "shall not specify any color", so
    /// `run_color_or_misc` turns every fill/stroke color-setting op into a
    /// no-op and the glyph keeps the color inherited from the text state.
    color_locked: bool,
}

impl Executor<'_> {
    /// Runs `ops` with resource lookups walking `chain` (innermost first).
    /// `depth` counts form recursion. All failures are lenient skips.
    fn run(&mut self, ops: &[Op], chain: &[&Dict], base: GState, depth: u32) {
        let mut gs = base;
        let mut stack: Vec<GState> = Vec::new();
        let mut path: Option<PathBuilder> = None;
        let mut pending_clip: Option<FillRule> = None;
        let mut ts = TextState::default();
        let mut fonts: HashMap<String, Option<Rc<GlyphFont>>> = HashMap::new();
        for op in ops {
            match op {
                Op::Save => {
                    if stack.len() < MAX_GSTATE_DEPTH {
                        stack.push(gs.clone());
                    }
                }
                Op::Restore => {
                    if let Some(prev) = stack.pop() {
                        gs = prev;
                    }
                }
                Op::Concat(m) => {
                    if finite_matrix(m) {
                        gs.ctm = m.concat(gs.ctm);
                    }
                }
                Op::SetLineWidth(w) => {
                    if w.is_finite() && *w >= 0.0 {
                        gs.line_width = *w;
                    }
                }
                Op::SetLineCap(c) => gs.line_cap = *c,
                Op::SetLineJoin(j) => gs.line_join = *j,
                Op::SetMiterLimit(m) => {
                    if m.is_finite() {
                        gs.miter_limit = *m;
                    }
                }
                Op::SetDash(d, phase) => {
                    if all_finite(d) && phase.is_finite() {
                        gs.dash = d.clone();
                        gs.dash_phase = *phase;
                    }
                }
                Op::SetExtGState(name) => self.apply_ext_gstate(name, chain, &mut gs),
                Op::SetRenderingIntent(_) | Op::SetFlatness(_) => {}

                // Path construction (user space; the builder applies the
                // CTM captured when the path starts).
                Op::MoveTo(x, y) => {
                    if all_finite(&[*x, *y]) {
                        builder(&mut path, &gs).move_to(*x, *y);
                    }
                }
                Op::LineTo(x, y) => {
                    if all_finite(&[*x, *y]) {
                        builder(&mut path, &gs).line_to(*x, *y);
                    }
                }
                Op::CurveTo(x1, y1, x2, y2, x3, y3) => {
                    if all_finite(&[*x1, *y1, *x2, *y2, *x3, *y3]) {
                        builder(&mut path, &gs).curve_to(*x1, *y1, *x2, *y2, *x3, *y3);
                    }
                }
                Op::CurveToV(x2, y2, x3, y3) => {
                    if all_finite(&[*x2, *y2, *x3, *y3]) {
                        builder(&mut path, &gs).curve_to_v(*x2, *y2, *x3, *y3);
                    }
                }
                Op::CurveToY(x1, y1, x3, y3) => {
                    if all_finite(&[*x1, *y1, *x3, *y3]) {
                        builder(&mut path, &gs).curve_to_y(*x1, *y1, *x3, *y3);
                    }
                }
                Op::ClosePath => {
                    if let Some(pb) = path.as_mut() {
                        pb.close();
                    }
                }
                Op::Rect(x, y, w, h) => {
                    if all_finite(&[*x, *y, *w, *h]) {
                        builder(&mut path, &gs).rect(*x, *y, *w, *h);
                    }
                }

                // Path painting: fill first, then stroke; a pending W/W*
                // clip takes effect after any of these (including n).
                Op::Stroke => self.paint(&mut gs, &mut path, &mut pending_clip, PAINT_STROKE),
                Op::CloseStroke => self.paint(
                    &mut gs,
                    &mut path,
                    &mut pending_clip,
                    Paint {
                        close: true,
                        ..PAINT_STROKE
                    },
                ),
                Op::Fill => self.paint(&mut gs, &mut path, &mut pending_clip, PAINT_FILL),
                Op::FillEvenOdd => self.paint(&mut gs, &mut path, &mut pending_clip, PAINT_FILL_EO),
                Op::FillStroke => self.paint(&mut gs, &mut path, &mut pending_clip, PAINT_BOTH),
                Op::FillStrokeEvenOdd => {
                    self.paint(&mut gs, &mut path, &mut pending_clip, PAINT_BOTH_EO)
                }
                Op::CloseFillStroke => self.paint(
                    &mut gs,
                    &mut path,
                    &mut pending_clip,
                    Paint {
                        close: true,
                        ..PAINT_BOTH
                    },
                ),
                Op::CloseFillStrokeEvenOdd => self.paint(
                    &mut gs,
                    &mut path,
                    &mut pending_clip,
                    Paint {
                        close: true,
                        ..PAINT_BOTH_EO
                    },
                ),
                Op::EndPath => self.paint(&mut gs, &mut path, &mut pending_clip, PAINT_NONE),
                Op::ClipNonZero => pending_clip = Some(FillRule::NonZero),
                Op::ClipEvenOdd => pending_clip = Some(FillRule::EvenOdd),

                // Text: a minimal show-string state machine that paints
                // embedded TrueType glyph outlines (other fonts stay unpainted).
                Op::BeginText => {
                    ts.tm = Matrix::identity();
                    ts.tlm = Matrix::identity();
                }
                Op::SetCharSpacing(v) if v.is_finite() => ts.char_spacing = *v,
                Op::SetWordSpacing(v) if v.is_finite() => ts.word_spacing = *v,
                Op::SetHorizScaling(v) if v.is_finite() => ts.horiz = v / 100.0,
                Op::SetLeading(v) if v.is_finite() => ts.leading = *v,
                Op::SetTextRise(v) if v.is_finite() => ts.rise = *v,
                Op::SetFont(name, size) => {
                    ts.size = if size.is_finite() { *size } else { 0.0 };
                    ts.font = self.glyph_font(&name.0, chain, &mut fonts);
                    // Type3 is the fallback when no outline font loads: a
                    // `/Type3` dict at a tier that paints embedded programs.
                    // The invariant (at most one of font/type3) holds because
                    // this only runs when `ts.font` is `None`.
                    ts.type3 = if ts.font.is_some() {
                        None
                    } else {
                        self.type3_font(&name.0, chain)
                    };
                }
                Op::SetTextMatrix(m) if finite_matrix(m) => {
                    ts.tm = *m;
                    ts.tlm = *m;
                }
                Op::TextMove(tx, ty) if all_finite(&[*tx, *ty]) => {
                    ts.tlm = Matrix::translate(*tx, *ty).concat(ts.tlm);
                    ts.tm = ts.tlm;
                }
                Op::TextMoveSetLeading(tx, ty) if all_finite(&[*tx, *ty]) => {
                    ts.leading = -*ty;
                    ts.tlm = Matrix::translate(*tx, *ty).concat(ts.tlm);
                    ts.tm = ts.tlm;
                }
                Op::TextNextLine => {
                    ts.tlm = Matrix::translate(0.0, -ts.leading).concat(ts.tlm);
                    ts.tm = ts.tlm;
                }
                Op::ShowText(s) => self.show_text(&gs, &mut ts, s, chain, depth),
                Op::ShowTextAdjusted(items) => {
                    for item in items {
                        match item {
                            TextItem::Str(s) => self.show_text(&gs, &mut ts, s, chain, depth),
                            TextItem::Offset(n) => {
                                let tx = -n / 1000.0 * ts.size * ts.horiz;
                                if tx.is_finite() {
                                    ts.tm = Matrix::translate(tx, 0.0).concat(ts.tm);
                                }
                            }
                        }
                    }
                }
                Op::NextLineShowText(s) => {
                    ts.tlm = Matrix::translate(0.0, -ts.leading).concat(ts.tlm);
                    ts.tm = ts.tlm;
                    self.show_text(&gs, &mut ts, s, chain, depth);
                }
                Op::NextLineShowTextSpaced(aw, ac, s) => {
                    if aw.is_finite() {
                        ts.word_spacing = *aw;
                    }
                    if ac.is_finite() {
                        ts.char_spacing = *ac;
                    }
                    ts.tlm = Matrix::translate(0.0, -ts.leading).concat(ts.tlm);
                    ts.tm = ts.tlm;
                    self.show_text(&gs, &mut ts, s, chain, depth);
                }

                other => self.run_color_or_misc(other, chain, &mut gs, depth),
            }
        }
    }
}

/// Starts (or continues) the current path with the CTM in effect.
fn builder<'p>(path: &'p mut Option<PathBuilder>, gs: &GState) -> &'p mut PathBuilder {
    path.get_or_insert_with(|| PathBuilder::new(gs.ctm))
}

/// What a painting operator does with the current path.
#[derive(Clone, Copy)]
struct Paint {
    close: bool,
    fill: Option<FillRule>,
    stroke: bool,
}

const PAINT_NONE: Paint = Paint {
    close: false,
    fill: None,
    stroke: false,
};
const PAINT_STROKE: Paint = Paint {
    stroke: true,
    ..PAINT_NONE
};
const PAINT_FILL: Paint = Paint {
    fill: Some(FillRule::NonZero),
    ..PAINT_NONE
};
const PAINT_FILL_EO: Paint = Paint {
    fill: Some(FillRule::EvenOdd),
    ..PAINT_NONE
};
const PAINT_BOTH: Paint = Paint {
    stroke: true,
    ..PAINT_FILL
};
const PAINT_BOTH_EO: Paint = Paint {
    stroke: true,
    ..PAINT_FILL_EO
};

impl Executor<'_> {
    /// Fills and/or strokes the current path, applies any pending clip
    /// from `W`/`W*`, and resets the path.
    fn paint(
        &mut self,
        gs: &mut GState,
        path: &mut Option<PathBuilder>,
        pending: &mut Option<FillRule>,
        how: Paint,
    ) {
        let polys = match path.take() {
            Some(mut pb) => {
                if how.close {
                    pb.close();
                }
                pb.finish()
            }
            None => Vec::new(),
        };
        if let Some(rule) = how.fill {
            fill_path(
                &mut self.pix,
                &polys,
                rule,
                gs.fill_rgba8(),
                gs.fill_alpha,
                gs.clip.as_deref(),
            );
        }
        if how.stroke {
            let s = ctm_scale(gs.ctm);
            let dash: Vec<f32> = gs.dash.iter().map(|d| d * s).collect();
            let quads = stroke_path(&polys, gs.line_width * s, &dash, gs.dash_phase * s);
            fill_path(
                &mut self.pix,
                &quads,
                FillRule::NonZero,
                gs.stroke_rgba8(),
                gs.stroke_alpha,
                gs.clip.as_deref(),
            );
        }
        if let Some(rule) = pending.take() {
            let mut mask = Mask::from_path(self.pix.width, self.pix.height, &polys, rule);
            if let Some(old) = &gs.clip {
                mask.intersect(old);
            }
            gs.clip = Some(Rc::new(mask));
        }
    }

    /// Resolves and caches a paintable font by resource name (`None` for fonts
    /// whose glyphs cannot be drawn).
    fn glyph_font(
        &self,
        name: &str,
        chain: &[&Dict],
        cache: &mut HashMap<String, Option<Rc<GlyphFont>>>,
    ) -> Option<Rc<GlyphFont>> {
        if let Some(f) = cache.get(name) {
            return f.clone();
        }
        let loaded = self
            .find_res(chain, "Font", name)
            .and_then(|o| o.as_dict().cloned())
            .and_then(|d| GlyphFont::load(self.doc, &d, self.painting).map(Rc::new));
        cache.insert(name.to_string(), loaded.clone());
        loaded
    }

    /// Resolves a `/Type3` font resource for painting, or `None` when the tier
    /// forbids embedded programs, the name is missing, or the resource is not a
    /// `/Type3` dict. Called only after the outline loader declined the name.
    fn type3_font(&self, name: &str, chain: &[&Dict]) -> Option<Rc<Type3Font>> {
        if !self.painting.paints_all_embedded() {
            return None;
        }
        let dict = self
            .find_res(chain, "Font", name)
            .and_then(|o| o.as_dict().cloned())?;
        if dict.get_name("Subtype").map(|n| n.0.as_str()) != Some("Type3") {
            return None;
        }
        Type3Font::load(self.doc, &dict).map(Rc::new)
    }

    /// Paints one show-string's glyphs and advances the text matrix. Codes with
    /// no drawable glyph still advance, so surrounding text stays positioned.
    /// `chain`/`depth` thread the resource chain and form-recursion depth
    /// through to a Type3 glyph's CharProc (which re-enters [`Executor::run`]).
    fn show_text(
        &mut self,
        gs: &GState,
        ts: &mut TextState,
        bytes: &[u8],
        chain: &[&Dict],
        depth: u32,
    ) {
        if ts.type3.is_some() {
            self.show_text_type3(gs, ts, bytes, chain, depth);
            return;
        }
        let Some(font) = ts.font.clone() else {
            return;
        };
        let upm = font.units_per_em();
        let two_byte = font.two_byte();
        let fill = gs.fill_rgba8();
        let mut i = 0;
        while i < bytes.len() {
            let (code, n) = if two_byte && i + 1 < bytes.len() {
                (u32::from(u16::from_be_bytes([bytes[i], bytes[i + 1]])), 2)
            } else {
                (u32::from(bytes[i]), 1)
            };
            i += n;
            let gid = font.gid(code);

            // glyph units -> text space (÷ em, then the text-scaling params),
            // -> user space (Tm) -> device (CTM).
            let params = Matrix {
                a: ts.size * ts.horiz,
                b: 0.0,
                c: 0.0,
                d: ts.size,
                e: 0.0,
                f: ts.rise,
            };
            let to_device = Matrix::scale(1.0 / upm, 1.0 / upm)
                .concat(params)
                .concat(ts.tm)
                .concat(gs.ctm);
            if gid != 0 && finite_matrix(&to_device) {
                let segs = font.outline(gid);
                if !segs.is_empty() {
                    let polys = build_glyph(&segs, to_device);
                    fill_path(
                        &mut self.pix,
                        &polys,
                        FillRule::NonZero,
                        fill,
                        gs.fill_alpha,
                        gs.clip.as_deref(),
                    );
                }
            }

            // Advance: (w0·Tfs + Tc + Tw[single-byte space]) · Th.
            let w0 = font.advance(code) / upm;
            let word = if n == 1 && code == 32 {
                ts.word_spacing
            } else {
                0.0
            };
            let tx = (w0 * ts.size + ts.char_spacing + word) * ts.horiz;
            if tx.is_finite() {
                ts.tm = Matrix::translate(tx, 0.0).concat(ts.tm);
            }
        }
    }

    /// Paints a `/Type3` show-string: each one-byte code's CharProc runs as a
    /// nested content stream (ISO 32000-1 §9.6.5). The glyph matrix mirrors the
    /// outline path with `/FontMatrix` in place of the `1/upm` scale, so glyph
    /// space maps through text space, the text state, and the CTM to device.
    /// Codes with no CharProc, a non-finite matrix, or a depth at the recursion
    /// limit still advance, keeping surrounding text positioned.
    fn show_text_type3(
        &mut self,
        gs: &GState,
        ts: &mut TextState,
        bytes: &[u8],
        chain: &[&Dict],
        depth: u32,
    ) {
        let Some(t3) = ts.type3.clone() else {
            return;
        };
        let font_matrix = t3.font_matrix();
        for &byte in bytes {
            let code = u32::from(byte);

            // glyph space -> text space (/FontMatrix), -> the text-scaling
            // params, -> user space (Tm) -> device (CTM): the outline chain
            // with `font_matrix` substituted for `scale(1/upm)`.
            let params = Matrix {
                a: ts.size * ts.horiz,
                b: 0.0,
                c: 0.0,
                d: ts.size,
                e: 0.0,
                f: ts.rise,
            };
            let glyph_ctm = font_matrix.concat(params).concat(ts.tm).concat(gs.ctm);

            // The depth guard bounds a self-referential glyph (one that shows
            // itself, directly or via a form): each CharProc re-entry increments
            // `depth`, so painting stops at `MAX_FORM_DEPTH`.
            if depth < MAX_FORM_DEPTH && finite_matrix(&glyph_ctm) {
                if let Some(proc_obj) = t3.char_proc(code).cloned() {
                    self.run_char_proc(&proc_obj, &t3, chain, gs, glyph_ctm, depth);
                }
            }

            // Advance: the glyph-space width becomes a text-space displacement
            // via the matrix x-scale, then (w0·Tfs + Tc + Tw[space]) · Th.
            let w0 = t3.width(code).unwrap_or(0.0) * font_matrix.a;
            let word = if code == 32 { ts.word_spacing } else { 0.0 };
            let tx = (w0 * ts.size + ts.char_spacing + word) * ts.horiz;
            if tx.is_finite() {
                ts.tm = Matrix::translate(tx, 0.0).concat(ts.tm);
            }
        }
    }

    /// Runs one Type3 CharProc: resolve its stream, parse it, and re-enter
    /// [`Executor::run`] with the glyph CTM, the font's own `/Resources`
    /// prepended to `chain`, and `depth + 1`. Inherits the caller's clip,
    /// alpha, and fill color (the color a `d0` glyph paints in). Every failure
    /// is a silent skip, matching the caller's still-advance leniency.
    ///
    /// ISO 32000-1 §9.6.5.2: the CharProc's *first* operator is `d0`
    /// (colored) or `d1` (uncolored). A `d1` glyph "shall not specify any
    /// color" -- its own color operators are ignored and it paints in the
    /// current text fill color -- so `color_locked` is set for the nested
    /// `run` iff the first op is `Op::SetGlyphWidthBBox`. The previous lock
    /// is saved and restored around the call (not just set to `true`) so a
    /// `d0` glyph nested inside a `d1` glyph regains color control for its
    /// own subtree, while a `d1` nested inside a `d1` stays locked, and the
    /// lock never leaks into sibling or outer content.
    fn run_char_proc(
        &mut self,
        proc_obj: &Object,
        t3: &Type3Font,
        chain: &[&Dict],
        base: &GState,
        glyph_ctm: Matrix,
        depth: u32,
    ) {
        let Ok(Object::Stream(stream)) = self.doc.resolve(proc_obj) else {
            return;
        };
        let Ok(data) = self.doc.stream_data(&stream) else {
            return;
        };
        let Ok(ops) = parse_content(&data) else {
            return;
        };
        let mut inner = base.clone();
        inner.ctm = glyph_ctm;
        let mut inner_chain: Vec<&Dict> = Vec::with_capacity(chain.len() + 1);
        if let Some(d) = t3.resources() {
            inner_chain.push(d);
        }
        inner_chain.extend_from_slice(chain);
        let is_d1 = matches!(ops.first(), Some(Op::SetGlyphWidthBBox(..)));
        let saved_lock = self.color_locked;
        self.color_locked = is_d1;
        self.run(&ops, &inner_chain, inner, depth + 1);
        self.color_locked = saved_lock;
    }

    /// Dispatches color, XObject, and marked-content operators (the remainder
    /// of the [`Op`] alphabet not handled directly in `run`).
    ///
    /// Every fill/stroke color-setting arm is a no-op while
    /// `self.color_locked` (inside a `d1` Type3 CharProc, ISO 32000-1
    /// §9.6.5.2): the glyph keeps the fill/stroke color inherited from the
    /// text graphics state instead of applying its own. XObject, inline
    /// image, shading, and marked-content ops are unaffected by the lock.
    fn run_color_or_misc(&mut self, op: &Op, chain: &[&Dict], gs: &mut GState, depth: u32) {
        if self.color_locked {
            match op {
                Op::SetFillColorSpace(_)
                | Op::SetStrokeColorSpace(_)
                | Op::SetFillColor(_)
                | Op::SetStrokeColor(_)
                | Op::SetFillColorN(_, _)
                | Op::SetStrokeColorN(_, _)
                | Op::SetFillGray(_)
                | Op::SetStrokeGray(_)
                | Op::SetFillRGB(_, _, _)
                | Op::SetStrokeRGB(_, _, _)
                | Op::SetFillCMYK(_, _, _, _)
                | Op::SetStrokeCMYK(_, _, _, _) => return,
                _ => {}
            }
        }
        match op {
            Op::SetFillColorSpace(name) => {
                let (cs, pattern) = self.resolve_colorspace(name, chain);
                gs.fill_rgb = initial_color(&cs);
                gs.fill_space = cs;
                gs.fill_pattern = pattern;
            }
            Op::SetStrokeColorSpace(name) => {
                let (cs, pattern) = self.resolve_colorspace(name, chain);
                gs.stroke_rgb = initial_color(&cs);
                gs.stroke_space = cs;
                gs.stroke_pattern = pattern;
            }
            Op::SetFillColor(c) => gs.fill_rgb = gs.fill_space.to_rgb(c),
            Op::SetStrokeColor(c) => gs.stroke_rgb = gs.stroke_space.to_rgb(c),
            Op::SetFillColorN(c, pattern_name) => {
                if pattern_name.is_some() {
                    gs.fill_pattern = true;
                } else if !gs.fill_pattern {
                    gs.fill_rgb = gs.fill_space.to_rgb(c);
                }
            }
            Op::SetStrokeColorN(c, pattern_name) => {
                if pattern_name.is_some() {
                    gs.stroke_pattern = true;
                } else if !gs.stroke_pattern {
                    gs.stroke_rgb = gs.stroke_space.to_rgb(c);
                }
            }
            Op::SetFillGray(g) => {
                gs.fill_space = ColorSpace::DeviceGray;
                gs.fill_pattern = false;
                gs.fill_rgb = ColorSpace::DeviceGray.to_rgb(&[*g]);
            }
            Op::SetStrokeGray(g) => {
                gs.stroke_space = ColorSpace::DeviceGray;
                gs.stroke_pattern = false;
                gs.stroke_rgb = ColorSpace::DeviceGray.to_rgb(&[*g]);
            }
            Op::SetFillRGB(r, g, b) => {
                gs.fill_space = ColorSpace::DeviceRGB;
                gs.fill_pattern = false;
                gs.fill_rgb = ColorSpace::DeviceRGB.to_rgb(&[*r, *g, *b]);
            }
            Op::SetStrokeRGB(r, g, b) => {
                gs.stroke_space = ColorSpace::DeviceRGB;
                gs.stroke_pattern = false;
                gs.stroke_rgb = ColorSpace::DeviceRGB.to_rgb(&[*r, *g, *b]);
            }
            Op::SetFillCMYK(c, m, y, k) => {
                gs.fill_space = ColorSpace::DeviceCMYK;
                gs.fill_pattern = false;
                gs.fill_rgb = ColorSpace::DeviceCMYK.to_rgb(&[*c, *m, *y, *k]);
            }
            Op::SetStrokeCMYK(c, m, y, k) => {
                gs.stroke_space = ColorSpace::DeviceCMYK;
                gs.stroke_pattern = false;
                gs.stroke_rgb = ColorSpace::DeviceCMYK.to_rgb(&[*c, *m, *y, *k]);
            }
            Op::XObject(name) => self.do_xobject(name, chain, gs, depth),
            Op::InlineImage(img) => self.draw_inline_image(img, chain, gs),
            // Shadings are out of scope for v0.1.
            Op::Shading(_) => {}
            // Text and marked content: state-only in v0.1, nothing painted.
            _ => {}
        }
    }
}

/// The initial color after selecting a color space: black for the device
/// and Indexed spaces (CMYK black is `K = 1`). Separation/DeviceN start at
/// full tint 1.0 (ISO 32000-1 8.6.6.4/8.6.6.5), which the tint
/// approximation paints as gray 0; feeding 1.0 everywhere also gives the
/// right dark initial color for Lab (`L = 0`), the other `Other` space.
fn initial_color(cs: &ColorSpace) -> [f32; 3] {
    match cs {
        ColorSpace::DeviceCMYK => cs.to_rgb(&[0.0, 0.0, 0.0, 1.0]),
        ColorSpace::Other(_) => cs.to_rgb(&[1.0; 8]),
        _ => cs.to_rgb(&[0.0, 0.0, 0.0, 0.0]),
    }
}

impl Executor<'_> {
    /// Looks up `/category/name` in the resource chain (innermost dict
    /// first), resolving references at every step.
    fn find_res(&self, chain: &[&Dict], category: &str, name: &str) -> Option<Object> {
        for res in chain {
            let Some(cat) = res.get(category) else {
                continue;
            };
            let Ok(Object::Dict(dict)) = self.doc.resolve(cat) else {
                continue;
            };
            let Some(value) = dict.get(name) else {
                continue;
            };
            if let Ok(obj) = self.doc.resolve(value) {
                if !obj.is_null() {
                    return Some(obj);
                }
            }
        }
        None
    }

    /// Resolves a `cs`/`CS` operand: a device space name directly, the
    /// `/Pattern` space as a mid-gray flag, anything else through the
    /// `/ColorSpace` resource dictionary. Returns `(space, is_pattern)`.
    fn resolve_colorspace(&self, name: &Name, chain: &[&Dict]) -> (ColorSpace, bool) {
        match name.0.as_str() {
            "Pattern" => return (ColorSpace::DeviceGray, true),
            "DeviceGray" | "G" | "CalGray" => return (ColorSpace::DeviceGray, false),
            "DeviceRGB" | "RGB" | "CalRGB" => return (ColorSpace::DeviceRGB, false),
            "DeviceCMYK" | "CMYK" => return (ColorSpace::DeviceCMYK, false),
            _ => {}
        }
        match self.find_res(chain, "ColorSpace", &name.0) {
            Some(obj) => {
                // `[/Pattern base]` resource entries are pattern spaces too.
                if let Object::Array(items) = &obj {
                    if let Some(Object::Name(n)) = items.first() {
                        if n.0 == "Pattern" {
                            return (ColorSpace::DeviceGray, true);
                        }
                    }
                }
                (ColorSpace::parse(self.doc, &obj), false)
            }
            None => (ColorSpace::DeviceGray, false),
        }
    }

    /// Applies the `/ca /CA /LW /LC /LJ /D` entries of the named
    /// `/ExtGState` resource (other entries are ignored in v0.1).
    fn apply_ext_gstate(&self, name: &Name, chain: &[&Dict], gs: &mut GState) {
        let Some(Object::Dict(dict)) = self.find_res(chain, "ExtGState", &name.0) else {
            return;
        };
        let num = |key: &str| -> Option<f32> {
            let v = self.doc.resolve(dict.get(key)?).ok()?.as_f64()? as f32;
            v.is_finite().then_some(v)
        };
        if let Some(ca) = num("ca") {
            gs.fill_alpha = ca.clamp(0.0, 1.0);
        }
        if let Some(ca) = num("CA") {
            gs.stroke_alpha = ca.clamp(0.0, 1.0);
        }
        if let Some(lw) = num("LW") {
            if lw >= 0.0 {
                gs.line_width = lw;
            }
        }
        if let Some(lc) = num("LC") {
            gs.line_cap = lc as i32;
        }
        if let Some(lj) = num("LJ") {
            gs.line_join = lj as i32;
        }
        if let Some(Ok(Object::Array(items))) = dict.get("D").map(|o| self.doc.resolve(o)) {
            if let (Some(Ok(Object::Array(lens))), Some(phase)) = (
                items.first().map(|o| self.doc.resolve(o)),
                items
                    .get(1)
                    .and_then(|o| self.doc.resolve(o).ok()?.as_f64()),
            ) {
                let dash: Vec<f32> = lens.iter().filter_map(|o| num_f32(self.doc, o)).collect();
                if dash.len() == lens.len() && (phase as f32).is_finite() {
                    gs.dash = dash;
                    gs.dash_phase = phase as f32;
                }
            }
        }
    }
}

/// Resolves an object to a finite `f32`.
fn num_f32(doc: &Document, obj: &Object) -> Option<f32> {
    let v = doc.resolve(obj).ok()?.as_f64()? as f32;
    v.is_finite().then_some(v)
}

/// Reads the first `n` finite numbers of a (possibly indirect) array.
fn floats_from(doc: &Document, obj: Option<&Object>, n: usize) -> Option<Vec<f32>> {
    let arr = match doc.resolve(obj?) {
        Ok(Object::Array(a)) if a.len() >= n => a,
        _ => return None,
    };
    let out: Vec<f32> = arr.iter().take(n).filter_map(|o| num_f32(doc, o)).collect();
    (out.len() == n).then_some(out)
}

impl Executor<'_> {
    /// Executes `Do`: draws an image XObject or recurses into a form.
    fn do_xobject(&mut self, name: &Name, chain: &[&Dict], gs: &GState, depth: u32) {
        let Some(Object::Stream(stream)) = self.find_res(chain, "XObject", &name.0) else {
            return;
        };
        match stream.dict.get_name("Subtype").map(|n| n.0.as_str()) {
            Some("Image") => self.draw_image_xobject(&stream, chain, gs),
            Some("Form") => self.run_form(&stream, chain, gs, depth),
            _ => {}
        }
    }

    /// Runs a form XObject: `/Matrix` concatenated before the CTM, `/BBox`
    /// intersected into the clip, own `/Resources` prepended to the chain,
    /// bounded recursion.
    fn run_form(&mut self, stream: &Stream, chain: &[&Dict], gs: &GState, depth: u32) {
        if depth >= MAX_FORM_DEPTH {
            return;
        }
        let Ok(data) = self.doc.stream_data(stream) else {
            return;
        };
        let Ok(ops) = parse_content(&data) else {
            return;
        };
        let mut inner = gs.clone();
        if let Some(m) = floats_from(self.doc, stream.dict.get("Matrix"), 6) {
            let matrix = Matrix {
                a: m[0],
                b: m[1],
                c: m[2],
                d: m[3],
                e: m[4],
                f: m[5],
            };
            inner.ctm = matrix.concat(inner.ctm);
        }
        if let Some(b) = floats_from(self.doc, stream.dict.get("BBox"), 4) {
            let (x0, x1) = (b[0].min(b[2]), b[0].max(b[2]));
            let (y0, y1) = (b[1].min(b[3]), b[1].max(b[3]));
            let mut pb = PathBuilder::new(inner.ctm);
            pb.rect(x0, y0, x1 - x0, y1 - y0);
            let mut mask = Mask::from_path(
                self.pix.width,
                self.pix.height,
                &pb.finish(),
                FillRule::NonZero,
            );
            if let Some(old) = &inner.clip {
                mask.intersect(old);
            }
            inner.clip = Some(Rc::new(mask));
        }
        let own_res = match stream.dict.get("Resources").map(|o| self.doc.resolve(o)) {
            Some(Ok(Object::Dict(d))) => Some(d),
            _ => None,
        };
        let mut inner_chain: Vec<&Dict> = Vec::with_capacity(chain.len() + 1);
        if let Some(d) = &own_res {
            inner_chain.push(d);
        }
        inner_chain.extend_from_slice(chain);
        self.run(&ops, &inner_chain, inner, depth + 1);
    }

    /// Draws an image XObject with the current CTM/clip/alpha; the fill
    /// color paints through `/ImageMask` stencils.
    fn draw_image_xobject(&mut self, stream: &Stream, chain: &[&Dict], gs: &GState) {
        let Ok(data) = self.doc.stream_data(stream) else {
            return;
        };
        let cs_obj = self.image_colorspace(&stream.dict, chain);
        self.blit_image(&stream.dict, &data, cs_obj, gs);
    }

    /// Draws an inline image: its filters (abbreviations included) are
    /// applied here, then it follows the XObject path.
    fn draw_inline_image(&mut self, img: &ImageParams, chain: &[&Dict], gs: &GState) {
        let stream = Stream {
            dict: img.dict.clone(),
            data: img.data.clone(),
        };
        let Ok(data) = decode_stream(&stream, self.doc) else {
            return;
        };
        let cs_obj = self.image_colorspace(&img.dict, chain);
        self.blit_image(&img.dict, &data, cs_obj, gs);
    }

    fn blit_image(&mut self, dict: &Dict, data: &[u8], cs_obj: Option<Object>, gs: &GState) {
        let fill = gs.fill_rgba8();
        image::draw(
            self.doc,
            &mut self.pix,
            dict,
            data,
            cs_obj.as_ref(),
            &DrawParams {
                ctm: gs.ctm,
                alpha: gs.fill_alpha,
                fill_rgb: [fill[0], fill[1], fill[2]],
                clip: gs.clip.as_deref(),
            },
        );
    }

    /// The image's `/ColorSpace` value with resource-name indirection
    /// resolved: a non-device name is looked up in `/ColorSpace` resources.
    fn image_colorspace(&self, dict: &Dict, chain: &[&Dict]) -> Option<Object> {
        let resolved = self.doc.resolve(dict.get("ColorSpace")?).ok()?;
        if let Object::Name(n) = &resolved {
            let device = matches!(
                n.0.as_str(),
                "DeviceGray" | "DeviceRGB" | "DeviceCMYK" | "G" | "RGB" | "CMYK"
            );
            if !device {
                if let Some(from_res) = self.find_res(chain, "ColorSpace", &n.0) {
                    return Some(from_res);
                }
            }
        }
        Some(resolved)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{GlyphPainting, RenderOptions};
    use pdfboss_testkit::{doc_with_graphics, PdfBuilder};

    #[test]
    fn render_options_default_is_all_embedded() {
        assert_eq!(
            RenderOptions::default().glyph_painting,
            GlyphPainting::AllEmbedded
        );
    }

    #[test]
    fn all_glyph_tiers_match_default_render_today() {
        // The content stream is a raw filled rectangle with no font at all,
        // so no glyph loading happens at any tier -- the render is
        // tier-invariant by construction, regardless of which loaders exist.
        let bytes = small_doc("", b"1 0 0 rg 10 10 80 80 re f", |_| {});
        let doc = Document::load(bytes).expect("load");
        let page = doc.page(0).expect("page");
        let base =
            render_page_with_options(&doc, &page, 1.0, &RenderOptions::default()).expect("render");
        for tier in [
            GlyphPainting::EmbeddedTrueTypeOnly,
            GlyphPainting::AllEmbedded,
            GlyphPainting::Full,
        ] {
            let opts = RenderOptions {
                glyph_painting: tier,
            };
            let got = render_page_with_options(&doc, &page, 1.0, &opts).expect("render");
            assert_eq!(got, base, "tier {tier:?} differs from default render");
        }
    }

    /// Renders page 0 of `bytes` at `scale`.
    fn render(bytes: Vec<u8>, scale: f32) -> Pixmap {
        let doc = Document::load(bytes).expect("load");
        let page = doc.page(0).expect("page");
        render_page_with_options(&doc, &page, scale, &RenderOptions::default()).expect("render")
    }

    fn px(pix: &Pixmap, x: u32, y: u32) -> [u8; 4] {
        let off = ((y * pix.width + x) * 4) as usize;
        pix.data[off..off + 4].try_into().unwrap()
    }

    const WHITE: [u8; 4] = [255, 255, 255, 255];
    const RED: [u8; 4] = [255, 0, 0, 255];
    const BLACK: [u8; 4] = [0, 0, 0, 255];

    /// A one-page 100x100 document with the given content and resources.
    fn small_doc(resources: &str, content: &[u8], extra: impl FnOnce(&mut PdfBuilder)) -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            &format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
                 /Resources << {resources} >> /Contents 4 0 R >>"
            ),
        );
        b.stream(4, "", content);
        extra(&mut b);
        b.build(1)
    }

    #[test]
    fn red_rect_fills_at_yflipped_device_location() {
        // 612x792 page; user rect [100,300]x[100,250] -> device rows
        // [542,692] after the y-flip.
        let pix = render(doc_with_graphics("1 0 0 rg 100 100 200 150 re f"), 1.0);
        assert_eq!((pix.width, pix.height), (612, 792));
        assert_eq!(px(&pix, 200, 600), RED, "interior");
        assert_eq!(px(&pix, 101, 543), RED, "top-left corner inside");
        assert_eq!(px(&pix, 298, 690), RED, "bottom-right corner inside");
        assert_eq!(px(&pix, 200, 530), WHITE, "above rect (device)");
        assert_eq!(px(&pix, 200, 700), WHITE, "below rect (device)");
        assert_eq!(px(&pix, 95, 600), WHITE, "left of rect");
        assert_eq!(px(&pix, 305, 600), WHITE, "right of rect");
        assert_eq!(
            px(&pix, 200, 100),
            WHITE,
            "user-space y kept would paint here"
        );
    }

    #[test]
    fn clip_limits_full_page_fill() {
        let content = "20 20 40 40 re W n 0 0 612 792 re f";
        let pix = render(doc_with_graphics(content), 1.0);
        // Clip rect [20,60]^2 user -> device rows [732,772].
        assert_eq!(px(&pix, 40, 750), BLACK, "inside clip");
        assert_eq!(px(&pix, 40, 700), WHITE, "above clip");
        assert_eq!(px(&pix, 70, 750), WHITE, "right of clip");
        assert_eq!(px(&pix, 300, 400), WHITE, "page center untouched");
    }

    #[test]
    fn cm_translate_scale_moves_rect() {
        let content = "1 0 0 rg q 2 0 0 2 50 30 cm 10 10 20 20 re f Q";
        let pix = render(doc_with_graphics(content), 1.0);
        // User rect [10,30]^2 through cm -> [70,110]x[50,90] -> device
        // rows [702,742].
        assert_eq!(px(&pix, 90, 720), RED, "transformed interior");
        assert_eq!(px(&pix, 60, 720), WHITE, "left of transformed rect");
        assert_eq!(px(&pix, 90, 750), WHITE, "below transformed rect");
        assert_eq!(px(&pix, 20, 770), WHITE, "untransformed location clear");
    }

    #[test]
    fn q_restore_resets_color_and_nonfinite_cm_is_skipped() {
        let content = "1 0 0 rg q 0 1 0 rg Q 10 10 20 20 re f";
        let pix = render(doc_with_graphics(content), 1.0);
        assert_eq!(px(&pix, 20, 770), RED, "Q restored the red fill");

        // 1e39 overflows f32 -> non-finite cm must be skipped entirely.
        let content = "1e39 0 0 1e39 0 0 cm 1 0 0 rg 10 10 20 20 re f";
        let pix = render(doc_with_graphics(content), 1.0);
        assert_eq!(px(&pix, 20, 770), RED, "rect painted with identity ctm");
    }

    #[test]
    fn extgstate_ca_blends_toward_white() {
        let bytes = small_doc(
            "/ExtGState << /G1 5 0 R >>",
            b"/G1 gs 1 0 0 rg 0 0 100 100 re f",
            |b| {
                b.object(5, "<< /Type /ExtGState /ca 0.5 >>");
            },
        );
        let pix = render(bytes, 1.0);
        let [r, g, b, a] = px(&pix, 50, 50);
        assert_eq!(r, 255);
        assert!((127..=129).contains(&g), "green {g}");
        assert!((127..=129).contains(&b), "blue {b}");
        assert_eq!(a, 255);
    }

    #[test]
    fn stroke_width_scales_with_ctm() {
        // 4x CTM scale turns a 1pt pen into a ~4px device band; the line
        // at user y=20 lands on device row 792 - 80 = 712.
        let content = "4 0 0 4 0 0 cm 1 w 10 20 m 140 20 l S";
        let pix = render(doc_with_graphics(content), 1.0);
        let dark = (700..725).filter(|&y| px(&pix, 300, y)[0] < 128).count();
        assert!((3..=5).contains(&dark), "band thickness {dark}");

        // Unscaled 1pt pen: ~1px of ink, possibly split across two rows
        // as 50% coverage each.
        let pix = render(doc_with_graphics("1 w 10 80 m 560 80 l S"), 1.0);
        let inked = (700..725).filter(|&y| px(&pix, 300, y)[0] < 200).count();
        assert!((1..=2).contains(&inked), "hairline thickness {inked}");
    }

    #[test]
    fn dashed_stroke_leaves_gaps() {
        let content = "2 w [6 6] 0 d 10 50 m 90 50 l S";
        let pix = render(small_doc("", content.as_bytes(), |_| {}), 1.0);
        assert_eq!((pix.width, pix.height), (100, 100));
        let mut runs = 0;
        let mut prev_on = false;
        for x in 0..100 {
            let on = px(&pix, x, 50)[0] < 128;
            if on && !prev_on {
                runs += 1;
            }
            prev_on = on;
        }
        assert!(runs >= 4, "expected several dash runs, got {runs}");
    }

    #[test]
    fn separation_and_devicen_initial_color_is_full_tint() {
        // ISO 32000-1 8.6.6.4/8.6.6.5: selecting a Separation or DeviceN
        // space with `cs` sets every component to 1.0, so painting before
        // any `scn` must give a full-tint (dark) mark, not white.
        for (entry, content) in [
            // Fill: broken initial color paints white-on-white.
            (
                "[/Separation /Spot /DeviceGray 5 0 R]",
                "/T cs 10 10 80 80 re f",
            ),
            (
                "[/DeviceN [/A /B] /DeviceGray 5 0 R]",
                "/T cs 10 10 80 80 re f",
            ),
            // Stroke: a thick line through the page center.
            (
                "[/Separation /Spot /DeviceGray 5 0 R]",
                "/T CS 20 w 10 50 m 90 50 l S",
            ),
        ] {
            let bytes = small_doc("/ColorSpace << /T 6 0 R >>", content.as_bytes(), |b| {
                b.object(5, "<< /FunctionType 2 /Domain [0 1] /N 1 >>");
                b.object(6, entry);
            });
            let pix = render(bytes, 1.0);
            assert_eq!(px(&pix, 50, 50), BLACK, "{entry} via `{content}`");
        }
        // An explicit `0 scn` still overrides the initial color to white.
        let bytes = small_doc(
            "/ColorSpace << /T 6 0 R >>",
            b"/T cs 0 scn 10 10 80 80 re f",
            |b| {
                b.object(5, "<< /FunctionType 2 /Domain [0 1] /N 1 >>");
                b.object(6, "[/Separation /Spot /DeviceGray 5 0 R]");
            },
        );
        assert_eq!(px(&render(bytes, 1.0), 50, 50), WHITE, "0 scn wins");
    }

    #[test]
    fn form_xobject_matrix_paints_displaced() {
        let bytes = small_doc("/XObject << /Fm1 5 0 R >>", b"/Fm1 Do", |b| {
            b.stream(
                5,
                "/Type /XObject /Subtype /Form /BBox [0 0 50 50] \
                     /Matrix [1 0 0 1 20 30]",
                b"1 0 0 rg 0 0 50 50 re f",
            );
        });
        let pix = render(bytes, 1.0);
        // Form square [0,50]^2 shifted to [20,70]x[30,80] user -> device
        // rows [20,70].
        assert_eq!(px(&pix, 40, 50), RED, "displaced interior");
        assert_eq!(px(&pix, 10, 50), WHITE, "left of form");
        assert_eq!(px(&pix, 40, 80), WHITE, "below form");
        assert_eq!(px(&pix, 40, 10), WHITE, "above form");
    }

    #[test]
    fn form_bbox_clips_its_content() {
        let bytes = small_doc("/XObject << /Fm1 5 0 R >>", b"/Fm1 Do", |b| {
            // Content paints [0,80]^2 but the BBox stops it at 40.
            b.stream(
                5,
                "/Type /XObject /Subtype /Form /BBox [0 0 40 40]",
                b"1 0 0 rg 0 0 80 80 re f",
            );
        });
        let pix = render(bytes, 1.0);
        assert_eq!(px(&pix, 20, 80), RED, "inside bbox (device)");
        assert_eq!(px(&pix, 60, 40), WHITE, "outside bbox");
    }

    #[test]
    fn inline_image_blits_quadrant_colors() {
        // 2x2 RGB hex image over the unit square [25,75]^2 (user): row 0
        // (red, green) lands on top in device space, row 1 (blue, white)
        // below.
        let content = "q 50 0 0 50 25 25 cm \
                       BI /W 2 /H 2 /CS /RGB /BPC 8 /F /AHx ID \
                       ff0000 00ff00 0000ff ffffff> EI Q";
        let pix = render(small_doc("", content.as_bytes(), |_| {}), 1.0);
        assert_eq!(px(&pix, 35, 35), RED, "top-left quadrant");
        assert_eq!(px(&pix, 65, 35), [0, 255, 0, 255], "top-right quadrant");
        assert_eq!(px(&pix, 35, 65), [0, 0, 255, 255], "bottom-left quadrant");
        assert_eq!(px(&pix, 65, 65), WHITE, "bottom-right quadrant");
        assert_eq!(px(&pix, 10, 50), WHITE, "outside image");
    }

    #[test]
    fn image_mask_stencils_fill_color() {
        // Rows: 0b01 (paint, skip) / 0b10 (skip, paint).
        let bytes = small_doc(
            "/XObject << /Im1 5 0 R >>",
            b"0 0 1 rg q 100 0 0 100 0 0 cm /Im1 Do Q",
            |b| {
                b.stream(
                    5,
                    "/Type /XObject /Subtype /Image /Width 2 /Height 2 \
                     /ImageMask true /BitsPerComponent 1",
                    &[0x40, 0x80],
                );
            },
        );
        let pix = render(bytes, 1.0);
        let blue = [0, 0, 255, 255];
        assert_eq!(px(&pix, 25, 25), blue, "row 0 sample 0 painted");
        assert_eq!(px(&pix, 75, 25), WHITE, "row 0 sample 1 clear");
        assert_eq!(px(&pix, 25, 75), WHITE, "row 1 sample 0 clear");
        assert_eq!(px(&pix, 75, 75), blue, "row 1 sample 1 painted");
    }

    #[test]
    fn image_mask_decode_inverts_stencil() {
        let bytes = small_doc(
            "/XObject << /Im1 5 0 R >>",
            b"0 0 1 rg q 100 0 0 100 0 0 cm /Im1 Do Q",
            |b| {
                b.stream(
                    5,
                    "/Type /XObject /Subtype /Image /Width 2 /Height 2 \
                     /ImageMask true /BitsPerComponent 1 /Decode [1 0]",
                    &[0x40, 0x80],
                );
            },
        );
        let pix = render(bytes, 1.0);
        let blue = [0, 0, 255, 255];
        assert_eq!(px(&pix, 25, 25), WHITE, "inverted: row 0 sample 0 clear");
        assert_eq!(px(&pix, 75, 25), blue, "inverted: row 0 sample 1 painted");
        assert_eq!(px(&pix, 25, 75), blue, "inverted: row 1 sample 0 painted");
        assert_eq!(px(&pix, 75, 75), WHITE, "inverted: row 1 sample 1 clear");
    }

    #[test]
    fn rotate_90_swaps_dimensions_and_spins_content() {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 200] \
             /Rotate 90 /Contents 4 0 R >>",
        );
        b.stream(4, "", b"1 0 0 rg 0 0 10 10 re f");
        let pix = render(b.build(1), 1.0);
        assert_eq!((pix.width, pix.height), (200, 100));
        // The page's bottom-left corner rect appears top-left after the
        // clockwise rotation.
        assert_eq!(px(&pix, 5, 5), RED, "rotated corner");
        assert_eq!(px(&pix, 5, 94), WHITE, "old corner clear");
        assert_eq!(px(&pix, 194, 94), WHITE);
    }

    #[test]
    fn scale_doubles_pixel_size_and_coordinates() {
        let content = "1 0 0 rg 10 10 20 20 re f";
        let pix = render(small_doc("", content.as_bytes(), |_| {}), 2.0);
        assert_eq!((pix.width, pix.height), (200, 200));
        // User rect [10,30]^2 -> device [20,60]x[140,180] at 2x.
        assert_eq!(px(&pix, 40, 160), RED, "scaled interior");
        assert_eq!(px(&pix, 40, 120), WHITE, "above scaled rect");
        assert_eq!(px(&pix, 80, 160), WHITE, "right of scaled rect");
    }

    fn fixture(name: &str) -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../tests/fixtures")
            .join(name)
    }

    #[test]
    fn shapes_fixture_renders_expected_colors() {
        let doc = Document::open(fixture("shapes.pdf")).expect("open");
        let page = doc.page(0).expect("page");
        let pix =
            render_page_with_options(&doc, &page, 1.0, &RenderOptions::default()).expect("render");
        assert_eq!((pix.width, pix.height), (612, 792));
        assert!(
            pix.data.chunks_exact(4).any(|p| p[0] != 255 || p[1] != 255),
            "page must contain non-white pixels"
        );
        // 1 0 0 rg 72 600 100 80 re -> device rows [112,192].
        assert_eq!(px(&pix, 100, 150), RED, "red rect");
        // 0 0.5 1 rg 200 600 120 60 re -> device rows [132,192].
        let [r, g, b, _] = px(&pix, 250, 150);
        assert_eq!((r, b), (0, 255), "blue-ish rect r/b");
        assert!((127..=129).contains(&g), "blue-ish rect g {g}");
        // 0.2 0.8 0.2 rg 340 590 90 90 re -> device rows [112,202].
        assert_eq!(px(&pix, 380, 150), [51, 204, 51, 255], "green rect");
        // q 0.5 0 0 0.5 300 100 cm 0.8 0 0.8 rg 0 0 200 200 re f Q ->
        // user [300,400]x[100,200] -> device rows [592,692].
        assert_eq!(px(&pix, 350, 650), [204, 0, 204, 255], "magenta rect");
        // Black 2pt Bezier stroke passes (200, 417) in device space.
        let dark = (410..425).any(|y| px(&pix, 200, y)[0] < 128);
        assert!(dark, "stroked curve missing");
        // Unpainted margin stays white.
        assert_eq!(px(&pix, 550, 750), WHITE);
    }

    #[test]
    fn hello_fixture_renders_all_white_without_error() {
        // Text is tracked but not painted in v0.1, so the page stays white.
        let doc = Document::open(fixture("hello.pdf")).expect("open");
        let page = doc.page(0).expect("page");
        let pix =
            render_page_with_options(&doc, &page, 1.0, &RenderOptions::default()).expect("render");
        assert_eq!((pix.width, pix.height), (612, 792));
        assert!(pix.data.iter().all(|&b| b == 255), "expected a white page");
    }

    #[test]
    fn even_odd_fill_and_close_fill_stroke() {
        // f* with two same-winding squares leaves an even-odd hole.
        let content = "1 0 0 rg 10 10 80 80 re 30 30 40 40 re f*";
        let pix = render(small_doc("", content.as_bytes(), |_| {}), 1.0);
        assert_eq!(px(&pix, 50, 50), WHITE, "even-odd hole");
        assert_eq!(px(&pix, 15, 50), RED, "ring");

        // b closes the open triangle, fills it, and strokes the closing
        // edge from (80,10) back to (20,10) -> device row ~90.
        let content = "1 0 0 rg 0 0 0 RG 2 w 20 10 m 80 10 l 50 60 l b";
        let pix = render(small_doc("", content.as_bytes(), |_| {}), 1.0);
        assert_eq!(px(&pix, 50, 70), RED, "triangle interior filled");
        assert!(px(&pix, 50, 90)[0] < 128, "closing edge stroked");
    }

    // --- Type3 glyph painting (re-entering the executor per CharProc) --------
    //
    // Geometry matches the shared box-glyph tests: a 200x200 page, 100pt font,
    // text origin (20,50), CharProc `100 0 500 700 re f` (the (100,0)-(600,700)
    // box in glyph space) under `/FontMatrix [0.001 ...]`. That lands the same
    // interior dark pixel at (55,115); an 800-glyph-unit advance puts a second
    // glyph's interior at (135,115).

    /// Renders page 0 of `bytes` at the given glyph-painting tier.
    fn render_at_tier(bytes: &[u8], tier: GlyphPainting) -> Pixmap {
        let doc = Document::load(bytes.to_vec()).expect("load");
        let page = doc.page(0).expect("page");
        let opts = RenderOptions {
            glyph_painting: tier,
        };
        render_page_with_options(&doc, &page, 1.0, &opts).expect("render")
    }

    /// True iff the pixel at `(x, y)` is dark on all three channels.
    fn dark_at(pix: &Pixmap, x: u32, y: u32) -> bool {
        let o = ((y * pix.width + x) * 4) as usize;
        pix.data[o] < 128 && pix.data[o + 1] < 128 && pix.data[o + 2] < 128
    }

    /// Builds a one-page 200x200 doc showing a `/Type3` font (object 5) whose
    /// `/boxglyph` CharProc (object 6) is `charproc`. `font_extra` is spliced
    /// into the font dict (e.g. `/FirstChar`+`/Widths`); `char_res` optionally
    /// gives the CharProc stream its own `/Resources` (for self-reference).
    /// Code 65 maps to `/boxglyph` via `/Differences`.
    fn type3_doc(
        charproc: &str,
        font_extra: &str,
        char_res: Option<&str>,
        content: &[u8],
    ) -> Vec<u8> {
        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", content);
        b.object(
            5,
            &format!(
                "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
                 /FontMatrix [0.001 0 0 0.001 0 0] \
                 /Encoding << /Differences [65 /boxglyph] >> \
                 /CharProcs << /boxglyph 6 0 R >> {font_extra} >>"
            ),
        );
        b.stream(6, char_res.unwrap_or(""), charproc.as_bytes());
        b.build(1)
    }

    /// A Type3 fixture whose `charproc` paints under `/FirstChar 65 /Widths
    /// [1000]`.
    fn type3_page_doc(charproc: &str, content: &[u8]) -> Vec<u8> {
        type3_doc(charproc, "/FirstChar 65 /Widths [1000]", None, content)
    }

    /// A Type3 fixture painting the standard box glyph with the given
    /// glyph-space `/Widths` entry for code 65.
    fn type3_page_doc_widths(width: i32, content: &[u8]) -> Vec<u8> {
        type3_doc(
            "1000 0 d0 100 0 500 700 re f",
            &format!("/FirstChar 65 /Widths [{width}]"),
            None,
            content,
        )
    }

    /// A Type3 fixture whose CharProc paints the box AND shows code 65 in the
    /// same font (via its own `/Resources /F0` pointing back at the font) --
    /// self-referential, so it must be depth-bounded.
    fn type3_recursive_doc() -> Vec<u8> {
        type3_doc(
            "1000 0 d0 100 0 500 700 re f BT /F0 100 Tf <41> Tj ET",
            "/FirstChar 65 /Widths [1000]",
            Some("/Resources << /Font << /F0 5 0 R >> >>"),
            b"BT /F0 100 Tf 20 50 Td <41> Tj ET",
        )
    }

    #[test]
    fn type3_glyph_paints_at_all_embedded_not_embedded_truetype_only() {
        let doc = type3_page_doc(
            "1000 0 d0 100 0 500 700 re f",
            b"BT /F0 100 Tf 20 50 Td <41> Tj ET", // code 65 -> /boxglyph
        );
        for tier in [GlyphPainting::AllEmbedded, GlyphPainting::Full] {
            let pix = render_at_tier(&doc, tier);
            assert!(
                dark_at(&pix, 55, 115),
                "Type3 glyph should paint at {tier:?}"
            );
        }
        let pix = render_at_tier(&doc, GlyphPainting::EmbeddedTrueTypeOnly);
        assert!(
            !dark_at(&pix, 55, 115),
            "Type3 must not paint at EmbeddedTrueTypeOnly"
        );
    }

    #[test]
    fn type3_self_referential_glyph_terminates() {
        let doc = type3_recursive_doc();
        let started = std::time::Instant::now();
        let pix = render_at_tier(&doc, GlyphPainting::AllEmbedded);
        assert!(
            started.elapsed() < std::time::Duration::from_secs(5),
            "self-referential Type3 must be depth-bounded, not hang/overflow"
        );
        assert!(dark_at(&pix, 55, 115), "the box still paints");
    }

    #[test]
    fn type3_width_governs_second_glyph_origin() {
        let doc = type3_page_doc_widths(800, b"BT /F0 100 Tf 20 50 Td <4141> Tj ET");
        let pix = render_at_tier(&doc, GlyphPainting::AllEmbedded);
        assert!(dark_at(&pix, 55, 115), "first glyph at (55,115)");
        assert!(
            dark_at(&pix, 135, 115),
            "second glyph at the /Widths-implied (135,115)"
        );
    }

    #[test]
    fn type3_d1_glyph_ignores_its_own_color_and_uses_text_fill() {
        // Page sets fill RED before the text; the d1 CharProc tries to set blue.
        // d1 is uncolored: the box must paint RED.
        let doc = type3_page_doc(
            "1000 0 0 0 1000 1000 d1 0 0 1 rg 100 0 500 700 re f",
            b"1 0 0 rg BT /F0 100 Tf 20 50 Td <41> Tj ET",
        );
        let pix = render_at_tier(&doc, GlyphPainting::AllEmbedded);
        let [r, g, b, _] = px(&pix, 55, 115);
        assert!(
            r > 200 && g < 60 && b < 60,
            "d1 glyph paints in the text fill (red), got {r},{g},{b}"
        );
    }

    #[test]
    fn type3_d0_glyph_honors_its_own_color() {
        // d0 is colored: the CharProc's blue takes effect despite red text fill.
        let doc = type3_page_doc(
            "1000 0 d0 0 0 1 rg 100 0 500 700 re f",
            b"1 0 0 rg BT /F0 100 Tf 20 50 Td <41> Tj ET",
        );
        let pix = render_at_tier(&doc, GlyphPainting::AllEmbedded);
        let [r, g, b, _] = px(&pix, 55, 115);
        assert!(
            b > 200 && r < 60 && g < 60,
            "d0 glyph paints its own color (blue), got {r},{g},{b}"
        );
    }

    #[test]
    fn type3_d0_nested_in_d1_regains_color() {
        // ISO 32000-1 9.6.5.2: a `d1` (uncolored) CharProc must not apply its
        // own color -- it paints in the inherited text fill (red here). But
        // that lock must not leak into a `d0` (colored) CharProc shown *from
        // inside* the `d1` glyph (a Type3 font showing itself, ISO 32000-1
        // 9.6.5): the nested `d0` must regain full color control and paint
        // its own color (blue), because the lock is saved/restored per
        // CharProc frame and set to `is_d1`, not hardcoded on.
        //
        // Geometry: FontMatrix [0.001 0 0 0.001 0 0], 100pt /Tf, page
        // 200x200 -- the shared box-glyph setup used throughout this module.
        // The outer `d1` box "100 0 500 700 re f" lands at device (55,115),
        // same as the other d0/d1 tests above. Its content then shows the
        // nested `d0` glyph via its own BT/Tf/Td/Tj at glyph-space offset
        // (800, 0); working through the nested glyph matrix, that glyph's
        // (deliberately larger, to stay easily samplable) box
        // "1000 0 2000 3000 re f" lands at device x in [110,130], y in
        // [120,150] -- disjoint in x from the outer box's [30,80], so the
        // two boxes cannot overlap on the device.
        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );
        // Red text fill, then show code 65 -> the outer `d1` glyph.
        b.stream(4, "", b"1 0 0 rg BT /F0 100 Tf 20 50 Td <41> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
             /FontMatrix [0.001 0 0 0.001 0 0] \
             /Encoding << /Differences [65 /d1glyph 66 /d0glyph] >> \
             /CharProcs << /d1glyph 6 0 R /d0glyph 7 0 R >> \
             /FirstChar 65 /Widths [1000 1000] >>",
        );
        // d1 (uncolored): tries blue on its own box (must be suppressed and
        // paint red instead), then shows the nested d0 glyph (code 66),
        // which must regain color control for its own subtree.
        b.stream(
            6,
            "",
            b"1000 0 0 0 1000 1000 d1 0 0 1 rg 100 0 500 700 re f \
              BT /F0 100 Tf 800 0 Td <42> Tj ET",
        );
        // d0 (colored): paints its own blue, at a glyph-space box that maps
        // to a device location disjoint from the outer one.
        b.stream(7, "", b"1000 0 d0 0 0 1 rg 1000 0 2000 3000 re f");
        let pix = render_at_tier(&b.build(1), GlyphPainting::AllEmbedded);

        let [r, g, bch, _] = px(&pix, 55, 115);
        assert!(
            r > 200 && g < 60 && bch < 60,
            "outer d1 box must paint the inherited text fill (red), got {r},{g},{bch}"
        );
        let [r, g, bch, _] = px(&pix, 120, 135);
        assert!(
            bch > 200 && r < 60 && g < 60,
            "nested d0 box must regain its own color (blue), got {r},{g},{bch}"
        );
    }
}
