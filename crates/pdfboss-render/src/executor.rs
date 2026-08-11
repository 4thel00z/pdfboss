//! Content-op execution against a graphics state stack: transforms, colors,
//! clipping, ExtGState, form XObject recursion, and paint dispatch.
//!
//! Limitations (v0.1): only embedded-TrueType glyph outlines are painted
//! (other fonts are positioned but not drawn); `sh` shadings are skipped;
//! pattern fills paint mid-gray; masks and blend modes are ignored;
//! annotation appearance streams are not drawn. Everything on that list
//! except the glyph tiers -- which the caller chooses -- is recorded in the
//! [`RenderReport`] this module returns, along with every content stream and
//! image leniency drops, so no caller is handed a blank page it cannot
//! account for.

use pdfboss_core::FastMap;
use std::sync::Arc;

use pdfboss_core::content::{parse_content, ImageParams, Op, TextItem};
use pdfboss_core::geom::{Matrix, Point};
use pdfboss_core::{
    block_on, content_stream_data_with, page_content_with, AsyncObjectSource, Dict, Document,
    Error, Immediate, Name, Object, Page, Result, Stream,
};

use crate::color::{self, ColorSpace};
use crate::glyph::GlyphFont;
use crate::image::{self, DrawParams};
use crate::path::{PathBuilder, Subpath};
use crate::raster::{fill_path, BlendMode, FillRule, Mask};
use crate::stroke::stroke_path;
#[cfg(feature = "substitute-fonts")]
use crate::substitute::BuiltinProvider;
use crate::substitute::{DirProvider, SubstituteProvider};
use crate::type3::Type3Font;
use crate::{
    GlyphPainting, Pixmap, RenderOptions, RenderReport, SkipReason, SkippedKind, SubstituteSource,
};

/// Maximum `q`/`Q` nesting depth.
const MAX_GSTATE_DEPTH: usize = 64;
/// Maximum form XObject recursion depth.
const MAX_FORM_DEPTH: u32 = 16;
/// Maximum pixmap side length, guarding malformed boxes and huge scales.
const MAX_SIDE: f32 = 16384.0;
/// Bound on `Executor::clip_cache`'s size: many real documents repeat the
/// exact same clip path (often a page-bounds "reset" rect) hundreds of times
/// per page, which used to re-rasterize it from scratch every time. Capped
/// like `GlyphFont`'s `flat_cache` so a pathological stream minting endless
/// distinct clip paths can't grow this unboundedly.
const MAX_CLIP_CACHE: usize = 256;

/// Identifies a clip path by its exact flattened (device-space) geometry and
/// fill rule, so an identical clip repeated later in the same page reuses
/// its rasterized [`Mask`] instead of rebuilding it. `f32` coordinates are
/// compared by bit pattern (exact match only — this is a cache key, not a
/// geometric equivalence test, so two paths that are merely numerically
/// close still miss and just re-rasterize).
#[derive(PartialEq, Eq, Hash, Clone)]
struct ClipKey {
    even_odd: bool,
    subpaths: Vec<(bool, Vec<(u32, u32)>)>,
}

impl ClipKey {
    fn new(polys: &[Subpath], rule: FillRule) -> ClipKey {
        ClipKey {
            even_odd: rule == FillRule::EvenOdd,
            subpaths: polys
                .iter()
                .map(|s| {
                    (
                        s.closed,
                        s.points
                            .iter()
                            .map(|p| (p.x.to_bits(), p.y.to_bits()))
                            .collect(),
                    )
                })
                .collect(),
        }
    }
}

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
    /// Active blend mode (`/BM`); the separable modes paint, see
    /// [`BlendMode`].
    blend_mode: BlendMode,
    /// Constant fill alpha (`ca`).
    fill_alpha: f32,
    /// Constant stroke alpha (`CA`).
    stroke_alpha: f32,
    /// Active clip as a device-space coverage mask. Shared behind an `Arc` so
    /// that saving state (`q`) and entering a form clone the graphics state
    /// without copying the full-page mask buffer; a new clip always builds a
    /// fresh `Mask`, so this is effectively clone-on-write.
    clip: Option<Arc<Mask>>,
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
            blend_mode: BlendMode::default(),
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

/// A loaded, paintable outline font paired with the name report entries
/// know it by — its `/BaseFont`, or the `Tf` resource name when the
/// dictionary has none — or `None` for a font whose glyphs cannot be drawn
/// (the [`crate::GlyphPainting`] tier, or a load failure).
type LoadedFont = Option<(Arc<GlyphFont>, Arc<str>)>;

/// Text-showing state within a `BT`/`ET` block. Held per content stream (not
/// saved by `q`/`Q`), matching how the extractor tracks text.
struct TextState {
    /// Text matrix and line matrix.
    tm: Matrix,
    tlm: Matrix,
    font: LoadedFont,
    /// A `/Type3` font whose glyphs paint by re-entering the executor per
    /// CharProc (ISO 32000-1 §9.6.5). Invariant: at most one of `font`
    /// (outline) / `type3` is `Some`.
    type3: Option<Arc<Type3Font>>,
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
/// Content errors are lenient: an unreadable stream renders blank. The
/// returned [`RenderReport`] names everything that leniency dropped or
/// approximated, so a caller can tell a blank page from an unreadable one.
pub(crate) fn render_page_reporting(
    doc: &Document,
    page: &Page,
    scale: f32,
    opts: &RenderOptions,
) -> Result<(Pixmap, RenderReport)> {
    block_on(render_page_reporting_with(
        Immediate(doc),
        page,
        scale,
        opts,
    ))
}

/// [`render_page_reporting`] against any object source, awaiting whatever
/// I/O the source needs to read the page. This is the implementation; the
/// synchronous form is this function over [`Immediate`], driven to
/// completion on the calling thread, so the two cannot disagree about what a
/// page looks like.
///
/// The source is taken by value and the page by reference — the combination
/// a consumer needs to spawn the result. The future is `Send` over a source
/// that is `Send + Sync`, and `'static` as long as the borrow of `page` is
/// created inside the consumer's own `async move` block, which owns the
/// page. See `pdfboss_core::source`'s "Signing a shared algorithm".
pub(crate) async fn render_page_reporting_with<S: AsyncObjectSource>(
    src: S,
    page: &Page,
    scale: f32,
    opts: &RenderOptions,
) -> Result<(Pixmap, RenderReport)> {
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
    let mut report = RenderReport::default();
    // A page whose own `/Contents` will not decode or will not parse
    // rasterizes blank, which is indistinguishable from an empty page unless
    // the report says so.
    let content = match page_content_with(&src, page).await {
        Ok(content) => content,
        Err(e) => {
            report.record(SkippedKind::PageContents, skip_reason_for(&e));
            Vec::new()
        }
    };
    let ops = match parse_content(&content) {
        Ok(ops) => ops,
        Err(e) => {
            report.record(SkippedKind::PageContents, skip_reason_for(&e));
            Vec::new()
        }
    };
    let ctm = base_ctm(page.crop_box.normalize(), page.rotate, scale);
    let provider: Option<Box<dyn SubstituteProvider>> = match &opts.substitutes {
        SubstituteSource::Dir(dir) => Some(Box::new(DirProvider { dir: dir.clone() })),
        #[cfg(feature = "substitute-fonts")]
        SubstituteSource::Builtin => Some(Box::new(BuiltinProvider)),
        // Without the `substitute-fonts` feature there are no compiled-in
        // faces, so `Builtin` falls back to no provider (`Full` degrades to
        // `AllEmbedded` for non-embedded fonts). `None` never substitutes.
        #[cfg(not(feature = "substitute-fonts"))]
        SubstituteSource::Builtin => None,
        SubstituteSource::None => None,
    };
    let mut exec = Executor {
        src: &src,
        pix,
        painting: opts.glyph_painting,
        color_locked: false,
        provider,
        glyph_blit: Vec::new(),
        clip_cache: FastMap::default(),
        charproc_cache: FastMap::default(),
        report,
    };
    let root = Frame::new(
        ops.into(),
        vec![Arc::new(page.resources.clone())],
        GState::new(ctm),
        0,
        FrameKind::PageOrForm,
    );
    exec.run(root).await;
    exec.paint_annotations(page, ctm).await;
    Ok((exec.pix, exec.report))
}

/// The `/F` flag bits whose annotations are not displayed even by a renderer
/// that paints appearance streams: Hidden (bit 2) and NoView (bit 6),
/// ISO 32000-1 §12.5.3.
const INVISIBLE_ANNOTS: i64 = (1 << 1) | (1 << 5);

/// Upper bound on distinct parsed CharProcs kept per page render. A real
/// Type3 font has at most 256 mapped codes, so this is never approached
/// honestly; past the cap a glyph re-parses uncached, bounding memory
/// against a hostile file minting CharProcs.
const MAX_CHARPROC_CACHE: usize = 1024;

/// Executes parsed content operators against a shared pixmap; forms and
/// Type3 CharProcs run as frames on [`Executor::run`]'s explicit stack.
struct Executor<'a, S> {
    src: &'a S,
    pix: Pixmap,
    painting: GlyphPainting,
    /// Set while painting a `d1` (uncolored) Type3 CharProc: ISO 32000-1
    /// §9.6.5.2 says such a glyph "shall not specify any color", so
    /// `run_color_or_misc` turns every fill/stroke color-setting op into a
    /// no-op and the glyph keeps the color inherited from the text state.
    color_locked: bool,
    /// The `Full`-tier substitute source built from
    /// [`RenderOptions::substitutes`], if any. Passed through to
    /// [`GlyphFont::load`], which consults it to substitute a non-embedded
    /// SIMPLE font (`/TrueType`, `/Type1`, `/MMType1`) at the `Full` tier;
    /// `/Type0` and `/Type3` fonts never consult it (see `glyph.rs`'s module
    /// doc).
    provider: Option<Box<dyn SubstituteProvider>>,
    /// Reused scratch for painting a cached glyph outline: the flattened
    /// (origin-relative) subpaths from [`GlyphFont::flattened`] are copied
    /// here translated to the glyph's device origin, so a whole page of text
    /// paints its glyphs without allocating a fresh polygon set per glyph.
    glyph_blit: Vec<Subpath>,
    /// Rasterized clip masks by exact path geometry, shared across the whole
    /// page render (including nested forms — a repeated clip means the same
    /// device-space geometry regardless of which resource scope drew it).
    /// See [`MAX_CLIP_CACHE`].
    clip_cache: FastMap<ClipKey, Arc<Mask>>,
    /// Parsed Type3 CharProc content, keyed by the CharProc stream's object
    /// reference and shared across the page. Body text repeats a small
    /// alphabet, so the same CharProc used to be fetched and re-parsed for
    /// every occurrence of its code; parsing is context-free, so one parse
    /// serves them all. See [`MAX_CHARPROC_CACHE`].
    charproc_cache: FastMap<pdfboss_core::ObjRef, Arc<[Op]>>,
    /// Content this render dropped rather than painted, accumulated across
    /// the page (forms and Type3 CharProcs included, since they run through
    /// the same [`Executor`]).
    report: RenderReport,
}

/// One suspended operator stream on the executor's stack: what to execute,
/// how far it has got, and every piece of state that stream owns.
///
/// This is what the recursion into a form XObject or a Type3 CharProc
/// became. A recursive `async fn` must box itself, and coercing the box to a
/// `Send` future needs `S: Sync` — which `Immediate<&Document>` cannot
/// supply — so boxing would cost the synchronous caller the shared
/// implementation. A stack of frames uses no `dyn`, so auto traits stay
/// inferred per instantiation: the future is `Send` over an asynchronous
/// source and merely non-`Send` over a synchronous one.
struct Frame {
    /// Shared so a handle can outlive pushes onto the frame stack. Cloned
    /// once per visit to the frame, never per operator.
    ops: Arc<[Op]>,
    /// Resource dictionaries, innermost first. Owned — a form's `/Resources`
    /// and a Type3 font's both come from below the frame that reads them.
    chain: Vec<Arc<Dict>>,
    /// Index of the next operator to execute.
    pc: usize,
    /// Form/CharProc nesting depth, carried explicitly and checked against
    /// [`MAX_FORM_DEPTH`]. Never derived from the stack's length: Type3
    /// sibling glyphs run as consecutive frames at the SAME depth, so the
    /// two quantities genuinely differ.
    depth: u32,
    gs: GState,
    /// The `q`/`Q` stack. Per frame, exactly as each recursive call had its
    /// own: a form's unbalanced `Q` must not pop its caller's state, and
    /// [`MAX_GSTATE_DEPTH`] caps each stream, not the page.
    saved: Vec<GState>,
    path: Option<PathBuilder>,
    pending_clip: Option<FillRule>,
    ts: TextState,
    /// Loaded fonts by resource name, each with its report label (see
    /// [`LoadedFont`]). Per frame, never hoisted: `/F0` names different
    /// fonts in different resource scopes.
    fonts: FastMap<String, LoadedFont>,
    /// Type3 glyphs planned by a show operator and not yet painted. Drained
    /// one CharProc frame at a time before the next operator runs.
    pending_glyphs: std::collections::VecDeque<Type3Glyph>,
    /// The font the pending glyphs paint from.
    pending_t3: Option<Arc<Type3Font>>,
    /// What this frame owes on the way out.
    kind: FrameKind,
}

/// What kind of content stream a [`Frame`] is running, and therefore what
/// its pop restores.
enum FrameKind {
    /// A page or form XObject content stream. Pops restore nothing. In
    /// particular the color lock is deliberately NOT saved here: a form
    /// invoked inside a `d1` CharProc inherits the lock, because the
    /// recursive `run_form` never touched it — the harness pins those
    /// pixels.
    PageOrForm,
    /// A Type3 CharProc. Its pop restores the executor's color lock to what
    /// it was before this glyph pushed (ISO 32000-1 9.6.5.2: a `d1` glyph's
    /// own color operators are ignored; a `d0` glyph nested inside a `d1`
    /// one regains color control for its own subtree).
    CharProc { saved_lock: bool },
}

impl Frame {
    fn new(
        ops: Arc<[Op]>,
        chain: Vec<Arc<Dict>>,
        gs: GState,
        depth: u32,
        kind: FrameKind,
    ) -> Frame {
        Frame {
            ops,
            chain,
            pc: 0,
            depth,
            gs,
            saved: Vec::new(),
            path: None,
            pending_clip: None,
            ts: TextState::default(),
            fonts: FastMap::default(),
            pending_glyphs: std::collections::VecDeque::new(),
            pending_t3: None,
            kind,
        }
    }
}

impl<S: AsyncObjectSource> Executor<'_, S> {
    /// Executes a frame and every form XObject and Type3 CharProc it
    /// invokes. All failures are lenient skips.
    ///
    /// Exactly one child is pushed at a time — a form inline at its `Do`,
    /// so its skips land in report order, or one Type3 glyph from the
    /// pending queue — and a child runs to completion before its parent's
    /// next operator, which is the recursive version's depth-first order.
    async fn run(&mut self, root: Frame) {
        let mut frames = vec![root];
        'frames: while let Some(mut frame) = frames.pop() {
            // Planned Type3 glyphs paint before the next operator, one
            // CharProc frame per pass; a glyph whose stream will not resolve
            // or parse is the same silent skip it always was.
            while let Some(glyph) = frame.pending_glyphs.pop_front() {
                let Some(t3) = frame.pending_t3.clone() else {
                    break;
                };
                let child = self.char_proc_frame(&glyph, &t3, &frame).await;
                if let Some(child) = child {
                    frames.push(frame);
                    frames.push(child);
                    continue 'frames;
                }
            }
            frame.pending_t3 = None;

            // Cloned once per visit, so the handle outlives the `&mut frame`
            // borrows below without an atomic pair per operator.
            let ops = Arc::clone(&frame.ops);
            let mut spawned: Option<Frame> = None;
            'ops: while frame.pc < ops.len() {
                let op = &ops[frame.pc];
                frame.pc += 1;
                let frame = &mut frame;
                match op {
                    Op::Save => {
                        if frame.saved.len() < MAX_GSTATE_DEPTH {
                            frame.saved.push(frame.gs.clone());
                        }
                    }
                    Op::Restore => {
                        if let Some(prev) = frame.saved.pop() {
                            frame.gs = prev;
                        }
                    }
                    Op::Concat(m) => {
                        if finite_matrix(m) {
                            frame.gs.ctm = m.concat(frame.gs.ctm);
                        }
                    }
                    Op::SetLineWidth(w) => {
                        if w.is_finite() && *w >= 0.0 {
                            frame.gs.line_width = *w;
                        }
                    }
                    Op::SetLineCap(c) => frame.gs.line_cap = *c,
                    Op::SetLineJoin(j) => frame.gs.line_join = *j,
                    Op::SetMiterLimit(m) => {
                        if m.is_finite() {
                            frame.gs.miter_limit = *m;
                        }
                    }
                    Op::SetDash(d, phase) => {
                        if all_finite(d) && phase.is_finite() {
                            frame.gs.dash = d.clone();
                            frame.gs.dash_phase = *phase;
                        }
                    }
                    Op::SetExtGState(name) => self.apply_ext_gstate_op(name, frame).await,
                    Op::SetRenderingIntent(_) | Op::SetFlatness(_) => {}

                    // Path construction (user space; the builder applies the
                    // CTM captured when the path starts).
                    Op::MoveTo(x, y) => {
                        if all_finite(&[*x, *y]) {
                            builder(&mut frame.path, &frame.gs).move_to(*x, *y);
                        }
                    }
                    Op::LineTo(x, y) => {
                        if all_finite(&[*x, *y]) {
                            builder(&mut frame.path, &frame.gs).line_to(*x, *y);
                        }
                    }
                    Op::CurveTo(x1, y1, x2, y2, x3, y3) => {
                        if all_finite(&[*x1, *y1, *x2, *y2, *x3, *y3]) {
                            builder(&mut frame.path, &frame.gs)
                                .curve_to(*x1, *y1, *x2, *y2, *x3, *y3);
                        }
                    }
                    Op::CurveToV(x2, y2, x3, y3) => {
                        if all_finite(&[*x2, *y2, *x3, *y3]) {
                            builder(&mut frame.path, &frame.gs).curve_to_v(*x2, *y2, *x3, *y3);
                        }
                    }
                    Op::CurveToY(x1, y1, x3, y3) => {
                        if all_finite(&[*x1, *y1, *x3, *y3]) {
                            builder(&mut frame.path, &frame.gs).curve_to_y(*x1, *y1, *x3, *y3);
                        }
                    }
                    Op::ClosePath => {
                        if let Some(pb) = frame.path.as_mut() {
                            pb.close();
                        }
                    }
                    Op::Rect(x, y, w, h) => {
                        if all_finite(&[*x, *y, *w, *h]) {
                            builder(&mut frame.path, &frame.gs).rect(*x, *y, *w, *h);
                        }
                    }

                    // Path painting: fill first, then stroke; a pending W/W*
                    // clip takes effect after any of these (including n).
                    Op::Stroke => self.paint_frame(frame, PAINT_STROKE),
                    Op::CloseStroke => self.paint_frame(
                        frame,
                        Paint {
                            close: true,
                            ..PAINT_STROKE
                        },
                    ),
                    Op::Fill => self.paint_frame(frame, PAINT_FILL),
                    Op::FillEvenOdd => self.paint_frame(frame, PAINT_FILL_EO),
                    Op::FillStroke => self.paint_frame(frame, PAINT_BOTH),
                    Op::FillStrokeEvenOdd => self.paint_frame(frame, PAINT_BOTH_EO),
                    Op::CloseFillStroke => self.paint_frame(
                        frame,
                        Paint {
                            close: true,
                            ..PAINT_BOTH
                        },
                    ),
                    Op::CloseFillStrokeEvenOdd => self.paint_frame(
                        frame,
                        Paint {
                            close: true,
                            ..PAINT_BOTH_EO
                        },
                    ),
                    Op::EndPath => self.paint_frame(frame, PAINT_NONE),
                    Op::ClipNonZero => frame.pending_clip = Some(FillRule::NonZero),
                    Op::ClipEvenOdd => frame.pending_clip = Some(FillRule::EvenOdd),

                    // Text: a minimal show-string state machine that paints
                    // embedded TrueType glyph outlines (other fonts stay unpainted).
                    Op::BeginText => {
                        frame.ts.tm = Matrix::identity();
                        frame.ts.tlm = Matrix::identity();
                    }
                    Op::SetCharSpacing(v) if v.is_finite() => frame.ts.char_spacing = *v,
                    Op::SetWordSpacing(v) if v.is_finite() => frame.ts.word_spacing = *v,
                    Op::SetHorizScaling(v) if v.is_finite() => frame.ts.horiz = v / 100.0,
                    Op::SetLeading(v) if v.is_finite() => frame.ts.leading = *v,
                    Op::SetTextRise(v) if v.is_finite() => frame.ts.rise = *v,
                    Op::SetFont(name, size) => {
                        frame.ts.size = if size.is_finite() { *size } else { 0.0 };
                        frame.ts.font = self
                            .glyph_font(&name.0, &frame.chain, &mut frame.fonts)
                            .await;
                        // Type3 is the fallback when no outline font loads: a
                        // `/Type3` dict at a tier that paints embedded programs.
                        // The invariant (at most one of font/type3) holds because
                        // this only runs when `frame.ts.font` is `None`.
                        frame.ts.type3 = if frame.ts.font.is_some() {
                            None
                        } else {
                            self.type3_font(&name.0, &frame.chain).await
                        };
                    }
                    Op::SetTextMatrix(m) if finite_matrix(m) => {
                        frame.ts.tm = *m;
                        frame.ts.tlm = *m;
                    }
                    Op::TextMove(tx, ty) if all_finite(&[*tx, *ty]) => {
                        frame.ts.tlm = Matrix::translate(*tx, *ty).concat(frame.ts.tlm);
                        frame.ts.tm = frame.ts.tlm;
                    }
                    Op::TextMoveSetLeading(tx, ty) if all_finite(&[*tx, *ty]) => {
                        frame.ts.leading = -*ty;
                        frame.ts.tlm = Matrix::translate(*tx, *ty).concat(frame.ts.tlm);
                        frame.ts.tm = frame.ts.tlm;
                    }
                    Op::TextNextLine => {
                        frame.ts.tlm =
                            Matrix::translate(0.0, -frame.ts.leading).concat(frame.ts.tlm);
                        frame.ts.tm = frame.ts.tlm;
                    }
                    Op::ShowText(s) => {
                        self.show_text(frame, s);
                        if !frame.pending_glyphs.is_empty() {
                            break 'ops;
                        }
                    }
                    Op::ShowTextAdjusted(items) => {
                        for item in items {
                            match item {
                                TextItem::Str(s) => self.show_text(frame, s),
                                TextItem::Offset(n) => {
                                    let tx = -n / 1000.0 * frame.ts.size * frame.ts.horiz;
                                    if tx.is_finite() {
                                        frame.ts.tm =
                                            Matrix::translate(tx, 0.0).concat(frame.ts.tm);
                                    }
                                }
                            }
                        }
                    }
                    Op::NextLineShowText(s) => {
                        frame.ts.tlm =
                            Matrix::translate(0.0, -frame.ts.leading).concat(frame.ts.tlm);
                        frame.ts.tm = frame.ts.tlm;
                        self.show_text(frame, s);
                    }
                    Op::NextLineShowTextSpaced(aw, ac, s) => {
                        if aw.is_finite() {
                            frame.ts.word_spacing = *aw;
                        }
                        if ac.is_finite() {
                            frame.ts.char_spacing = *ac;
                        }
                        frame.ts.tlm =
                            Matrix::translate(0.0, -frame.ts.leading).concat(frame.ts.tlm);
                        frame.ts.tm = frame.ts.tlm;
                        self.show_text(frame, s);
                    }

                    other => {
                        spawned = self.run_color_or_misc(other, frame).await;
                    }
                }
                // A `Do`/CharProc pushed a child, or a show operator planned
                // Type3 glyphs: either way this frame suspends here and the
                // child (or the glyph queue) runs before its next operator.
                if spawned.is_some() || !frame.pending_glyphs.is_empty() {
                    break;
                }
            }
            if let Some(child) = spawned {
                frames.push(frame);
                frames.push(child);
                continue 'frames;
            }
            if !frame.pending_glyphs.is_empty() {
                frames.push(frame);
                continue 'frames;
            }
            // The frame is done; a CharProc restores the color lock its
            // glyph saved.
            if let FrameKind::CharProc { saved_lock } = frame.kind {
                self.color_locked = saved_lock;
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

impl<S: AsyncObjectSource> Executor<'_, S> {
    /// [`Executor::paint`] on a frame's own path, pending clip and state.
    fn paint_frame(&mut self, frame: &mut Frame, how: Paint) {
        let Frame {
            gs,
            path,
            pending_clip,
            ..
        } = frame;
        self.paint(gs, path, pending_clip, how);
    }

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
        // A pattern paints its stand-in gray (see `GState::fill_rgba8`), so
        // every such paint is an approximation the caller should hear about
        // -- but only once the path actually covers something.
        if !polys.is_empty()
            && ((how.fill.is_some() && gs.fill_pattern) || (how.stroke && gs.stroke_pattern))
        {
            self.skip(SkippedKind::Pattern, SkipReason::Unsupported);
        }
        if let Some(rule) = how.fill {
            fill_path(
                &mut self.pix,
                &polys,
                rule,
                gs.fill_rgba8(),
                gs.fill_alpha,
                gs.clip.as_deref(),
                gs.blend_mode,
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
                gs.blend_mode,
            );
        }
        if let Some(rule) = pending.take() {
            let rasterized = self.rasterize_clip(&polys, rule);
            gs.clip = Some(match &gs.clip {
                Some(old) => Arc::new(Mask::intersected(&rasterized, old)),
                None => rasterized,
            });
        }
    }

    /// Rasterizes `polys` under `rule` into a clip [`Mask`], reusing a
    /// cached rasterization when the exact same path was clipped earlier on
    /// this page (very common: many generators repeat an identical
    /// page-bounds "reset" clip hundreds of times per page, and re-running
    /// the scanline rasterizer over the same geometry every time is pure
    /// waste). The returned mask is pre-intersection — the caller still
    /// applies any enclosing clip on top.
    fn rasterize_clip(&mut self, polys: &[Subpath], rule: FillRule) -> Arc<Mask> {
        let key = ClipKey::new(polys, rule);
        if let Some(cached) = self.clip_cache.get(&key) {
            return Arc::clone(cached);
        }
        let mask = Arc::new(Mask::from_path(
            self.pix.width,
            self.pix.height,
            polys,
            rule,
        ));
        if self.clip_cache.len() < MAX_CLIP_CACHE {
            self.clip_cache.insert(key, Arc::clone(&mask));
        }
        mask
    }

    /// Resolves and caches a paintable font by resource name, paired with
    /// its report label (see [`LoadedFont`]).
    async fn glyph_font(
        &self,
        name: &str,
        chain: &[Arc<Dict>],
        cache: &mut FastMap<String, LoadedFont>,
    ) -> LoadedFont {
        if let Some(f) = cache.get(name) {
            return f.clone();
        }
        let dict = self
            .find_res(chain, "Font", name)
            .await
            .and_then(|o| o.as_dict().cloned());
        let label: Arc<str> = dict
            .as_ref()
            .and_then(|d| d.get_name("BaseFont"))
            .map_or(name, |n| n.0.as_str())
            .into();
        let loaded = match dict {
            Some(d) => GlyphFont::load_with(self.src, &d, self.painting, self.provider.as_deref())
                .await
                .map(|f| (Arc::new(f), label)),
            None => None,
        };
        cache.insert(name.to_string(), loaded.clone());
        loaded
    }

    /// Resolves a `/Type3` font resource for painting, or `None` when the tier
    /// forbids embedded programs, the name is missing, or the resource is not a
    /// `/Type3` dict. Called only after the outline loader declined the name.
    async fn type3_font(&self, name: &str, chain: &[Arc<Dict>]) -> Option<Arc<Type3Font>> {
        if !self.painting.paints_all_embedded() {
            return None;
        }
        let dict = self
            .find_res(chain, "Font", name)
            .await
            .and_then(|o| o.as_dict().cloned())?;
        if dict.get_name("Subtype").map(|n| n.0.as_str()) != Some("Type3") {
            return None;
        }
        Type3Font::load_with(self.src, &dict).await.map(Arc::new)
    }

    /// Paints a cached, origin-relative flattened glyph outline at device
    /// origin `(dx, dy)` in color `fill`, reusing [`Executor::glyph_blit`] so
    /// a page of text paints without a fresh polygon allocation per glyph.
    /// The fill is anti-aliased, nonzero-rule, alpha- and clip-scaled exactly
    /// as a direct `fill_path` on the untranslated glyph would be.
    fn blit_glyph(&mut self, cached: &[Subpath], dx: f32, dy: f32, fill: [u8; 4], gs: &GState) {
        for (i, src) in cached.iter().enumerate() {
            if i == self.glyph_blit.len() {
                self.glyph_blit.push(Subpath {
                    points: Vec::new(),
                    closed: src.closed,
                });
            }
            let dst = &mut self.glyph_blit[i];
            dst.points.clear();
            dst.points
                .extend(src.points.iter().map(|p| Point::new(p.x + dx, p.y + dy)));
            dst.closed = src.closed;
        }
        fill_path(
            &mut self.pix,
            &self.glyph_blit[..cached.len()],
            FillRule::NonZero,
            fill,
            gs.fill_alpha,
            gs.clip.as_deref(),
            gs.blend_mode,
        );
    }

    /// Paints one show-string's glyphs and advances the text matrix. Codes with
    /// no drawable glyph still advance, so surrounding text stays positioned.
    ///
    /// Synchronous on purpose: an outline font is already loaded, so no I/O
    /// enters the per-glyph loop; and a Type3 string only *plans* here — its
    /// CharProc frames are pushed by the driver, one at a time, before the
    /// frame's next operator.
    fn show_text(&mut self, frame: &mut Frame, bytes: &[u8]) {
        if let Some(t3) = frame.ts.type3.clone() {
            // The depth guard bounds a self-referential glyph: each CharProc
            // frame is pushed at `depth + 1`, so painting stops at
            // `MAX_FORM_DEPTH` while the advances still happen.
            let paint = frame.depth < MAX_FORM_DEPTH;
            let planned = type3_glyph_plan(&mut frame.ts, &t3, bytes, frame.gs.ctm, paint);
            frame.pending_glyphs.extend(planned);
            frame.pending_t3 = Some(t3);
            return;
        }
        let gs = &frame.gs;
        let ts = &mut frame.ts;
        let Some((font, label)) = ts.font.clone() else {
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
            if !font.paints() {
                // A metrics-only font: the advance below is the whole point,
                // and its unpainted codes are configured tier behavior, so
                // neither the paint attempt nor the no-glyph report applies.
            } else if gid != 0 && finite_matrix(&to_device) {
                // Flatten under the linear part only (memoized per glyph +
                // linear map); the per-glyph translation is applied when the
                // cached outline is blitted, keeping the flatten reusable
                // across every occurrence in the run.
                let linear = Matrix {
                    a: to_device.a,
                    b: to_device.b,
                    c: to_device.c,
                    d: to_device.d,
                    e: 0.0,
                    f: 0.0,
                };
                let polys = font.flattened(gid, linear);
                if !polys.is_empty() {
                    self.blit_glyph(&polys, to_device.e, to_device.f, fill, gs);
                }
            } else if gid == 0 && !(n == 1 && code == 32) {
                // A loaded font with no glyph for this code: the advance
                // below still happens, so surrounding text stays positioned,
                // but nothing painted here — report it so a lossy render is
                // never mistaken for a clean one. The single-byte space is
                // exempt because a space paints nothing whether or not the
                // font maps it; a two-byte 0x20 is a real CID, not a space.
                self.skip(
                    SkippedKind::Glyph,
                    SkipReason::NoGlyph {
                        code,
                        font: label.to_string(),
                    },
                );
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

    /// Builds the frame for one Type3 CharProc: resolve its stream, parse
    /// it, and frame it with the glyph CTM, the font's own `/Resources`
    /// prepended to the parent's chain, and `depth + 1`. Inherits the
    /// caller's clip, alpha, and fill color (the color a `d0` glyph paints
    /// in). Every failure is `None` — a silent skip, matching the
    /// still-advance leniency of the planner.
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
    async fn char_proc_frame(
        &mut self,
        glyph: &Type3Glyph,
        t3: &Type3Font,
        parent: &Frame,
    ) -> Option<Frame> {
        let cached = match &glyph.proc_obj {
            Object::Ref(r) => self.charproc_cache.get(r).cloned(),
            _ => None,
        };
        let ops: Arc<[Op]> = match cached {
            Some(ops) => ops,
            None => {
                let Ok(Object::Stream(stream)) = self.src.resolve(&glyph.proc_obj).await else {
                    return None;
                };
                // Through the content chokepoint: a CharProc labelled with
                // an image codec is passthrough bytes, refused rather than
                // parsed (silently, like every other CharProc fetch
                // failure).
                let Ok(data) = content_stream_data_with(self.src, &stream).await else {
                    return None;
                };
                let Ok(ops) = parse_content(&data) else {
                    return None;
                };
                let ops: Arc<[Op]> = ops.into();
                if let Object::Ref(r) = &glyph.proc_obj {
                    if self.charproc_cache.len() < MAX_CHARPROC_CACHE {
                        self.charproc_cache.insert(*r, Arc::clone(&ops));
                    }
                }
                ops
            }
        };
        let mut inner = parent.gs.clone();
        inner.ctm = glyph.ctm;
        let mut inner_chain: Vec<Arc<Dict>> = Vec::with_capacity(parent.chain.len() + 1);
        if let Some(d) = t3.resources() {
            inner_chain.push(Arc::clone(d));
        }
        inner_chain.extend_from_slice(&parent.chain);
        let is_d1 = matches!(ops.first(), Some(Op::SetGlyphWidthBBox(..)));
        // Setting the lock here is sound only because the driver pushes the
        // returned frame unconditionally; its pop restores `saved_lock`. On
        // `None` the lock is untouched, exactly as the recursive version's
        // early returns left it.
        let saved_lock = self.color_locked;
        self.color_locked = is_d1;
        Some(Frame::new(
            ops,
            inner_chain,
            inner,
            parent.depth + 1,
            FrameKind::CharProc { saved_lock },
        ))
    }

    /// Dispatches color, XObject, and marked-content operators (the remainder
    /// of the [`Op`] alphabet not handled directly in `run`).
    ///
    /// Every fill/stroke color-setting arm is a no-op while
    /// `self.color_locked` (inside a `d1` Type3 CharProc, ISO 32000-1
    /// §9.6.5.2): the glyph keeps the fill/stroke color inherited from the
    /// text graphics state instead of applying its own. XObject, inline
    /// image, shading, and marked-content ops are unaffected by the lock.
    async fn run_color_or_misc(&mut self, op: &Op, frame: &mut Frame) -> Option<Frame> {
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
                | Op::SetStrokeCMYK(_, _, _, _) => return None,
                _ => {}
            }
        }
        match op {
            Op::SetFillColorSpace(name) => {
                let (cs, pattern) = self.resolve_colorspace(name, &frame.chain).await;
                let gs = &mut frame.gs;
                gs.fill_rgb = initial_color(&cs);
                gs.fill_space = cs;
                gs.fill_pattern = pattern;
            }
            Op::SetStrokeColorSpace(name) => {
                let (cs, pattern) = self.resolve_colorspace(name, &frame.chain).await;
                let gs = &mut frame.gs;
                gs.stroke_rgb = initial_color(&cs);
                gs.stroke_space = cs;
                gs.stroke_pattern = pattern;
            }
            Op::SetFillColor(c) => frame.gs.fill_rgb = frame.gs.fill_space.to_rgb(c),
            Op::SetStrokeColor(c) => frame.gs.stroke_rgb = frame.gs.stroke_space.to_rgb(c),
            Op::SetFillColorN(c, pattern_name) => {
                let gs = &mut frame.gs;
                if pattern_name.is_some() {
                    gs.fill_pattern = true;
                } else if !gs.fill_pattern {
                    gs.fill_rgb = gs.fill_space.to_rgb(c);
                }
            }
            Op::SetStrokeColorN(c, pattern_name) => {
                let gs = &mut frame.gs;
                if pattern_name.is_some() {
                    gs.stroke_pattern = true;
                } else if !gs.stroke_pattern {
                    gs.stroke_rgb = gs.stroke_space.to_rgb(c);
                }
            }
            Op::SetFillGray(g) => {
                let gs = &mut frame.gs;
                gs.fill_space = ColorSpace::DeviceGray;
                gs.fill_pattern = false;
                gs.fill_rgb = ColorSpace::DeviceGray.to_rgb(&[*g]);
            }
            Op::SetStrokeGray(g) => {
                let gs = &mut frame.gs;
                gs.stroke_space = ColorSpace::DeviceGray;
                gs.stroke_pattern = false;
                gs.stroke_rgb = ColorSpace::DeviceGray.to_rgb(&[*g]);
            }
            Op::SetFillRGB(r, g, b) => {
                let gs = &mut frame.gs;
                gs.fill_space = ColorSpace::DeviceRGB;
                gs.fill_pattern = false;
                gs.fill_rgb = ColorSpace::DeviceRGB.to_rgb(&[*r, *g, *b]);
            }
            Op::SetStrokeRGB(r, g, b) => {
                let gs = &mut frame.gs;
                gs.stroke_space = ColorSpace::DeviceRGB;
                gs.stroke_pattern = false;
                gs.stroke_rgb = ColorSpace::DeviceRGB.to_rgb(&[*r, *g, *b]);
            }
            Op::SetFillCMYK(c, m, y, k) => {
                let gs = &mut frame.gs;
                gs.fill_space = ColorSpace::DeviceCMYK;
                gs.fill_pattern = false;
                gs.fill_rgb = ColorSpace::DeviceCMYK.to_rgb(&[*c, *m, *y, *k]);
            }
            Op::SetStrokeCMYK(c, m, y, k) => {
                let gs = &mut frame.gs;
                gs.stroke_space = ColorSpace::DeviceCMYK;
                gs.stroke_pattern = false;
                gs.stroke_rgb = ColorSpace::DeviceCMYK.to_rgb(&[*c, *m, *y, *k]);
            }
            Op::XObject(name) => {
                return self
                    .do_xobject(name, &frame.chain, &frame.gs, frame.depth)
                    .await;
            }
            Op::InlineImage(img) => {
                self.draw_inline_image(img, &frame.chain, &frame.gs).await;
            }
            // Shadings are out of scope for v0.1: `sh` paints nothing, so a
            // page whose visible content is a gradient comes out blank.
            Op::Shading(_) => self.skip(SkippedKind::Shading, SkipReason::Unsupported),
            // Text and marked content: state-only in v0.1, nothing painted.
            _ => {}
        }
        None
    }
}

/// One planned Type3 glyph invocation: the CharProc to run (stored as given,
/// possibly an indirect reference, resolved at paint time) and the device
/// matrix its content stream runs under.
struct Type3Glyph {
    proc_obj: Object,
    ctm: Matrix,
}

/// Plans a `/Type3` show-string: which CharProcs run under which glyph CTMs,
/// advancing the text matrix as it goes. Pure — the whole plan is knowable
/// before any glyph paints, because the advance comes from the font's
/// `/Widths` entry, never from the `wx` operand of the CharProc's `d0`/`d1`
/// (whose only consumer is the color-lock check at paint time). Were that
/// not so, this function could not exist and every glyph's position would
/// wait on the previous glyph's content stream.
///
/// Codes with no CharProc, a non-finite matrix, or `paint` false (the caller
/// is at the recursion limit) still advance, keeping surrounding text
/// positioned.
fn type3_glyph_plan(
    ts: &mut TextState,
    t3: &Type3Font,
    bytes: &[u8],
    ctm: Matrix,
    paint: bool,
) -> Vec<Type3Glyph> {
    let font_matrix = t3.font_matrix();
    let mut planned = Vec::new();
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
        let glyph_ctm = font_matrix.concat(params).concat(ts.tm).concat(ctm);
        if paint && finite_matrix(&glyph_ctm) {
            if let Some(proc_obj) = t3.char_proc(code).cloned() {
                planned.push(Type3Glyph {
                    proc_obj,
                    ctm: glyph_ctm,
                });
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
    planned
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

impl<S: AsyncObjectSource> Executor<'_, S> {
    /// Looks up `/category/name` in the resource chain (innermost dict
    /// first), resolving references at every step.
    ///
    /// # Why the chain owns its entries
    ///
    /// `Arc<Dict>` rather than `&Dict`, which buys nothing while `run` is
    /// recursive: both chains are built one stack frame below the `run` that reads
    /// them. It is what makes the chain outlive that frame. Both build sites
    /// currently push something local — `run_form`'s `own_res` is a local `Dict`,
    /// and `run_char_proc` reaches into the `Type3Font` its caller happens to hold
    /// — so neither could survive being stored in a work-stack frame, which is how
    /// the asynchronous path has to express this recursion.
    async fn find_res(&self, chain: &[Arc<Dict>], category: &str, name: &str) -> Option<Object> {
        for res in chain {
            let Some(cat) = res.get(category) else {
                continue;
            };
            let Ok(Object::Dict(dict)) = self.src.resolve(cat).await else {
                continue;
            };
            let Some(value) = dict.get(name) else {
                continue;
            };
            if let Ok(obj) = self.src.resolve(value).await {
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
    async fn resolve_colorspace(&self, name: &Name, chain: &[Arc<Dict>]) -> (ColorSpace, bool) {
        match name.0.as_str() {
            "Pattern" => return (ColorSpace::DeviceGray, true),
            "DeviceGray" | "G" | "CalGray" => return (ColorSpace::DeviceGray, false),
            "DeviceRGB" | "RGB" | "CalRGB" => return (ColorSpace::DeviceRGB, false),
            "DeviceCMYK" | "CMYK" => return (ColorSpace::DeviceCMYK, false),
            _ => {}
        }
        match self.find_res(chain, "ColorSpace", &name.0).await {
            Some(obj) => {
                // `[/Pattern base]` resource entries are pattern spaces too.
                if let Object::Array(items) = &obj {
                    if let Some(Object::Name(n)) = items.first() {
                        if n.0 == "Pattern" {
                            return (ColorSpace::DeviceGray, true);
                        }
                    }
                }
                (ColorSpace::parse_with(self.src, &obj).await, false)
            }
            None => (ColorSpace::DeviceGray, false),
        }
    }

    /// Applies the `/ca /CA /LW /LC /LJ /D` entries of the named
    /// `/ExtGState` resource. Other entries are ignored in v0.1; the two
    /// that change what the page looks like -- a `/SMask` mask group and a
    /// non-`Normal` `/BM` blend mode -- are reported so the caller knows the
    /// render is an approximation.
    async fn apply_ext_gstate_op(&mut self, name: &Name, frame: &mut Frame) {
        let Some(Object::Dict(dict)) = self.find_res(&frame.chain, "ExtGState", &name.0).await
        else {
            return;
        };
        if ignores_mask(self.src, &dict).await {
            self.skip(SkippedKind::SoftMask, SkipReason::Unsupported);
        }
        match blend_mode_entry(self.src, &dict).await {
            Some(Ok(mode)) => frame.gs.blend_mode = mode,
            Some(Err(())) => self.skip(SkippedKind::BlendMode, SkipReason::Unsupported),
            None => {}
        }
        let gs = &mut frame.gs;
        if let Some(ca) = dict_f32(self.src, &dict, "ca").await {
            gs.fill_alpha = ca.clamp(0.0, 1.0);
        }
        if let Some(ca) = dict_f32(self.src, &dict, "CA").await {
            gs.stroke_alpha = ca.clamp(0.0, 1.0);
        }
        if let Some(lw) = dict_f32(self.src, &dict, "LW").await {
            if lw >= 0.0 {
                gs.line_width = lw;
            }
        }
        if let Some(lc) = dict_f32(self.src, &dict, "LC").await {
            gs.line_cap = lc as i32;
        }
        if let Some(lj) = dict_f32(self.src, &dict, "LJ").await {
            gs.line_join = lj as i32;
        }
        let d = match dict.get("D") {
            Some(o) => self.src.resolve(o).await.ok(),
            None => None,
        };
        if let Some(Object::Array(items)) = d {
            let lens = match items.first() {
                Some(o) => self.src.resolve(o).await.ok(),
                None => None,
            };
            let phase = match items.get(1) {
                Some(o) => self.src.resolve(o).await.ok().and_then(|o| o.as_f64()),
                None => None,
            };
            if let (Some(Object::Array(lens)), Some(phase)) = (lens, phase) {
                let mut dash: Vec<f32> = Vec::with_capacity(lens.len());
                for o in &lens {
                    if let Some(v) = num_f32(self.src, o).await {
                        dash.push(v);
                    }
                }
                if dash.len() == lens.len() && (phase as f32).is_finite() {
                    gs.dash = dash;
                    gs.dash_phase = phase as f32;
                }
            }
        }
    }
}

/// Resolves an object to a finite `f32`.
async fn num_f32<S: AsyncObjectSource>(src: &S, obj: &Object) -> Option<f32> {
    let v = src.resolve(obj).await.ok()?.as_f64()? as f32;
    v.is_finite().then_some(v)
}

/// Resolves `dict[key]` to a finite `f32`.
async fn dict_f32<S: AsyncObjectSource>(src: &S, dict: &Dict, key: &str) -> Option<f32> {
    num_f32(src, dict.get(key)?).await
}

/// Reads the first `n` finite numbers of a (possibly indirect) array.
async fn floats_from<S: AsyncObjectSource>(
    src: &S,
    obj: Option<&Object>,
    n: usize,
) -> Option<Vec<f32>> {
    let arr = match src.resolve(obj?).await {
        Ok(Object::Array(a)) if a.len() >= n => a,
        _ => return None,
    };
    let mut out: Vec<f32> = Vec::with_capacity(n);
    for o in arr.iter().take(n) {
        if let Some(v) = num_f32(src, o).await {
            out.push(v);
        }
    }
    (out.len() == n).then_some(out)
}

/// Maps a stream-decode or content-parse failure onto the reason reported
/// to callers.
fn skip_reason_for(e: &Error) -> SkipReason {
    match e {
        Error::UnsupportedFilter(name) => SkipReason::UnsupportedFilter(name.clone()),
        other => SkipReason::DecodeFailed(other.to_string()),
    }
}

/// Whether an image dictionary carries masking this renderer ignores: a
/// `/SMask` alpha channel, or a `/Mask` stencil or color-key array (ISO
/// 32000-1 8.9.6). What the author masked out paints solid instead, so the
/// caller reports it rather than passing the result off as the real image.
async fn ignores_mask<S: AsyncObjectSource>(src: &S, dict: &Dict) -> bool {
    for key in ["SMask", "Mask"] {
        let Some(obj) = dict.get(key) else {
            continue;
        };
        let ignored = match src.resolve(obj).await {
            Ok(Object::Null) => false,
            // `/SMask /None` in particular is the explicit "no mask" value.
            Ok(Object::Name(n)) => n.0 != "None",
            _ => true,
        };
        if ignored {
            return true;
        }
    }
    false
}

/// Whether an `/ExtGState` selects a blend mode this renderer does not
/// apply (ISO 32000-1 11.3.5). Everything composites source-over, so
/// anything but `Normal` (and its deprecated alias `Compatible`) paints
/// differently than the page asks for.
/// The `/BM` entry classified for painting: `None` when the dictionary
/// sets no mode, `Some(Ok(mode))` for a mode the rasterizer paints, and
/// `Some(Err(()))` for the recognized-but-unpainted non-separable four
/// (Hue, Saturation, Color, Luminosity), which the caller reports. An
/// unrecognized name reads as Normal, exactly as ISO 32000-1 §11.3.5
/// tells a conforming reader to treat it — that is compliance, not an
/// approximation, so it is not reported.
async fn blend_mode_entry<S: AsyncObjectSource>(
    src: &S,
    dict: &Dict,
) -> Option<std::result::Result<BlendMode, ()>> {
    let bm = dict.get("BM")?;
    // An array-valued `/BM` names the first mode the reader supports.
    let selected = match src.resolve(bm).await {
        Ok(Object::Name(n)) => n.0,
        Ok(Object::Array(items)) => match src.resolve(items.first()?).await {
            Ok(Object::Name(n)) => n.0,
            _ => return None,
        },
        _ => return None,
    };
    Some(match selected.as_str() {
        "Normal" | "Compatible" => Ok(BlendMode::Normal),
        "Multiply" => Ok(BlendMode::Multiply),
        "Screen" => Ok(BlendMode::Screen),
        "Overlay" => Ok(BlendMode::Overlay),
        "Darken" => Ok(BlendMode::Darken),
        "Lighten" => Ok(BlendMode::Lighten),
        "ColorDodge" => Ok(BlendMode::ColorDodge),
        "ColorBurn" => Ok(BlendMode::ColorBurn),
        "HardLight" => Ok(BlendMode::HardLight),
        "SoftLight" => Ok(BlendMode::SoftLight),
        "Difference" => Ok(BlendMode::Difference),
        "Exclusion" => Ok(BlendMode::Exclusion),
        "Hue" | "Saturation" | "Color" | "Luminosity" => Err(()),
        _ => Ok(BlendMode::Normal),
    })
}

impl<S: AsyncObjectSource> Executor<'_, S> {
    /// Executes `Do`: draws an image XObject inline, or builds the frame for
    /// a form, which the driver pushes. Every await and every skip happens
    /// here at the `Do`, so the order-sensitive report reads exactly as the
    /// recursive version wrote it.
    async fn do_xobject(
        &mut self,
        name: &Name,
        chain: &[Arc<Dict>],
        gs: &GState,
        depth: u32,
    ) -> Option<Frame> {
        let Some(Object::Stream(stream)) = self.find_res(chain, "XObject", &name.0).await else {
            // The name resolves to nothing, or to something that is not a
            // stream: whatever it was meant to draw, it is not drawn.
            self.skip(SkippedKind::XObject, SkipReason::Missing);
            return None;
        };
        // `/Subtype` may be indirect like any dictionary value (ISO 32000-1
        // 7.3.8.1): a direct name answers on the spot, a reference resolves.
        let resolved;
        let subtype = match stream.dict.get("Subtype") {
            Some(Object::Name(n)) => Some(n.0.as_str()),
            Some(indirect @ Object::Ref(_)) => {
                resolved = self.src.resolve(indirect).await.ok();
                resolved
                    .as_ref()
                    .and_then(|o| o.as_name())
                    .map(|n| n.0.as_str())
            }
            _ => None,
        };
        match subtype {
            Some("Image") => {
                self.draw_image_xobject(&stream, chain, gs).await;
                None
            }
            Some("Form") => self.form_frame(&stream, chain, gs, depth).await,
            // Neither subtype: a `/PS` XObject, or (seen in the wild) an
            // image whose dictionary omits `/Subtype` entirely.
            _ => {
                self.skip(SkippedKind::XObject, SkipReason::Unsupported);
                None
            }
        }
    }

    /// Paints every visible annotation's normal appearance over the page
    /// content (ISO 32000-1 §12.5.5). Each appearance is a form XObject
    /// whose `/BBox`, transformed by its `/Matrix`, is fitted onto the
    /// annotation's `/Rect` and then run like any other form, in default
    /// user space. Annotations flagged Hidden or NoView (§12.5.3), `/Popup`
    /// annotations (a viewer-UI artifact), and annotations with no usable
    /// normal appearance paint nothing and report nothing; an appearance
    /// that exists but cannot be read or placed reports as a dropped
    /// annotation, so a page whose visible content is a stamp or a filled
    /// form field never rasterizes blank without saying why.
    async fn paint_annotations(&mut self, page: &Page, base: Matrix) {
        let Some(annots) = page.dict().get("Annots") else {
            return;
        };
        let Ok(Object::Array(items)) = self.src.resolve(annots).await else {
            return;
        };
        let chain: Vec<Arc<Dict>> = vec![Arc::new(page.resources.clone())];
        for item in &items {
            let Ok(resolved) = self.src.resolve(item).await else {
                continue;
            };
            let Some(dict) = resolved.as_dict() else {
                continue;
            };
            if dict.get_int("F").unwrap_or(0) & INVISIBLE_ANNOTS != 0 {
                continue;
            }
            if dict.get_name("Subtype").is_some_and(|n| n.0 == "Popup") {
                continue;
            }
            let Some(stream) = self.normal_appearance(dict).await else {
                continue;
            };
            if let Some(frame) = self.appearance_frame(&stream, dict, &chain, base).await {
                self.run(frame).await;
            }
        }
    }

    /// The annotation's normal appearance stream, or `None` when it has
    /// nothing to paint. `/AP` `/N` is the appearance; when `/N` is a
    /// dictionary of states, `/AS` selects one, and a single-state
    /// dictionary needs no `/AS` to be unambiguous. A declared appearance
    /// that cannot be resolved — an `/AP` that is not a dictionary, `/N`
    /// that is neither stream nor dictionary, an `/AS` naming no state, an
    /// ambiguous stateless dictionary — is a real drop and is reported; an
    /// absent `/AP` or `/N` declares nothing and stays silent.
    async fn normal_appearance(&mut self, annot: &Dict) -> Option<Stream> {
        let ap = match annot.get("AP") {
            Some(o) => match self.src.resolve(o).await {
                Ok(Object::Dict(d)) => d,
                _ => {
                    self.skip(SkippedKind::Annotation, SkipReason::Missing);
                    return None;
                }
            },
            None => return None,
        };
        let n = ap.get("N")?;
        let states = match self.src.resolve(n).await {
            Ok(Object::Stream(s)) => return Some(s),
            Ok(Object::Dict(states)) => states,
            _ => {
                self.skip(SkippedKind::Annotation, SkipReason::Missing);
                return None;
            }
        };
        let selected = match annot.get_name("AS") {
            Some(name) => states.get(&name.0),
            None if states.len() == 1 => states.iter().map(|(_, v)| v).next(),
            None => None,
        };
        let Some(entry) = selected else {
            self.skip(SkippedKind::Annotation, SkipReason::Missing);
            return None;
        };
        match self.src.resolve(entry).await {
            Ok(Object::Stream(s)) => Some(s),
            _ => {
                self.skip(SkippedKind::Annotation, SkipReason::Missing);
                None
            }
        }
    }

    /// Builds the frame that paints one appearance stream: the form's
    /// `/BBox` corners are transformed by its `/Matrix`, their bounding box
    /// is fitted onto the annotation's normalized `/Rect` (§12.5.5's
    /// appearance algorithm), and the form runs under `Matrix ∘ fit ∘ base`
    /// with the untransformed `/BBox` as its clip. `None` reports the
    /// annotation as dropped — every bail-out here loses a declared
    /// appearance.
    async fn appearance_frame(
        &mut self,
        stream: &Stream,
        annot: &Dict,
        chain: &[Arc<Dict>],
        base: Matrix,
    ) -> Option<Frame> {
        let Some(rect) = floats_from(self.src, annot.get("Rect"), 4).await else {
            self.skip(SkippedKind::Annotation, SkipReason::Missing);
            return None;
        };
        let data = match content_stream_data_with(self.src, stream).await {
            Ok(data) => data,
            Err(e) => {
                self.skip(SkippedKind::Annotation, skip_reason_for(&e));
                return None;
            }
        };
        let ops = match parse_content(&data) {
            Ok(ops) => ops,
            Err(e) => {
                self.skip(SkippedKind::Annotation, skip_reason_for(&e));
                return None;
            }
        };
        let Some(bbox) = floats_from(self.src, stream.dict.get("BBox"), 4).await else {
            self.skip(SkippedKind::Annotation, SkipReason::Missing);
            return None;
        };
        let matrix = floats_from(self.src, stream.dict.get("Matrix"), 6)
            .await
            .map(|m| Matrix {
                a: m[0],
                b: m[1],
                c: m[2],
                d: m[3],
                e: m[4],
                f: m[5],
            })
            .unwrap_or_else(Matrix::identity);

        let (bx0, bx1) = (bbox[0].min(bbox[2]), bbox[0].max(bbox[2]));
        let (by0, by1) = (bbox[1].min(bbox[3]), bbox[1].max(bbox[3]));
        let corners = [
            matrix.apply(Point { x: bx0, y: by0 }),
            matrix.apply(Point { x: bx1, y: by0 }),
            matrix.apply(Point { x: bx0, y: by1 }),
            matrix.apply(Point { x: bx1, y: by1 }),
        ];
        let tx0 = corners.iter().map(|p| p.x).fold(f32::INFINITY, f32::min);
        let tx1 = corners
            .iter()
            .map(|p| p.x)
            .fold(f32::NEG_INFINITY, f32::max);
        let ty0 = corners.iter().map(|p| p.y).fold(f32::INFINITY, f32::min);
        let ty1 = corners
            .iter()
            .map(|p| p.y)
            .fold(f32::NEG_INFINITY, f32::max);
        let (rx0, rx1) = (rect[0].min(rect[2]), rect[0].max(rect[2]));
        let (ry0, ry1) = (rect[1].min(rect[3]), rect[1].max(rect[3]));
        // A degenerate transformed box cannot be fitted by scaling; §12.5.5
        // defines the fit through a division by its extent, so paint the
        // appearance unscaled on that axis rather than inventing geometry.
        let sx = if tx1 - tx0 > 0.0 {
            (rx1 - rx0) / (tx1 - tx0)
        } else {
            1.0
        };
        let sy = if ty1 - ty0 > 0.0 {
            (ry1 - ry0) / (ty1 - ty0)
        } else {
            1.0
        };
        let fit = Matrix {
            a: sx,
            b: 0.0,
            c: 0.0,
            d: sy,
            e: rx0 - tx0 * sx,
            f: ry0 - ty0 * sy,
        };
        let ctm = matrix.concat(fit).concat(base);
        if !finite_matrix(&ctm) {
            self.skip(SkippedKind::Annotation, SkipReason::Undecodable);
            return None;
        }

        let mut gs = GState::new(ctm);
        let mut pb = PathBuilder::new(ctm);
        pb.rect(bx0, by0, bx1 - bx0, by1 - by0);
        gs.clip = Some(self.rasterize_clip(&pb.finish(), FillRule::NonZero));

        let own_res = match stream.dict.get("Resources") {
            Some(o) => match self.src.resolve(o).await {
                Ok(Object::Dict(d)) => Some(d),
                _ => None,
            },
            None => None,
        };
        let mut inner_chain: Vec<Arc<Dict>> = Vec::with_capacity(chain.len() + 1);
        if let Some(d) = own_res {
            inner_chain.push(Arc::new(d));
        }
        inner_chain.extend_from_slice(chain);
        Some(Frame::new(
            ops.into(),
            inner_chain,
            gs,
            0,
            FrameKind::PageOrForm,
        ))
    }

    /// Builds the frame for a form XObject: `/Matrix` concatenated before
    /// the CTM, `/BBox` intersected into the clip, own `/Resources` prepended
    /// to the chain, depth-bounded. `None` where the recursive version bailed
    /// out, with the identical report entry.
    async fn form_frame(
        &mut self,
        stream: &Stream,
        chain: &[Arc<Dict>],
        gs: &GState,
        depth: u32,
    ) -> Option<Frame> {
        // Every bail-out below drops the form's whole content subtree --
        // images, shadings and nested forms included -- so each one is
        // reported rather than leaving a hole nobody can account for.
        if depth >= MAX_FORM_DEPTH {
            self.skip(SkippedKind::Form, SkipReason::LimitExceeded);
            return None;
        }
        // Through the content chokepoint, not raw `stream_data`: a form
        // whose trailing filter is an image codec holds passthrough bytes
        // no content parser may read (ISO 32000-1 7.4.9), and the refusal
        // reports like any other unsupported filter.
        let data = match content_stream_data_with(self.src, stream).await {
            Ok(data) => data,
            Err(e) => {
                self.skip(SkippedKind::Form, skip_reason_for(&e));
                return None;
            }
        };
        let ops = match parse_content(&data) {
            Ok(ops) => ops,
            Err(e) => {
                self.skip(SkippedKind::Form, skip_reason_for(&e));
                return None;
            }
        };
        let mut inner = gs.clone();
        if let Some(m) = floats_from(self.src, stream.dict.get("Matrix"), 6).await {
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
        if let Some(b) = floats_from(self.src, stream.dict.get("BBox"), 4).await {
            let (x0, x1) = (b[0].min(b[2]), b[0].max(b[2]));
            let (y0, y1) = (b[1].min(b[3]), b[1].max(b[3]));
            let mut pb = PathBuilder::new(inner.ctm);
            pb.rect(x0, y0, x1 - x0, y1 - y0);
            let rasterized = self.rasterize_clip(&pb.finish(), FillRule::NonZero);
            inner.clip = Some(match &inner.clip {
                Some(old) => Arc::new(Mask::intersected(&rasterized, old)),
                None => rasterized,
            });
        }
        let own_res = match stream.dict.get("Resources") {
            Some(o) => match self.src.resolve(o).await {
                Ok(Object::Dict(d)) => Some(d),
                _ => None,
            },
            None => None,
        };
        let mut inner_chain: Vec<Arc<Dict>> = Vec::with_capacity(chain.len() + 1);
        if let Some(d) = own_res {
            inner_chain.push(Arc::new(d));
        }
        inner_chain.extend_from_slice(chain);
        Some(Frame::new(
            ops.into(),
            inner_chain,
            inner,
            depth + 1,
            FrameKind::PageOrForm,
        ))
    }

    /// Records one piece of content this render could not reproduce.
    fn skip(&mut self, kind: SkippedKind, reason: SkipReason) {
        self.report.record(kind, reason);
    }

    /// Draws an image XObject with the current CTM/clip/alpha; the fill
    /// color paints through `/ImageMask` stencils.
    async fn draw_image_xobject(&mut self, stream: &Stream, chain: &[Arc<Dict>], gs: &GState) {
        let data = match self.src.stream_data(stream).await {
            Ok(data) => data,
            Err(e) => {
                self.skip(SkippedKind::Image, skip_reason_for(&e));
                return;
            }
        };
        let cs_obj = self.image_colorspace(&stream.dict, chain).await;
        self.blit_image(&stream.dict, &data, cs_obj, gs).await;
    }

    /// Draws an inline image: its filters (abbreviations included) are
    /// applied here, then it follows the XObject path.
    async fn draw_inline_image(&mut self, img: &ImageParams, chain: &[Arc<Dict>], gs: &GState) {
        let stream = Stream {
            dict: img.dict.clone(),
            data: img.data.clone(),
        };
        // `stream_data` on a synthetic stream applies exactly the filter
        // chain `decode_stream` would: the synchronous `Document::stream_data`
        // IS `decode_stream(s, self)`, and an asynchronous source decodes the
        // same way.
        let data = match self.src.stream_data(&stream).await {
            Ok(data) => data,
            Err(e) => {
                self.skip(SkippedKind::Image, skip_reason_for(&e));
                return;
            }
        };
        let cs_obj = self.image_colorspace(&img.dict, chain).await;
        self.blit_image(&img.dict, &data, cs_obj, gs).await;
    }

    /// The per-sample alpha an image's `/SMask` or `/Mask` entry asks for
    /// (`/SMask` wins when both are present, §8.9.6.4). A mask that exists
    /// but cannot be honored reports as an ignored soft mask and the image
    /// draws unmasked — exactly the pre-mask behavior, now the exception
    /// instead of the rule.
    async fn image_alpha_mask(
        &mut self,
        dict: &Dict,
        base: &image::ImageMeta,
        base_data: &[u8],
    ) -> Option<image::SampleMask> {
        if let Some(obj) = dict.get("SMask") {
            match self.src.resolve(obj).await {
                Ok(Object::Stream(s)) => {
                    if s.dict.get("Matte").is_some() {
                        // Pre-blended matte colors are not un-blended here;
                        // the alpha still applies, the colors approximate.
                        self.skip(SkippedKind::SoftMask, SkipReason::Unsupported);
                    }
                    let data = match self.src.stream_data(&s).await {
                        Ok(data) => data,
                        Err(e) => {
                            self.skip(SkippedKind::SoftMask, skip_reason_for(&e));
                            return None;
                        }
                    };
                    let cs_obj = s.dict.get("ColorSpace").cloned();
                    let meta =
                        image::ImageMeta::read_with(self.src, &s.dict, cs_obj.as_ref()).await;
                    let mask = image::decode_alpha(&meta, &data);
                    if mask.is_none() {
                        self.skip(SkippedKind::SoftMask, SkipReason::Undecodable);
                    }
                    return mask;
                }
                Ok(Object::Name(n)) if n.0 == "None" => {}
                Ok(Object::Null) => {}
                _ => {
                    self.skip(SkippedKind::SoftMask, SkipReason::Missing);
                    return None;
                }
            }
        }
        match dict.get("Mask") {
            None => None,
            Some(obj) => match self.src.resolve(obj).await {
                Ok(Object::Stream(s)) => {
                    let data = match self.src.stream_data(&s).await {
                        Ok(data) => data,
                        Err(e) => {
                            self.skip(SkippedKind::SoftMask, skip_reason_for(&e));
                            return None;
                        }
                    };
                    let meta = image::ImageMeta::read_with(self.src, &s.dict, None).await;
                    if !meta.stencil {
                        // A stream-valued /Mask must be a stencil (§8.9.6.4).
                        self.skip(SkippedKind::SoftMask, SkipReason::Undecodable);
                        return None;
                    }
                    let mask = image::decode_alpha(&meta, &data);
                    if mask.is_none() {
                        self.skip(SkippedKind::SoftMask, SkipReason::Undecodable);
                    }
                    mask
                }
                Ok(Object::Array(items)) => {
                    let mut key = Vec::with_capacity(items.len());
                    for item in &items {
                        match self.src.resolve(item).await.ok().and_then(|o| o.as_f64()) {
                            Some(v) => key.push(v as i64),
                            None => {
                                self.skip(SkippedKind::SoftMask, SkipReason::Undecodable);
                                return None;
                            }
                        }
                    }
                    let mask = image::color_key_mask(base, base_data, &key);
                    if mask.is_none() {
                        self.skip(SkippedKind::SoftMask, SkipReason::Unsupported);
                    }
                    mask
                }
                Ok(Object::Null) => None,
                _ => {
                    self.skip(SkippedKind::SoftMask, SkipReason::Missing);
                    None
                }
            },
        }
    }

    async fn blit_image(&mut self, dict: &Dict, data: &[u8], cs_obj: Option<Object>, gs: &GState) {
        // `data` is decoded samples, a raw JPEG, or a JPEG 2000 file: the
        // filter chain passes only `DCTDecode` and `JPXDecode` through
        // (ISO 32000-1 7.4.9) and rejects every other codec, so nothing
        // reaches the sample reader that the image layer cannot decode.
        // That rejection surfaces above, where `stream_data` fails.
        //
        // An `/Indexed` palette stored as a stream that will not decode
        // leaves the space with no palette at all, painting every sample
        // black; the image still draws, but not as the page describes it.
        let palette_err = match cs_obj.as_ref() {
            Some(o) => color::palette_error_with(self.src, o).await,
            None => None,
        };
        if let Some(e) = palette_err {
            self.skip(SkippedKind::Image, skip_reason_for(&e));
        }
        let meta = image::ImageMeta::read_with(self.src, dict, cs_obj.as_ref()).await;
        let smask = self.image_alpha_mask(dict, &meta, data).await;
        if gs.fill_pattern && meta.stencil {
            // The stencil paints the pattern's stand-in gray, not the
            // pattern (see `GState::fill_rgba8`).
            self.skip(SkippedKind::Pattern, SkipReason::Unsupported);
        }
        let fill = gs.fill_rgba8();
        let outcome = image::draw(
            &mut self.pix,
            &meta,
            data,
            &DrawParams {
                ctm: gs.ctm,
                alpha: gs.fill_alpha,
                fill_rgb: [fill[0], fill[1], fill[2]],
                clip: gs.clip.as_deref(),
                blend: gs.blend_mode,
                smask: smask.as_ref(),
            },
        );
        match outcome {
            image::Drawn::Whole => {}
            image::Drawn::Truncated => self.skip(SkippedKind::Image, SkipReason::Truncated),
            image::Drawn::Nothing => self.skip(SkippedKind::Image, SkipReason::Undecodable),
            image::Drawn::Failed(reason) => {
                self.skip(SkippedKind::Image, SkipReason::DecodeFailed(reason))
            }
            // The image painted, but not whole: one entry per distinct loss,
            // so the caller knows the render is an approximation.
            image::Drawn::Degraded(notes) => {
                for note in notes {
                    self.skip(SkippedKind::Image, SkipReason::DecodeFailed(note));
                }
            }
        }
    }

    /// The image's `/ColorSpace` value with resource-name indirection
    /// resolved: a non-device name is looked up in `/ColorSpace` resources.
    async fn image_colorspace(&self, dict: &Dict, chain: &[Arc<Dict>]) -> Option<Object> {
        let resolved = self.src.resolve(dict.get("ColorSpace")?).await.ok()?;
        if let Object::Name(n) = &resolved {
            let device = matches!(
                n.0.as_str(),
                "DeviceGray" | "DeviceRGB" | "DeviceCMYK" | "G" | "RGB" | "CMYK"
            );
            if !device {
                if let Some(from_res) = self.find_res(chain, "ColorSpace", &n.0).await {
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
    // The crate-root wrapper, exercised here so these tests keep covering the
    // exact entry point external callers use.
    use crate::render_page_with_options;
    use crate::type1::tests::build_type1_box_fixture;
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
                ..Default::default()
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

    /// Wraps `raw` in a zlib stream (RFC 1950) carrying a single stored
    /// (uncompressed) deflate block (RFC 1951 §3.2.4) — genuine
    /// `/FlateDecode` input without a compressor in this crate.
    fn zlib_stored(raw: &[u8]) -> Vec<u8> {
        // CMF 0x78 (deflate, 32K window) with FLG 0x01: no preset dictionary
        // and (0x78 << 8) | 0x01 is the multiple of 31 the header check wants.
        let mut out = vec![0x78, 0x01];
        let len = raw.len() as u16;
        out.push(0x01); // BFINAL = 1, BTYPE = 00 (stored)
        out.extend_from_slice(&len.to_le_bytes());
        out.extend_from_slice(&(!len).to_le_bytes());
        out.extend_from_slice(raw);
        // Adler-32 of the uncompressed data, big-endian.
        let (mut low, mut high) = (1u32, 0u32);
        for &byte in raw {
            low = (low + u32::from(byte)) % 65521;
            high = (high + low) % 65521;
        }
        out.extend_from_slice(&((high << 16) | low).to_be_bytes());
        out
    }

    /// A one-page document whose only content is an 8x8 one-bit gray image
    /// XObject carrying `/Filter /<filter>`. `FlateDecode` gets genuinely
    /// encoded samples; any other name gets bytes that filter never reads.
    fn doc_with_image_filter(filter: &str) -> Vec<u8> {
        let samples = [0b1010_1010u8; 8];
        let data = if filter == "FlateDecode" {
            zlib_stored(&samples)
        } else {
            samples.to_vec()
        };
        small_doc(
            "/XObject << /Im0 5 0 R >>",
            b"q 100 0 0 100 0 0 cm /Im0 Do Q",
            |b| {
                b.stream(
                    5,
                    &format!(
                        "/Type /XObject /Subtype /Image /Width 8 /Height 8 \
                         /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /{filter}"
                    ),
                    &data,
                );
            },
        )
    }

    /// Renders page 0 of `bytes` with the default options, returning the
    /// pixmap and the report of everything the render had to drop.
    fn render_reporting(bytes: Vec<u8>) -> (Pixmap, RenderReport) {
        let doc = Document::load(bytes).expect("load");
        let page = doc.page(0).expect("page 0");
        render_page_reporting(&doc, &page, 1.0, &RenderOptions::default())
            .expect("render succeeds despite any dropped content")
    }

    /// The report's entries as `(kind, reason, count)` triples, the shape
    /// most of these assertions want to compare against.
    fn drops(report: &RenderReport) -> Vec<(SkippedKind, SkipReason, u64)> {
        report
            .skipped
            .iter()
            .map(|item| (item.kind, item.reason.clone(), item.count))
            .collect()
    }

    /// A one-page 200x200 document showing `content` with `/F0`, a simple
    /// `/Type1` font over the embedded box-glyph program. Only code 128 maps
    /// to a glyph (via `/Differences`); every other code resolves to gid 0.
    fn doc_with_type1_box_font(content: &[u8]) -> Vec<u8> {
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
            "<< /Type /Font /Subtype /Type1 /BaseFont /TheBoxFont \
             /FontDescriptor 6 0 R \
             /Encoding << /Differences [128 /theboxglyphname] >> >>",
        );
        b.object(
            6,
            "<< /Type /FontDescriptor /FontName /TheBoxFont /Flags 4 /FontFile 7 0 R >>",
        );
        b.stream(7, "", &build_type1_box_fixture("theboxglyphname"));
        b.build(1)
    }

    #[test]
    fn code_without_a_glyph_in_a_loaded_font_is_reported() {
        // Code 65 maps to no glyph in the box font: the show operator
        // advances but paints nothing, and the report must say so, naming
        // both the code and the font so the warning is actionable.
        let (_, report) = render_reporting(doc_with_type1_box_font(
            b"BT /F0 100 Tf 20 50 Td <41> Tj ET",
        ));
        assert_eq!(
            drops(&report),
            vec![(
                SkippedKind::Glyph,
                SkipReason::NoGlyph {
                    code: 65,
                    font: "TheBoxFont".to_string(),
                },
                1
            )]
        );
        assert_eq!(
            report.warnings(),
            vec!["1 glyph skipped: no glyph for code 65 in /TheBoxFont"]
        );
    }

    #[test]
    fn repeated_unmappable_code_is_one_entry_with_the_count() {
        // The same unmappable code three times must merge into one entry
        // counted 3, not three lines.
        let (_, report) = render_reporting(doc_with_type1_box_font(
            b"BT /F0 100 Tf 20 50 Td <414141> Tj ET",
        ));
        assert_eq!(
            drops(&report),
            vec![(
                SkippedKind::Glyph,
                SkipReason::NoGlyph {
                    code: 65,
                    font: "TheBoxFont".to_string(),
                },
                3
            )]
        );
        assert_eq!(
            report.warnings(),
            vec!["3 glyphs skipped: no glyph for code 65 in /TheBoxFont"]
        );
    }

    #[test]
    fn drawn_glyph_and_single_byte_space_report_nothing() {
        // Code 128 paints the box glyph; the single-byte space (code 32)
        // maps to no glyph but would paint nothing even if the font mapped
        // it, so a warning there would be pure noise.
        let (pix, report) = render_reporting(doc_with_type1_box_font(
            b"BT /F0 100 Tf 20 50 Td <8020> Tj ET",
        ));
        // (55,115) is the known interior point of the box glyph shown at
        // 100pt from origin (20,50) on a 200x200 page.
        let interior = ((115 * pix.width + 55) * 4) as usize;
        assert!(pix.data[interior] < 128, "the mapped box glyph must paint");
        assert!(
            report.is_empty(),
            "a drawn glyph and a bare space are clean: {:?}",
            report.warnings()
        );
    }

    #[test]
    fn unsupported_image_filter_is_reported() {
        // The page's only content is `/Im0 Do`, where Im0 carries a filter
        // the core does not implement. The page must still render (lenient),
        // but the drop must be reported.
        let (pix, report) = render_reporting(doc_with_image_filter("Crypt"));

        assert!(pix.width > 0 && pix.height > 0, "page still rasterizes");
        assert_eq!(
            drops(&report),
            vec![(
                SkippedKind::Image,
                SkipReason::UnsupportedFilter("Crypt".to_string()),
                1,
            )],
        );
        assert!(!report.is_empty());
        assert_eq!(report.summary().as_deref(), Some("1 image skipped"));
        assert_eq!(
            report.warnings(),
            vec!["1 image skipped: unsupported filter /Crypt".to_string()],
        );
    }

    #[test]
    fn clean_page_reports_nothing() {
        let (pix, report) = render_reporting(doc_with_image_filter("FlateDecode"));
        // The 0b10101010 rows alternate white/black across the 8 columns the
        // image stretches over the 100pt page -- proof the image really
        // painted, so the empty report is not vacuous.
        assert_eq!(px(&pix, 6, 50), WHITE, "column 0 sample is white");
        assert_eq!(px(&pix, 18, 50), BLACK, "column 1 sample is black");
        assert!(report.is_empty(), "a decodable image reports no skips");
        assert_eq!(report.summary(), None);
        assert!(report.warnings().is_empty());
    }

    #[test]
    fn unsupported_inline_image_filter_is_reported() {
        let content = "q 100 0 0 100 0 0 cm BI /W 8 /H 8 /BPC 1 /CS /G \
                       /F /Crypt ID 01234567 EI Q";
        let (_, report) = render_reporting(small_doc("", content.as_bytes(), |_| {}));
        assert_eq!(
            drops(&report),
            vec![(
                SkippedKind::Image,
                SkipReason::UnsupportedFilter("Crypt".to_string()),
                1,
            )],
        );
    }

    #[test]
    fn image_that_decodes_but_cannot_be_interpreted_is_reported() {
        // Filters apply cleanly (there are none); `/Width 0` makes the
        // samples uninterpretable as an image.
        let bytes = small_doc(
            "/XObject << /Im0 5 0 R >>",
            b"q 100 0 0 100 0 0 cm /Im0 Do Q",
            |b| {
                b.stream(
                    5,
                    "/Type /XObject /Subtype /Image /Width 0 /Height 8 \
                     /BitsPerComponent 1 /ColorSpace /DeviceGray",
                    &[0; 8],
                );
            },
        );
        let (_, report) = render_reporting(bytes);
        assert_eq!(
            drops(&report),
            vec![(SkippedKind::Image, SkipReason::Undecodable, 1)],
        );
        assert_eq!(report.summary().as_deref(), Some("1 image skipped"));
    }

    #[test]
    fn a_repeated_drop_costs_one_entry_and_counts_up() {
        // Two draws of the same broken image are one entry with count 2 --
        // the property that keeps a page drawing a million of them from
        // growing the report a million times over.
        let content = "q 100 0 0 100 0 0 cm /Im0 Do /Im0 Do Q";
        let bytes = small_doc("/XObject << /Im0 5 0 R >>", content.as_bytes(), |b| {
            b.stream(
                5,
                "/Type /XObject /Subtype /Image /Width 8 /Height 8 \
                 /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /Crypt",
                &[0; 8],
            );
        });
        let (_, report) = render_reporting(bytes);
        assert_eq!(report.skipped.len(), 1, "one entry, not one per draw");
        assert_eq!(report.skipped[0].count, 2);
        assert_eq!(report.summary().as_deref(), Some("2 images skipped"));
    }

    #[test]
    fn nested_forms_repeating_a_broken_image_keep_the_report_small() {
        // Four levels of forms with a fanout of ten each draw the same
        // undecodable image, so `/Im0 Do` runs 10,000 times from a document
        // of a few hundred bytes. The report must stay one entry: an entry
        // per draw would let a page amplify a caller's memory use by the
        // fanout, on the plain `render_page` path that throws the report
        // away.
        const LEVELS: u32 = 4;
        const FANOUT: u32 = 10;
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] \
             /Resources << /XObject << /F0 10 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"/F0 Do");
        b.stream(
            5,
            "/Type /XObject /Subtype /Image /Width 8 /Height 8 \
             /BitsPerComponent 1 /ColorSpace /DeviceGray /Filter /Crypt",
            &[0; 8],
        );
        for level in 0..LEVELS {
            let (child, child_obj) = if level + 1 < LEVELS {
                (format!("F{}", level + 1), 11 + level)
            } else {
                ("Im0".to_string(), 5)
            };
            let content = format!("/{child} Do ").repeat(FANOUT as usize);
            b.stream(
                10 + level,
                &format!(
                    "/Type /XObject /Subtype /Form /BBox [0 0 100 100] \
                     /Resources << /XObject << /{child} {child_obj} 0 R >> >>"
                ),
                content.as_bytes(),
            );
        }
        let (_, report) = render_reporting(b.build(1));
        assert_eq!(report.skipped.len(), 1, "one entry for 10,000 draws");
        assert_eq!(report.skipped[0].count, u64::from(FANOUT.pow(LEVELS)));
        assert_eq!(report.unlisted, 0);
    }

    #[test]
    fn distinct_drops_stop_at_the_report_cap() {
        // 70 inline images, each naming a different unsupported filter, so
        // every drop is a distinct entry. The list stops at 64 and the rest
        // are counted -- an unbounded `Vec` would have taken all 70.
        let mut content = String::new();
        for i in 0..70 {
            content.push_str(&format!(
                "BI /W 8 /H 8 /BPC 1 /CS /G /F /Bogus{i}Decode ID 01234567 EI\n"
            ));
        }
        let (_, report) = render_reporting(small_doc("", content.as_bytes(), |_| {}));
        assert_eq!(report.skipped.len(), 64, "entry list is capped");
        assert_eq!(report.unlisted, 6, "the rest are counted, not described");
        assert!(report
            .warnings()
            .last()
            .expect("a warning per entry plus the overflow line")
            .starts_with("6 further drops"));
    }

    #[test]
    fn undecodable_page_contents_are_reported() {
        // The page's own `/Contents` names a filter the core cannot run:
        // the page renders blank, which must not look like a clean render.
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>",
        );
        b.stream(4, "/Filter /Crypt", b"0 0 100 100 re f");
        let (pix, report) = render_reporting(b.build(1));
        assert_eq!(px(&pix, 50, 50), WHITE, "nothing painted");
        assert_eq!(
            drops(&report),
            vec![(
                SkippedKind::PageContents,
                SkipReason::UnsupportedFilter("Crypt".to_string()),
                1,
            )],
        );
        assert_eq!(
            report.summary().as_deref(),
            Some("1 content stream skipped")
        );
    }

    #[test]
    fn undecodable_form_xobject_is_reported() {
        // The form's content -- and everything it would have drawn -- is
        // dropped whole, one level below where an image drop is caught.
        let bytes = small_doc("/XObject << /Fm0 5 0 R >>", b"/Fm0 Do", |b| {
            b.stream(
                5,
                "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Filter /Crypt",
                b"0 0 100 100 re f",
            );
        });
        let (pix, report) = render_reporting(bytes);
        assert_eq!(px(&pix, 50, 50), WHITE, "the form painted nothing");
        assert_eq!(
            drops(&report),
            vec![(
                SkippedKind::Form,
                SkipReason::UnsupportedFilter("Crypt".to_string()),
                1,
            )],
        );
    }

    #[test]
    fn jpx_labelled_page_contents_are_refused_not_parsed() {
        // The filter chain passes JPXDecode bytes through still encoded for
        // the IMAGE layer (ISO 32000-1 7.4.9); a page /Contents stream so
        // labelled must be refused, never handed to the content parser.
        // The stream below is deliberately valid operator syntax, so a
        // renderer that parses the passthrough WOULD paint the page red:
        // white pixels are the proof the bytes were refused, not chewed.
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R >>",
        );
        b.stream(4, "/Filter /JPXDecode", b"1 0 0 rg 0 0 100 100 re f");
        let (pix, report) = render_reporting(b.build(1));
        assert_eq!(px(&pix, 50, 50), WHITE, "nothing painted");
        assert_eq!(
            drops(&report),
            vec![(
                SkippedKind::PageContents,
                SkipReason::UnsupportedFilter("JPXDecode".to_string()),
                1,
            )],
        );
    }

    #[test]
    fn dct_labelled_form_content_is_refused_not_parsed() {
        // The same wart with the other passthrough codec, one level down: a
        // form XObject whose trailing filter is DCTDecode holds raw JPEG
        // bytes no content parser may read.
        let bytes = small_doc("/XObject << /Fm0 5 0 R >>", b"/Fm0 Do", |b| {
            b.stream(
                5,
                "/Type /XObject /Subtype /Form /BBox [0 0 100 100] /Filter /DCTDecode",
                b"1 0 0 rg 0 0 100 100 re f",
            );
        });
        let (pix, report) = render_reporting(bytes);
        assert_eq!(px(&pix, 50, 50), WHITE, "the form painted nothing");
        assert_eq!(
            drops(&report),
            vec![(
                SkippedKind::Form,
                SkipReason::UnsupportedFilter("DCTDecode".to_string()),
                1,
            )],
        );
    }

    #[test]
    fn jpx_labelled_charproc_is_skipped_not_parsed() {
        // A Type3 CharProc is the third content consumer. Its failures are
        // silent by contract (the glyph advances unpainted, like every other
        // CharProc fetch failure), so the proof is pixels alone: the box the
        // ops would paint stays white.
        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F0 100 Tf 20 50 Td <41> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
             /FontMatrix [0.001 0 0 0.001 0 0] \
             /Encoding << /Differences [65 /boxglyph] >> \
             /CharProcs << /boxglyph 6 0 R >> /FirstChar 65 /Widths [1000] >>",
        );
        b.stream(6, "/Filter /JPXDecode", b"1000 0 d0 100 0 500 700 re f");
        let (pix, _) = render_reporting(b.build(1));
        assert!(
            !dark_at(&pix, 55, 115),
            "the CharProc bytes must not be parsed as content"
        );
    }

    /// `/Subtype` may be indirect like any dictionary value (ISO 32000-1
    /// 7.3.8.1). The XObject dispatch resolves it: a form declared through
    /// a reference paints, rather than being skipped as an unsupported
    /// XObject with its whole content subtree.
    #[test]
    fn a_form_whose_subtype_is_indirect_still_paints() {
        let bytes = small_doc("/XObject << /Fm0 5 0 R >>", b"/Fm0 Do", |b| {
            b.stream(
                5,
                "/Type /XObject /Subtype 6 0 R /BBox [0 0 100 100]",
                b"1 0 0 rg 0 0 100 100 re f",
            );
            b.object(6, "/Form");
        });
        let (pix, report) = render_reporting(bytes);
        assert_ne!(px(&pix, 50, 50), WHITE, "the form painted");
        assert!(drops(&report).is_empty(), "nothing to report");
    }

    #[test]
    fn unresolvable_and_untyped_xobjects_are_reported() {
        // `/Im0` is not in the resource dictionary at all; `/X1` is a stream
        // with no `/Subtype`, so nothing knows how to draw it.
        let bytes = small_doc("/XObject << /X1 5 0 R >>", b"/Im0 Do /X1 Do", |b| {
            b.stream(5, "/Width 8 /Height 8", &[0; 8]);
        });
        let (_, report) = render_reporting(bytes);
        assert_eq!(
            drops(&report),
            vec![
                (SkippedKind::XObject, SkipReason::Missing, 1),
                (SkippedKind::XObject, SkipReason::Unsupported, 1),
            ],
        );
    }

    #[test]
    fn a_garbage_jpx_stream_is_reported_instead_of_painted_as_noise() {
        // The filter chain passes JPXDecode through for the image layer to
        // decode (ISO 32000-1 7.4.9). The 0x42 bytes below are no JPEG 2000
        // file, so the decode fails; were they painted as gray samples the
        // render would claim success, so the drop must carry the decoder's
        // reason instead.
        let bytes = small_doc(
            "/XObject << /Im0 5 0 R >>",
            b"q 100 0 0 100 0 0 cm /Im0 Do Q",
            |b| {
                b.stream(
                    5,
                    "/Type /XObject /Subtype /Image /Width 8 /Height 8 \
                     /BitsPerComponent 8 /ColorSpace /DeviceRGB /Filter /JPXDecode",
                    &[0x42; 192],
                );
            },
        );
        let (pix, report) = render_reporting(bytes);
        assert_eq!(px(&pix, 50, 50), WHITE, "no noise painted");
        assert_eq!(report.skipped.len(), 1);
        let drop = &report.skipped[0];
        assert_eq!((drop.kind, drop.count), (SkippedKind::Image, 1));
        assert!(
            matches!(&drop.reason, SkipReason::DecodeFailed(msg) if msg.contains("JPXDecode")),
            "the caller can name what went missing: {:?}",
            drop.reason
        );
    }

    #[test]
    fn image_with_too_few_samples_is_reported() {
        // 8x8 at 8 bits gray needs 64 bytes; 4 are supplied, so 60 pixels
        // come from zero padding rather than from the image.
        let bytes = small_doc(
            "/XObject << /Im0 5 0 R >>",
            b"q 100 0 0 100 0 0 cm /Im0 Do Q",
            |b| {
                b.stream(
                    5,
                    "/Type /XObject /Subtype /Image /Width 8 /Height 8 \
                     /BitsPerComponent 8 /ColorSpace /DeviceGray",
                    &[0xFF; 4],
                );
            },
        );
        let (pix, report) = render_reporting(bytes);
        assert_eq!(px(&pix, 6, 6), [255, 255, 255, 255], "real sample painted");
        assert_eq!(px(&pix, 50, 50), BLACK, "padding painted black");
        assert_eq!(
            drops(&report),
            vec![(SkippedKind::Image, SkipReason::Truncated, 1)],
        );
    }

    #[test]
    fn indexed_image_with_undecodable_palette_is_reported() {
        // The palette stream will not decode, so the space has no colors at
        // all and every sample paints black -- a plausible-looking image
        // that is not the page's image.
        let bytes = small_doc(
            "/XObject << /Im0 5 0 R >>",
            b"q 100 0 0 100 0 0 cm /Im0 Do Q",
            |b| {
                b.stream(
                    5,
                    "/Type /XObject /Subtype /Image /Width 8 /Height 8 \
                     /BitsPerComponent 8 /ColorSpace [/Indexed /DeviceRGB 255 6 0 R]",
                    &[0; 64],
                );
                b.stream(6, "/Filter /Crypt", &[0; 12]);
            },
        );
        let (_, report) = render_reporting(bytes);
        assert_eq!(
            drops(&report),
            vec![(
                SkippedKind::Image,
                SkipReason::UnsupportedFilter("Crypt".to_string()),
                1,
            )],
        );
    }

    #[test]
    fn shading_operator_is_reported() {
        let (pix, report) = render_reporting(small_doc("", b"q /Sh0 sh Q", |_| {}));
        assert_eq!(px(&pix, 50, 50), WHITE, "shadings paint nothing");
        assert_eq!(
            drops(&report),
            vec![(SkippedKind::Shading, SkipReason::Unsupported, 1)],
        );
    }

    #[test]
    fn pattern_fill_is_reported_as_an_approximation() {
        let content = b"/Pattern cs /P0 scn 0 0 100 100 re f";
        let (pix, report) = render_reporting(small_doc("", content, |_| {}));
        assert_eq!(px(&pix, 50, 50), [128, 128, 128, 255], "stand-in gray");
        assert_eq!(
            drops(&report),
            vec![(SkippedKind::Pattern, SkipReason::Unsupported, 1)],
        );
    }

    #[test]
    fn ignored_soft_mask_and_nonseparable_blend_are_reported() {
        // A group soft mask is still ignored and reported; so is a
        // NON-separable blend mode (/Hue). The separable ones paint (see
        // the blend tests below) and are no longer drops.
        let resources = "/ExtGState << /GS0 << /SMask << /S /Luminosity /G 5 0 R >> \
                         /BM /Hue >> >>";
        let bytes = small_doc(resources, b"/GS0 gs 0 0 100 100 re f", |b| {
            b.stream(5, "/Type /XObject /Subtype /Form /BBox [0 0 8 8]", b"");
        });
        let (_, report) = render_reporting(bytes);
        assert_eq!(
            drops(&report),
            vec![
                (SkippedKind::SoftMask, SkipReason::Unsupported, 1),
                (SkippedKind::BlendMode, SkipReason::Unsupported, 1),
            ],
        );
    }

    #[test]
    fn stencil_mask_stream_hides_where_it_is_one() {
        // A /Mask stencil whose top half is all ones: those samples of the
        // black base image are not painted (§8.9.6.4).
        let mut mask = [0u8; 8]; // 8 rows x 8 one-BIT samples = 1 byte/row
        mask[..4].fill(0xFF); // stencil rows are top-first
        let bytes = small_doc(
            "/XObject << /Im0 5 0 R >>",
            b"q 100 0 0 100 0 0 cm /Im0 Do Q",
            |b| {
                b.stream(
                    5,
                    "/Type /XObject /Subtype /Image /Width 8 /Height 8 \
                     /BitsPerComponent 8 /ColorSpace /DeviceGray /Mask 6 0 R",
                    &[0x00; 64],
                );
                b.stream(
                    6,
                    "/Type /XObject /Subtype /Image /ImageMask true \
                     /Width 8 /Height 8 /BitsPerComponent 1",
                    &mask,
                );
            },
        );
        let (pix, report) = render_reporting(bytes);
        assert_eq!(px(&pix, 50, 25), WHITE, "mask=1 rows stay unpainted");
        assert_eq!(px(&pix, 50, 75), [0, 0, 0, 255], "mask=0 rows paint");
        assert!(report.is_empty(), "an applied stencil mask is not a drop");
    }

    #[test]
    fn color_key_mask_hides_matching_samples() {
        // /Mask [0 32]: the dark half of a two-tone gray image becomes
        // transparent, the light half paints.
        let mut samples = [0u8; 64];
        for row in samples.chunks_mut(8) {
            row[4..].fill(0xC0);
        }
        let bytes = small_doc(
            "/XObject << /Im0 5 0 R >>",
            b"q 100 0 0 100 0 0 cm /Im0 Do Q",
            |b| {
                b.stream(
                    5,
                    "/Type /XObject /Subtype /Image /Width 8 /Height 8 \
                     /BitsPerComponent 8 /ColorSpace /DeviceGray /Mask [0 32]",
                    &samples,
                );
            },
        );
        let (pix, report) = render_reporting(bytes);
        assert_eq!(px(&pix, 25, 50), WHITE, "keyed-out samples are transparent");
        assert_eq!(px(&pix, 75, 50), [192, 192, 192, 255], "others paint");
        assert!(report.is_empty(), "an applied color key is not a drop");
    }

    #[test]
    fn multiply_blend_darkens_the_overlap() {
        // A red square, then a blue square multiplied over it: the overlap
        // multiplies to black, the blue-only region lands on white where
        // Multiply degenerates to Normal, and the red-only region is
        // untouched. Painting the overlap plain blue is the pre-blend bug.
        let resources = "/ExtGState << /GS0 << /BM /Multiply >> >>";
        let content = b"1 0 0 rg 0 0 60 60 re f /GS0 gs 0 0 1 rg 30 30 60 60 re f";
        let (pix, report) = render_reporting(small_doc(resources, content, |_| {}));
        assert!(report.is_empty(), "separable blends are not drops");
        assert_eq!(
            px(&pix, 45, 55),
            [0, 0, 0, 255],
            "overlap multiplies to black"
        );
        assert_eq!(
            px(&pix, 75, 15),
            [0, 0, 255, 255],
            "blue over white stays blue"
        );
        assert_eq!(px(&pix, 10, 89), RED, "red-only region untouched");
    }

    #[test]
    fn blend_mode_array_takes_the_first_recognized_name() {
        let resources = "/ExtGState << /GS0 << /BM [/Multiply /Normal] >> >>";
        let content = b"1 0 0 rg 0 0 60 60 re f /GS0 gs 0 0 1 rg 30 30 60 60 re f";
        let (pix, report) = render_reporting(small_doc(resources, content, |_| {}));
        assert!(report.is_empty());
        assert_eq!(px(&pix, 45, 55), [0, 0, 0, 255], "array form blends too");
    }

    #[test]
    fn screen_blend_lightens_the_overlap() {
        // Screen of red and blue is magenta: 1-(1-r)(1-b) per channel.
        let resources = "/ExtGState << /GS0 << /BM /Screen >> >>";
        let content = b"1 0 0 rg 0 0 60 60 re f /GS0 gs 0 0 1 rg 30 30 60 60 re f";
        let (pix, report) = render_reporting(small_doc(resources, content, |_| {}));
        assert!(report.is_empty());
        assert_eq!(px(&pix, 45, 55), [255, 0, 255, 255], "screen makes magenta");
    }

    #[test]
    fn image_soft_mask_applies_per_sample_alpha() {
        // A solid black image whose /SMask is transparent on its left half
        // and opaque on its right: the left half shows the page, the right
        // half paints, and nothing is a drop anymore.
        let mut smask = [0u8; 64];
        for row in smask.chunks_mut(8) {
            row[4..].fill(0xFF);
        }
        let bytes = small_doc(
            "/XObject << /Im0 5 0 R >>",
            b"q 100 0 0 100 0 0 cm /Im0 Do Q",
            |b| {
                b.stream(
                    5,
                    "/Type /XObject /Subtype /Image /Width 8 /Height 8 \
                     /BitsPerComponent 8 /ColorSpace /DeviceGray /SMask 6 0 R",
                    &[0x00; 64],
                );
                b.stream(
                    6,
                    "/Type /XObject /Subtype /Image /Width 8 /Height 8 \
                     /BitsPerComponent 8 /ColorSpace /DeviceGray",
                    &smask,
                );
            },
        );
        let (pix, report) = render_reporting(bytes);
        assert_eq!(px(&pix, 25, 50), WHITE, "masked-out half shows the page");
        assert_eq!(px(&pix, 75, 50), [0, 0, 0, 255], "kept half paints");
        assert!(report.is_empty(), "an applied mask is not a drop");
    }

    #[test]
    fn annotation_normal_appearance_paints_onto_rect() {
        // A stamp whose /AP /N fills its whole /BBox red must paint the
        // whole /Rect red: BBox [0 0 10 10] scales 4x onto Rect [20 20 60
        // 60], user y 20..60 = device y 40..80 on a 100pt page. The report
        // stays empty — a painted annotation is not a drop.
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
             /Annots [5 0 R] >>",
        );
        b.stream(4, "", b"");
        b.object(
            5,
            "<< /Type /Annot /Subtype /Stamp /Rect [20 20 60 60] /AP << /N 8 0 R >> >>",
        );
        b.stream(
            8,
            "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
            b"1 0 0 rg 0 0 10 10 re f",
        );
        let (pix, report) = render_reporting(b.build(1));
        assert_eq!(px(&pix, 40, 60), RED, "the appearance must fill the rect");
        assert_eq!(px(&pix, 10, 50), WHITE, "outside the rect stays clear");
        assert!(
            report.is_empty(),
            "a painted annotation is not a drop: {:?}",
            report.warnings()
        );
    }

    /// One page, 100pt square, whose `/Annots` array holds `annots` (raw
    /// object bodies added from object number 10 up) — the appearance
    /// streams they reference are given as `(num, dict_extra, content)`.
    fn annots_doc(annots: &[&str], streams: &[(u32, &str, &[u8])]) -> Vec<u8> {
        let mut b = PdfBuilder::new();
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        let refs: Vec<String> = (0..annots.len())
            .map(|i| format!("{} 0 R", 10 + i))
            .collect();
        b.object(
            3,
            &format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 100 100] /Contents 4 0 R \
                 /Annots [{}] >>",
                refs.join(" ")
            ),
        );
        b.stream(4, "", b"");
        for (i, body) in annots.iter().enumerate() {
            b.object(10 + i as u32, body);
        }
        for (num, dict, content) in streams {
            b.stream(*num, dict, content);
        }
        b.build(1)
    }

    #[test]
    fn invisible_and_apless_annotations_stay_silent() {
        // A hidden stamp, a Link with no /AP, and a Popup: none paints,
        // none is a drop.
        let bytes = annots_doc(
            &[
                "<< /Type /Annot /Subtype /Stamp /Rect [20 20 60 60] /F 2 /AP << /N 20 0 R >> >>",
                "<< /Type /Annot /Subtype /Link /Rect [0 0 10 10] >>",
                "<< /Type /Annot /Subtype /Popup /Rect [20 20 60 60] /AP << /N 20 0 R >> >>",
            ],
            &[(
                20,
                "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
                b"1 0 0 rg 0 0 10 10 re f",
            )],
        );
        let (pix, report) = render_reporting(bytes);
        assert_eq!(px(&pix, 40, 60), WHITE, "nothing may paint");
        assert!(report.is_empty(), "none of these is a drop");
    }

    #[test]
    fn appearance_state_dictionary_selects_by_as() {
        // /N is a dictionary of states: /AS picks /On (red). The /Off
        // stream would paint green — a wrong selection is visible.
        let on = "<< /Type /Annot /Subtype /Widget /Rect [20 20 60 60] /AS /On \
                  /AP << /N << /On 20 0 R /Off 21 0 R >> >> >>";
        let streams: &[(u32, &str, &[u8])] = &[
            (
                20,
                "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
                b"1 0 0 rg 0 0 10 10 re f",
            ),
            (
                21,
                "/Type /XObject /Subtype /Form /BBox [0 0 10 10]",
                b"0 1 0 rg 0 0 10 10 re f",
            ),
        ];
        let (pix, report) = render_reporting(annots_doc(&[on], streams));
        assert_eq!(px(&pix, 40, 60), RED, "/AS /On must select the red state");
        assert!(report.is_empty());

        // /AS naming a state that does not exist is a declared appearance
        // lost, and must be reported.
        let missing = "<< /Type /Annot /Subtype /Widget /Rect [20 20 60 60] /AS /Nope \
                       /AP << /N << /On 20 0 R /Off 21 0 R >> >> >>";
        let (pix, report) = render_reporting(annots_doc(&[missing], streams));
        assert_eq!(px(&pix, 40, 60), WHITE);
        assert_eq!(
            drops(&report),
            vec![(SkippedKind::Annotation, SkipReason::Missing, 1)],
        );
    }

    #[test]
    fn appearance_matrix_is_normalized_by_the_rect_fit() {
        // §12.5.5: the form /Matrix participates in the bbox-to-rect fit,
        // so a pure translation cancels out — the appearance lands exactly
        // where the untranslated one does. An implementation that applied
        // /Matrix as a plain CTM would shift the content out of the rect.
        let bytes = annots_doc(
            &["<< /Type /Annot /Subtype /Stamp /Rect [20 20 60 60] /AP << /N 20 0 R >> >>"],
            &[(
                20,
                "/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Matrix [1 0 0 1 500 0]",
                b"1 0 0 rg 0 0 10 10 re f",
            )],
        );
        let (pix, report) = render_reporting(bytes);
        assert_eq!(px(&pix, 40, 60), RED, "translation must cancel in the fit");
        assert!(report.is_empty());
    }

    #[test]
    fn unreadable_appearance_stream_is_reported() {
        // The declared appearance names a filter nobody decodes: the
        // annotation is genuinely lost and the report must say so.
        let bytes = annots_doc(
            &["<< /Type /Annot /Subtype /Stamp /Rect [20 20 60 60] /AP << /N 20 0 R >> >>"],
            &[(
                20,
                "/Type /XObject /Subtype /Form /BBox [0 0 10 10] /Filter /NoSuchFilter",
                b"1 0 0 rg 0 0 10 10 re f",
            )],
        );
        let (pix, report) = render_reporting(bytes);
        assert_eq!(px(&pix, 40, 60), WHITE);
        assert_eq!(
            drops(&report),
            vec![(
                SkippedKind::Annotation,
                SkipReason::UnsupportedFilter("NoSuchFilter".into()),
                1
            )],
        );
    }

    #[test]
    fn non_dictionary_ap_is_reported() {
        // /AP is declared but is not a dictionary: the annotation declared
        // an appearance and lost it, which must be reported — only an
        // absent /AP declares nothing and stays silent.
        let bytes = annots_doc(
            &["<< /Type /Annot /Subtype /Stamp /Rect [20 20 60 60] /AP [1 2 3] >>"],
            &[],
        );
        let (pix, report) = render_reporting(bytes);
        assert_eq!(px(&pix, 40, 60), WHITE);
        assert_eq!(
            drops(&report),
            vec![(SkippedKind::Annotation, SkipReason::Missing, 1)],
        );
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
            ..Default::default()
        };
        render_page_with_options(&doc, &page, 1.0, &opts).expect("render")
    }

    /// Every handle the renderer shares has to be shareable across threads: the
    /// asynchronous render path hands its future to a runtime free to move it
    /// between them, and `Arc<T>` is `Send` only when `T` is `Send + Sync`. All
    /// three of these are plain data, so the handle type was the only thing in the
    /// way.
    ///
    /// `Executor` is deliberately absent. It holds `&Document`, which is `!Sync`
    /// through its `Rc` object cache, and stays absent until the executor becomes
    /// generic over an object source.
    #[test]
    fn every_shared_render_handle_is_shareable_across_threads() {
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<Mask>();
        assert_send_sync::<Type3Font>();
        assert_send_sync::<GlyphFont>();
        assert_send_sync::<Arc<Mask>>();
        assert_send_sync::<Arc<Type3Font>>();
        assert_send_sync::<Arc<GlyphFont>>();
        // The two states that carry those handles through the operator loop, and
        // so through a form or CharProc frame.
        assert_send_sync::<GState>();
        assert_send_sync::<TextState>();
    }

    /// True iff the pixel at `(x, y)` is dark on all three channels.
    fn dark_at(pix: &Pixmap, x: u32, y: u32) -> bool {
        let o = ((y * pix.width + x) * 4) as usize;
        pix.data[o] < 128 && pix.data[o + 1] < 128 && pix.data[o + 2] < 128
    }

    /// Builds a one-page 200x200 doc showing a `/Type3` font (object 5) whose
    /// `/boxglyph` CharProc (object 6) is `charproc`. `font_extra` is spliced
    /// into the font dict — `/FirstChar`+`/Widths`, and `/Resources` for the
    /// fixtures that need the font to carry its own. Code 65 maps to `/boxglyph`
    /// via `/Differences`.
    fn type3_doc(charproc: &str, font_extra: &str, content: &[u8]) -> Vec<u8> {
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
        b.stream(6, "", charproc.as_bytes());
        b.build(1)
    }

    /// A Type3 fixture whose `charproc` paints under `/FirstChar 65 /Widths
    /// [1000]`.
    fn type3_page_doc(charproc: &str, content: &[u8]) -> Vec<u8> {
        type3_doc(charproc, "/FirstChar 65 /Widths [1000]", content)
    }

    /// A Type3 fixture painting the standard box glyph with the given
    /// glyph-space `/Widths` entry for code 65.
    fn type3_page_doc_widths(width: i32, content: &[u8]) -> Vec<u8> {
        type3_doc(
            "1000 0 d0 100 0 500 700 re f",
            &format!("/FirstChar 65 /Widths [{width}]"),
            content,
        )
    }

    /// A Type3 fixture whose CharProc paints the box AND shows code 65 in the
    /// same font, via the font's own `/Resources /F0` pointing back at the font
    /// -- self-referential, so it must be depth-bounded.
    ///
    /// The `/Resources` goes on the **font** dictionary because that is where
    /// ISO 32000-1 9.6.5 puts a Type3 glyph's resources; a CharProc is a content
    /// stream, not a form XObject, and has none of its own. This fixture used to
    /// place it on the CharProc stream's dictionary, where `Type3Font::load` never
    /// looks, and still recursed — because `/F0` also resolves through the page's
    /// chain. It passed for a reason this comment misdescribed.
    fn type3_recursive_doc() -> Vec<u8> {
        type3_doc(
            "1000 0 d0 100 0 500 700 re f BT /F0 100 Tf <41> Tj ET",
            "/FirstChar 65 /Widths [1000] /Resources << /Font << /F0 5 0 R >> >>",
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

    /// A Type3 CharProc resolves names through the **font's** own `/Resources`,
    /// prepended to the surrounding chain (ISO 32000-1 9.6.5). The form XObject
    /// this glyph draws is named nowhere else — the page's `/Resources` carries
    /// only `/Font` — so the box paints only if that prepend happens.
    ///
    /// Nothing covered that before. The one fixture carrying a CharProc
    /// `/Resources` put it on the stream dictionary, which `Type3Font::load` never
    /// reads, and reached its font through the page's chain instead.
    #[test]
    fn a_char_proc_resolves_names_from_the_fonts_own_resources() {
        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        // The page names the font and nothing else, so there is no /XObject entry
        // anywhere in the chain the CharProc would otherwise inherit.
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F0 100 Tf 20 50 Td <41> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
             /FontMatrix [0.001 0 0 0.001 0 0] \
             /Encoding << /Differences [65 /boxglyph] >> \
             /CharProcs << /boxglyph 6 0 R >> /FirstChar 65 /Widths [1000] \
             /Resources << /XObject << /Fx 7 0 R >> >> >>",
        );
        b.stream(6, "", b"1000 0 d0 /Fx Do");
        b.stream(
            7,
            "/Type /XObject /Subtype /Form /BBox [0 0 1000 1000]",
            b"100 0 500 700 re f",
        );
        let pix = render_at_tier(&b.build(1), GlyphPainting::AllEmbedded);
        assert!(
            dark_at(&pix, 55, 115),
            "the CharProc's form must resolve through the font's own /Resources"
        );
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

    // --- Task 3 review-fix: substitution scoped to simple fonts only --------
    //
    // `GlyphFont::load` used to run `Full`-tier substitution unconditionally
    // once every embedded loader had declined, regardless of `/Subtype`. That
    // let a `/Type3` font (whose `FaceRequest::from_font_dict` resolves fine
    // -- nothing there checks `/Subtype`) reach `load_substitute`, so at
    // `Full` + a provider, `ts.font` came back `Some` and `Executor::run`'s
    // `ts.type3 = if ts.font.is_some() { None } else { ... }` never resolved
    // the Type3 font at all -- the CharProcs were silently replaced by a
    // substitute glyph. The fix chains `substitute_at_full` only onto the
    // `TrueType`/`Type1`/`MMType1` arms of `GlyphFont::load`'s match, leaving
    // `Type0` and the `Type3`/unknown `_` catch-all substitution-free.

    /// Writes `bytes` to `basename` inside a freshly created temp directory,
    /// ready to hand to `SubstituteSource::Dir`/`DirProvider` (mirrors
    /// `glyph::tests::write_temp_face`).
    fn write_temp_face(tag: &str, basename: &str, bytes: &[u8]) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "pdfboss-executor-{tag}-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        std::fs::create_dir_all(&dir).expect("create temp dir");
        std::fs::write(dir.join(basename), bytes).expect("write fixture face");
        dir
    }

    #[test]
    fn type3_at_full_with_provider_still_paints_via_charprocs() {
        // The critical guard: code 0x80 is deliberately NOT 'A' (0x41) -- the
        // only code point `truetype::tests::build_font` (used here as the
        // SUBSTITUTE face) maps to a paintable glyph. If substitution ever
        // wrongly fired for this Type3 font, gid 0 (.notdef) would leave the
        // page blank; only the real Type3 CharProc path paints the box.
        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F0 100 Tf 20 50 Td <80> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type3 /FontBBox [0 0 1000 1000] \
             /FontMatrix [0.001 0 0 0.001 0 0] \
             /Encoding << /Differences [128 /boxglyph] >> \
             /CharProcs << /boxglyph 6 0 R >> /FirstChar 128 /Widths [1000] >>",
        );
        b.stream(6, "", b"1000 0 d0 100 0 500 700 re f");
        let bytes = b.build(1);

        let dir = write_temp_face(
            "type3-substitute",
            "Arimo[wght].ttf",
            &crate::truetype::tests::build_font(),
        );

        let doc = Document::load(bytes).expect("load");
        let page = doc.page(0).expect("page");
        let opts = RenderOptions {
            glyph_painting: GlyphPainting::Full,
            substitutes: SubstituteSource::Dir(dir.clone()),
        };
        let pix = render_page_with_options(&doc, &page, 1.0, &opts).expect("render");
        assert!(
            dark_at(&pix, 55, 115),
            "Type3 CharProc box must still paint at Full+provider, not be \
             clobbered by wrongly-fired substitution"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    #[test]
    fn non_embedded_type0_at_full_with_provider_stays_blank() {
        // The important guard: a /Type0 font with no embedded FontFile* at
        // all must never reach substitution -- `load_substitute` builds a
        // 1-byte-per-code table, but Type0 codes under Identity-H are two
        // bytes wide, so if substitution ever fired here, <0041> would
        // mis-split into codes 0x00 and 0x41 (the latter resolving, via
        // StandardEncoding and the substitute's cmap, to the paintable box
        // glyph) and paint stray ink instead of staying blank.
        let mut b = PdfBuilder::new().version(1, 5);
        b.object(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.object(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.object(
            3,
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
             /Resources << /Font << /F0 5 0 R >> >> /Contents 4 0 R >>",
        );
        b.stream(4, "", b"BT /F0 100 Tf 20 50 Td <0041> Tj ET");
        b.object(
            5,
            "<< /Type /Font /Subtype /Type0 /BaseFont /Helvetica \
             /Encoding /Identity-H /DescendantFonts [6 0 R] >>",
        );
        b.object(
            6,
            "<< /Type /Font /Subtype /CIDFontType2 /BaseFont /Helvetica >>",
        );
        let bytes = b.build(1);

        let dir = write_temp_face(
            "type0-substitute",
            "Arimo[wght].ttf",
            &crate::truetype::tests::build_font(),
        );

        let doc = Document::load(bytes).expect("load");
        let page = doc.page(0).expect("page");
        let opts = RenderOptions {
            glyph_painting: GlyphPainting::Full,
            substitutes: SubstituteSource::Dir(dir.clone()),
        };
        let pix = render_page_with_options(&doc, &page, 1.0, &opts).expect("render");
        assert!(
            !dark_at(&pix, 55, 115),
            "non-embedded Type0 must not be substituted into mis-split garbage"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}
